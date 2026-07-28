mod ble;
mod cli;

use std::fmt;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context as _};
use clap::Parser;
use lxd2_core::protocol::job::PrintJob;
use lxd2_core::raster::{bitmap_to_png, image_to_bitmap, prepare, render_text, Bitmap, Dither};

use crate::ble::{NoPaper, NoPrinterFound};
use crate::cli::{Cli, Command, DeviceArgs, PrintArgs};

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

async fn cmd_status(device: DeviceArgs) -> anyhow::Result<()> {
    let mut printer = ble::connect(device.device.as_deref(), SCAN_TIMEOUT).await?;
    eprintln!("Connected to {}.", printer.name());
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
    } = args;
    let mut bitmap = build_bitmap(text, file, dither.into(), size)?;
    bitmap.extend_blank(feed);

    if let Some(path) = preview {
        std::fs::write(&path, bitmap_to_png(&bitmap))
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("{}", path.display());
        return Ok(());
    }

    let mut printer = ble::connect(device.device.as_deref(), SCAN_TIMEOUT).await?;
    eprintln!("Connected to {}.", printer.name());

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

    let mut job = PrintJob::new(&bitmap, density, rand::random(), INTER_PACKET_DELAY_MS);
    let result = printer.run_job(&mut job).await;
    printer.disconnect().await;
    result.map_err(|e| e.context(PrintFailure))?;

    println!("Printed {} lines.", bitmap.height());
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
            _ => bail!(
                "unsupported file type: {} (expected .png, .jpg, .jpeg or .txt)",
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
