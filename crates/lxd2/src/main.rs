mod ble;
mod cli;
mod config;

use std::fmt;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context as _};
use clap::Parser;
use lxd2_core::protocol::job::PrintJob;
use lxd2_core::raster::{
    bitmap_to_png, image_to_bitmap, prepare, render_markdown, render_qr, render_text, Bitmap,
    Dither,
};

use crate::ble::{NoPaper, NoPrinterFound};
use crate::cli::{Cli, Command, DeviceArgs, PrintArgs, QrArgs};
use crate::config::{Config, SavedDevice};

/// How long `connect` keeps scanning for a matching device.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between raster packet writes, in milliseconds.
const INTER_PACKET_DELAY_MS: u64 = 15;

/// Marker context: authentication or printing failed (exit code 4).
#[derive(Debug)]
struct PrintFailure;

impl fmt::Display for PrintFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("print failed")
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            exit_code(&e)
        }
    };
    std::process::exit(code);
}

/// Distinct exit codes: 2 no printer found, 3 no paper, 4 auth/print
/// failure, 1 anything else.
fn exit_code(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<NoPrinterFound>().is_some() {
        2
    } else if e.downcast_ref::<NoPaper>().is_some() {
        3
    } else if e.downcast_ref::<PrintFailure>().is_some() {
        4
    } else {
        1
    }
}

async fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Scan { timeout } => cmd_scan(timeout).await,
        Command::Status(device) => cmd_status(device).await.map(|()| 0),
        Command::Print(args) => cmd_print(args).await.map(|()| 0),
        Command::Qr(args) => cmd_qr(args).await.map(|()| 0),
    }
}

async fn cmd_scan(timeout: u64) -> anyhow::Result<i32> {
    let found = ble::scan(Duration::from_secs(timeout)).await?;
    if found.is_empty() {
        eprintln!("No LX printers found. Is the printer on?");
        // Same exit code as a failed connect: no printer found.
        return Ok(2);
    }
    println!("{:<20} ID", "NAME");
    for (name, id) in &found {
        println!("{name:<20} {id}");
    }
    Ok(0)
}

/// Remember the connected printer in the config file, if it changed.
///
/// Best effort: a failed save warns but never fails the command.
fn remember_device(config: &mut Config, printer: &ble::Printer) {
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

async fn cmd_status(device: DeviceArgs) -> anyhow::Result<()> {
    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        device.device.as_deref(),
        config.device.as_ref(),
        SCAN_TIMEOUT,
    )
    .await?;
    eprintln!("Connected to {}.", printer.name());
    remember_device(&mut config, &printer);
    let status = printer.wait_status(Duration::from_secs(5)).await;
    printer.disconnect().await;
    let s = status?;

    let charge = if s.charging {
        " (charging)"
    } else if s.charged {
        " (charged)"
    } else {
        ""
    };
    println!("Battery:  {}%{charge}", s.battery_pct);
    println!("Paper:    {}", if s.no_paper { "OUT" } else { "OK" });
    if let Some(d) = s.density {
        println!("Density:  {d}");
    }
    if let Some(mv) = s.voltage_mv {
        println!("Voltage:  {:.2} V", f32::from(mv) / 1000.0);
    }
    if s.overheat {
        println!("Warning:  print head is overheating");
    }
    if s.low_battery {
        println!("Warning:  battery is low");
    }
    Ok(())
}

async fn cmd_print(args: PrintArgs) -> anyhow::Result<()> {
    let PrintArgs {
        device,
        text,
        file,
        density,
        feed,
        dither,
        size,
        preview,
        copies,
    } = args;
    let bitmap = build_bitmap(text, file, dither.into(), size)?;
    dispatch(bitmap, device, density, feed, preview, copies).await
}

async fn cmd_qr(args: QrArgs) -> anyhow::Result<()> {
    let QrArgs {
        data,
        caption,
        device,
        density,
        feed,
        preview,
        copies,
    } = args;
    let bitmap = render_qr(&data, caption.as_deref()).context("cannot render QR code")?;
    dispatch(bitmap, device, density, feed, preview, copies).await
}

/// Common print tail: append feed, preview or connect, and print `copies`
/// jobs over a single connection.
async fn dispatch(
    mut bitmap: Bitmap,
    device: DeviceArgs,
    density: u8,
    feed: usize,
    preview: Option<PathBuf>,
    copies: u16,
) -> anyhow::Result<()> {
    bitmap.extend_blank(feed);

    if let Some(path) = preview {
        if copies > 1 {
            eprintln!("note: preview renders a single copy; --copies is ignored");
        }
        std::fs::write(&path, bitmap_to_png(&bitmap))
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("{}", path.display());
        return Ok(());
    }

    // Validate the job before touching BLE so an oversized bitmap fails fast.
    PrintJob::new(&bitmap, density, rand::random(), INTER_PACKET_DELAY_MS)
        .context("cannot print this job")?;

    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        device.device.as_deref(),
        config.device.as_ref(),
        SCAN_TIMEOUT,
    )
    .await?;
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
    for copy in 1..=copies {
        let mut job = PrintJob::new(&bitmap, density, rand::random(), INTER_PACKET_DELAY_MS)
            .context("cannot print this job")?;
        if let Err(e) = printer.run_job(&mut job).await {
            printer.disconnect().await;
            return Err(e.context(PrintFailure));
        }
        if copies > 1 {
            println!("Printed copy {copy}/{copies}.");
        }
    }
    printer.disconnect().await;

    if copies == 1 {
        println!("Printed {} lines.", bitmap.height());
    }
    Ok(())
}

/// Build the bitmap to print from the text argument, a file, or stdin.
fn build_bitmap(
    text: Option<String>,
    file: Option<PathBuf>,
    dither: Dither,
    size: f32,
) -> anyhow::Result<Bitmap> {
    if let Some(path) = file {
        if text.is_some() {
            bail!("cannot combine a text argument with --file");
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        return match ext.as_str() {
            "png" | "jpg" | "jpeg" => {
                let img = image::open(&path)
                    .with_context(|| format!("failed to open {}", path.display()))?;
                if img.width() == 0 {
                    bail!("image has zero width");
                }
                Ok(image_to_bitmap(&prepare(&img), dither))
            }
            "txt" => {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                text_bitmap(&text, size)
            }
            "md" | "markdown" => {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                if text.trim().is_empty() {
                    bail!("nothing to print");
                }
                Ok(render_markdown(&text))
            }
            _ => bail!(
                "unsupported file type: {} (expected .png, .jpg, .jpeg, .txt, .md or .markdown)",
                path.display()
            ),
        };
    }

    let text = match text {
        Some(t) => t,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read stdin")?;
            buf
        }
    };
    text_bitmap(&text, size)
}

fn text_bitmap(text: &str, size: f32) -> anyhow::Result<Bitmap> {
    if text.trim().is_empty() {
        bail!("nothing to print");
    }
    Ok(render_text(text, size))
}
