//! BLE transport for LX-D02 printers, built on btleplug.
//!
//! macOS note: the first BLE access triggers the TCC permission prompt for
//! the terminal app. If permission is denied, btleplug errors out — the
//! messages below point the user at System Settings.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};

/// Find the first Bluetooth adapter, with a hint if Bluetooth is off.
async fn default_adapter() -> Result<Adapter> {
    let manager = Manager::new()
        .await
        .context("failed to initialize Bluetooth")?;
    let adapters = manager
        .adapters()
        .await
        .context("failed to enumerate Bluetooth adapters")?;
    adapters
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no Bluetooth adapter found — is Bluetooth turned on?"))
}

/// Start scanning, mapping permission failures to a helpful message.
async fn start_scan(adapter: &Adapter) -> Result<()> {
    adapter.start_scan(ScanFilter::default()).await.context(
        "failed to start BLE scan; on macOS, grant Bluetooth permission to your \
         terminal in System Settings > Privacy & Security > Bluetooth",
    )
}

/// Scan for `timeout`, returning (name, id) of every device named `LX*`.
pub async fn scan(timeout: Duration) -> Result<Vec<(String, String)>> {
    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;
    tokio::time::sleep(timeout).await;

    let mut found = Vec::new();
    for p in adapter.peripherals().await? {
        let Ok(Some(props)) = p.properties().await else {
            continue;
        };
        let Some(name) = props.local_name else {
            continue;
        };
        if name.starts_with("LX") {
            found.push((name, p.id().to_string()));
        }
    }
    let _ = adapter.stop_scan().await;
    Ok(found)
}
