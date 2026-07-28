//! BLE transport for LX-D02 printers, built on btleplug.
//!
//! The printer exposes service 0xFFE6 with a write-without-response
//! characteristic 0xFFE1 and a notify characteristic 0xFFE2. Status frames
//! (5A 02) arrive unsolicited once subscribed.
//!
//! macOS note: the first BLE access triggers the TCC permission prompt for
//! the terminal app. If permission is denied, btleplug errors out — the
//! messages below point the user at System Settings.

use std::fmt;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use btleplug::api::bleuuid::uuid_from_u16;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use lxd2_core::protocol::job::{Action, PrintJob};
use lxd2_core::protocol::notifications::{self, Notification, Status};
use tokio::sync::mpsc;

/// How long `run_job` waits for an expected notification before giving up.
const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval while waiting for a matching device to appear.
const DISCOVERY_POLL: Duration = Duration::from_millis(300);

/// No matching printer was discovered before the scan timeout.
///
/// Kept as a distinct type so `main` can map it to its own exit code.
#[derive(Debug)]
pub struct NoPrinterFound;

impl fmt::Display for NoPrinterFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no LX printer found. Is the printer on and in range?")
    }
}

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

/// A device matches if its name contains `filter` (or its id does), or —
/// with no filter — if its name starts with "LX".
async fn find_match(
    adapter: &Adapter,
    filter: Option<&str>,
) -> Result<Option<(Peripheral, String)>> {
    for p in adapter.peripherals().await? {
        let Ok(Some(props)) = p.properties().await else {
            continue;
        };
        let Some(name) = props.local_name else {
            continue;
        };
        let matched = match filter {
            Some(f) => name.contains(f) || p.id().to_string().contains(f),
            None => name.starts_with("LX"),
        };
        if matched {
            return Ok(Some((p, name)));
        }
    }
    Ok(None)
}

/// A connected printer with its notification stream already subscribed.
pub struct Printer {
    peripheral: Peripheral,
    write_char: Characteristic,
    notify_rx: mpsc::UnboundedReceiver<Notification>,
    name: String,
}

/// Scan until a matching device appears (up to `scan_timeout`), then connect,
/// discover characteristics, and subscribe to notifications.
pub async fn connect(filter: Option<&str>, scan_timeout: Duration) -> Result<Printer> {
    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;

    let deadline = tokio::time::Instant::now() + scan_timeout;
    let (peripheral, name) = loop {
        if let Some(found) = find_match(&adapter, filter).await? {
            break found;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = adapter.stop_scan().await;
            return Err(anyhow::Error::msg(NoPrinterFound));
        }
        tokio::time::sleep(DISCOVERY_POLL).await;
    };
    let _ = adapter.stop_scan().await;

    peripheral
        .connect()
        .await
        .with_context(|| format!("failed to connect to {name}"))?;
    peripheral
        .discover_services()
        .await
        .context("service discovery failed")?;

    let write_uuid = uuid_from_u16(0xFFE1);
    let notify_uuid = uuid_from_u16(0xFFE2);
    let chars = peripheral.characteristics();
    let write_char = chars
        .iter()
        .find(|c| c.uuid == write_uuid)
        .cloned()
        .ok_or_else(|| anyhow!("{name} has no 0xFFE1 write characteristic — not an LX printer?"))?;
    let notify_char = chars
        .iter()
        .find(|c| c.uuid == notify_uuid)
        .cloned()
        .ok_or_else(|| {
            anyhow!("{name} has no 0xFFE2 notify characteristic — not an LX printer?")
        })?;

    peripheral
        .subscribe(&notify_char)
        .await
        .context("failed to subscribe to printer notifications")?;

    // Forward parsed notifications into a channel; unparseable frames are
    // ignored silently for now.
    let mut stream = peripheral
        .notifications()
        .await
        .context("failed to open notification stream")?;
    let (tx, notify_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(data) = stream.next().await {
            if data.uuid != notify_uuid {
                continue;
            }
            if let Some(n) = notifications::parse(&data.value) {
                if tx.send(n).is_err() {
                    break; // Printer was dropped.
                }
            }
        }
    });

    Ok(Printer {
        peripheral,
        write_char,
        notify_rx,
        name,
    })
}

impl Printer {
    /// The device's advertised name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait for the first Status notification. Status frames arrive
    /// spontaneously after subscribing; if none shows up within `timeout`,
    /// this errors.
    pub async fn wait_status(&mut self, timeout: Duration) -> Result<Status> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let n = tokio::time::timeout_at(deadline, self.notify_rx.recv())
                .await
                .map_err(|_| anyhow!("no status received"))?
                .ok_or_else(|| anyhow!("notification stream closed"))?;
            if let Notification::Status(s) = n {
                return Ok(s);
            }
        }
    }

    /// Drive a print job to completion, pumping its sans-IO state machine.
    pub async fn run_job(&mut self, job: &mut PrintJob) -> Result<()> {
        loop {
            // Drain pending notifications first so mid-stream flow control
            // (Hold / LostPacket / Cooldown) reaches the FSM even while we
            // are on the Send/WaitMs fast path.
            while let Ok(n) = self.notify_rx.try_recv() {
                job.on_notification(n);
            }
            match job.next_action() {
                Action::Send(bytes) => {
                    self.peripheral
                        .write(&self.write_char, &bytes, WriteType::WithoutResponse)
                        .await
                        .context("BLE write failed")?;
                }
                Action::WaitMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Action::WaitNotification => {
                    let n = tokio::time::timeout(NOTIFICATION_TIMEOUT, self.notify_rx.recv())
                        .await
                        .map_err(|_| anyhow!("printer stopped responding"))?
                        .ok_or_else(|| anyhow!("notification stream closed"))?;
                    job.on_notification(n);
                }
                Action::Done => break,
            }
        }
        if let Some(e) = job.error() {
            bail!("{e}");
        }
        Ok(())
    }

    /// Disconnect, ignoring errors (the OS drops the link anyway on exit).
    pub async fn disconnect(self) {
        let _ = self.peripheral.disconnect().await;
    }
}
