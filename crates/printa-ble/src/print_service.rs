//! Shared print pipeline used by the CLI (and, later, the HTTP server).
//!
//! Owns the marker error types that `main` maps to distinct exit codes, and
//! the connect-print-disconnect flow common to every print path.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use printa_ble_core::protocol::job::{JobStats, PrintJob};
use printa_ble_core::raster::{image_to_bitmap, prepare, Bitmap, Dither};
use tracing::{debug, info};

use crate::ble;
use crate::config::{Config, SavedDevice};

/// How long `connect` keeps scanning for a matching device.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between raster packet writes, in milliseconds.
const INTER_PACKET_DELAY_MS: u64 = 15;

/// No matching printer was discovered before the scan timeout.
///
/// Kept as a distinct type so `main` can map it to its own exit code.
#[derive(Debug)]
pub struct NoPrinterFound;

impl fmt::Display for NoPrinterFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no LX printer found. Is the printer on and in range?")
    }
}

/// A device with the right name was found, but nothing answered on it.
///
/// This is not the same fault as [`NoPrinterFound`], and the difference
/// matters to whoever is standing next to the printer: the radio found the
/// device, so it is in range and paired — it just is not listening. On macOS
/// that is almost always a printer that is switched off, because CoreBluetooth
/// answers `connect` and service discovery out of its cached GATT database for
/// any peripheral it has paired with before.
///
/// Shares [`NoPrinterFound`]'s exit code (2) and HTTP status (503): from a
/// caller's point of view there is still no printer to print on.
#[derive(Debug)]
pub struct PrinterNotResponding {
    /// The advertised name of the device that stayed silent.
    pub name: String,
}

impl PrinterNotResponding {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl fmt::Display for PrinterNotResponding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "found {} but it did not respond — is the printer powered on?",
            self.name
        )
    }
}

/// The printer reported it is out of paper.
///
/// Kept as a distinct type so `main` can map it to its own exit code.
#[derive(Debug)]
pub struct NoPaper;

impl fmt::Display for NoPaper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("printer is out of paper")
    }
}

/// Marker context: authentication or printing failed (exit code 4).
#[derive(Debug)]
pub struct PrintFailure;

impl fmt::Display for PrintFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("print failed")
    }
}

/// Knobs for a print job, independent of what is being printed.
#[derive(Debug, Clone, Copy)]
pub struct PrintOptions {
    /// Print density, 1-7.
    pub density: u8,
    /// Blank feed lines appended after the content.
    pub feed: usize,
    /// Number of copies, 1-20.
    pub copies: u16,
}

/// What a completed print actually did, across every copy.
///
/// Carries the flow-control counters as well as the line count so callers can
/// explain a slow print instead of just reporting success.
#[derive(Debug, Clone, Copy)]
pub struct PrintOutcome {
    /// Total lines printed (bitmap height times copies).
    pub lines: usize,
    /// Counters summed over every copy.
    pub stats: JobStats,
    /// Wall clock from the start of the connect to the last copy finishing.
    pub elapsed: Duration,
}

/// Remember the connected printer in the config file, if it changed.
///
/// Best effort: a failed save warns but never fails the command.
pub fn remember_device(config: &mut Config, printer: &ble::Printer) {
    // A device saved before model support reconnects with `model: None` on
    // file but `Some(..)` here, so it is re-saved once to gain the field.
    let current = SavedDevice {
        id: printer.id(),
        name: printer.name().to_string(),
        model: Some(printer.model().to_string()),
    };
    if config.device.as_ref() != Some(&current) {
        debug!("remembering device {} ({})", current.name, current.id);
        config.device = Some(current);
        if let Err(e) = config.save() {
            eprintln!("warning: failed to save config: {e:#}");
        }
    }
}

/// Decode image bytes into a printer-ready bitmap.
pub fn bitmap_from_image_bytes(bytes: &[u8], dither: Dither) -> anyhow::Result<Bitmap> {
    let img = image::load_from_memory(bytes).context("failed to decode image")?;
    if img.width() == 0 {
        bail!("image has zero width");
    }
    Ok(image_to_bitmap(&prepare(&img), dither))
}

/// Append feed, validate, connect (resolution: explicit > saved > any LX),
/// run `opts.copies` jobs over one connection, remember the device.
///
/// Known wart: progress ("Connected to …", "Printed copy i/N.") is printed
/// directly to stdout/stderr to preserve exact CLI behavior. The server task
/// should move reporting out to the caller.
pub async fn print_bitmap(
    mut bitmap: Bitmap,
    explicit_device: Option<&str>,
    opts: PrintOptions,
) -> anyhow::Result<PrintOutcome> {
    let started = Instant::now();
    bitmap.extend_blank(opts.feed);
    debug!(
        "print job: {} lines, density {}, feed {}, {} copies",
        bitmap.height(),
        opts.density,
        opts.feed,
        opts.copies
    );

    // Validate the job before touching BLE so an oversized bitmap fails fast.
    PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
        .context("cannot print this job")?;

    let mut config = Config::load();
    let mut printer =
        ble::connect_resolved(explicit_device, config.device.as_ref(), SCAN_TIMEOUT).await?;
    // Earned, not assumed: `connect_resolved` only returns once the printer
    // has answered a hello frame of its own accord.
    eprintln!("Connected to {}.", printer.name());
    remember_device(&mut config, &printer);

    // Pre-print check, best effort: status frames arrive unsolicited after
    // subscribing, but not receiving one is not fatal.
    match printer.wait_status(Duration::from_secs(3)).await {
        Ok(s) => {
            debug!(
                "pre-print status: battery {}%, paper {}",
                s.battery_pct,
                if s.no_paper { "out" } else { "ok" }
            );
            if s.no_paper {
                printer.disconnect().await;
                return Err(anyhow::Error::msg(NoPaper));
            }
            if s.low_battery {
                eprintln!("warning: printer battery is low");
            }
        }
        Err(e) => debug!("no pre-print status frame: {e:#}"),
    }

    // One connection, one full job (fresh challenge, auth included) per copy.
    let mut stats = JobStats::default();
    for copy in 1..=opts.copies {
        let mut job = PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
            .context("cannot print this job")?;
        if opts.copies > 1 {
            info!("printing copy {copy}/{}", opts.copies);
        }
        match printer.run_job(&mut job).await {
            Ok(s) => stats = add_stats(stats, s),
            Err(e) => {
                printer.disconnect().await;
                return Err(e.context(PrintFailure));
            }
        }
        if opts.copies > 1 {
            println!("Printed copy {copy}/{}.", opts.copies);
        }
    }
    printer.disconnect().await;

    Ok(PrintOutcome {
        lines: bitmap.height() * usize::from(opts.copies),
        stats,
        elapsed: started.elapsed(),
    })
}

/// Sum two copies' worth of counters.
fn add_stats(a: JobStats, b: JobStats) -> JobStats {
    JobStats {
        packets_sent: a.packets_sent.saturating_add(b.packets_sent),
        retransmits: a.retransmits.saturating_add(b.retransmits),
        holds: a.holds.saturating_add(b.holds),
        cooldowns: a.cooldowns.saturating_add(b.cooldowns),
    }
}
