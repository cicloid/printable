mod ble;
#[cfg(feature = "url")]
mod chrome;
mod cli;
mod config;
mod ipp_command;
mod md_images;
mod print_service;
mod server;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context as _};
use clap::Parser;
use printa_ble_core::raster::{
    bitmap_to_png, render_markdown_with, render_qr, render_text, Bitmap, Dither,
};

use crate::cli::{Cli, Command, DeviceArgs, PrintArgs, QrArgs};
use crate::config::Config;
use crate::print_service::{
    NoPaper, NoPrinterFound, PrintFailure, PrintOptions, PrinterNotResponding, SCAN_TIMEOUT,
};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            exit_code(&e)
        }
    };
    std::process::exit(code);
}

/// Install the log subscriber for this run.
///
/// Logs go to stderr, never stdout: stdout carries the command's actual output
/// (the preview path, the scan table) and scripts parse it. The default filter
/// is `printable=warn` — this crate's warnings and nothing else, so an
/// unflagged invocation prints exactly what it always did and no dependency
/// gets to editorialize. `RUST_LOG` wins over `-v` when set.
fn init_tracing(verbose: u8) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(cli::log_filter(verbose)));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Distinct exit codes: 2 no usable printer (none found, or one found that
/// never answered), 3 no paper, 4 auth/print failure, 1 anything else.
fn exit_code(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<NoPrinterFound>().is_some()
        || e.downcast_ref::<PrinterNotResponding>().is_some()
    {
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
        Command::Scan { timeout, all } => cmd_scan(timeout, all).await,
        Command::Status(device) => cmd_status(device).await.map(|()| 0),
        Command::Print(args) => cmd_print(args).await.map(|()| 0),
        Command::Qr(args) => cmd_qr(args).await.map(|()| 0),
        Command::Serve {
            port,
            bind,
            no_remote_images,
            device,
        } => server::serve(&bind, port, device.device, device.model, !no_remote_images)
            .await
            .map(|()| 0),
        Command::Ipp(args) => ipp_command::run(args).await,
    }
}

async fn cmd_scan(timeout: u64, all: bool) -> anyhow::Result<i32> {
    let timeout = Duration::from_secs(timeout);
    if all {
        return cmd_scan_all(timeout).await;
    }
    let found = ble::scan(timeout).await?;
    if found.is_empty() {
        eprintln!("No supported printers found. Is the printer on?");
        // Same exit code as a failed connect: no printer found.
        return Ok(2);
    }
    println!("{:<20} {:<8} ID", "NAME", "MODEL");
    for (name, id, model) in &found {
        println!("{name:<20} {model:<8} {id}");
    }
    Ok(0)
}

/// The `scan --all` diagnostic: every advertiser seen, recognized printers
/// first, so a printer shipping under an arbitrary name can be picked out by
/// its advertised services (a cat-family printer shows `0xAF30`).
async fn cmd_scan_all(timeout: Duration) -> anyhow::Result<i32> {
    let seen = ble::scan_all(timeout).await?;
    if seen.is_empty() {
        eprintln!("No BLE devices seen. Is Bluetooth on?");
        // Same exit code as an empty plain scan: nothing usable found.
        return Ok(2);
    }
    println!("{:<24} {:<8} {:<38} SERVICES", "NAME", "MODEL", "ID");
    for d in &seen {
        let name = d.name.as_deref().unwrap_or("(no name)");
        let model = d.model.map_or_else(|| "-".to_string(), |m| m.to_string());
        let services = if d.services.is_empty() {
            "-".to_string()
        } else {
            d.services
                .iter()
                .map(|u| ble::format_service_uuid(u))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!("{name:<24} {model:<8} {:<38} {services}", d.id);
    }
    Ok(0)
}

async fn cmd_status(device: DeviceArgs) -> anyhow::Result<()> {
    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        device.device.as_deref(),
        config.device.as_ref(),
        device.model,
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

    let outcome = print_service::print_bitmap(
        bitmap,
        device.device.as_deref(),
        device.model,
        PrintOptions {
            density,
            feed,
            copies,
        },
    )
    .await?;
    if copies == 1 {
        println!("Printed {} lines.", outcome.lines);
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
        // `-f -` is the Unix spelling of "read stdin"; there is no extension to
        // dispatch on, so it takes the same route as a bare pipe.
        if is_stdin_path(path) {
            return inline_bitmap(&read_stdin()?, args.markdown, size).await;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        return match ext.as_str() {
            "png" | "jpg" | "jpeg" => {
                if args.markdown {
                    bail!(
                        "--markdown does not apply to an image file ({})",
                        path.display()
                    );
                }
                let bytes = std::fs::read(path)
                    .with_context(|| format!("failed to open {}", path.display()))?;
                print_service::bitmap_from_image_bytes(&bytes, dither)
            }
            "txt" => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                if args.markdown {
                    markdown_bitmap(&text, path.parent()).await
                } else {
                    text_bitmap(&text, size)
                }
            }
            // `--markdown` is redundant here — this is already the markdown
            // path — so it is accepted without comment.
            "md" | "markdown" => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                markdown_bitmap(&text, path.parent()).await
            }
            _ => bail!(
                "unsupported file type: {} (expected .png, .jpg, .jpeg, .txt, .md or .markdown)",
                path.display()
            ),
        };
    }

    let text = match &args.text {
        Some(t) => t.clone(),
        None => read_stdin()?,
    };
    inline_bitmap(&text, args.markdown, size).await
}

/// Does this `--file` value mean stdin? Only a bare `-`; `./-` is a real file.
fn is_stdin_path(path: &Path) -> bool {
    path.as_os_str() == "-"
}

fn read_stdin() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("failed to read stdin")?;
    Ok(buf)
}

/// Render text that arrived without a file behind it — stdin, `-f -`, or a
/// positional argument.
///
/// `--markdown` is the only way to reach the markdown renderer here: there is
/// no filename, so nothing else could tell a document from literal text.
async fn inline_bitmap(text: &str, markdown: bool, size: f32) -> anyhow::Result<Bitmap> {
    if markdown {
        // No source file means no directory to anchor relative image
        // references to, so they resolve against the working directory — what
        // `![](logo.png)` means to someone piping a document from their shell.
        let cwd = std::env::current_dir().ok();
        markdown_bitmap(text, cwd.as_deref()).await
    } else {
        text_bitmap(text, size)
    }
}

/// Render a markdown document, resolving its image references first.
async fn markdown_bitmap(text: &str, base_dir: Option<&Path>) -> anyhow::Result<Bitmap> {
    if text.trim().is_empty() {
        bail!("nothing to print");
    }
    // Relative refs resolve against `base_dir`. Local reads are allowed here:
    // this is the user's own shell and filesystem.
    let images = md_images::resolve(
        text, base_dir, /* local */ true, /* remote */ true,
    )
    .await;
    Ok(render_markdown_with(text, &images))
}

fn text_bitmap(text: &str, size: f32) -> anyhow::Result<Bitmap> {
    if text.trim().is_empty() {
        bail!("nothing to print");
    }
    Ok(render_text(text, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Heading\n\nA paragraph with **bold** in it.\n";

    fn pixels(bitmap: &Bitmap) -> Vec<u8> {
        (0..bitmap.height())
            .flat_map(|y| bitmap.row(y).iter().copied())
            .collect()
    }

    /// The user's actual request: the same piped bytes must render as a
    /// document with `-m` and as literal source without it.
    #[tokio::test]
    async fn markdown_flag_changes_how_piped_text_renders() {
        let md = inline_bitmap(DOC, true, 24.0).await.unwrap();
        let plain = inline_bitmap(DOC, false, 24.0).await.unwrap();
        assert_ne!(pixels(&md), pixels(&plain));
    }

    /// A heading is set in a larger face than body text, so it occupies more
    /// rows as markdown than the same characters typed out literally.
    #[tokio::test]
    async fn markdown_heading_renders_taller_than_the_same_text_plain() {
        let md = inline_bitmap("# Heading", true, 24.0).await.unwrap();
        let plain = inline_bitmap("# Heading", false, 24.0).await.unwrap();
        assert!(
            md.height() > plain.height(),
            "markdown {} rows, plain {} rows",
            md.height(),
            plain.height()
        );
    }

    #[tokio::test]
    async fn empty_input_refuses_to_print_in_either_mode() {
        for markdown in [true, false] {
            assert!(inline_bitmap("   \n\t\n", markdown, 24.0).await.is_err());
        }
    }

    /// A document's own directory still anchors its relative image refs; only
    /// input with no file behind it falls back to the working directory
    /// (pinned end to end in `tests/stdin_markdown.rs`).
    #[tokio::test]
    async fn markdown_resolves_images_against_the_given_base_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut logo = Bitmap::new(40);
        for x in 0..384 {
            logo.set(x, 20, true);
        }
        std::fs::write(dir.path().join("logo.png"), bitmap_to_png(&logo)).unwrap();

        let resolved = markdown_bitmap("![logo](logo.png)", Some(dir.path()))
            .await
            .unwrap();
        let placeholder = markdown_bitmap("![logo](logo.png)", None).await.unwrap();
        assert!(
            resolved.height() > placeholder.height(),
            "resolved {} rows, placeholder {} rows",
            resolved.height(),
            placeholder.height()
        );
    }

    /// A printer that is present but never answers is still "no usable
    /// printer": a script testing for exit 2 must catch it there, not in the
    /// catch-all.
    #[test]
    fn exit_codes_separate_the_failure_modes() {
        use crate::print_service::PrinterNotResponding;
        assert_eq!(exit_code(&anyhow::Error::msg(NoPrinterFound)), 2);
        assert_eq!(
            exit_code(&anyhow::Error::msg(PrinterNotResponding::new("LX-D02"))),
            2
        );
        assert_eq!(exit_code(&anyhow::Error::msg(NoPaper)), 3);
        assert_eq!(exit_code(&anyhow::anyhow!("x").context(PrintFailure)), 4);
        assert_eq!(exit_code(&anyhow::anyhow!("x")), 1);
    }

    #[test]
    fn only_a_bare_dash_means_stdin() {
        assert!(is_stdin_path(Path::new("-")));
        assert!(!is_stdin_path(Path::new("./-")));
        assert!(!is_stdin_path(Path::new("-.md")));
        assert!(!is_stdin_path(Path::new("notes.md")));
    }
}
