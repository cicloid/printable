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
    Print {
        #[command(flatten)]
        device: DeviceArgs,
        /// Text to print; reads stdin if omitted and no --file
        text: Option<String>,
        /// File to print (.png/.jpg/.jpeg/.txt)
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
        /// Density 1-7
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=7))]
        density: u8,
        /// Blank feed lines after printing
        #[arg(long, default_value_t = 40)]
        feed: usize,
        /// Dithering for images
        #[arg(long, value_enum, default_value_t = DitherArg::Floyd)]
        dither: DitherArg,
        /// Font size for text in pixels
        #[arg(long, default_value_t = 24.0)]
        size: f32,
        /// Render to PNG instead of printing
        #[arg(long)]
        preview: Option<std::path::PathBuf>,
    },
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
