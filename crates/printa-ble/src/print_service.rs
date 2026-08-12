//! Shared print pipeline used by the CLI (and, later, the HTTP server).
//!
//! Owns the marker error types that `main` maps to distinct exit codes, and
//! the connect-print-disconnect flow common to every print path.

use std::fmt;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use printa_ble_core::model::PrinterModel;
use printa_ble_core::protocol::job::{JobStats, PrintJob};
use printa_ble_core::protocol_x6::job::X6PrintJob;
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
        f.write_str("no supported printer found. Is the printer on and in range?")
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
    /// Print density, 1-7. On the LX-D02 this is the `5A 0C` density
    /// command; on the X6 it maps to feed speed and printhead energy (see
    /// `protocol_x6::job::density_to_speed` and `density_to_energy`).
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
    let current = device_record(printer.id(), printer.name(), printer.detected_model());
    if config.device.as_ref() != Some(&current) {
        debug!("remembering device {} ({})", current.name, current.id);
        config.device = Some(current);
        if let Err(e) = config.save() {
            eprintln!("warning: failed to save config: {e:#}");
        }
    }
}

/// The [`SavedDevice`] a connected printer is remembered as.
///
/// Saves the model the advertised name *identified*, not the model the
/// connection was driven as: an explicit `--device` filter can match a name
/// no model claims, and the LX-D02 that `connect_resolved` then assumes is a
/// guess. Recording that guess would make the next flagless run restrict its
/// scan to LX-D02 — a restriction the unclaimed name can never satisfy, so
/// the saved device would become permanently unreachable. Saving `None`
/// instead leaves the reconnect to name detection and saved-id matching,
/// exactly as before models were remembered.
///
/// A device saved before model support existed reconnects with `model: None`
/// on file; if its name identifies a model, it is re-saved once to gain the
/// field.
fn device_record(id: String, name: &str, detected: Option<PrinterModel>) -> SavedDevice {
    SavedDevice {
        id,
        name: name.to_string(),
        model: detected.map(|m| m.to_string()),
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

/// Connect (resolution: explicit > saved > any supported printer, optionally
/// restricted to `model` — see `ble::connect_resolved`), then run
/// `opts.copies` jobs over one connection with the connected model's
/// protocol, and remember the device.
///
/// Feed handling and validation are per-model, so both happen after connect,
/// once the model is known: the LX appends `opts.feed` blank raster lines to
/// the bitmap, the X6 sends the feed as its own command. An oversized LX job
/// is therefore rejected after the connect rather than before it — same
/// error, later.
///
/// Known wart: progress ("Connected to …", "Printed copy i/N.") is printed
/// directly to stdout/stderr to preserve exact CLI behavior. The server task
/// should move reporting out to the caller.
pub async fn print_bitmap(
    mut bitmap: Bitmap,
    explicit_device: Option<&str>,
    model: Option<PrinterModel>,
    opts: PrintOptions,
) -> anyhow::Result<PrintOutcome> {
    let started = Instant::now();
    // Refuse an empty bitmap before touching BLE: with content of height 0
    // an X6 job would still consume paper (its blank lead row plus the
    // feed), and an LX job would print only the feed. Defensive — every
    // existing caller already rejects empty content upstream ("nothing to
    // print" in the CLI, 400s in the server, zero-width images in
    // `bitmap_from_image_bytes`) — so no caller-observable behavior changes.
    if bitmap.height() == 0 {
        bail!("nothing to print: bitmap is empty");
    }
    debug!(
        "print job: {} lines, density {}, feed {}, {} copies",
        bitmap.height(),
        opts.density,
        opts.feed,
        opts.copies
    );

    let mut config = Config::load();
    let mut printer =
        ble::connect_resolved(explicit_device, config.device.as_ref(), model, SCAN_TIMEOUT).await?;
    // Earned, not assumed: `connect_resolved` only returns once the printer
    // has answered a hello frame of its own accord. (X6: subscribed only —
    // the protocol has no liveness probe; see `ble::initialize`.)
    eprintln!("Connected to {}.", printer.name());
    remember_device(&mut config, &printer);

    let mut stats = JobStats::default();
    match printer.model() {
        PrinterModel::LxD02 => {
            // The feed rides along as blank raster lines on this model.
            bitmap.extend_blank(opts.feed);

            // Validate before the paper check so an oversized job fails
            // without sitting out the status wait.
            if let Err(e) =
                PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
            {
                printer.disconnect().await;
                return Err(anyhow::Error::new(e).context("cannot print this job"));
            }

            // Pre-print check, best effort: status frames arrive unsolicited
            // after subscribing, but not receiving one is not fatal. LX-D02
            // only — the X6 has no paper (or any other status) signal, so
            // there is nothing to wait for there.
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

            // One connection, one full job (fresh challenge, auth included)
            // per copy.
            for copy in 1..=opts.copies {
                let mut job =
                    PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
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
        }
        PrinterModel::X6 => {
            // No `extend_blank`: the X6 feed is a command carrying a pixel
            // count, sent by the job after the raster. `opts.density` maps
            // to printhead energy — see `PrintOptions::density`.
            for copy in 1..=opts.copies {
                let mut job = X6PrintJob::new(
                    &bitmap,
                    opts.density,
                    feed_px(opts.feed),
                    INTER_PACKET_DELAY_MS,
                );
                if opts.copies > 1 {
                    info!("printing copy {copy}/{}", opts.copies);
                }
                match printer.run_x6_job(&mut job).await {
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
        }
    }
    printer.disconnect().await;

    Ok(PrintOutcome {
        // On the LX the feed lines are part of the bitmap by now; on the X6
        // they are fed, not printed, and are not counted.
        lines: bitmap.height() * usize::from(opts.copies),
        stats,
        elapsed: started.elapsed(),
    })
}

/// Clamp a feed request to the u16 the X6 feed command carries. The CLI
/// deliberately leaves `--feed` unbounded, so out-of-range saturates rather
/// than failing a job over blank paper.
fn feed_px(feed: usize) -> u16 {
    u16::try_from(feed).unwrap_or(u16::MAX)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The X6 feed command carries a u16; a CLI `--feed` beyond that must
    /// saturate rather than wrap or panic (the CLI deliberately leaves
    /// `feed` unbounded, unlike the server).
    #[test]
    fn feed_clamps_to_the_x6_command_range() {
        assert_eq!(feed_px(0), 0);
        assert_eq!(feed_px(320), 320);
        assert_eq!(feed_px(65_535), u16::MAX);
        assert_eq!(feed_px(65_536), u16::MAX);
        assert_eq!(feed_px(usize::MAX), u16::MAX);
    }

    /// A device whose name no model claims (reachable only via an explicit
    /// `--device` filter) is driven as an LX-D02 by assumption — but that
    /// guess must not be written to the config as fact. Saving
    /// `model: "lx-d02"` would restrict the next flagless scan to LX-D02,
    /// which the unclaimed name can never satisfy, making the saved device
    /// permanently unreachable.
    #[test]
    fn an_unclaimed_name_is_remembered_without_a_model() {
        let record = device_record("aabbccdd".to_string(), "Oddball", None);
        assert_eq!(
            record,
            SavedDevice {
                id: "aabbccdd".to_string(),
                name: "Oddball".to_string(),
                model: None,
            }
        );
    }

    /// A detected model is still remembered, so a reconnect to a recognized
    /// printer keeps restricting the scan to its family.
    #[test]
    fn a_detected_model_is_remembered() {
        let record = device_record("x6-id".to_string(), "X6h-A1B2", Some(PrinterModel::X6));
        assert_eq!(record.model.as_deref(), Some("x6"));
        let record = device_record("lx-id".to_string(), "LX-D02", Some(PrinterModel::LxD02));
        assert_eq!(record.model.as_deref(), Some("lx-d02"));
    }
}
