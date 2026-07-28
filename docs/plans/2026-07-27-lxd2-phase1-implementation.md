# lxd2 Phase 1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build the sans-IO protocol core (`lxd2-core`) and a minimal CLI (`lxd2`) that can scan for, query, and print text/images to an LX-D02/LX-D2 BLE thermal printer, with `--preview` for paperless testing.

**Architecture:** Cargo workspace. `lxd2-core` is pure (no BLE, no async): packet builders/parsers, CRC16 auth, a print state machine driven by `next_action()`/`on_notification()`, and a raster pipeline producing 384-px-wide 1-bit bitmaps. The `lxd2` binary wraps it with `btleplug` (CoreBluetooth) and `clap`.

**Tech Stack:** Rust stable, `btleplug` 0.11, `tokio`, `clap` 4 (derive), `image`, `fontdue`, `thiserror`, `anyhow`.

**Reference:** Design doc at `docs/plans/2026-07-27-lxd2-design.md`. Protocol reference sources (downloaded): ValdikSS `rastertofunnyprint.py` (protocol spec), paradon `lxprinter.ts` (auth), rusq `fsm.go` (state machine).

---

### Task 1: Workspace scaffolding

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/lxd2-core/Cargo.toml`, `crates/lxd2-core/src/lib.rs`
- Create: `crates/lxd2/Cargo.toml`, `crates/lxd2/src/main.rs`
- Create: `.gitignore`

**Step 1: Create workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/lxd2-core", "crates/lxd2"]

[workspace.package]
edition = "2021"
license = "MIT"
```

**Step 2: Create `crates/lxd2-core/Cargo.toml`**

```toml
[package]
name = "lxd2-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
thiserror = "1"

[dev-dependencies]
# none yet
```

`src/lib.rs`: empty module declarations added as tasks progress; start with just `pub mod protocol;` commented out or omitted.

**Step 3: Create `crates/lxd2/Cargo.toml`**

```toml
[package]
name = "lxd2"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[dependencies]
lxd2-core = { path = "../lxd2-core" }
anyhow = "1"
clap = { version = "4", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
btleplug = "0.11"
futures = "0.3"
image = "0.25"
```

`src/main.rs`: `fn main() { println!("lxd2"); }` placeholder.

**Step 4: `.gitignore`**

```
/target
```

**Step 5: Verify it builds**

Run: `cargo build`
Expected: compiles with no errors.

**Step 6: Commit**

```bash
git add -A && git commit -m "Scaffold cargo workspace with lxd2-core and lxd2 crates"
```

---

### Task 2: CRC16-XMODEM

**Files:**
- Create: `crates/lxd2-core/src/protocol/crc.rs`
- Create: `crates/lxd2-core/src/protocol/mod.rs` (`pub mod crc;`)
- Modify: `crates/lxd2-core/src/lib.rs` (`pub mod protocol;`)

**Step 1: Write the failing test** (bottom of `crc.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_xmodem_check_value() {
        // Standard CRC16/XMODEM check: "123456789" -> 0x31C3
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn crc16_xmodem_empty_is_zero() {
        assert_eq!(crc16_xmodem(&[]), 0x0000);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p lxd2-core crc`
Expected: FAIL — `crc16_xmodem` not found.

**Step 3: Write minimal implementation**

```rust
/// CRC16/XMODEM: poly 0x1021, init 0x0000, no reflection, no xorout.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p lxd2-core crc`
Expected: 2 passed.

**Step 5: Commit**

```bash
git add -A && git commit -m "Add CRC16-XMODEM implementation"
```

---

### Task 3: Auth response computation

**Files:**
- Create: `crates/lxd2-core/src/protocol/auth.rs`
- Modify: `crates/lxd2-core/src/protocol/mod.rs` (`pub mod auth;`)

The handshake (per paradon `lxprinter.ts` / ValdikSS): for each of the 10
challenge bytes `c` we sent in `5A 0A`, compute
`crc16_xmodem(&[c, mac0, mac1, mac2, mac3, mac4, mac5])` and take the **high
byte**. The 10 high bytes are the `5A 0B` payload.

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::crc::crc16_xmodem;

    const MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33];

    #[test]
    fn auth_response_matches_manual_crc() {
        let challenge = [0u8; 10];
        let resp = auth_response(&challenge, &MAC);
        // Every byte identical for an all-zero challenge (ValdikSS's shortcut)
        let mut buf = vec![0u8];
        buf.extend_from_slice(&MAC);
        let expected = (crc16_xmodem(&buf) >> 8) as u8;
        assert_eq!(resp, [expected; 10]);
    }

    #[test]
    fn auth_response_uses_each_challenge_byte() {
        let challenge = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let resp = auth_response(&challenge, &MAC);
        for (i, &c) in challenge.iter().enumerate() {
            let mut buf = vec![c];
            buf.extend_from_slice(&MAC);
            assert_eq!(resp[i], (crc16_xmodem(&buf) >> 8) as u8);
        }
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p lxd2-core auth`
Expected: FAIL — `auth_response` not found.

**Step 3: Write minimal implementation**

```rust
use crate::protocol::crc::crc16_xmodem;

/// Compute the 10-byte `5A 0B` auth payload from our challenge bytes and the
/// printer's MAC (learned from the `5A 01` hello reply, bytes 4..10).
pub fn auth_response(challenge: &[u8; 10], mac: &[u8; 6]) -> [u8; 10] {
    let mut out = [0u8; 10];
    for (i, &c) in challenge.iter().enumerate() {
        let mut buf = [0u8; 7];
        buf[0] = c;
        buf[1..].copy_from_slice(mac);
        out[i] = (crc16_xmodem(&buf) >> 8) as u8;
    }
    out
}
```

**Step 4: Run tests** — Expected: PASS.

**Step 5: Commit** — `git commit -m "Add MAC-keyed auth response computation"`

---

### Task 4: Command packet builders

**Files:**
- Create: `crates/lxd2-core/src/protocol/packets.rs`
- Modify: `crates/lxd2-core/src/protocol/mod.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_packet_bytes() {
        assert_eq!(
            hello(),
            [0x5A, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn density_packet_bytes() {
        assert_eq!(set_density(3), [0x5A, 0x0C, 3]);
    }

    #[test]
    fn print_start_end_encode_length_big_endian() {
        assert_eq!(print_start(0x0142), [0x5A, 0x04, 0x01, 0x42, 0x00, 0x00]);
        assert_eq!(print_end(0x0142), [0x5A, 0x04, 0x01, 0x42, 0x01, 0x00]);
    }

    #[test]
    fn auth_challenge_packet() {
        let c = [9u8; 10];
        let p = auth_challenge(&c);
        assert_eq!(&p[..2], &[0x5A, 0x0A]);
        assert_eq!(&p[2..], &c);
    }

    #[test]
    fn raster_packet_layout() {
        let data = [0xFFu8; 96];
        let p = raster(0x0203, &data);
        assert_eq!(p.len(), 100);
        assert_eq!(&p[..3], &[0x55, 0x02, 0x03]);
        assert_eq!(&p[3..99], &data[..]);
        assert_eq!(p[99], 0x00);
    }
}
```

**Step 2: Run** `cargo test -p lxd2-core packets` — Expected: FAIL.

**Step 3: Implement**

```rust
pub const RASTER_DATA_LEN: usize = 96; // two 48-byte print lines

pub fn hello() -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x01;
    p
}

pub fn set_density(level: u8) -> [u8; 3] {
    [0x5A, 0x0C, level]
}

pub fn print_start(num_packets: u16) -> [u8; 6] {
    let [hi, lo] = num_packets.to_be_bytes();
    [0x5A, 0x04, hi, lo, 0x00, 0x00]
}

pub fn print_end(num_packets: u16) -> [u8; 6] {
    let [hi, lo] = num_packets.to_be_bytes();
    [0x5A, 0x04, hi, lo, 0x01, 0x00]
}

pub fn auth_challenge(challenge: &[u8; 10]) -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x0A;
    p[2..].copy_from_slice(challenge);
    p
}

pub fn auth_reply(response: &[u8; 10]) -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x0B;
    p[2..].copy_from_slice(response);
    p
}

pub fn raster(index: u16, data: &[u8; RASTER_DATA_LEN]) -> [u8; 100] {
    let mut p = [0u8; 100];
    p[0] = 0x55;
    p[1..3].copy_from_slice(&index.to_be_bytes());
    p[3..99].copy_from_slice(data);
    p
}
```

**Step 4: Run tests** — Expected: PASS. **Step 5: Commit** — `"Add command packet builders"`

---

### Task 5: Notification parser

**Files:**
- Create: `crates/lxd2-core/src/protocol/notifications.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hello_reply_mac() {
        let n = [0x5A, 0x01, 0, 0, 0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33, 0, 0];
        assert_eq!(
            parse(&n),
            Some(Notification::Hello { mac: [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33] })
        );
    }

    #[test]
    fn parses_status() {
        // battery 80%, no_paper, charging, overheat=0, low_batt=0, density 3
        let n = [0x5A, 0x02, 80, 1, 1, 0, 0, 3, 0x0F, 0xA0];
        assert_eq!(
            parse(&n),
            Some(Notification::Status(Status {
                battery_pct: 80,
                no_paper: true,
                charging: true,
                charged: false,
                overheat: false,
                low_battery: false,
                density: Some(3),
                voltage_mv: Some(4000),
            }))
        );
    }

    #[test]
    fn parses_short_status_without_extended_fields() {
        let n = [0x5A, 0x02, 55, 0, 2];
        let parsed = parse(&n);
        match parsed {
            Some(Notification::Status(s)) => {
                assert_eq!(s.battery_pct, 55);
                assert!(!s.no_paper);
                assert!(s.charged);
                assert_eq!(s.density, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_flow_control() {
        assert_eq!(parse(&[0x5A, 0x05, 0x01, 0x40]), Some(Notification::LostPacket { index: 0x0140 }));
        assert_eq!(parse(&[0x5A, 0x06, 0x01, 0x40]), Some(Notification::Finished { num_packets: 0x0140 }));
        assert_eq!(parse(&[0x5A, 0x07]), Some(Notification::Cooldown));
        assert_eq!(parse(&[0x5A, 0x08]), Some(Notification::Hold));
    }

    #[test]
    fn parses_auth_results() {
        assert_eq!(parse(&[0x5A, 0x0B, 0x01]), Some(Notification::AuthResult { ok: true }));
        assert_eq!(parse(&[0x5A, 0x0B, 0x00]), Some(Notification::AuthResult { ok: false }));
        // 5A 0A reply payload is unused garbage; still recognized
        let n = [0x5A, 0x0A, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(parse(&n), Some(Notification::AuthChallengeReply));
    }

    #[test]
    fn unknown_or_short_returns_none() {
        assert_eq!(parse(&[0x5A]), None);
        assert_eq!(parse(&[0x42, 0x00]), None);
    }
}
```

**Step 2: Run** — Expected: FAIL.

**Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub battery_pct: u8,
    pub no_paper: bool,
    pub charging: bool,
    pub charged: bool,
    pub overheat: bool,
    pub low_battery: bool,
    pub density: Option<u8>,
    pub voltage_mv: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    Hello { mac: [u8; 6] },
    Status(Status),
    AuthChallengeReply,
    AuthResult { ok: bool },
    LostPacket { index: u16 },
    Finished { num_packets: u16 },
    Cooldown,
    Hold,
}

pub fn parse(data: &[u8]) -> Option<Notification> {
    if data.len() < 2 || data[0] != 0x5A {
        return None;
    }
    match data[1] {
        0x01 if data.len() >= 10 => {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&data[4..10]);
            Some(Notification::Hello { mac })
        }
        0x02 if data.len() >= 5 => Some(Notification::Status(Status {
            battery_pct: data[2],
            no_paper: data[3] != 0,
            charging: data[4] == 1,
            charged: data[4] == 2,
            overheat: data.get(5).is_some_and(|&b| b != 0),
            low_battery: data.get(6).is_some_and(|&b| b != 0),
            density: data.get(7).copied(),
            voltage_mv: data
                .get(8..10)
                .map(|v| u16::from_be_bytes([v[0], v[1]])),
        })),
        0x05 if data.len() >= 4 => Some(Notification::LostPacket {
            index: u16::from_be_bytes([data[2], data[3]]),
        }),
        0x06 if data.len() >= 4 => Some(Notification::Finished {
            num_packets: u16::from_be_bytes([data[2], data[3]]),
        }),
        0x07 => Some(Notification::Cooldown),
        0x08 => Some(Notification::Hold),
        0x0A => Some(Notification::AuthChallengeReply),
        0x0B if data.len() >= 3 => Some(Notification::AuthResult { ok: data[2] == 0x01 }),
        _ => None,
    }
}
```

**Step 4: Run tests** — PASS. **Step 5: Commit** — `"Add notification parser"`

---

### Task 6: Bitmap type and raster chunking

**Files:**
- Create: `crates/lxd2-core/src/raster/mod.rs` (`pub mod bitmap;`)
- Create: `crates/lxd2-core/src/raster/bitmap.rs`
- Modify: `crates/lxd2-core/src/lib.rs` (`pub mod raster;`)

`Bitmap` is 384 px wide, arbitrary height, 1 bit/px packed MSB-first
(bit 1 = black), 48 bytes/row. `to_raster_payloads()` yields 96-byte chunks
(2 rows each), zero-padding a trailing odd row.

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pixel_packs_msb_first() {
        let mut b = Bitmap::new(2);
        b.set(0, 0, true); // leftmost pixel -> bit 7 of byte 0
        b.set(383, 1, true); // rightmost pixel -> bit 0 of byte 47 of row 1
        assert_eq!(b.row(0)[0], 0b1000_0000);
        assert_eq!(b.row(1)[47], 0b0000_0001);
    }

    #[test]
    fn payloads_pack_two_rows_per_chunk() {
        let mut b = Bitmap::new(4);
        b.set(0, 2, true);
        let chunks = b.to_raster_payloads();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1][0], 0b1000_0000); // row 2 = first row of chunk 1
    }

    #[test]
    fn odd_height_pads_final_chunk_with_zeros() {
        let b = Bitmap::new(3);
        let chunks = b.to_raster_payloads();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1][48..].iter().all(|&x| x == 0));
    }
}
```

**Step 2: Run** — FAIL.

**Step 3: Implement**

```rust
pub const WIDTH: usize = 384;
pub const BYTES_PER_ROW: usize = WIDTH / 8; // 48

#[derive(Debug, Clone)]
pub struct Bitmap {
    rows: Vec<[u8; BYTES_PER_ROW]>,
}

impl Bitmap {
    pub fn new(height: usize) -> Self {
        Self { rows: vec![[0u8; BYTES_PER_ROW]; height] }
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, y: usize) -> &[u8; BYTES_PER_ROW] {
        &self.rows[y]
    }

    /// Set pixel; x < 384, bit 1 = black, MSB-first within each byte.
    pub fn set(&mut self, x: usize, y: usize, black: bool) {
        let byte = &mut self.rows[y][x / 8];
        let mask = 0x80 >> (x % 8);
        if black { *byte |= mask } else { *byte &= !mask }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.rows[y][x / 8] & (0x80 >> (x % 8)) != 0
    }

    /// 96-byte payloads for raster packets: two rows each, zero-padded.
    pub fn to_raster_payloads(&self) -> Vec<[u8; 2 * BYTES_PER_ROW]> {
        self.rows
            .chunks(2)
            .map(|pair| {
                let mut chunk = [0u8; 2 * BYTES_PER_ROW];
                chunk[..BYTES_PER_ROW].copy_from_slice(&pair[0]);
                if let Some(second) = pair.get(1) {
                    chunk[BYTES_PER_ROW..].copy_from_slice(second);
                }
                chunk
            })
            .collect()
    }
}
```

**Step 4: Run tests** — PASS. **Step 5: Commit** — `"Add 1-bit bitmap with raster chunking"`

---

### Task 7: Print job state machine (sans-IO)

**Files:**
- Create: `crates/lxd2-core/src/protocol/job.rs`

The FSM (modeled on rusq `fsm.go`). API:

```rust
pub enum Action {
    Send(Vec<u8>),        // write these bytes to 0xFFE1
    WaitMs(u64),          // sleep then call next_action() again
    WaitNotification,     // block on 0xFFE2 until on_notification() is fed
    Done,
}
```

States: `SendHello → AwaitHello → SendChallenge → AwaitChallengeReply →
SendAuthResponse → AwaitAuthResult → SendDensity → SendStart → Streaming(idx)
→ Holding → AwaitFinish → SendEnd → Done`. Constructor takes payloads, density,
a caller-supplied random challenge (keeps core deterministic/testable), and
`inter_packet_delay_ms` (default 15).

Behaviors under test:
- happy path emits hello → challenge → auth → density → start → N rasters → (finish) → end → done
- `LostPacket{i}` during streaming rewinds send index to `i.saturating_sub(1)`
- `Hold` pauses; following `LostPacket` resumes
- `Cooldown` inserts a `WaitMs(100)`
- `AuthResult{ok:false}` → error state (`Action::Send` never resumes; expose `error()`)
- `Finished` before all packets acked still transitions to SendEnd (printer decides)

**Step 1: Write failing tests** — drive the FSM with a script helper:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::notifications::Notification;

    const MAC: [u8; 6] = [1, 2, 3, 4, 5, 6];
    const CHALLENGE: [u8; 10] = [7u8; 10];

    fn hello_reply() -> Notification { Notification::Hello { mac: MAC } }

    fn drain_sends(job: &mut PrintJob) -> Vec<Vec<u8>> {
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

    fn two_packet_job() -> PrintJob {
        // 3-row bitmap -> 2 raster payloads
        let bitmap = crate::raster::bitmap::Bitmap::new(3);
        PrintJob::new(&bitmap, 3, CHALLENGE, 0)
    }

    #[test]
    fn happy_path_full_sequence() {
        let mut job = two_packet_job();

        // hello
        let sent = drain_sends(&mut job);
        assert_eq!(sent.len(), 1);
        assert_eq!(&sent[0][..2], &[0x5A, 0x01]);

        job.on_notification(hello_reply());
        let sent = drain_sends(&mut job); // challenge
        assert_eq!(&sent[0][..2], &[0x5A, 0x0A]);

        job.on_notification(Notification::AuthChallengeReply);
        let sent = drain_sends(&mut job); // auth response
        assert_eq!(&sent[0][..2], &[0x5A, 0x0B]);

        job.on_notification(Notification::AuthResult { ok: true });
        let sent = drain_sends(&mut job);
        // density, start, raster 0, raster 1 — then waiting for Finished
        assert_eq!(&sent[0][..2], &[0x5A, 0x0C]);
        assert_eq!(&sent[1][..4], &[0x5A, 0x04, 0x00, 0x02]);
        assert_eq!(&sent[2][..3], &[0x55, 0x00, 0x00]);
        assert_eq!(&sent[3][..3], &[0x55, 0x00, 0x01]);
        assert_eq!(sent.len(), 4);

        job.on_notification(Notification::Finished { num_packets: 2 });
        let sent = drain_sends(&mut job); // print end
        assert_eq!(&sent[0], &[0x5A, 0x04, 0x00, 0x02, 0x01, 0x00]);
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn lost_packet_rewinds_to_index_minus_one() {
        let mut job = two_packet_job();
        // fast-forward through handshake
        drain_sends(&mut job);
        job.on_notification(hello_reply());
        drain_sends(&mut job);
        job.on_notification(Notification::AuthChallengeReply);
        drain_sends(&mut job);
        job.on_notification(Notification::AuthResult { ok: true });
        drain_sends(&mut job); // all packets streamed

        job.on_notification(Notification::LostPacket { index: 1 });
        let sent = drain_sends(&mut job);
        // resent from index 0 (= 1 - 1): packets 0 and 1 again
        assert_eq!(&sent[0][..3], &[0x55, 0x00, 0x00]);
        assert_eq!(&sent[1][..3], &[0x55, 0x00, 0x01]);
    }

    #[test]
    fn hold_pauses_until_lost_packet_resumes() {
        let mut job = two_packet_job();
        drain_sends(&mut job);
        job.on_notification(hello_reply());
        drain_sends(&mut job);
        job.on_notification(Notification::AuthChallengeReply);
        drain_sends(&mut job);
        job.on_notification(Notification::AuthResult { ok: true });

        // stream first packet, then printer says hold
        let _ = job.next_action(); // density
        let _ = job.next_action(); // start
        let _ = job.next_action(); // raster 0
        job.on_notification(Notification::Hold);
        assert!(matches!(job.next_action(), Action::WaitNotification));

        job.on_notification(Notification::LostPacket { index: 1 });
        match job.next_action() {
            Action::Send(p) => assert_eq!(&p[..3], &[0x55, 0x00, 0x00]),
            other => panic!("expected resume send, got {other:?}"),
        }
    }

    #[test]
    fn auth_failure_is_fatal() {
        let mut job = two_packet_job();
        drain_sends(&mut job);
        job.on_notification(hello_reply());
        drain_sends(&mut job);
        job.on_notification(Notification::AuthChallengeReply);
        drain_sends(&mut job);
        job.on_notification(Notification::AuthResult { ok: false });
        assert!(job.error().is_some());
    }
}
```

**Step 2: Run** — FAIL.

**Step 3: Implement `PrintJob`.** Keep state as a private enum; store
`payloads: Vec<[u8; 96]>`, `density`, `challenge`, `mac: Option<[u8;6]>`,
`send_idx`, `delay_ms`, `error: Option<JobError>`. `next_action()` matches on
state and advances it; `on_notification()` transitions per the table in the
design doc. Insert `WaitMs(delay_ms)` between raster sends (tests use delay 0
and skip `WaitMs` in `drain_sends`). After the last raster packet, state
becomes `AwaitFinish` returning `WaitNotification`.

**Step 4: Run tests** — PASS.

**Step 5: Also run full crate tests** — `cargo test -p lxd2-core` all green.

**Step 6: Commit** — `"Add sans-IO print job state machine"`

---

### Task 8: Image → bitmap (scale + dither)

**Files:**
- Create: `crates/lxd2-core/src/raster/dither.rs`
- Modify: `crates/lxd2-core/Cargo.toml` — add `image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }`

API: `fn image_to_bitmap(img: &image::GrayImage, dither: Dither) -> Bitmap`
where `enum Dither { FloydSteinberg, Threshold }`. Scaling to width 384
(preserving aspect) happens in a helper `prepare(img: &DynamicImage) -> GrayImage`.

**Step 1: Failing tests** — feed tiny synthetic images:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    #[test]
    fn threshold_maps_dark_to_black() {
        let mut img = GrayImage::new(384, 2);
        for p in img.pixels_mut() { *p = Luma([10]); } // dark
        let b = image_to_bitmap(&img, Dither::Threshold);
        assert!(b.get(0, 0) && b.get(383, 1)); // black everywhere
    }

    #[test]
    fn threshold_maps_light_to_white() {
        let mut img = GrayImage::new(384, 2);
        for p in img.pixels_mut() { *p = Luma([250]); }
        let b = image_to_bitmap(&img, Dither::Threshold);
        assert!(!b.get(0, 0) && !b.get(383, 1));
    }

    #[test]
    fn floyd_steinberg_mid_gray_is_half_black() {
        let mut img = GrayImage::new(384, 100);
        for p in img.pixels_mut() { *p = Luma([128]); }
        let b = image_to_bitmap(&img, Dither::FloydSteinberg);
        let black: usize = (0..100)
            .flat_map(|y| (0..384).map(move |x| (x, y)))
            .filter(|&(x, y)| b.get(x, y))
            .count();
        let ratio = black as f64 / (384.0 * 100.0);
        assert!((0.4..0.6).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn prepare_scales_to_384_wide() {
        let img = image::DynamicImage::new_luma8(768, 200);
        let g = prepare(&img);
        assert_eq!(g.width(), 384);
        assert_eq!(g.height(), 100);
    }
}
```

**Step 2: Run** — FAIL. **Step 3: Implement** standard Floyd–Steinberg error
diffusion over an `f32` buffer, threshold at 128; `prepare` = grayscale +
`resize` with Lanczos3. **Step 4: Run** — PASS. **Step 5: Commit** —
`"Add image scaling and dithering to 1-bit bitmap"`

---

### Task 9: Text → bitmap

**Files:**
- Create: `crates/lxd2-core/src/raster/text.rs`
- Create: `crates/lxd2-core/assets/` — download a font, e.g. JetBrains Mono
  Regular TTF (check license file in). Embed with `include_bytes!`.
- Modify: `crates/lxd2-core/Cargo.toml` — add `fontdue = "0.9"`

API: `fn render_text(text: &str, size_px: f32) -> Bitmap` — greedy word-wrap at
384 px, left-aligned, line height = 1.3 × size, threshold coverage at 0.5.

**Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nonempty_bitmap_with_ink() {
        let b = render_text("Hello", 24.0);
        assert!(b.height() > 0);
        let ink = (0..b.height())
            .flat_map(|y| (0..384).map(move |x| (x, y)))
            .any(|(x, y)| b.get(x, y));
        assert!(ink, "expected some black pixels");
    }

    #[test]
    fn wraps_long_lines() {
        let short = render_text("hi", 24.0);
        let long = render_text(&"word ".repeat(40), 24.0);
        assert!(long.height() > short.height() * 3);
    }

    #[test]
    fn empty_text_gives_empty_bitmap() {
        assert_eq!(render_text("", 24.0).height(), 0);
    }
}
```

**Step 2: Run** — FAIL. **Step 3: Implement** with `fontdue::Font` +
per-glyph rasterize, tracking a pen position; wrap when pen.x + advance > 384.
Handle `\n` as hard breaks. **Step 4: Run** — PASS. **Step 5: Commit** —
`"Add text rendering with embedded font"`

---

### Task 10: Preview PNG output

**Files:**
- Create: `crates/lxd2-core/src/raster/preview.rs`

API: `fn bitmap_to_png(b: &Bitmap) -> Vec<u8>` — 1-bit → 8-bit gray PNG
(black = 0, white = 255), used by `--preview` and later the server.

**Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::bitmap::Bitmap;

    #[test]
    fn roundtrips_through_png() {
        let mut b = Bitmap::new(2);
        b.set(0, 0, true);
        let png = bitmap_to_png(&b);
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!(img.width(), 384);
        assert_eq!(img.height(), 2);
        assert_eq!(img.get_pixel(0, 0).0[0], 0);     // black
        assert_eq!(img.get_pixel(1, 0).0[0], 255);   // white
    }
}
```

**Step 2–4:** Run (FAIL) → implement with `image::GrayImage` + PNG encode into
`Vec<u8>` → run (PASS). **Step 5: Commit** — `"Add bitmap PNG preview export"`

---

### Task 11: BLE transport + `scan` command

**Files:**
- Create: `crates/lxd2/src/ble.rs`
- Create: `crates/lxd2/src/cli.rs`
- Modify: `crates/lxd2/src/main.rs`

No unit tests for BLE itself (hardware); keep this layer thin. Manual
verification steps instead.

**Step 1: CLI skeleton with clap derive**

```rust
// cli.rs
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lxd2", about = "Print to LX-D02/LX-D2 BLE thermal printers")]
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
        /// File to print (.png/.jpg/.txt)
        #[arg(short, long)]
        file: Option<std::path::PathBuf>,
        /// Density 1-7
        #[arg(long, default_value_t = 3)]
        density: u8,
        /// Blank feed lines after printing
        #[arg(long, default_value_t = 40)]
        feed: usize,
        /// Dithering for images
        #[arg(long, default_value = "floyd")]
        dither: String,
        /// Render to PNG instead of printing
        #[arg(long)]
        preview: Option<std::path::PathBuf>,
    },
}

#[derive(clap::Args)]
pub struct DeviceArgs {
    /// Device name or identifier (default: first device named LX*)
    #[arg(long)]
    pub device: Option<String>,
}
```

**Step 2: BLE layer (`ble.rs`)** — with btleplug:
- `scan(timeout) -> Vec<(name, id)>`: start scan, filter peripherals whose
  local name starts with `LX`.
- `connect(filter) -> Printer`: connect, discover services, locate `0xFFE6`
  service, `0xFFE1` write char, `0xFFE2` notify char, subscribe.
- `Printer::run_job(&mut PrintJob)`: the async pump —
  loop on `next_action()`: `Send` → `write_without_response`; `WaitMs` →
  `tokio::time::sleep`; `WaitNotification` → recv from notification stream with
  a 10 s timeout, `parse()` and feed `on_notification()`; `Done` → return.
  Any status notification carrying `no_paper: true` before streaming aborts
  with a clear error.
- `Printer::status()`: run only the hello/auth part... **No** — simpler: status
  notifications arrive unsolicited after subscribe; implement
  `Printer::wait_status(timeout)` that returns the first parsed `5A 02`.

**Step 3: Wire `scan` in `main.rs`, verify manually**

Run: `cargo run -p lxd2 -- scan`
Expected: prompts macOS Bluetooth permission on first run; prints the LX
device, e.g. `LX-D02 (uuid…)`. If permission denied, print a hint to enable
it in System Settings → Privacy & Security → Bluetooth.

**Step 4: Commit** — `"Add BLE transport and scan command"`

---

### Task 12: `status` and `print` commands end-to-end

**Files:**
- Modify: `crates/lxd2/src/main.rs`

**Step 1: Implement `status`** — connect, wait for `5A 02`, pretty-print:

```
Battery:  80% (charging)
Paper:    OK
Density:  3
Voltage:  4.00 V
```

**Step 2: Implement `print` pipeline** — build bitmap from input:
- `--file *.png/jpg` → `prepare` + `image_to_bitmap` (dither per `--dither`)
- text (arg, stdin, or `--file *.txt`) → `render_text(text, 24.0)`
- append `feed` blank rows
- `--preview out.png` → write `bitmap_to_png`, **skip BLE entirely**, exit 0
- else: random challenge via `rand`, `PrintJob::new(&bitmap, density, challenge, 15)`,
  connect, `run_job`.

**Step 3: Verify preview without hardware**

Run: `echo "Hello LX-D02" | cargo run -p lxd2 -- print --preview /tmp/out.png`
Expected: PNG exists, 384 px wide, shows the text. Also test an image file.

**Step 4: Hardware validation (user present, printer on)**

- `cargo run -p lxd2 -- scan` → device listed
- `cargo run -p lxd2 -- status` → sane battery/paper values
- `cargo run -p lxd2 -- print "hello from rust"` → paper comes out
- A tall image (forces many packets) → exercises flow control; if `5A 05`
  rewind or timing issues appear, tune `inter_packet_delay_ms` (try 7–20 ms).

**Step 5: Commit** — `"Add status and print commands"`

---

### Task 13: Wrap-up

- `cargo clippy --workspace` and `cargo fmt --check` — fix everything.
- Write `README.md`: what it is, supported hardware, install, usage examples,
  credit to the three reference repos, macOS Bluetooth permission note.
- Commit — `"Add README"`. Tag nothing yet; phase 2 (markdown/QR/config) is a
  separate plan.

---

## Notes for the implementer

- **Never trust rusq's hard-coded auth bytes** — they're a capture replay for
  his unit. The MAC-derived CRC (Task 3) is the correct approach.
- Inter-packet delay: references disagree (7 ms rusq, 20 ms ValdikSS). Start
  at 15 ms; hardware validation in Task 12 tunes it.
- btleplug on macOS uses UUIDs, not MAC addresses, as peripheral IDs — the MAC
  for auth **must** come from the `5A 01` hello reply, not from the transport.
- The `5A 0A` reply payload is garbage RAM per ValdikSS — never validate it.
- Keep `lxd2-core` free of `tokio`/`btleplug`/`rand`: randomness (auth
  challenge) is injected by the caller. This is what keeps it WASM-ready for
  phase 4.
