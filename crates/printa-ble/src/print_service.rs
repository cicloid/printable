//! Shared print pipeline used by the CLI (and, later, the HTTP server).
//!
//! Owns the marker error types that `main` maps to distinct exit codes, and
//! the connect-print-disconnect flow common to every print path.

use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context as _};
use printa_ble_core::protocol::job::PrintJob;
use printa_ble_core::raster::{image_to_bitmap, prepare, Bitmap, Dither};

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

/// Remember the connected printer in the config file, if it changed.
///
/// Best effort: a failed save warns but never fails the command.
pub fn remember_device(config: &mut Config, printer: &ble::Printer) {
    let current = SavedDevice {
        id: printer.id(),
        name: printer.name().to_string(),
    };
    if config.device.as_ref() != Some(&current) {
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
/// Returns the total number of lines printed.
///
/// Known wart: progress ("Connected to …", "Printed copy i/N.") is printed
/// directly to stdout/stderr to preserve exact CLI behavior. The server task
/// should move reporting out to the caller.
pub async fn print_bitmap(
    mut bitmap: Bitmap,
    explicit_device: Option<&str>,
    opts: PrintOptions,
) -> anyhow::Result<usize> {
    bitmap.extend_blank(opts.feed);

    // Validate the job before touching BLE so an oversized bitmap fails fast.
    PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
        .context("cannot print this job")?;

    let mut config = Config::load();
    let mut printer =
        ble::connect_resolved(explicit_device, config.device.as_ref(), SCAN_TIMEOUT).await?;
    eprintln!("Connected to {}.", printer.name());
    remember_device(&mut config, &printer);

    // Pre-print check, best effort: status frames arrive unsolicited after
    // subscribing, but not receiving one is not fatal.
    if let Ok(s) = printer.wait_status(Duration::from_secs(3)).await {
        if s.no_paper {
            printer.disconnect().await;
            return Err(anyhow::Error::msg(NoPaper));
        }
        if s.low_battery {
            eprintln!("warning: printer battery is low");
        }
    }

    // One connection, one full job (fresh challenge, auth included) per copy.
    for copy in 1..=opts.copies {
        let mut job = PrintJob::new(&bitmap, opts.density, rand::random(), INTER_PACKET_DELAY_MS)
            .context("cannot print this job")?;
        if let Err(e) = printer.run_job(&mut job).await {
            printer.disconnect().await;
            return Err(e.context(PrintFailure));
        }
        if opts.copies > 1 {
            println!("Printed copy {copy}/{}.", opts.copies);
        }
    }
    printer.disconnect().await;

    Ok(bitmap.height() * usize::from(opts.copies))
}
