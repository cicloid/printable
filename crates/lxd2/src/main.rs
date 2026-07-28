mod ble;
mod cli;

use std::time::Duration;

use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Command::Scan { timeout } => cmd_scan(timeout).await,
    }
}

async fn cmd_scan(timeout: u64) -> anyhow::Result<i32> {
    let found = ble::scan(Duration::from_secs(timeout)).await?;
    if found.is_empty() {
        eprintln!("No LX printers found. Is the printer on?");
        return Ok(1);
    }
    println!("{:<20} ID", "NAME");
    for (name, id) in &found {
        println!("{name:<20} {id}");
    }
    Ok(0)
}
