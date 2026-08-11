# X6 Printer Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Print to the X6/X6h "cat printer" family from every surface (CLI, HTTP server, AirPrint, Web Bluetooth page), 1bpp only, without touching the hardware-validated LX-D02 protocol layer.

**Architecture:** A sibling sans-IO module `protocol_x6/` in `printa-ble-core` (framing, CRC8, notifications, job state machine mirroring the existing `PrintJob` drive contract), a `PrinterModel` enum carrying per-model UUIDs and name prefixes, and model dispatch in the existing transports. Design doc: `docs/plans/2026-08-11-x6-printer-design.md`.

**Tech stack:** Rust 2021 workspace; no new dependencies anywhere (the 1bpp raw-scanline path needs no LZO). `printa-ble-core` stays sans-IO and WASM-clean.

**Protocol ground truth (do not work from memory):**
- <https://parzivail.github.io/ble-thermal-printer/> — X6h frame format, command table, raw scanlines, status notification.
- <https://github.com/nazarovmi/tinyprint-x6h> — working Python implementation; CRC8 table (`encoding.py`), header layout (`protocol.py`), `X6h-`/`x6h-` name prefixes, 20-byte/4 ms write pacing.
- Frame: `51 78 | cmd u8 | direction u8 (00 host→printer, 01 printer→host) | len LE u16 | payload | CRC8(payload only, poly 0x07, init 0) | FF`.
- BLE: service `0xAE30`, write `0xAE01` (write-without-response), notify `0xAE02`.
- Commands used here: `0xA1` feed paper (LE u16 pixels), `0xA2` raw 1bpp scanline (48-byte payload, **leftmost pixel = least-significant bit** — the inverse of our `Bitmap`'s MSB-first layout), `0xAE` device status notification (payload `0x10` = buffer full / stop, `0x00` = ready / resume).
- The first scanline must be all zeroes (white) or the printer prints artifacts (parzivail; tinyprint prepends a blank line too).
- **Known discrepancy for the future 4bpp phase (out of scope now):** parzivail says the 4bpp lower nibble is the leftmost column; tinyprint packs the first pixel into the *upper* nibble (`(p0 >> 4) << 4 | (p1 >> 4)`). Do not implement 4bpp from either source without hardware confirmation.
- **Hardware-validation risk:** tinyprint validates the LZO `0xCF` path, parzivail validates raw `0xA2` on his unit. Ours is unproven on `0xA2` until the maintainer prints. If `0xA2` misprints on real hardware, the fallback is `0xCE` (LZO-compressed binary scanline), which needs an LZO crate — a separate decision, not part of this plan.

**Rules that bind every task:**
- Never modify files under `crates/printa-ble-core/src/protocol/` except the one doc-comment edit in Task 4.
- `cargo fmt --all` before every commit. No AI attribution in commit messages.
- Any task touching `printa-ble-core` or `printa-ble-web` must pass `cargo clippy -p printa-ble-web --target wasm32-unknown-unknown` before its commit.
- Real prints are the maintainer's job. Never run `printable print` against hardware; verify with tests and `--preview`.

---

### Task 1: X6 CRC8

**Files:**
- Create: `crates/printa-ble-core/src/protocol_x6/mod.rs`
- Create: `crates/printa-ble-core/src/protocol_x6/crc.rs`
- Modify: `crates/printa-ble-core/src/lib.rs`

**Step 1: Write the failing test**

Create `crates/printa-ble-core/src/protocol_x6/mod.rs`:

```rust
//! X6/X6h ("cat printer" family) wire protocol.
//!
//! Reverse-engineering sources are pinned in docs/PROTOCOL.md; do not adjust
//! constants from memory. Unlike the LX-D02 protocol this family has no auth
//! handshake, uses CRC8 (poly 0x07) over the payload only, and streams one
//! 48-byte scanline per packet.

pub mod crc;
```

Create `crates/printa-ble-core/src/protocol_x6/crc.rs` with only the test module:

```rust
//! CRC8, polynomial 0x07, init 0, no reflection — matches the checksum table
//! in tinyprint-x6h `encoding.py` and the frames captured by parzivail.

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector is lifted from a captured frame, not computed by us:
    /// `51 78 A4 00 01 00 35 8B FF` (quality packet, parzivail's example),
    /// `51 78 AE 01 01 00 10 70 FF` (buffer full),
    /// `51 78 AE 01 01 00 00 00 FF` (ready).
    #[test]
    fn matches_captured_frames() {
        assert_eq!(crc8(&[0x35]), 0x8B);
        assert_eq!(crc8(&[0x10]), 0x70);
        assert_eq!(crc8(&[0x00]), 0x00);
    }

    #[test]
    fn empty_payload_is_zero() {
        assert_eq!(crc8(&[]), 0x00);
    }

    #[test]
    fn multi_byte_payload() {
        // Feed-paper payload 0x0140 pixels, LE: 40 01.
        // Hand-walked through the tinyprint table: table[0x40]=0xC7,
        // table[0xC7 ^ 0x01]=table[0xC6]=0x5C.
        assert_eq!(crc8(&[0x40, 0x01]), 0x5C);
    }
}
```

In `crates/printa-ble-core/src/lib.rs` add below `pub mod protocol;`:

```rust
pub mod protocol_x6;
```

**Step 2: Run the test to verify it fails**

Run: `cargo test -p printa-ble-core protocol_x6::crc -- --nocapture`
Expected: compile error, `crc8` not found.

**Step 3: Write minimal implementation**

Add above the test module in `crc.rs`:

```rust
/// CRC8 over `data`: polynomial 0x07, init 0, MSB-first, no final XOR.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}
```

**Step 4: Run the tests and make sure they pass**

Run: `cargo test -p printa-ble-core protocol_x6`
Expected: 3 passed.

**Step 5: Clippy (both targets), fmt, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo clippy -p printa-ble-web --target wasm32-unknown-unknown
git add crates/printa-ble-core/src/lib.rs crates/printa-ble-core/src/protocol_x6/
git commit -m "Add X6 protocol module with CRC8"
```

---

### Task 2: X6 packet framing

**Files:**
- Create: `crates/printa-ble-core/src/protocol_x6/packets.rs`
- Modify: `crates/printa-ble-core/src/protocol_x6/mod.rs` (add `pub mod packets;`)

**Step 1: Write the failing tests**

`packets.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Layout check against the parzivail worked example
    /// `51 78 A4 00 01 00 35 8B FF` (command 0xA4, payload [0x35]).
    #[test]
    fn frame_matches_documented_example() {
        assert_eq!(
            frame(0xA4, &[0x35]),
            vec![0x51, 0x78, 0xA4, 0x00, 0x01, 0x00, 0x35, 0x8B, 0xFF]
        );
    }

    #[test]
    fn feed_paper_encodes_pixels_le() {
        // 0x0140 = 320 pixels; CRC over [0x40, 0x01] is 0x5C (Task 1 vector).
        assert_eq!(
            feed_paper(0x0140),
            vec![0x51, 0x78, 0xA1, 0x00, 0x02, 0x00, 0x40, 0x01, 0x5C, 0xFF]
        );
    }

    #[test]
    fn raw_scanline_frame_shape() {
        let row = [0u8; 48];
        let p = raw_scanline(&row);
        assert_eq!(p.len(), 2 + 1 + 1 + 2 + 48 + 1 + 1); // 56
        assert_eq!(&p[..6], &[0x51, 0x78, 0xA2, 0x00, 0x30, 0x00]);
        assert_eq!(p[54], 0x00); // CRC of 48 zero bytes is 0
        assert_eq!(p[55], 0xFF);
    }

    /// Bitmap is MSB-first (leftmost pixel = 0x80); the X6 wants the leftmost
    /// pixel in the least-significant bit, so every byte is bit-reversed.
    #[test]
    fn raw_scanline_reverses_bit_order() {
        let mut row = [0u8; 48];
        row[0] = 0x80; // pixel x=0 black
        row[1] = 0x40; // pixel x=9 black
        let p = raw_scanline(&row);
        assert_eq!(p[6], 0x01);
        assert_eq!(p[7], 0x02);
    }
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p printa-ble-core protocol_x6::packets`
Expected: compile error, `frame` not found.

**Step 3: Implement**

```rust
//! Command frame builders for the X6 wire protocol.
//!
//! Frame layout (see docs/PROTOCOL.md, X6 section):
//! `51 78 | cmd | 00 | len LE u16 | payload | crc8(payload) | FF`.

use super::crc::crc8;
use crate::raster::bitmap::BYTES_PER_ROW;

const MAGIC: [u8; 2] = [0x51, 0x78];
const HOST_TO_PRINTER: u8 = 0x00;
const TRAILER: u8 = 0xFF;

const CMD_FEED_PAPER: u8 = 0xA1;
const CMD_RAW_SCANLINE: u8 = 0xA2;

/// Build one framed command.
pub fn frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u16;
    let mut p = Vec::with_capacity(8 + payload.len());
    p.extend_from_slice(&MAGIC);
    p.push(cmd);
    p.push(HOST_TO_PRINTER);
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(payload);
    p.push(crc8(payload));
    p.push(TRAILER);
    p
}

/// Feed `pixels` rows of blank paper (0xA1).
pub fn feed_paper(pixels: u16) -> Vec<u8> {
    frame(CMD_FEED_PAPER, &pixels.to_le_bytes())
}

/// One uncompressed 1bpp scanline (0xA2).
///
/// `row` is a [`crate::raster::bitmap::Bitmap`] row: MSB-first, bit 1 =
/// black. The X6 wants the leftmost pixel in the least-significant bit, so
/// each byte is bit-reversed on the way out.
pub fn raw_scanline(row: &[u8; BYTES_PER_ROW]) -> Vec<u8> {
    let wire: Vec<u8> = row.iter().map(|b| b.reverse_bits()).collect();
    frame(CMD_RAW_SCANLINE, &wire)
}
```

**Step 4: Run the tests**

Run: `cargo test -p printa-ble-core protocol_x6`
Expected: all pass (Task 1's 3 + these 4).

**Step 5: Clippy (both targets), fmt, commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo clippy -p printa-ble-web --target wasm32-unknown-unknown
git add crates/printa-ble-core/src/protocol_x6/
git commit -m "Add X6 packet framing"
```

---

### Task 3: X6 notification parsing

**Files:**
- Create: `crates/printa-ble-core/src/protocol_x6/notifications.rs`
- Modify: `crates/printa-ble-core/src/protocol_x6/mod.rs` (add `pub mod notifications;`)

**Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The two flow-control frames as captured (parzivail, verbatim hex).
    #[test]
    fn parses_captured_status_frames() {
        let full = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x10, 0x70, 0xFF];
        let ready = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x00, 0x00, 0xFF];
        assert_eq!(parse(&full), Some(X6Notification::BufferFull));
        assert_eq!(parse(&ready), Some(X6Notification::Ready));
    }

    #[test]
    fn rejects_wrong_magic_direction_or_command() {
        // wrong magic
        assert_eq!(parse(&[0x5A, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x10, 0x70, 0xFF]), None);
        // host->printer direction byte
        assert_eq!(parse(&[0x51, 0x78, 0xAE, 0x00, 0x01, 0x00, 0x10, 0x70, 0xFF]), None);
        // unknown command id: not ours to interpret
        assert_eq!(parse(&[0x51, 0x78, 0xBA, 0x01, 0x01, 0x00, 0x63, 0x00, 0xFF]), None);
        // unknown status payload value
        assert_eq!(parse(&[0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x42, 0x00, 0xFF]), None);
    }

    #[test]
    fn rejects_truncated_frames() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x51]), None);
        assert_eq!(parse(&[0x51, 0x78, 0xAE, 0x01, 0x01, 0x00]), None);
    }
}
```

**Step 2: Run to verify failure** — `cargo test -p printa-ble-core protocol_x6::notifications` → compile error.

**Step 3: Implement**

```rust
//! Parser for X6 frames received on characteristic 0xAE02.
//!
//! Only the 0xAE device-status frames are understood; everything else (battery
//! frames, device info, models that prefix frames with 0x12) parses to `None`
//! and is logged by the transport as an unparseable frame rather than treated
//! as fatal — the family has many undocumented variants.

const CMD_DEVICE_STATUS: u8 = 0xAE;
const PRINTER_TO_HOST: u8 = 0x01;
const STATUS_BUFFER_FULL: u8 = 0x10;
const STATUS_READY: u8 = 0x00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X6Notification {
    /// RX buffer full: stop sending scanlines.
    BufferFull,
    /// Buffer drained: sending may resume.
    Ready,
}

pub fn parse(data: &[u8]) -> Option<X6Notification> {
    // 51 78 | cmd | dir | len LE u16 | payload | crc | FF
    if data.len() < 9 || data[0] != 0x51 || data[1] != 0x78 {
        return None;
    }
    if data[3] != PRINTER_TO_HOST || data[2] != CMD_DEVICE_STATUS {
        return None;
    }
    let len = u16::from_le_bytes([data[4], data[5]]) as usize;
    if len != 1 || data.len() < 6 + len + 2 {
        return None;
    }
    match data[6] {
        STATUS_BUFFER_FULL => Some(X6Notification::BufferFull),
        STATUS_READY => Some(X6Notification::Ready),
        _ => None,
    }
}
```

**Step 4: Run** — `cargo test -p printa-ble-core protocol_x6` → all pass.

**Step 5: Clippy (both targets), fmt, commit** — message: `Parse X6 device status notifications`.

---

### Task 4: X6 print job state machine

**Files:**
- Create: `crates/printa-ble-core/src/protocol_x6/job.rs`
- Modify: `crates/printa-ble-core/src/protocol_x6/mod.rs` (add `pub mod job;`)
- Modify: `crates/printa-ble-core/src/protocol/job.rs` — **doc comments only** (see step 3); no code changes.

The X6 job reuses `Action` and `JobStats` from `crate::protocol::job` so the transport pump and job-summary logging work identically for both printers. `retransmits` and `cooldowns` stay 0 forever on X6 — the protocol has no such events.

**Step 1: Failing tests** (bottom of the new `job.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::job::Action;
    use crate::raster::bitmap::Bitmap;

    fn drain_sends(job: &mut X6PrintJob) -> Vec<Vec<u8>> {
        let mut sent = vec![];
        loop {
            match job.next_action() {
                Action::Send(b) => sent.push(b),
                Action::WaitMs(_) => continue,
                _ => break,
            }
        }
        sent
    }

    #[test]
    fn happy_path_streams_blank_lead_then_rows_then_feed() {
        let mut bitmap = Bitmap::new(2);
        bitmap.set(0, 0, true); // MSB-first 0x80 -> wire 0x01
        let mut job = X6PrintJob::new(&bitmap, 64, 0);

        let sent = drain_sends(&mut job);
        // blank artifact-guard line, 2 bitmap rows, feed
        assert_eq!(sent.len(), 4);
        assert_eq!(&sent[0][..3], &[0x51, 0x78, 0xA2]);
        assert!(sent[0][6..54].iter().all(|&b| b == 0)); // lead row is blank
        assert_eq!(sent[1][6], 0x01); // bit-reversed pixel
        assert_eq!(&sent[3][..3], &[0x51, 0x78, 0xA1]); // trailing feed
        assert_eq!(&sent[3][6..8], &[64, 0]); // 64 px LE
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn buffer_full_pauses_until_ready() {
        let bitmap = Bitmap::new(4);
        let mut job = X6PrintJob::new(&bitmap, 0, 0);
        let _ = job.next_action(); // lead row
        let _ = job.next_action(); // row 0

        job.on_notification(X6Notification::BufferFull);
        assert!(matches!(job.next_action(), Action::WaitNotification));
        assert!(matches!(job.next_action(), Action::WaitNotification));

        job.on_notification(X6Notification::Ready);
        match job.next_action() {
            Action::Send(p) => assert_eq!(p[2], 0xA2),
            other => panic!("expected resumed scanline, got {other:?}"),
        }
        assert_eq!(job.stats().holds, 1);
    }

    #[test]
    fn ready_without_pause_is_ignored() {
        let bitmap = Bitmap::new(2);
        let mut job = X6PrintJob::new(&bitmap, 0, 0);
        job.on_notification(X6Notification::Ready);
        assert_eq!(job.stats(), JobStats::default());
        // still streams from the start
        match job.next_action() {
            Action::Send(p) => assert_eq!(p[2], 0xA2),
            other => panic!("expected scanline, got {other:?}"),
        }
    }

    #[test]
    fn inter_packet_delay_between_scanlines_only() {
        let bitmap = Bitmap::new(2);
        let mut job = X6PrintJob::new(&bitmap, 64, 15);
        assert!(matches!(job.next_action(), Action::Send(_))); // lead
        assert!(matches!(job.next_action(), Action::WaitMs(15)));
        assert!(matches!(job.next_action(), Action::Send(_))); // row 0
        assert!(matches!(job.next_action(), Action::WaitMs(15)));
        assert!(matches!(job.next_action(), Action::Send(_))); // row 1
        // no delay between last scanline and the feed command
        match job.next_action() {
            Action::Send(p) => assert_eq!(p[2], 0xA1),
            other => panic!("expected feed, got {other:?}"),
        }
    }

    #[test]
    fn settle_wait_before_done() {
        let bitmap = Bitmap::new(1);
        let mut job = X6PrintJob::new(&bitmap, 64, 0);
        let _ = job.next_action(); // lead row
        let _ = job.next_action(); // row 0
        let _ = job.next_action(); // feed
        assert!(matches!(job.next_action(), Action::WaitMs(SETTLE_MS)));
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn zero_feed_skips_feed_command() {
        let bitmap = Bitmap::new(1);
        let mut job = X6PrintJob::new(&bitmap, 0, 0);
        let sent = drain_sends(&mut job);
        assert_eq!(sent.len(), 2); // lead + row, no 0xA1
        assert!(sent.iter().all(|p| p[2] == 0xA2));
    }

    #[test]
    fn stats_count_scanlines_not_feed() {
        let bitmap = Bitmap::new(3);
        let mut job = X6PrintJob::new(&bitmap, 64, 0);
        drain_sends(&mut job);
        assert_eq!(job.stats().packets_sent, 4); // lead + 3 rows
        assert_eq!(job.stats().retransmits, 0);
        assert_eq!(job.stats().cooldowns, 0);
    }
}
```

**Step 2: Run to verify failure** — `cargo test -p printa-ble-core protocol_x6::job` → compile error.

**Step 3: Implement**

```rust
//! Sans-IO X6 print job state machine.
//!
//! Far simpler than the LX-D02 flow: no hello, no auth, no completion
//! notification. Stream one 0xA2 scanline frame per bitmap row (plus one
//! blank lead row — the printer prints artifacts if the first row has ink),
//! pause on BufferFull / resume on Ready, then feed and settle.

use crate::protocol::job::{Action, JobStats};
use crate::protocol_x6::notifications::X6Notification;
use crate::protocol_x6::packets;
use crate::raster::bitmap::{Bitmap, BYTES_PER_ROW};

/// Wait after the final feed before declaring the job done, so the transport
/// does not tear the link down while the printer is still draining its
/// buffer. The printer sends no completion event, so this is a guess; tune
/// against hardware.
const SETTLE_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Streaming,
    /// Printer said BufferFull; waiting for Ready.
    Paused,
    SendFeed,
    Settle,
    Done,
}

/// Sans-IO driver for one X6 print job. Same drive contract as
/// [`crate::protocol::job::PrintJob`]: call `next_action`, perform it, feed
/// notifications back in.
#[derive(Debug)]
pub struct X6PrintJob {
    rows: Vec<[u8; BYTES_PER_ROW]>,
    state: State,
    send_idx: usize,
    feed_px: u16,
    inter_packet_delay_ms: u64,
    pending_wait_ms: Option<u64>,
    stats: JobStats,
}

impl X6PrintJob {
    /// A job that prints `bitmap`, then feeds `feed_px` rows of blank paper.
    ///
    /// Unlike the LX-D02 job this cannot fail to construct: there is no
    /// packet-index limit, no density, and no auth challenge.
    pub fn new(bitmap: &Bitmap, feed_px: u16, inter_packet_delay_ms: u64) -> Self {
        // Blank lead row: the printer misprints if row 0 carries ink.
        let mut rows = vec![[0u8; BYTES_PER_ROW]];
        rows.extend((0..bitmap.height()).map(|y| *bitmap.row(y)));
        Self {
            rows,
            state: State::Streaming,
            send_idx: 0,
            feed_px,
            inter_packet_delay_ms,
            pending_wait_ms: None,
            stats: JobStats::default(),
        }
    }

    #[must_use]
    pub fn next_action(&mut self) -> Action {
        if let Some(ms) = self.pending_wait_ms.take() {
            return Action::WaitMs(ms);
        }
        match self.state {
            State::Streaming => match self.rows.get(self.send_idx) {
                Some(row) => {
                    let packet = packets::raw_scanline(row);
                    self.send_idx += 1;
                    self.stats.packets_sent = self.stats.packets_sent.saturating_add(1);
                    if self.inter_packet_delay_ms > 0 && self.send_idx < self.rows.len() {
                        self.pending_wait_ms = Some(self.inter_packet_delay_ms);
                    }
                    Action::Send(packet)
                }
                None => {
                    self.state = State::SendFeed;
                    self.next_action()
                }
            },
            State::SendFeed => {
                self.state = State::Settle;
                if self.feed_px == 0 {
                    return self.next_action();
                }
                Action::Send(packets::feed_paper(self.feed_px))
            }
            State::Settle => {
                self.state = State::Done;
                Action::WaitMs(SETTLE_MS)
            }
            State::Paused => Action::WaitNotification,
            State::Done => Action::Done,
        }
    }

    /// Feed a parsed notification from 0xAE02 into the state machine.
    /// Notifications that make no sense in the current state are ignored.
    pub fn on_notification(&mut self, n: X6Notification) {
        match (self.state, n) {
            (State::Streaming, X6Notification::BufferFull) => {
                self.state = State::Paused;
                self.pending_wait_ms = None;
                self.stats.holds = self.stats.holds.saturating_add(1);
            }
            (State::Paused, X6Notification::Ready) => {
                self.state = State::Streaming;
            }
            _ => {}
        }
    }

    pub fn stats(&self) -> JobStats {
        self.stats
    }
}
```

Note the recursion in `next_action` is bounded (Streaming→SendFeed→Settle, at most two hops); if clippy objects, convert to a `loop`.

Then edit **doc comments only** in `crates/printa-ble-core/src/protocol/job.rs` on the `Action` enum so they are not LX-specific, e.g. `/// Write these bytes to the printer's write characteristic.` and `/// Block on the notify characteristic until a notification is fed via on_notification().` — no code changes, byte tests untouched.

**Step 4: Run** — `cargo test -p printa-ble-core` → everything passes, including all pre-existing tests.

**Step 5: Clippy (both targets), fmt, commit** — `Add X6 print job state machine`.

---

### Task 5: `PrinterModel` in core

**Files:**
- Create: `crates/printa-ble-core/src/model.rs`
- Modify: `crates/printa-ble-core/src/lib.rs` (add `pub mod model;`)

**Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_models_from_advertised_names() {
        assert_eq!(PrinterModel::from_device_name("LX-D02"), Some(PrinterModel::LxD02));
        assert_eq!(PrinterModel::from_device_name("LXP-42"), Some(PrinterModel::LxD02));
        assert_eq!(PrinterModel::from_device_name("X6h-A1B2"), Some(PrinterModel::X6));
        assert_eq!(PrinterModel::from_device_name("x6h-A1B2"), Some(PrinterModel::X6));
        assert_eq!(PrinterModel::from_device_name("GB01"), None);
        // "X6H-" (capital H) is a *different* model per parzivail; do not match it.
        assert_eq!(PrinterModel::from_device_name("X6H-A1B2"), None);
    }

    #[test]
    fn uuids_per_model() {
        assert_eq!(PrinterModel::LxD02.service_uuid16(), 0xFFE6);
        assert_eq!(PrinterModel::LxD02.write_char_uuid16(), 0xFFE1);
        assert_eq!(PrinterModel::LxD02.notify_char_uuid16(), 0xFFE2);
        assert_eq!(PrinterModel::X6.service_uuid16(), 0xAE30);
        assert_eq!(PrinterModel::X6.write_char_uuid16(), 0xAE01);
        assert_eq!(PrinterModel::X6.notify_char_uuid16(), 0xAE02);
    }

    #[test]
    fn string_round_trip_for_config_and_cli() {
        assert_eq!("lx-d02".parse::<PrinterModel>(), Ok(PrinterModel::LxD02));
        assert_eq!("x6".parse::<PrinterModel>(), Ok(PrinterModel::X6));
        assert!("gb01".parse::<PrinterModel>().is_err());
        assert_eq!(PrinterModel::LxD02.to_string(), "lx-d02");
        assert_eq!(PrinterModel::X6.to_string(), "x6");
    }
}
```

**Step 2: Run to verify failure** — compile error.

**Step 3: Implement**

```rust
//! The printer models this workspace can drive, and the per-model facts the
//! transports need. Values, not behavior: keeping UUIDs and name prefixes
//! here lets the CLI, server, and browser share one source of truth without
//! core doing any I/O.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterModel {
    /// LX-D02, the original reverse-engineered target (protocol/).
    LxD02,
    /// X6/X6h "cat printer" family (protocol_x6/).
    X6,
}

impl PrinterModel {
    /// Infer the model from a BLE advertised name, if it looks like a
    /// printer we support. `X6h-` matches case-insensitively on the prefix's
    /// first letter only: parzivail notes `X6H` (capital H) is a distinct
    /// model, so it is deliberately not claimed here.
    pub fn from_device_name(name: &str) -> Option<Self> {
        if name.starts_with("LX") {
            Some(Self::LxD02)
        } else if name.starts_with("X6h-") || name.starts_with("x6h-") {
            Some(Self::X6)
        } else {
            None
        }
    }

    pub fn service_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE6,
            Self::X6 => 0xAE30,
        }
    }

    pub fn write_char_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE1,
            Self::X6 => 0xAE01,
        }
    }

    pub fn notify_char_uuid16(self) -> u16 {
        match self {
            Self::LxD02 => 0xFFE2,
            Self::X6 => 0xAE02,
        }
    }
}

impl fmt::Display for PrinterModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LxD02 => "lx-d02",
            Self::X6 => "x6",
        })
    }
}

impl FromStr for PrinterModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lx-d02" => Ok(Self::LxD02),
            "x6" => Ok(Self::X6),
            other => Err(format!("unknown printer model '{other}' (expected 'lx-d02' or 'x6')")),
        }
    }
}
```

**Step 4: Run** — `cargo test -p printa-ble-core model` → pass; full core suite still green.

**Step 5: Clippy (both targets), fmt, commit** — `Add PrinterModel enum`.

---

### Task 6: Model-aware discovery in `ble.rs`

The matcher currently hardcodes `LX*`. Replace name checks with `PrinterModel::from_device_name`, thread an optional model restriction through, and rename `Target::AnyLx` to `Target::AnySupported`.

**Files:**
- Modify: `crates/printa-ble/src/ble.rs` — the `Target` enum (~line 378), `match_target` (~line 400), `scan` (~line 423), and their tests at the bottom of the file.

**Steps:**

1. Read the existing matcher tests in `ble.rs` (`grep -n "fn match_target\|mod tests" crates/printa-ble/src/ble.rs`) to see the test style, then **write failing tests**: `X6h-A1B2` with no filter matches as `AnySupported`; `GB01` never matches; with a `model` restriction of `X6`, an `LX-D02` device is skipped even though it is a supported printer; `SavedId` fallback prefers saved-name over any supported device (existing semantics, now cross-model).
2. Run `cargo test -p printa-ble match_target` — new tests fail.
3. Implement: `match_target` returns the matched model alongside the name (`(MatchKind, String, PrinterModel)`); `FallbackRank::AnyLx` becomes `AnySupported`; add `model: Option<PrinterModel>` to the `Target::Filter`/`AnySupported` paths (a `--model` restriction filters candidates before matching). Update `scan()` so `printable devices` reports each device's model. Keep the log line's meaning ("scan saw N devices, M supported").
4. `cargo test -p printa-ble` — all green.
5. fmt, clippy, commit: `Discover X6 printers alongside LX`.

---

### Task 7: Model-aware connect and X6 job pump in `ble.rs`

**Files:**
- Modify: `crates/printa-ble/src/ble.rs` — `initialize` (~line 577), the notification forwarder (~line 628), `Printer` struct (~line 481), `run_job`/`pump` (~lines 778–860), `connect_resolved`.

**Design (follow exactly):**

- `Printer` gains `model: PrinterModel` and a `pub fn model(&self)` accessor.
- `initialize` selects UUIDs via `uuid_from_u16(model.write_char_uuid16())` etc. Error messages become model-aware ("not an LX printer?" → include the expected model).
- The forwarder channel carries `enum PrinterEvent { Lx(Notification), X6(X6Notification) }`; the forwarder parses with `notifications::parse` or `protocol_x6::notifications::parse` depending on `model`. Unparseable frames stay debug-logged.
- **LX-D02 behavior must not change:** the hello proof-of-life handshake runs only for `LxD02`. For X6 there is no known liveness probe yet; `initialize` returns after subscribe, and the plan accepts the weaker "connected" claim (macOS cached-GATT caveat documented in Task 12). Do not invent a probe from the unknown commands (0xA3/0xA8).
- `run_job`/`pump` keep their exact current code paths for LX (unwrapping `PrinterEvent::Lx`; a `PrinterEvent::X6` there is impossible by construction — `debug!` and drop it).
- Add `run_x6_job(&mut self, job: &mut X6PrintJob) -> Result<JobStats>` + `pump_x6`, mirroring `run_job`/`pump`: same `StallGuard`, `NOTIFICATION_TIMEOUT`, stall messaging, and `JobLog` pause accounting (a `BufferFull` pauses the log like `Hold` does; `Ready` resumes). No paper-status abort — X6 has no known paper signal. The X6 job has no fatal-error accessor, so skip the `job.error()` check.
- `wait_status` stays LX-only: for an X6 printer return `Err(anyhow!("status not supported on this printer"))` immediately (callers already treat status failure as non-fatal).

**Steps:**

1. Write failing native tests for whatever is pure: `PrinterEvent` forwarding/parse selection (feed captured X6 frames through the parse-dispatch helper), `is_raster`-equivalent labelling for X6 frames in `describe_write` (an `0xA2` frame should be described as a scanline, not hex-dumped — mirror the existing raster-packet rule at ~line 146), and pause accounting driven by `BufferFull`/`Ready`.
2. Run to see them fail; implement; run the full `cargo test -p printa-ble`.
3. fmt, clippy, commit: `Drive X6 print jobs over BLE`.

---

### Task 8: Config remembers the model

**Files:**
- Modify: `crates/printa-ble/src/config.rs` — `SavedDevice`
- Modify: `crates/printa-ble/src/print_service.rs` — `remember_device`

**Steps:**

1. Failing test in `config.rs`: a `SavedDevice` with `model: Some(PrinterModel::X6)` round-trips through TOML; an **old config file without a model field still loads** (`toml::from_str` on a literal `id = "..."\nname = "..."` table) — backward compatibility is the point of the test.
2. Implement: `pub model: Option<String>` — store the `Display` form (`"x6"`), not a serde enum, so the config file stays human-editable and core types stay out of serde. `#[serde(default, skip_serializing_if = "Option::is_none")]`.
3. `remember_device` fills it from `printer.model().to_string()`.
4. Full `cargo test -p printa-ble`; fmt; clippy; commit: `Remember printer model in config`.

---

### Task 9: Model dispatch in `print_service.rs`

**Files:**
- Modify: `crates/printa-ble/src/print_service.rs`

**Design (follow exactly):**

- `print_bitmap` gains a `model: Option<PrinterModel>` parameter (the `--model` override), passed to `ble::connect_resolved`.
- The early `PrintJob::new` fail-fast validation and `bitmap.extend_blank(opts.feed)` both move to **after** connect, where the model is known:
  - `LxD02`: `extend_blank(opts.feed)` + `PrintJob` per copy, exactly as today (fresh `rand::random()` challenge per copy, `INTER_PACKET_DELAY_MS`).
  - `X6`: no `extend_blank`; `X6PrintJob::new(&bitmap, feed_px, INTER_PACKET_DELAY_MS)` per copy, where `feed_px = u16::try_from(opts.feed).unwrap_or(u16::MAX)`. `opts.density` is accepted but unused (the X6 quality/energy commands are a later phase) — document that in the `PrintOptions.density` doc comment.
- The pre-print `wait_status` paper check runs only for `LxD02`.
- `NoPrinterFound`'s message "no LX printer found..." becomes "no supported printer found. Is the printer on and in range?".

**Steps:**

1. Failing tests: `print_service.rs` has no BLE-free path through `print_bitmap`, so the tests here are for the pure helpers — add one for the feed clamp (`usize::MAX` → `u16::MAX`) if you extract it as a function; the dispatch itself is covered by compilation and by Task 10's CLI plumbing. Keep honest: note in the commit that the dispatch arm is hardware-exercised only.
2. Implement; update the two `print_bitmap` call sites (`main.rs`, `server.rs`) with `None` for now so the workspace compiles.
3. Full `cargo test --workspace`; fmt; clippy; commit: `Dispatch print jobs by printer model`.

---

### Task 10: CLI and server `--model` flag

**Files:**
- Modify: `crates/printa-ble/src/cli.rs` — `DeviceArgs` (~line 177)
- Modify: `crates/printa-ble/src/main.rs` — thread `model` into `print_service::print_bitmap` and `ble` calls (status, devices)
- Modify: `crates/printa-ble/src/server.rs` — serve args/state (~lines 48, 201–265): add `model` to `ServeState`, pass through both connect paths

**Steps:**

1. Failing test: `cli.rs` has clap parse tests (check `grep -n "mod tests" crates/printa-ble/src/cli.rs`); add one asserting `--model x6` parses to `Some(PrinterModel::X6)` and an invalid value errors listing the choices. Add to `DeviceArgs`:

```rust
/// Printer model to target (lx-d02 | x6). Default: detect from the device name.
#[arg(long, global = false, value_parser = clap::value_parser!(PrinterModel))]
pub model: Option<PrinterModel>,
```

(`value_parser!` needs `PrinterModel: FromStr<Err: Display> + Clone + Send + Sync` — already true. If clap balks, fall back to `value_parser = PrinterModel::from_str`.)

2. Thread it: every `DeviceArgs` consumer passes `args.device.model` down; server's `--model` lands in `ServeState` next to `device` and flows into both `connect_resolved` call sites (lines ~264 and inside the print path). Server route behavior needs no new parameters — the flag is serve-time, like `--device`.
3. Existing in-process server tests (`tower::ServiceExt`) must stay green: `cargo test --workspace`.
4. fmt; clippy; commit: `Add --model flag to CLI and server`.

---

### Task 11: Web Bluetooth support

**Files:**
- Create: `crates/printa-ble-web/src/x6job.rs`
- Modify: `crates/printa-ble-web/src/lib.rs` (add `pub mod x6job;` and re-export)
- Modify: `web/app.js` (~line 14 `SERVICE`, ~line 204 `requestDevice`, connect/drive path)

**Steps:**

1. Failing native tests in `x6job.rs` (the wrapper is testable off-wasm, same as `job.rs`'s `next_action_inner` pattern): a `WasmX6Job` built from a `WasmBitmap` streams `0xA2` frames and finishes `{kind:"done"}` after the feed + settle; empty bitmap is rejected with the same message as `WasmJob` ("nothing to print: bitmap is empty"); raw `0xAE02` bytes fed to `on_notification` pause/resume it.
2. Implement `WasmX6Job` mirroring `job.rs`: constructor `new(bitmap: &WasmBitmap, feed_px: u16)` (no density, no challenge), `next_action()` reusing `ActionMsg` (make `ActionMsg` + its `From<Action>` `pub(crate)`-reachable from the new module), `on_notification(&[u8])` via `protocol_x6::notifications::parse`. Use the same `INTER_PACKET_DELAY_MS = 15`.
3. Add `#[wasm_bindgen]` UUID helpers to `lib.rs` so JS stops hardcoding: `pub fn lx_service_uuid() -> u16`, `pub fn x6_service_uuid() -> u16`, etc., delegating to `PrinterModel` (or one function returning a JS object per model — keep it dumb).
4. `app.js`: request both families —

```js
const device = await navigator.bluetooth.requestDevice({
  filters: [{ namePrefix: 'LX' }, { namePrefix: 'X6h-' }, { namePrefix: 'x6h-' }],
  optionalServices: [LX_SERVICE, X6_SERVICE],
});
```

then probe: `try { svc = await server.getPrimaryService(LX_SERVICE); model = 'lx'; } catch { svc = await server.getPrimaryService(X6_SERVICE); model = 'x6'; }` — the exposed service *is* the model detection. Select write/notify characteristics and job constructor (`WasmJob` with `crypto.getRandomValues(new Uint8Array(10))` vs `WasmX6Job`) off `model`. The drive loop is shared and unchanged.
5. Verify: `cargo test -p printa-ble-web` (native tests), `cargo clippy -p printa-ble-web --target wasm32-unknown-unknown`, and `scripts/build-web.sh` builds. Browser check is manual (needs Chrome + printer) — not claimed in the commit.
6. fmt; clippy; commit: `Add X6 support to the web app`.

---

### Task 12: Documentation

**Files:**
- Modify: `docs/PROTOCOL.md` — new "X6 / X6h family" section: BLE UUIDs, frame layout, the command subset used, status notification, bit order, blank lead row, the no-liveness-probe caveat, the 4bpp nibble-order discrepancy, links to both sources.
- Modify: `docs/CLI.md`, `docs/API.md` — `--model` flag, model column in `devices` output, X6 notes (density ignored, no paper detection).
- Modify: `README.md` — supported printers list; note X6 support is new and which parts are hardware-validated (fill honestly at the time of writing).
- Modify: `SECURITY.md` only if `serve` gains surface (it does not — skip unless something changed).

Steps: write, `cargo test --workspace` (doc tests, if any), commit: `Document X6 printer support`.

---

### Task 13: Final verification and hardware handoff

**Step 1: Full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo clippy -p printa-ble-web --target wasm32-unknown-unknown
cargo test --workspace
```

Everything passes; exactly 1 ignored test (Chrome). Count, don't quote.

**Step 2: Render a preview and look at it**

```bash
cargo run -p printa-ble -- print "X6 hello" --preview /tmp/x6-preview.png
```

Open the PNG (Read tool) and confirm it renders text — the rendering path is shared, so this is a regression check, not an X6 check.

**Step 3: Hardware validation — maintainer only.** Print nothing yourself. Hand the maintainer this checklist:

1. `printable devices` — the X6 appears with model `x6`.
2. `printable print "X6 test" ` — short text, 1 copy, default feed. Watch for: garbled bit order (mirrored/noise → bit-reversal wrong), dark first line (lead row not honored), premature cut-off (settle too short / disconnect too eager), stalls (flow control).
3. If output is garbled on `0xA2`, that is the known raw-scanline risk; report back and we evaluate the `0xCE`+LZO fallback as a follow-up.

**Step 4:** Update README/PROTOCOL "hardware-validated" claims to match what the maintainer actually confirmed, commit, then use superpowers:finishing-a-development-branch.
