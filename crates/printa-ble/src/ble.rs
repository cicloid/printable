//! BLE transport for LX-D02 printers, built on btleplug.
//!
//! The printer exposes service 0xFFE6 with a write-without-response
//! characteristic 0xFFE1 and a notify characteristic 0xFFE2. Status frames
//! (5A 02) arrive unsolicited once subscribed.
//!
//! macOS note: the first BLE access triggers the TCC permission prompt for
//! the terminal app. If permission is denied, btleplug errors out — the
//! messages below point the user at System Settings.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use btleplug::api::bleuuid::uuid_from_u16;
use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use tracing::{debug, info, trace, warn};

use crate::config::SavedDevice;
use crate::print_service::{NoPaper, NoPrinterFound, PrinterNotResponding};
use printa_ble_core::model::PrinterModel;
use printa_ble_core::protocol::job::{Action, JobStats, PrintJob};
use printa_ble_core::protocol::notifications::{self, Notification, Status};
use printa_ble_core::protocol::packets;
use tokio::sync::mpsc;

/// How long `run_job` waits for an expected notification before giving up.
///
/// This catches a printer that has gone off the air entirely. It does *not*
/// catch a printer that keeps talking without doing anything — see
/// [`STALL_TIMEOUT`].
const NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the job may go without moving before it is abandoned.
///
/// [`NOTIFICATION_TIMEOUT`] measures radio silence and is re-armed by any
/// frame at all, including the periodic unsolicited `5A 02` Status
/// heartbeats. A printer that holds the stream and then never resumes keeps
/// sending those, so the notification deadline is never reached and the job
/// waits forever — as does any HTTP client behind it. This deadline measures
/// something the printer cannot fake: whether raster data is actually moving.
///
/// A minute is deliberately generous. A genuine thermal cooldown resumes in
/// seconds, so anything past this is a printer that is not coming back.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Polling interval while waiting for a matching device to appear.
const DISCOVERY_POLL: Duration = Duration::from_millis(300);

/// How long the link may take to come up.
///
/// CoreBluetooth's own connect has no deadline: asked to connect to a
/// peripheral that is not there, it waits for it to come back, forever. A real
/// printer in range connects in about a second, so this is loose enough never
/// to cut one off and tight enough that a switched-off printer is reported
/// rather than waited on.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the printer gets to answer the liveness hello.
///
/// A round trip over BLE is milliseconds; this is sized for a printer that is
/// busy waking up, not for one that is off.
const HELLO_TIMEOUT: Duration = Duration::from_secs(4);

/// How long a best-effort teardown may take before it is abandoned.
///
/// btleplug's `disconnect` waits for CoreBluetooth to report the peripheral
/// disconnected, and a link that never really came up may never produce that
/// callback. Cleanup must not outlast the error it is cleaning up after.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Shortest pause worth an info-level "resumed after" line. Below this the
/// printer barely broke stride (a lone cooldown is a fixed 100 ms back-off)
/// and the summary at the end of the job covers it.
const NOTEWORTHY_PAUSE: Duration = Duration::from_millis(250);

/// Minimum gap between info-level cooldown reports. A printer running hot
/// emits a cooldown per packet; one line each would bury everything else.
const COOLDOWN_REPORT_GAP: Duration = Duration::from_secs(2);

/// Bail if a notification is a Status frame reporting no paper.
fn check_paper(n: &Notification) -> Result<()> {
    if let Notification::Status(s) = n {
        if s.no_paper {
            return Err(anyhow::Error::msg(NoPaper));
        }
    }
    Ok(())
}

/// Log what an incoming notification means for the job, and abort on the one
/// condition the FSM does not handle (no paper).
///
/// The flow-control events land at info: they are the whole reason a print
/// appears to hang, and the user asked to be told rather than left guessing.
fn observe(n: &Notification, log: &mut JobLog) -> Result<()> {
    let now = Instant::now();
    match n {
        Notification::Hold => {
            log.pause(now);
            info!("printer paused the stream (print head too hot); waiting to resume…");
        }
        Notification::Cooldown => {
            log.pause(now);
            match log.note_cooldown(now) {
                Some(1) => info!("printer is cooling down"),
                Some(n) => info!("printer is cooling down ({n} requests since the last report)"),
                None => debug!("cooldown requested"),
            }
        }
        Notification::LostPacket { index } => {
            info!("printer requested a resend from packet {index}");
        }
        Notification::Finished { num_packets } => {
            debug!("printer reported the job finished after {num_packets} packets");
        }
        Notification::Status(s) => {
            if s.overheat && !log.warned_overheat {
                log.warned_overheat = true;
                warn!("printer reports the print head is overheating");
            }
            if s.low_battery && !log.warned_low_battery {
                log.warned_low_battery = true;
                warn!("printer battery is low ({}%)", s.battery_pct);
            }
        }
        _ => {}
    }
    check_paper(n)
}

/// Lowercase hex, for trace-logging raw frames. Notification frames are at
/// most a dozen bytes, so they are dumped whole.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A short label for an outgoing frame.
///
/// Raster packets are named by index, never dumped: the payload is 96 bytes
/// of pixels per packet and a page is thousands of them.
fn describe_write(bytes: &[u8]) -> String {
    match bytes {
        [0x55, hi, lo, ..] => format!("raster idx={}", u16::from_be_bytes([*hi, *lo])),
        [0x5A, 0x01, ..] => "hello".to_string(),
        [0x5A, 0x0A, ..] => "auth challenge".to_string(),
        [0x5A, 0x0B, ..] => "auth response".to_string(),
        [0x5A, 0x0C, level, ..] => format!("set density {level}"),
        [0x5A, 0x04, hi, lo, 0x00, ..] => {
            format!("print start ({} packets)", u16::from_be_bytes([*hi, *lo]))
        }
        [0x5A, 0x04, hi, lo, ..] => {
            format!("print end ({} packets)", u16::from_be_bytes([*hi, *lo]))
        }
        _ => format!("unknown frame {}", hex(bytes)),
    }
}

/// Is this an outgoing raster packet (as opposed to a control frame)?
fn is_raster(bytes: &[u8]) -> bool {
    bytes.first() == Some(&0x55)
}

/// Render a duration the way a person reads it off a stopwatch.
fn secs(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

/// One-line account of how a job went, for the log at the end of it.
///
/// This is the line that answers "why did that take so long": a job whose
/// elapsed time dwarfs its packet count spent the difference paused, and the
/// counters say so. Flow-control terms are omitted entirely when the printer
/// never invoked any, so a healthy print stays a short line.
fn job_summary(elapsed: Duration, paused: Duration, stats: JobStats) -> String {
    let mut s = format!("{}, {} packets sent", secs(elapsed), stats.packets_sent);
    if stats.holds > 0 {
        let _ = write!(s, ", {} holds", stats.holds);
    }
    if stats.cooldowns > 0 {
        let _ = write!(s, ", {} cooldowns", stats.cooldowns);
    }
    if stats.retransmits > 0 {
        let _ = write!(s, ", {} resends", stats.retransmits);
    }
    if stats.holds > 0 || stats.cooldowns > 0 {
        let _ = write!(s, ", {} paused for thermal flow control", secs(paused));
    }
    s
}

/// Per-job logging state: how long the printer has kept us waiting, and what
/// has already been reported so a repeating condition is not logged forever.
#[derive(Default)]
struct JobLog {
    /// Start of the pause the printer currently has us in, if any.
    paused_since: Option<Instant>,
    /// Time already spent paused, excluding any open window.
    paused_total: Duration,
    /// Cooldowns seen since the last one reported at info level.
    cooldowns_pending: u32,
    last_cooldown_report: Option<Instant>,
    warned_overheat: bool,
    warned_low_battery: bool,
}

impl JobLog {
    /// Open a pause window, if one is not already open.
    fn pause(&mut self, now: Instant) {
        self.paused_since.get_or_insert(now);
    }

    /// Close the open pause window, returning how long it lasted.
    fn resume(&mut self, now: Instant) -> Option<Duration> {
        let since = self.paused_since.take()?;
        let held = now.saturating_duration_since(since);
        self.paused_total += held;
        Some(held)
    }

    /// Total time paused, counting a window that is still open.
    fn paused_total(&self, now: Instant) -> Duration {
        match self.paused_since {
            Some(since) => self.paused_total + now.saturating_duration_since(since),
            None => self.paused_total,
        }
    }

    /// Record a cooldown, returning how many to report if one is due.
    ///
    /// The first cooldown always reports; after that at most one report per
    /// [`COOLDOWN_REPORT_GAP`], carrying the number suppressed in between.
    fn note_cooldown(&mut self, now: Instant) -> Option<u32> {
        self.cooldowns_pending += 1;
        let due = self
            .last_cooldown_report
            .is_none_or(|last| now.saturating_duration_since(last) >= COOLDOWN_REPORT_GAP);
        if !due {
            return None;
        }
        self.last_cooldown_report = Some(now);
        Some(std::mem::take(&mut self.cooldowns_pending))
    }
}

/// A cheap fingerprint of how far a job has actually got.
///
/// Deliberately narrow. Raster packets written and resends requested are the
/// only counters that move when the printer is taking data; the flow-control
/// counters (`holds`, `cooldowns`) and the pending action are excluded on
/// purpose, because a printer that emits `Cooldown` every 100 ms while
/// refusing another packet churns both of those, and that churn is precisely
/// the stall this exists to catch.
///
/// Nothing else in the job runs long: the handshake is a few round trips, so
/// a minute with the packet count frozen means the print is not moving,
/// whatever state the FSM is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Progress {
    packets_sent: u32,
    retransmits: u32,
}

impl Progress {
    fn of(job: &PrintJob) -> Self {
        let stats = job.stats();
        Self {
            packets_sent: stats.packets_sent,
            retransmits: stats.retransmits,
        }
    }
}

/// What to tell the user about an abandoned job.
///
/// A job that stalled after real thermal flow control gets the density hint.
/// One that stalled having never been asked to pause did not overheat, and
/// blaming the print head would send the user after the wrong thing — so that
/// message says what it does know: the link is up, the job is not moving.
///
/// Neither wording overlaps [`NOTIFICATION_TIMEOUT`]'s "printer went silent":
/// a stall is the printer talking and doing nothing, silence is neither.
fn stall_message(idle: Duration, paused: Duration) -> String {
    if paused.is_zero() {
        format!(
            "printer stalled for {} without making progress; it is still sending frames, \
             so the connection is up but the print is not moving",
            secs(idle)
        )
    } else {
        format!(
            "printer stalled for {} without resuming, {} of this job spent paused for \
             thermal flow control; the print head may be overheating — try a lower --density",
            secs(idle),
            secs(paused)
        )
    }
}

/// Tracks when a job last moved, so a printer that keeps the radio busy
/// without taking data cannot hold the caller forever.
struct StallGuard {
    last: Progress,
    since: Instant,
}

impl StallGuard {
    fn new(now: Instant, progress: Progress) -> Self {
        Self {
            last: progress,
            since: now,
        }
    }

    /// Record where the job is now, and report how long it has gone without
    /// moving. Zero means it just moved.
    fn observe(&mut self, now: Instant, progress: Progress) -> Duration {
        if progress != self.last {
            self.last = progress;
            self.since = now;
        }
        now.saturating_duration_since(self.since)
    }
}

/// Drop the link, giving up if the platform never confirms it.
async fn release(peripheral: &Peripheral) {
    if tokio::time::timeout(DISCONNECT_TIMEOUT, peripheral.disconnect())
        .await
        .is_err()
    {
        debug!(
            "disconnect did not complete within {}",
            secs(DISCONNECT_TIMEOUT)
        );
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

/// What a connect attempt is hunting for. Resolution order (highest first):
///
/// 1. `Filter` — an explicit `--device` string; name or id substring match.
/// 2. `SavedId` — the id remembered in the config file; an exact id match
///    wins, but fallbacks are kept in case the saved device never shows up
///    before the scan deadline: a device whose local name equals the saved
///    name is preferred over any other supported printer's name.
/// 3. `AnySupported` — no flag, no saved device: first device whose name
///    identifies a printer model this workspace can drive.
enum Target<'a> {
    Filter(&'a str),
    SavedId { id: &'a str, name: &'a str },
    AnySupported,
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
    /// Any device whose name identifies a supported printer model.
    AnySupported,
    /// Local name equals the saved device's name.
    SavedName,
}

/// Match an advertised name and platform id against `target`, restricted to
/// `restrict` when a specific model was requested. Pure, so the resolution
/// ladder is testable without a radio; [`match_target`] is the async shim.
///
/// Returns how well the device matched and the model its name identifies —
/// `None` for a [`Target::Filter`] hit on a name no model claims, because an
/// explicit `--device` string may point at anything.
///
/// The restriction is absolute: a device of a different model (or of no
/// recognizable model) never matches while `restrict` is set, even under a
/// filter string that would otherwise hit it.
fn match_advertised(
    name: &str,
    id: &str,
    target: &Target<'_>,
    restrict: Option<PrinterModel>,
) -> Option<(MatchKind, Option<PrinterModel>)> {
    let detected = PrinterModel::from_device_name(name);
    if restrict.is_some() && detected != restrict {
        return None;
    }
    match target {
        Target::Filter(f) => {
            (name.contains(f) || id.contains(f)).then_some((MatchKind::Exact, detected))
        }
        Target::SavedId {
            id: saved_id,
            name: saved,
        } => {
            if id == *saved_id {
                Some((MatchKind::Exact, detected))
            } else if name == *saved {
                Some((MatchKind::Fallback(FallbackRank::SavedName), detected))
            } else if detected.is_some() {
                Some((MatchKind::Fallback(FallbackRank::AnySupported), detected))
            } else {
                None
            }
        }
        Target::AnySupported => detected.is_some().then_some((MatchKind::Exact, detected)),
    }
}

/// Match a peripheral against `target`, returning its advertised name and
/// the model that name identifies, if any.
async fn match_target(
    p: &Peripheral,
    target: &Target<'_>,
    restrict: Option<PrinterModel>,
) -> Option<(MatchKind, String, Option<PrinterModel>)> {
    let props = p.properties().await.ok()??;
    let name = props.local_name?;
    let (kind, model) = match_advertised(&name, &p.id().to_string(), target, restrict)?;
    Some((kind, name, model))
}

/// Scan for `timeout`, returning (name, id, model) of every supported
/// printer seen.
pub async fn scan(timeout: Duration) -> Result<Vec<(String, String, PrinterModel)>> {
    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;
    info!("scanning for {}s", timeout.as_secs());
    tokio::time::sleep(timeout).await;

    let mut found = Vec::new();
    let mut seen = 0usize;
    for p in adapter.peripherals().await? {
        seen += 1;
        match match_target(&p, &Target::AnySupported, None).await {
            // `AnySupported` only matches names a model claims, so the model
            // is always present here.
            Some((_, name, Some(model))) => {
                debug!("match: {name} ({}) [{model}]", p.id());
                found.push((name, p.id().to_string(), model));
            }
            _ => trace!("skipping {}", p.id()),
        }
    }
    let _ = adapter.stop_scan().await;
    info!("scan saw {seen} devices, {} supported", found.len());
    Ok(found)
}

/// One pass over the currently discovered peripherals: the first exact match
/// for `target`, plus the best-ranked fallback candidate (see [`MatchKind`]).
#[allow(clippy::type_complexity)]
async fn find_match(
    adapter: &Adapter,
    target: &Target<'_>,
    restrict: Option<PrinterModel>,
) -> Result<(
    Option<(Peripheral, String)>,
    Option<(Peripheral, String, FallbackRank)>,
)> {
    let mut fallback: Option<(Peripheral, String, FallbackRank)> = None;
    for p in adapter.peripherals().await? {
        match match_target(&p, target, restrict).await {
            Some((MatchKind::Exact, name, _)) => return Ok((Some((p, name)), fallback)),
            Some((MatchKind::Fallback(rank), name, _))
                if fallback.as_ref().is_none_or(|(_, _, held)| rank > *held) =>
            {
                fallback = Some((p, name, rank));
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
/// discover characteristics, subscribe to notifications, and exchange a hello
/// with the printer so that a returned [`Printer`] means a printer that is
/// actually switched on.
///
/// Resolution order (see [`Target`]): `explicit` filter > `saved` device id
/// (falling back to a device with the saved name, else any supported
/// printer, if the saved id is not seen before the deadline) > first device
/// advertising a supported printer name.
pub async fn connect_resolved(
    explicit: Option<&str>,
    saved: Option<&SavedDevice>,
    scan_timeout: Duration,
) -> Result<Printer> {
    // No model restriction yet: the `--model` flag reaches here when model
    // dispatch is wired up.
    let restrict: Option<PrinterModel> = None;
    let target = match (explicit, saved) {
        (Some(f), _) => Target::Filter(f),
        (None, Some(d)) => Target::SavedId {
            id: &d.id,
            name: &d.name,
        },
        (None, None) => Target::AnySupported,
    };

    let adapter = default_adapter().await?;
    start_scan(&adapter).await?;
    debug!(
        "scanning up to {}s for {}",
        scan_timeout.as_secs(),
        match &target {
            Target::Filter(f) => format!("a device matching `{f}`"),
            Target::SavedId { id, .. } => format!("saved device {id}"),
            Target::AnySupported => "any supported printer".to_string(),
        }
    );

    let deadline = tokio::time::Instant::now() + scan_timeout;
    let mut fallback: Option<(Peripheral, String, FallbackRank)> = None;
    let (peripheral, name) = loop {
        let (exact, fb) = find_match(&adapter, &target, restrict).await?;
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
    debug!("connecting to {name} ({})", peripheral.id());

    // CoreBluetooth will wait for an absent peripheral indefinitely, so the
    // connect gets a deadline of its own.
    match tokio::time::timeout(CONNECT_TIMEOUT, peripheral.connect()).await {
        Ok(r) => r.with_context(|| format!("failed to connect to {name}"))?,
        Err(_) => {
            debug!(
                "connect to {name} did not complete within {}",
                secs(CONNECT_TIMEOUT)
            );
            // Cancel the attempt still pending in the OS, so it cannot
            // complete later against a peripheral nobody holds.
            release(&peripheral).await;
            return Err(anyhow::Error::msg(PrinterNotResponding::new(&name)));
        }
    }

    // From here on the link is up: drop it again if setup fails.
    match initialize(peripheral.clone(), name).await {
        Ok(printer) => Ok(printer),
        Err(e) => {
            release(&peripheral).await;
            Err(e)
        }
    }
}

/// Post-connect setup: discover characteristics, subscribe, spawn the
/// notification forwarder, and make the printer prove it is there. The caller
/// disconnects on error.
///
/// Returning `Ok` from here is what lets a caller say "Connected to LX-D02",
/// so it has to mean the printer answered — see [`handshake`].
async fn initialize(peripheral: Peripheral, name: String) -> Result<Printer> {
    // `connect` returning Ok is not the same as a live link. Asking outright
    // turns a dead connection into one legible error instead of a puzzling
    // failure three steps further down.
    match peripheral.is_connected().await {
        Ok(true) => {}
        Ok(false) => {
            debug!("{name} is not connected immediately after a successful connect()");
            return Err(anyhow::Error::msg(PrinterNotResponding::new(&name)));
        }
        // Not knowing is not the same as knowing it is down; the handshake
        // below settles it either way.
        Err(e) => debug!("could not confirm the link to {name}: {e}"),
    }

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
    // `tx` is cloned into the forwarder and dropped when this function ends,
    // so the channel still closes with the forwarder — which is how the
    // receiver learns the link is gone.
    let (tx, mut notify_rx) = mpsc::unbounded_channel();
    let forwarder = tokio::spawn({
        let tx = tx.clone();
        async move {
            while let Some(data) = stream.next().await {
                if data.uuid != notify_uuid {
                    continue;
                }
                trace!("notification frame {}", hex(&data.value));
                match notifications::parse(&data.value) {
                    Some(n) => {
                        debug!("notification: {n:?}");
                        if tx.send(n).is_err() {
                            break; // Printer was dropped.
                        }
                    }
                    None => debug!("ignoring unparseable frame {}", hex(&data.value)),
                }
            }
            debug!("notification forwarder stopped");
        }
    });

    // Proof of life, and the point of this function. Everything above can
    // succeed against a printer that is switched off: macOS keeps the GATT
    // database of a peripheral it has paired with before, so the connect and
    // the service discovery are answered from that cache. The `5A 01` reply
    // is the first thing in the flow that only the hardware itself can
    // produce, so nothing may claim a connection until it arrives.
    let mut skipped = Vec::new();
    match handshake(
        &peripheral,
        &write_char,
        &mut notify_rx,
        &name,
        &mut skipped,
    )
    .await
    {
        Ok(mac) => debug!("{name} answered hello, MAC {}", hex(&mac)),
        Err(e) => {
            // The forwarder is parked on a stream nobody will read again.
            forwarder.abort();
            return Err(e);
        }
    }
    // Hand back whatever the probe had to read past. The printer starts
    // emitting status frames the moment we subscribe, so one of them is
    // usually already in the queue — and it is the frame the pre-print paper
    // check is waiting for.
    for n in skipped {
        let _ = tx.send(n);
    }

    info!("connected to {name}, subscribed to notifications");
    Ok(Printer {
        peripheral,
        write_char,
        notify_char,
        notify_rx,
        forwarder,
        name,
    })
}

/// Greet the printer and wait for it to greet us back, returning its MAC.
///
/// The hello exchange is idempotent: every print job opens with its own hello
/// (each copy in a multi-copy run does so over the same connection), and the
/// printer answers each one. This extra round trip therefore costs a frame and
/// tells the truth about whether anything is listening.
async fn handshake(
    peripheral: &Peripheral,
    write_char: &Characteristic,
    rx: &mut mpsc::UnboundedReceiver<Notification>,
    name: &str,
    skipped: &mut Vec<Notification>,
) -> Result<[u8; 6]> {
    trace!("write hello (liveness probe)");
    peripheral
        .write(write_char, &packets::hello(), WriteType::WithoutResponse)
        .await
        .with_context(|| format!("failed to send hello to {name}"))?;
    await_hello(rx, HELLO_TIMEOUT, name, skipped).await
}

/// Wait up to `timeout` for the printer's `5A 01` hello reply.
///
/// Only that reply will do here, but the frames read past on the way to it —
/// status heartbeats, mostly — are collected into `skipped` rather than
/// discarded, so the probe costs the rest of the program nothing.
///
/// Silence is [`PrinterNotResponding`], which reads differently from
/// [`NoPrinterFound`] on purpose: the device is right there.
async fn await_hello(
    rx: &mut mpsc::UnboundedReceiver<Notification>,
    timeout: Duration,
    name: &str,
    skipped: &mut Vec<Notification>,
) -> Result<[u8; 6]> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let n = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .map_err(|_| {
                debug!("{name} did not answer hello within {}", secs(timeout));
                anyhow::Error::msg(PrinterNotResponding::new(name))
            })?
            .ok_or_else(|| anyhow!("notification stream closed while greeting {name}"))?;
        match n {
            Notification::Hello { mac } => return Ok(mac),
            other => {
                trace!("holding {other:?} until the hello reply arrives");
                skipped.push(other);
            }
        }
    }
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
    ///
    /// Logs a one-line summary of the job either way — that line is how a
    /// caller tells a slow print from a hung one after the fact.
    pub async fn run_job(&mut self, job: &mut PrintJob) -> Result<JobStats> {
        let started = Instant::now();
        let mut log = JobLog::default();

        let mut result = self.pump(job, &mut log).await;
        // A fatal FSM error (rejected auth) is not a transport error, but it
        // ends the job just the same.
        if result.is_ok() {
            if let Some(e) = job.error() {
                result = Err(anyhow!("{e}"));
            }
        }

        let stats = job.stats();
        let summary = job_summary(started.elapsed(), log.paused_total(Instant::now()), stats);
        match &result {
            Ok(()) => info!("print job finished: {summary}"),
            // Info, not warn: the caller reports the failure itself, and the
            // CLI must stay silent at the default log level.
            Err(e) => info!("print job aborted after {summary}: {e:#}"),
        }
        result.map(|()| stats)
    }

    /// The action pump itself. Split out of [`Printer::run_job`] so the job
    /// summary is logged on the failure paths too.
    async fn pump(&mut self, job: &mut PrintJob, log: &mut JobLog) -> Result<()> {
        let mut stall = StallGuard::new(Instant::now(), Progress::of(job));
        loop {
            // Drain pending notifications first so mid-stream flow control
            // (Hold / LostPacket / Cooldown) reaches the FSM even while we
            // are on the Send/WaitMs fast path. A no-paper Status aborts the
            // job here rather than dying later on a misleading timeout.
            while let Ok(n) = self.notify_rx.try_recv() {
                observe(&n, log)?;
                job.on_notification(n);
            }

            // Every path below is bounded, so this runs at least once per
            // NOTIFICATION_TIMEOUT even when the printer says nothing at all.
            let now = Instant::now();
            let idle = stall.observe(now, Progress::of(job));
            if idle >= STALL_TIMEOUT {
                warn!("printer stalled for {}; abandoning the job", secs(idle));
                return Err(anyhow!(stall_message(idle, log.paused_total(now))));
            }

            match job.next_action() {
                Action::Send(bytes) => {
                    // A raster write is the printer letting us move again.
                    if is_raster(&bytes) {
                        if let Some(held) = log.resume(Instant::now()) {
                            if held >= NOTEWORTHY_PAUSE {
                                info!("printing resumed after {}", secs(held));
                            } else {
                                debug!("printing resumed after {}", secs(held));
                            }
                        }
                    }
                    trace!(len = bytes.len(), "write {}", describe_write(&bytes));
                    self.peripheral
                        .write(&self.write_char, &bytes, WriteType::WithoutResponse)
                        .await
                        .context("BLE write failed")?;
                }
                Action::WaitMs(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Action::WaitNotification => {
                    let n = tokio::time::timeout(NOTIFICATION_TIMEOUT, self.notify_rx.recv())
                        .await
                        .map_err(|_| {
                            anyhow!(
                                "printer went silent (no BLE notification at all for {}s)",
                                NOTIFICATION_TIMEOUT.as_secs()
                            )
                        })?
                        .ok_or_else(|| anyhow!("notification stream closed"))?;
                    observe(&n, log)?;
                    job.on_notification(n);
                }
                Action::Done => return Ok(()),
            }
        }
    }

    /// Disconnect, ignoring errors (the OS drops the link anyway on exit).
    ///
    /// Unsubscribes first, then stops the notification forwarder so it does
    /// not sit parked on a stream that will never yield again.
    pub async fn disconnect(self) {
        debug!("disconnecting from {}", self.name);
        let _ = tokio::time::timeout(
            DISCONNECT_TIMEOUT,
            self.peripheral.unsubscribe(&self.notify_char),
        )
        .await;
        self.forwarder.abort();
        release(&self.peripheral).await;
    }
}

// ---------------------------------------------------------------------------
// Tests. Everything below the transport boundary — frame labelling, pause
// accounting, the summary line — is pure and tested here. Connecting and
// streaming need a real printer and are exercised by hand.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use printa_ble_core::protocol::packets;

    #[test]
    fn hex_is_lowercase_and_unpadded() {
        assert_eq!(hex(&[0x5A, 0x08]), "5a08");
        assert_eq!(hex(&[0x00, 0xff]), "00ff");
        assert_eq!(hex(&[]), "");
    }

    /// The point of the labels: a raster packet is identified by index, never
    /// by dumping its 96 bytes of pixels into the log.
    #[test]
    fn raster_writes_are_labelled_by_index_not_payload() {
        let frame = packets::raster(0x0142, &[0xFF; packets::RASTER_DATA_LEN]);
        let label = describe_write(&frame);
        assert_eq!(label, "raster idx=322");
        assert!(!label.contains("ff"), "payload leaked into the label");
        assert!(is_raster(&frame));
    }

    #[test]
    fn control_writes_are_named() {
        assert_eq!(describe_write(&packets::hello()), "hello");
        assert_eq!(describe_write(&packets::set_density(5)), "set density 5");
        assert_eq!(
            describe_write(&packets::auth_challenge(&[0; 10])),
            "auth challenge"
        );
        assert_eq!(
            describe_write(&packets::auth_reply(&[0; 10])),
            "auth response"
        );
        assert_eq!(
            describe_write(&packets::print_start(7)),
            "print start (7 packets)"
        );
        assert_eq!(
            describe_write(&packets::print_end(7)),
            "print end (7 packets)"
        );
        for frame in [
            packets::hello().as_slice(),
            packets::print_start(7).as_slice(),
        ] {
            assert!(!is_raster(frame));
        }
    }

    #[test]
    fn unknown_writes_fall_back_to_hex() {
        assert_eq!(describe_write(&[0x99, 0x01]), "unknown frame 9901");
        assert_eq!(describe_write(&[]), "unknown frame ");
    }

    /// A clean job says only what happened; no flow-control noise.
    #[test]
    fn summary_omits_flow_control_when_there_was_none() {
        let stats = JobStats {
            packets_sent: 208,
            ..JobStats::default()
        };
        assert_eq!(
            job_summary(Duration::from_millis(4100), Duration::ZERO, stats),
            "4.1s, 208 packets sent"
        );
    }

    /// The line that answers "why did it stall".
    #[test]
    fn summary_reports_every_flow_control_event() {
        let stats = JobStats {
            packets_sent: 812,
            retransmits: 2,
            holds: 3,
            cooldowns: 17,
        };
        let s = job_summary(
            Duration::from_millis(41_200),
            Duration::from_millis(28_400),
            stats,
        );
        assert_eq!(
            s,
            "41.2s, 812 packets sent, 3 holds, 17 cooldowns, 2 resends, \
             28.4s paused for thermal flow control"
        );
    }

    /// A resend with no pause behind it is a dropped packet, not overheating,
    /// so the paused time is left out.
    #[test]
    fn summary_omits_paused_time_without_holds_or_cooldowns() {
        let stats = JobStats {
            packets_sent: 10,
            retransmits: 1,
            ..JobStats::default()
        };
        let s = job_summary(Duration::from_secs(1), Duration::ZERO, stats);
        assert_eq!(s, "1.0s, 10 packets sent, 1 resends");
        assert!(!s.contains("paused"));
    }

    #[test]
    fn pause_accounting_measures_one_window() {
        let mut log = JobLog::default();
        let t0 = Instant::now();
        log.pause(t0);
        let held = log.resume(t0 + Duration::from_secs(3)).unwrap();
        assert_eq!(held, Duration::from_secs(3));
        assert_eq!(log.paused_total(t0 + Duration::from_secs(9)), held);
    }

    /// Hold then Cooldown is one pause, not two: the second event arrives
    /// while we are already stopped and must not restart the clock.
    #[test]
    fn overlapping_pauses_do_not_restart_the_clock() {
        let mut log = JobLog::default();
        let t0 = Instant::now();
        log.pause(t0);
        log.pause(t0 + Duration::from_secs(2));
        assert_eq!(
            log.resume(t0 + Duration::from_secs(3)).unwrap(),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn pauses_accumulate_across_windows() {
        let mut log = JobLog::default();
        let t0 = Instant::now();
        log.pause(t0);
        log.resume(t0 + Duration::from_secs(2));
        log.pause(t0 + Duration::from_secs(5));
        log.resume(t0 + Duration::from_secs(9));
        assert_eq!(log.paused_total(t0), Duration::from_secs(6));
    }

    /// The summary must be honest about a job that is still stopped: an open
    /// pause window counts toward the total.
    #[test]
    fn open_pause_window_counts_toward_the_total() {
        let mut log = JobLog::default();
        let t0 = Instant::now();
        log.pause(t0);
        assert_eq!(
            log.paused_total(t0 + Duration::from_secs(4)),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn resume_without_a_pause_is_a_no_op() {
        let mut log = JobLog::default();
        let t0 = Instant::now();
        assert!(log.resume(t0).is_none());
        assert_eq!(log.paused_total(t0), Duration::ZERO);
    }

    // -----------------------------------------------------------------
    // Stall detection.
    // -----------------------------------------------------------------

    /// The printer's periodic unsolicited heartbeat: `5A 02`, battery 80%,
    /// paper present. Enough of these keep [`NOTIFICATION_TIMEOUT`] happy
    /// forever without the job moving an inch.
    fn heartbeat() -> Notification {
        notifications::parse(&[0x5A, 0x02, 80, 0, 0]).expect("valid status frame")
    }

    /// A four-packet job that has authenticated and streamed its first
    /// raster packet.
    fn streaming_job() -> PrintJob {
        let bitmap = printa_ble_core::raster::Bitmap::new(8);
        let mut job = PrintJob::new(&bitmap, 3, [7u8; 10], 0).unwrap();
        let _ = job.next_action(); // hello
        job.on_notification(Notification::Hello {
            mac: [1, 2, 3, 4, 5, 6],
        });
        let _ = job.next_action(); // auth challenge
        job.on_notification(Notification::AuthChallengeReply);
        let _ = job.next_action(); // auth response
        job.on_notification(Notification::AuthResult { ok: true });
        let _ = job.next_action(); // density
        let _ = job.next_action(); // print start
        let _ = job.next_action(); // raster 0
        job
    }

    #[test]
    fn a_job_that_just_started_is_not_stalled() {
        let job = streaming_job();
        let t0 = Instant::now();
        let mut stall = StallGuard::new(t0, Progress::of(&job));
        assert_eq!(stall.observe(t0, Progress::of(&job)), Duration::ZERO);
    }

    /// Every raster packet written restarts the clock, so a slow but moving
    /// print never trips the guard however long it runs.
    #[test]
    fn a_printer_taking_packets_never_stalls() {
        let mut job = streaming_job();
        let t0 = Instant::now();
        let mut stall = StallGuard::new(t0, Progress::of(&job));

        // One packet every 59s — glacial, but never a stall.
        for i in 1..=3 {
            let _ = job.next_action(); // raster i
            let idle = stall.observe(
                t0 + Duration::from_secs(i * (STALL_TIMEOUT.as_secs() - 1)),
                Progress::of(&job),
            );
            assert!(idle < STALL_TIMEOUT, "iteration {i} idle for {idle:?}");
        }
    }

    /// The bug, in one test: the printer holds, then keeps the radio busy
    /// with heartbeats and cooldowns while never accepting another packet.
    /// `NOTIFICATION_TIMEOUT` is satisfied by every one of those frames, so
    /// only a progress deadline can end this.
    #[test]
    fn a_held_printer_that_keeps_talking_is_still_stalled() {
        let mut job = streaming_job();
        job.on_notification(Notification::Hold);
        assert!(matches!(job.next_action(), Action::WaitNotification));

        let t0 = Instant::now();
        let mut stall = StallGuard::new(t0, Progress::of(&job));
        let mut idle = Duration::ZERO;
        for i in 1..=STALL_TIMEOUT.as_secs() {
            job.on_notification(heartbeat());
            job.on_notification(Notification::Cooldown);
            let _ = job.next_action();
            idle = stall.observe(t0 + Duration::from_secs(i), Progress::of(&job));
        }
        assert!(idle >= STALL_TIMEOUT, "idle only {idle:?}");
    }

    /// Same trap one state later: every packet is out and the printer never
    /// says `Finished`, but the heartbeats keep coming.
    #[test]
    fn a_printer_that_never_finishes_is_stalled() {
        let mut job = streaming_job();
        while let Action::Send(_) | Action::WaitMs(_) = job.next_action() {}

        let t0 = Instant::now();
        let mut stall = StallGuard::new(t0, Progress::of(&job));
        let mut idle = Duration::ZERO;
        for i in 1..=STALL_TIMEOUT.as_secs() {
            job.on_notification(heartbeat());
            let _ = job.next_action();
            idle = stall.observe(t0 + Duration::from_secs(i), Progress::of(&job));
        }
        assert!(idle >= STALL_TIMEOUT, "idle only {idle:?}");
    }

    /// A hold the printer actually comes back from must clear the clock, or
    /// a long healthy print would abort on its accumulated pauses.
    #[test]
    fn resuming_from_a_hold_clears_the_stall_clock() {
        let mut job = streaming_job();
        job.on_notification(Notification::Hold);

        let t0 = Instant::now();
        let mut stall = StallGuard::new(t0, Progress::of(&job));
        let almost = STALL_TIMEOUT - Duration::from_secs(1);
        assert!(stall.observe(t0 + almost, Progress::of(&job)) < STALL_TIMEOUT);

        // The printer asks for a resend, which is how a hold ends.
        job.on_notification(Notification::LostPacket { index: 1 });
        let _ = job.next_action(); // raster 0 again
        assert_eq!(
            stall.observe(t0 + almost, Progress::of(&job)),
            Duration::ZERO
        );
    }

    /// Flow-control counters are deliberately not progress: a printer can
    /// emit `Cooldown` forever without taking a single byte of raster.
    #[test]
    fn flow_control_counters_are_not_treated_as_progress() {
        let mut job = streaming_job();
        job.on_notification(Notification::Hold);
        let before = Progress::of(&job);
        for _ in 0..50 {
            job.on_notification(Notification::Cooldown);
        }
        job.on_notification(Notification::Hold);
        assert!(job.stats().cooldowns > 0 || job.stats().holds > 0);
        assert_eq!(Progress::of(&job), before);
    }

    /// The actionable case: the printer paused for heat and never came back.
    #[test]
    fn stall_after_thermal_flow_control_suggests_a_lower_density() {
        let msg = stall_message(Duration::from_secs(61), Duration::from_secs(58));
        assert!(msg.contains("61.0s"), "{msg}");
        assert!(msg.contains("58.0s"), "{msg}");
        assert!(msg.contains("--density"), "{msg}");
    }

    /// A stall with no pause behind it is not overheating, and must not say
    /// it is — the user would go chasing the wrong setting.
    #[test]
    fn stall_without_flow_control_does_not_blame_the_print_head() {
        let msg = stall_message(Duration::from_secs(60), Duration::ZERO);
        assert!(msg.contains("60.0s"), "{msg}");
        assert!(!msg.contains("overheating"), "{msg}");
        assert!(!msg.contains("--density"), "{msg}");
    }

    /// Total radio silence and a talkative stall are different faults, and
    /// the two messages must not be mistaken for one another.
    #[test]
    fn stall_and_silence_read_differently() {
        let stall = stall_message(Duration::from_secs(60), Duration::from_secs(60));
        assert!(stall.contains("stalled"), "{stall}");
        assert!(!stall.contains("silent"), "{stall}");
    }

    // -----------------------------------------------------------------
    // Liveness. The write that starts the probe needs a radio, but the half
    // that decides whether the printer is really there is just the wait for
    // its answer — and that is testable.
    // -----------------------------------------------------------------

    const MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33];

    /// A short deadline keeps the timeout tests quick; the probe's real
    /// budget is [`HELLO_TIMEOUT`].
    const BRIEFLY: Duration = Duration::from_millis(50);

    /// The `5A 01` reply is the proof: only a powered-on printer knows its
    /// own MAC, and macOS's cached GATT database cannot invent one.
    #[tokio::test]
    async fn a_hello_reply_proves_the_printer_answered() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(Notification::Hello { mac: MAC }).unwrap();
        let mut skipped = vec![];
        let mac = await_hello(&mut rx, HELLO_TIMEOUT, "LX-D02", &mut skipped)
            .await
            .unwrap();
        assert_eq!(mac, MAC);
        assert!(skipped.is_empty());
    }

    /// Status heartbeats may arrive first; they are not the answer we asked
    /// for, so the probe keeps waiting — and hands them back, because the
    /// pre-print paper check is waiting for exactly that frame.
    #[tokio::test]
    async fn frames_read_past_on_the_way_to_the_reply_are_handed_back() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(heartbeat()).unwrap();
        tx.send(Notification::Cooldown).unwrap();
        tx.send(Notification::Hello { mac: MAC }).unwrap();
        let mut skipped = vec![];
        let mac = await_hello(&mut rx, HELLO_TIMEOUT, "LX-D02", &mut skipped)
            .await
            .unwrap();
        assert_eq!(mac, MAC);
        assert_eq!(skipped, vec![heartbeat(), Notification::Cooldown]);
    }

    /// The bug this exists for: on macOS everything up to here succeeds
    /// against a printer that is switched off, and the user was told
    /// "Connected to LX-D02." Silence must instead be an error that names the
    /// device and says what is probably wrong.
    #[tokio::test]
    async fn silence_means_the_printer_is_not_really_there() {
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let err = await_hello(&mut rx, BRIEFLY, "LX-D02", &mut vec![])
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<PrinterNotResponding>().is_some(),
            "wrong error type: {err:#}"
        );
        let msg = format!("{err:#}");
        assert!(msg.contains("LX-D02"), "{msg}");
        assert!(msg.contains("powered on"), "{msg}");
    }

    /// "Found it but it is dead" is not "found nothing": the two send the
    /// user to different places, so the messages must not read alike.
    #[tokio::test]
    async fn a_silent_printer_does_not_read_like_a_missing_one() {
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let err = await_hello(&mut rx, BRIEFLY, "LX-D02", &mut vec![])
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<NoPrinterFound>().is_none());
        assert!(!format!("{err:#}").contains("no LX printer found"));
    }

    /// A dropped forwarder (the link went away mid-probe) is a distinct
    /// failure from silence, and must not hang.
    #[tokio::test]
    async fn a_closed_notification_stream_ends_the_probe() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Notification>();
        drop(tx);
        let err = await_hello(&mut rx, HELLO_TIMEOUT, "LX-D02", &mut vec![])
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("stream closed"), "{err:#}");
    }

    /// A printer running hot cooldowns on every packet; the log must report
    /// the condition without one line per packet.
    #[test]
    fn cooldown_reports_are_rate_limited_and_count_the_gap() {
        let mut log = JobLog::default();
        let t0 = Instant::now();

        assert_eq!(log.note_cooldown(t0), Some(1));
        // Nine more inside the window: silent, but counted.
        for i in 1..10 {
            assert_eq!(log.note_cooldown(t0 + Duration::from_millis(i * 10)), None);
        }
        // Once the gap elapses, the suppressed ones are reported together.
        assert_eq!(log.note_cooldown(t0 + COOLDOWN_REPORT_GAP), Some(10));
        assert_eq!(log.note_cooldown(t0 + COOLDOWN_REPORT_GAP), None);
    }

    // -----------------------------------------------------------------
    // Discovery: matching an advertised device against a target. The
    // resolution ladder is pure (`match_advertised`); the async
    // `match_target` only feeds it BLE advertisement properties.
    // -----------------------------------------------------------------

    /// A stand-in platform id, shaped like nothing a filter test matches on.
    const PID: &str = "hhhhhhhh-hhhh-hhhh-hhhh-hhhhhhhhhhhh";

    /// No flag, no saved device: an X6 is now as discoverable as an LX.
    #[test]
    fn an_x6_device_matches_any_supported() {
        assert!(matches!(
            match_advertised("X6h-A1B2", PID, &Target::AnySupported, None),
            Some((MatchKind::Exact, Some(PrinterModel::X6)))
        ));
    }

    /// The original family keeps matching exactly as before.
    #[test]
    fn lx_devices_still_match_any_supported() {
        assert!(matches!(
            match_advertised("LX-D02", PID, &Target::AnySupported, None),
            Some((MatchKind::Exact, Some(PrinterModel::LxD02)))
        ));
    }

    /// A device from an unsupported family is invisible to discovery, both
    /// with no target and as a saved-device fallback candidate.
    #[test]
    fn an_unsupported_device_never_matches() {
        assert!(match_advertised("GB01", PID, &Target::AnySupported, None).is_none());
        let target = Target::SavedId {
            id: "some-other-id",
            name: "LX-D02",
        };
        assert!(match_advertised("GB01", PID, &target, None).is_none());
    }

    /// An explicit `--device` string is taken literally — name or id
    /// substring, whatever the name looks like. Today's behavior, preserved.
    #[test]
    fn filter_matches_by_name_or_id_substring() {
        assert!(matches!(
            match_advertised("LX-D02", PID, &Target::Filter("LX"), None),
            Some((MatchKind::Exact, Some(PrinterModel::LxD02)))
        ));
        assert!(matches!(
            match_advertised("X6h-A1B2", PID, &Target::Filter("A1B2"), None),
            Some((MatchKind::Exact, Some(PrinterModel::X6)))
        ));
        // A hit on the platform id, under a name no model claims: still a
        // match, with no model to report.
        assert!(matches!(
            match_advertised("Oddball", "aabbccdd", &Target::Filter("bbcc"), None),
            Some((MatchKind::Exact, None))
        ));
        assert!(match_advertised("LX-D02", PID, &Target::Filter("GB"), None).is_none());
    }

    /// A requested model is a hard restriction: a device of another model
    /// never matches, even under a filter string that hits it.
    #[test]
    fn model_restriction_excludes_other_models() {
        let x6 = Some(PrinterModel::X6);
        assert!(match_advertised("LX-D02", PID, &Target::Filter("LX"), x6).is_none());
        assert!(match_advertised("LX-D02", PID, &Target::AnySupported, x6).is_none());

        let lx = Some(PrinterModel::LxD02);
        assert!(match_advertised("X6h-A1B2", PID, &Target::Filter("X6h"), lx).is_none());
        assert!(match_advertised("X6h-A1B2", PID, &Target::AnySupported, lx).is_none());
    }

    /// The restriction admits its own model unchanged.
    #[test]
    fn model_restriction_admits_the_requested_model() {
        assert!(matches!(
            match_advertised(
                "X6h-A1B2",
                PID,
                &Target::AnySupported,
                Some(PrinterModel::X6)
            ),
            Some((MatchKind::Exact, Some(PrinterModel::X6)))
        ));
        assert!(matches!(
            match_advertised(
                "LX-D02",
                PID,
                &Target::Filter("LX"),
                Some(PrinterModel::LxD02)
            ),
            Some((MatchKind::Exact, Some(PrinterModel::LxD02)))
        ));
    }

    /// Saved-device resolution: the saved id wins outright.
    #[test]
    fn saved_id_matches_exactly() {
        let target = Target::SavedId {
            id: PID,
            name: "LX-D02",
        };
        assert!(matches!(
            match_advertised("LX-D02", PID, &target, None),
            Some((MatchKind::Exact, Some(PrinterModel::LxD02)))
        ));
    }

    /// While waiting for the saved id, a device named exactly the saved name
    /// outranks a merely-supported device — and an X6 now qualifies for the
    /// any-supported fallback where only `LX*` names used to.
    #[test]
    fn saved_name_outranks_any_supported_device() {
        let target = Target::SavedId {
            id: "id-never-seen",
            name: "LX-D02",
        };
        let Some((MatchKind::Fallback(named), _)) =
            match_advertised("LX-D02", "another-id", &target, None)
        else {
            panic!("a device named exactly the saved name must be a fallback");
        };
        let Some((MatchKind::Fallback(supported), Some(PrinterModel::X6))) =
            match_advertised("X6h-A1B2", "x6-id", &target, None)
        else {
            panic!("a supported device must be a fallback while the saved id is missing");
        };
        assert_eq!(named, FallbackRank::SavedName);
        assert_eq!(supported, FallbackRank::AnySupported);
        assert!(
            named > supported,
            "saved name must outrank merely-supported"
        );
    }
}
