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

use crate::config::SavedDevice;
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

/// The printer reported it is out of paper.
///
/// Kept as a distinct type so `main` can map it to its own exit code.
#[derive(Debug)]
pub struct NoPaper;

impl fmt::Display for NoPaper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("printer is out of paper")
    }
}

/// Bail if a notification is a Status frame reporting no paper.
fn check_paper(n: &Notification) -> Result<()> {
    if let Notification::Status(s) = n {
        if s.no_paper {
            return Err(anyhow::Error::msg(NoPaper));
        }
    }
    Ok(())
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

/// What a connect attempt is hunting for. Resolution order (highest first):
///
/// 1. `Filter` — an explicit `--device` string; name or id substring match.
/// 2. `SavedId` — the id remembered in the config file; an exact id match
///    wins, but fallbacks are kept in case the saved device never shows up
///    before the scan deadline: a device whose local name equals the saved
///    name is preferred over any other `LX*` name.
/// 3. `AnyLx` — no flag, no saved device: first device named `LX*`.
enum Target<'a> {
    Filter(&'a str),
    SavedId { id: &'a str, name: &'a str },
    AnyLx,
}

/// How well a peripheral satisfies a [`Target`].
enum MatchKind {
    /// Take this device immediately.
    Exact,
    /// Use this device only once the scan deadline expires; while waiting,
    /// a higher-ranked fallback replaces a lower-ranked one.
    Fallback(FallbackRank),
}

/// Preference order among fallback candidates (higher wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FallbackRank {
    /// Any device named `LX*`.
    AnyLx,
    /// Local name equals the saved device's name.
    SavedName,
}

/// Match a peripheral against `target`, returning its advertised name.
async fn match_target(p: &Peripheral, target: &Target<'_>) -> Option<(MatchKind, String)> {
    let props = p.properties().await.ok()??;
    let name = props.local_name?;
    match target {
        Target::Filter(f) => {
            (name.contains(f) || p.id().to_string().contains(f)).then_some((MatchKind::Exact, name))
        }
        Target::SavedId { id, name: saved } => {
            if p.id().to_string() == *id {
                Some((MatchKind::Exact, name))
            } else if name == *saved {
                Some((MatchKind::Fallback(FallbackRank::SavedName), name))
            } else if name.starts_with("LX") {
                Some((MatchKind::Fallback(FallbackRank::AnyLx), name))
            } else {
                None
            }
        }
        Target::AnyLx => name.starts_with("LX").then_some((MatchKind::Exact, name)),
    }
}

/// Scan for `timeout`, returning (name, id) of every device named `LX*`.
pub async fn scan(timeout: Duration) -> Result<Vec<(String, String)>> {
    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;
    tokio::time::sleep(timeout).await;

    let mut found = Vec::new();
    for p in adapter.peripherals().await? {
        if let Some((_, name)) = match_target(&p, &Target::AnyLx).await {
            found.push((name, p.id().to_string()));
        }
    }
    let _ = adapter.stop_scan().await;
    Ok(found)
}

/// One pass over the currently discovered peripherals: the first exact match
/// for `target`, plus the best-ranked fallback candidate (see [`MatchKind`]).
#[allow(clippy::type_complexity)]
async fn find_match(
    adapter: &Adapter,
    target: &Target<'_>,
) -> Result<(
    Option<(Peripheral, String)>,
    Option<(Peripheral, String, FallbackRank)>,
)> {
    let mut fallback: Option<(Peripheral, String, FallbackRank)> = None;
    for p in adapter.peripherals().await? {
        match match_target(&p, target).await {
            Some((MatchKind::Exact, name)) => return Ok((Some((p, name)), fallback)),
            Some((MatchKind::Fallback(rank), name)) => {
                if fallback.as_ref().is_none_or(|(_, _, held)| rank > *held) {
                    fallback = Some((p, name, rank));
                }
            }
            _ => {}
        }
    }
    Ok((None, fallback))
}

/// A connected printer with its notification stream already subscribed.
pub struct Printer {
    peripheral: Peripheral,
    write_char: Characteristic,
    notify_char: Characteristic,
    notify_rx: mpsc::UnboundedReceiver<Notification>,
    /// The task forwarding raw notifications into `notify_rx`; aborted on
    /// disconnect so it does not park on the stream forever.
    forwarder: tokio::task::JoinHandle<()>,
    name: String,
}

/// Scan until a matching device appears (up to `scan_timeout`), then connect,
/// discover characteristics, and subscribe to notifications.
///
/// Resolution order (see [`Target`]): `explicit` filter > `saved` device id
/// (falling back to a device with the saved name, else any `LX*` name, if
/// the saved id is not seen before the deadline) > first device named `LX*`.
pub async fn connect_resolved(
    explicit: Option<&str>,
    saved: Option<&SavedDevice>,
    scan_timeout: Duration,
) -> Result<Printer> {
    let target = match (explicit, saved) {
        (Some(f), _) => Target::Filter(f),
        (None, Some(d)) => Target::SavedId {
            id: &d.id,
            name: &d.name,
        },
        (None, None) => Target::AnyLx,
    };

    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;

    let deadline = tokio::time::Instant::now() + scan_timeout;
    let mut fallback: Option<(Peripheral, String, FallbackRank)> = None;
    let (peripheral, name) = loop {
        let (exact, fb) = find_match(&adapter, &target).await?;
        if let Some(found) = exact {
            break found;
        }
        // Upgrade the held fallback when a better-ranked candidate appears.
        if let Some(fb) = fb {
            if fallback.as_ref().is_none_or(|(_, _, held)| fb.2 > *held) {
                fallback = Some(fb);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            match fallback.take() {
                Some((p, n, _)) => break (p, n),
                None => {
                    let _ = adapter.stop_scan().await;
                    return Err(anyhow::Error::msg(NoPrinterFound));
                }
            }
        }
        tokio::time::sleep(DISCOVERY_POLL).await;
    };
    let _ = adapter.stop_scan().await;

    peripheral
        .connect()
        .await
        .with_context(|| format!("failed to connect to {name}"))?;

    // From here on the link is up: drop it again if setup fails.
    match initialize(peripheral.clone(), name).await {
        Ok(printer) => Ok(printer),
        Err(e) => {
            let _ = peripheral.disconnect().await;
            Err(e)
        }
    }
}

/// Post-connect setup: discover characteristics, subscribe, and spawn the
/// notification forwarder. The caller disconnects on error.
async fn initialize(peripheral: Peripheral, name: String) -> Result<Printer> {
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
    let forwarder = tokio::spawn(async move {
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
        notify_char,
        notify_rx,
        forwarder,
        name,
    })
}

impl Printer {
    /// The device's advertised name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The peripheral's platform identifier as a string (what the config
    /// file stores to reconnect later).
    pub fn id(&self) -> String {
        self.peripheral.id().to_string()
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
            // are on the Send/WaitMs fast path. A no-paper Status aborts the
            // job here rather than dying later on a misleading timeout.
            while let Ok(n) = self.notify_rx.try_recv() {
                check_paper(&n)?;
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
                    check_paper(&n)?;
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
    ///
    /// Unsubscribes first, then stops the notification forwarder so it does
    /// not sit parked on a stream that will never yield again.
    pub async fn disconnect(self) {
        let _ = self.peripheral.unsubscribe(&self.notify_char).await;
        self.forwarder.abort();
        let _ = self.peripheral.disconnect().await;
    }
}
