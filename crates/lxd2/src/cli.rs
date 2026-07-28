//! Command-line interface definitions.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "lxd2",
    about = "Print to LX-D02/LX-D2 BLE thermal printers",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
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
}

#[derive(clap::Args)]
pub struct PrintArgs {
    #[command(flatten)]
    pub device: DeviceArgs,
    /// Text to print; reads stdin if omitted and no --file
    pub text: Option<String>,
    /// File to print (.png/.jpg/.jpeg/.txt)
    #[arg(short, long)]
    pub file: Option<std::path::PathBuf>,
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
    Floyd,
    Threshold,
}

impl From<DitherArg> for lxd2_core::raster::Dither {
    fn from(d: DitherArg) -> Self {
        match d {
            DitherArg::Floyd => Self::FloydSteinberg,
            DitherArg::Threshold => Self::Threshold,
        }
    }
}
