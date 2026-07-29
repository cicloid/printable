mod ble;
#[cfg(feature = "url")]
mod chrome;
mod cli;
mod config;
mod print_service;
mod server;

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context as _};
use clap::Parser;
use printa_ble_core::raster::{
    bitmap_to_png, render_markdown, render_qr, render_text, Bitmap, Dither,
};

use crate::cli::{Cli, Command, DeviceArgs, PrintArgs, QrArgs};
use crate::config::Config;
use crate::print_service::{NoPaper, NoPrinterFound, PrintFailure, PrintOptions, SCAN_TIMEOUT};

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
        Command::Serve { port, bind, device } => {
            server::serve(&bind, port, device.device).await.map(|()| 0)
        }
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
    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        device.device.as_deref(),
        config.device.as_ref(),
        SCAN_TIMEOUT,
    )
    .await?;
    eprintln!("Connected to {}.", printer.name());
    print_service::remember_device(&mut config, &printer);
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
    let bitmap = build_bitmap(&args).await?;
    let PrintArgs {
        device,
        density,
        feed,
        preview,
        copies,
        ..
    } = args;
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

/// Common print tail: preview short-circuit, else hand off to the shared
/// print service (append feed, connect, print `copies` jobs).
async fn dispatch(
    mut bitmap: Bitmap,
    device: DeviceArgs,
    density: u8,
    feed: usize,
    preview: Option<PathBuf>,
    copies: u16,
) -> anyhow::Result<()> {
    if let Some(path) = preview {
        if copies > 1 {
            eprintln!("note: preview renders a single copy; --copies is ignored");
        }
        bitmap.extend_blank(feed);
        std::fs::write(&path, bitmap_to_png(&bitmap))
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("{}", path.display());
        return Ok(());
    }

    let lines = print_service::print_bitmap(
        bitmap,
        device.device.as_deref(),
        PrintOptions {
            density,
            feed,
            copies,
        },
    )
    .await?;
    if copies == 1 {
        println!("Printed {lines} lines.");
    }
    Ok(())
}

/// Build the bitmap to print from the text argument, a file, a URL, or stdin.
async fn build_bitmap(args: &PrintArgs) -> anyhow::Result<Bitmap> {
    let dither: Dither = args.dither.into();
    let size = args.size;

    #[cfg(feature = "url")]
    if let Some(url) = &args.url {
        let png = chrome::render_url_png(url).await?;
        return print_service::bitmap_from_image_bytes(&png, dither);
    }

    if let Some(path) = &args.file {
        if args.text.is_some() {
            bail!("cannot combine a text argument with --file");
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        return match ext.as_str() {
            "png" | "jpg" | "jpeg" => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("failed to open {}", path.display()))?;
                print_service::bitmap_from_image_bytes(&bytes, dither)
            }
            "txt" => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                text_bitmap(&text, size)
            }
            "md" | "markdown" => {
                let text = std::fs::read_to_string(path)
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

    match &args.text {
        Some(t) => text_bitmap(t, size),
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("failed to read stdin")?;
            text_bitmap(&buf, size)
        }
    }
}

fn text_bitmap(text: &str, size: f32) -> anyhow::Result<Bitmap> {
    if text.trim().is_empty() {
        bail!("nothing to print");
    }
    Ok(render_text(text, size))
}
