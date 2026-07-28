//! Command-line interface definitions.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lxd2", about = "Print to LX-D02/LX-D2 BLE thermal printers", version)]
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
}
