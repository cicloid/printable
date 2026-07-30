//! Command-line interface definitions.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "printable",
    about = "Print to LX-D02/LX-D2 BLE thermal printers",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Increase logging on stderr: -v info, -vv debug, -vvv trace.
    /// `RUST_LOG` overrides this entirely.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
}

#[derive(Subcommand)]
pub enum Command {
    /// List nearby LX printers
    Scan {
        /// Seconds to scan
        #[arg(long, default_value_t = 5)]
        timeout: u64,
    },
    /// Show printer status (battery, paper, density)
    Status(DeviceArgs),
    /// Print text (arg or stdin) or a file
    Print(PrintArgs),
    /// Print a QR code
    Qr(QrArgs),
    /// Run the HTTP print server (REST API + web UI)
    Serve {
        /// Port to listen on
        #[arg(long, default_value_t = 8000)]
        port: u16,
        /// Address to bind (0.0.0.0 = every interface, for LAN printing)
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// Never fetch http(s) images referenced by markdown (removes the
        /// server's outbound request surface: no SSRF, no fetch amplification).
        /// Local file references are already refused either way.
        #[arg(long)]
        no_remote_images: bool,
        #[command(flatten)]
        device: DeviceArgs,
    },
}

#[derive(clap::Args)]
pub struct PrintArgs {
    #[command(flatten)]
    pub device: DeviceArgs,
    /// Text to print; reads stdin if omitted and no --file
    pub text: Option<String>,
    /// File to print (.png/.jpg/.jpeg/.txt/.md/.markdown), or `-` for stdin
    #[arg(short, long)]
    pub file: Option<std::path::PathBuf>,
    /// Render the input as markdown rather than plain text.
    ///
    /// Applies to stdin, a text argument and a `.txt` file; redundant (and
    /// silently ignored) for a `.md` file, and rejected for images and URLs.
    #[arg(short, long)]
    #[cfg_attr(feature = "url", arg(conflicts_with = "url"))]
    pub markdown: bool,
    /// Web page to render (via headless Chrome) and print
    #[cfg(feature = "url")]
    #[arg(long, conflicts_with_all = ["text", "file"])]
    pub url: Option<String>,
    /// Density 1-7
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=7))]
    pub density: u8,
    /// Blank feed lines after printing
    #[arg(long, default_value_t = 40)]
    pub feed: usize,
    /// Dithering for images
    #[arg(long, value_enum, default_value_t = DitherArg::Floyd)]
    pub dither: DitherArg,
    /// Font size for text in pixels
    #[arg(long, default_value_t = 24.0, value_parser = parse_font_size)]
    pub size: f32,
    /// Render to PNG instead of printing
    #[arg(long)]
    pub preview: Option<std::path::PathBuf>,
    /// Number of copies to print (1-20)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=20))]
    pub copies: u16,
}

#[derive(clap::Args)]
pub struct QrArgs {
    /// Data to encode (URL or text)
    pub data: String,
    /// Caption text printed below the code
    #[arg(long)]
    pub caption: Option<String>,
    #[command(flatten)]
    pub device: DeviceArgs,
    /// Density 1-7
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=7))]
    pub density: u8,
    /// Blank feed lines after printing
    #[arg(long, default_value_t = 40)]
    pub feed: usize,
    /// Render to PNG instead of printing
    #[arg(long)]
    pub preview: Option<std::path::PathBuf>,
    /// Number of copies to print (1-20)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=20))]
    pub copies: u16,
}

/// The `EnvFilter` directive a `-v` count maps to.
///
/// Dependencies stay at `warn` until `-vvv`, so `-v`/`-vv` show this crate's
/// own story without btleplug and hyper drowning it out. `RUST_LOG` (handled
/// by the caller) is the escape hatch for anything finer.
pub fn log_filter(verbose: u8) -> String {
    match verbose {
        0 => "warn".to_string(),
        1 => "warn,printable=info".to_string(),
        2 => "warn,printable=debug".to_string(),
        _ => "debug,printable=trace".to_string(),
    }
}

/// Parse a font size: must be a positive, finite number of pixels.
fn parse_font_size(s: &str) -> Result<f32, String> {
    let size: f32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if size.is_finite() && size > 0.0 {
        Ok(size)
    } else {
        Err("font size must be greater than 0".to_string())
    }
}

#[derive(clap::Args)]
pub struct DeviceArgs {
    /// Device name or identifier substring (default: first device named LX*)
    #[arg(long)]
    pub device: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum DitherArg {
    /// Floyd–Steinberg error diffusion
    Floyd,
    /// Atkinson error diffusion (higher contrast)
    Atkinson,
    /// Plain threshold at 128, no dithering
    #[value(alias = "none")]
    Threshold,
}

impl From<DitherArg> for printa_ble_core::raster::Dither {
    fn from(d: DitherArg) -> Self {
        match d {
            DitherArg::Floyd => Self::FloydSteinberg,
            DitherArg::Atkinson => Self::Atkinson,
            DitherArg::Threshold => Self::Threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// No flag must stay silent: the CLI's default output is load-bearing
    /// (scripts read the preview path off stdout).
    #[test]
    fn default_verbosity_logs_nothing_below_warn() {
        assert_eq!(log_filter(0), "warn");
    }

    #[test]
    fn verbosity_ladder_increases_this_crate_first() {
        assert!(log_filter(1).contains("printable=info"));
        assert!(log_filter(2).contains("printable=debug"));
        assert!(log_filter(3).contains("printable=trace"));
        // Dependencies stay quiet until the last rung.
        assert!(log_filter(1).starts_with("warn,"));
        assert!(log_filter(2).starts_with("warn,"));
        assert!(log_filter(3).starts_with("debug,"));
    }

    #[test]
    fn verbosity_saturates_past_three() {
        assert_eq!(log_filter(3), log_filter(9));
    }

    /// `-v` is global, so it must parse after any subcommand too.
    #[test]
    fn verbose_flag_is_global() {
        let cli = Cli::try_parse_from(["printable", "print", "-vv", "hi"]).unwrap();
        assert_eq!(cli.verbose, 2);
        let cli = Cli::try_parse_from(["printable", "-v", "serve"]).unwrap();
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn verbose_defaults_to_zero() {
        let cli = Cli::try_parse_from(["printable", "print", "hi"]).unwrap();
        assert_eq!(cli.verbose, 0);
    }

    fn print_args(argv: &[&str]) -> PrintArgs {
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Print(args) => args,
            _ => panic!("expected a print command"),
        }
    }

    #[test]
    fn markdown_flag_defaults_off() {
        assert!(!print_args(&["printable", "print", "hi"]).markdown);
    }

    #[test]
    fn markdown_flag_has_a_short_and_a_long_form() {
        assert!(print_args(&["printable", "print", "-m", "# hi"]).markdown);
        assert!(print_args(&["printable", "print", "--markdown", "# hi"]).markdown);
    }

    /// Piping a document is the whole point: `-m` must parse with no
    /// positional text at all.
    #[test]
    fn markdown_flag_works_without_a_text_argument() {
        assert!(print_args(&["printable", "print", "-m"]).markdown);
    }

    /// A rendered web page is not a markdown document; clap rejects the
    /// combination before anything touches the network.
    #[cfg(feature = "url")]
    #[test]
    fn markdown_flag_conflicts_with_url() {
        let Err(err) = Cli::try_parse_from(["printable", "print", "-m", "--url", "http://x/"])
        else {
            panic!("--markdown --url must not parse");
        };
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// `-m` alongside `--file doc.md` is redundant, not wrong: it parses.
    #[test]
    fn markdown_flag_is_accepted_with_a_file() {
        let args = print_args(&["printable", "print", "-m", "--file", "doc.md"]);
        assert!(args.markdown);
        assert_eq!(args.file.as_deref(), Some(std::path::Path::new("doc.md")));
    }
}
