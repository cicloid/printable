//! Sans-IO X6 print job state machine.
//!
//! Far simpler than the LX-D02 flow: no hello, no auth, no completion
//! notification. Send 0xAF SetEnergy then 0xBE ApplyEnergy (the darkness
//! knob — see [`density_to_energy`]), stream one 0xA2 scanline frame per
//! bitmap row (plus one blank lead row — the printer prints artifacts if
//! the first row has ink), pause on BufferFull / resume on Ready, then
//! feed and settle.

use crate::protocol::job::{Action, JobStats};
use crate::protocol_x6::notifications::X6Notification;
use crate::protocol_x6::packets;
use crate::raster::bitmap::{Bitmap, BYTES_PER_ROW};

/// Wait after the final feed before declaring the job done, so the transport
/// does not tear the link down while the printer is still draining its
/// buffer. The printer sends no completion event, so this is a guess; tune
/// against hardware.
const SETTLE_MS: u64 = 500;

/// Map the user-facing density knob (1-7, the LX-D02's scale) to X6
/// printhead energy for the 0xAF SetEnergy command.
///
/// `energy = 12000 + 6000 × (density − 1)`, with `density` clamped to
/// 1-7. The endpoints and midpoint land exactly on kitty-printer's
/// "strength" presets: density 1 = 12000 (low), 3 = 24000 (medium, its
/// `DEF_ENERGY`), 7 = 48000 (high).
pub fn density_to_energy(density: u8) -> u16 {
    12000 + 6000 * (u16::from(density.clamp(1, 7)) - 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SendEnergy,
    SendApplyEnergy,
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
    energy: u16,
    feed_px: u16,
    inter_packet_delay_ms: u64,
    pending_wait_ms: Option<u64>,
    stats: JobStats,
}

impl X6PrintJob {
    /// A job that sets the printhead energy from `density` (1-7, clamped —
    /// see [`density_to_energy`]), prints `bitmap`, then feeds `feed_px`
    /// rows of blank paper.
    ///
    /// Unlike the LX-D02 job this cannot fail to construct: there is no
    /// packet-index limit, no auth challenge, and an out-of-range density
    /// clamps instead of erroring.
    pub fn new(bitmap: &Bitmap, density: u8, feed_px: u16, inter_packet_delay_ms: u64) -> Self {
        // Blank lead row: the printer misprints if row 0 carries ink.
        let mut rows = vec![[0u8; BYTES_PER_ROW]];
        rows.extend((0..bitmap.height()).map(|y| *bitmap.row(y)));
        Self {
            rows,
            state: State::SendEnergy,
            send_idx: 0,
            energy: density_to_energy(density),
            feed_px,
            inter_packet_delay_ms,
            pending_wait_ms: None,
            stats: JobStats::default(),
        }
    }

    /// Returns the next action the caller should perform.
    #[must_use]
    pub fn next_action(&mut self) -> Action {
        if let Some(ms) = self.pending_wait_ms.take() {
            return Action::WaitMs(ms);
        }
        match self.state {
            State::SendEnergy => {
                self.state = State::SendApplyEnergy;
                Action::Send(packets::set_energy(self.energy))
            }
            State::SendApplyEnergy => {
                self.state = State::Streaming;
                Action::Send(packets::apply_energy())
            }
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

    /// Counters for what the job has done so far. `packets_sent` counts
    /// scanlines only — not the energy setup frames or the feed.
    /// `retransmits` and `cooldowns` are always 0 — the X6 protocol has no
    /// such events.
    pub fn stats(&self) -> JobStats {
        self.stats
    }
}

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

    /// Endpoints and midpoint are kitty-printer's strength presets:
    /// low 12000, medium 24000 (its DEF_ENERGY), high 48000.
    #[test]
    fn density_to_energy_maps_and_clamps() {
        assert_eq!(density_to_energy(1), 12000);
        assert_eq!(density_to_energy(3), 24000);
        assert_eq!(density_to_energy(7), 48000);
        assert_eq!(density_to_energy(0), 12000); // clamped up
        assert_eq!(density_to_energy(9), 48000); // clamped down
    }

    #[test]
    fn happy_path_streams_energy_blank_lead_then_rows_then_feed() {
        let mut bitmap = Bitmap::new(2);
        bitmap.set(0, 0, true); // MSB-first 0x80 -> wire 0x01
        let mut job = X6PrintJob::new(&bitmap, 3, 64, 0);

        let sent = drain_sends(&mut job);
        // set energy, apply energy, blank artifact-guard line, 2 bitmap
        // rows, feed
        assert_eq!(sent.len(), 6);
        assert_eq!(&sent[0][..3], &[0x51, 0x78, 0xAF]);
        assert_eq!(&sent[0][6..8], &[0xC0, 0x5D]); // 24000 LE, density 3
        assert_eq!(&sent[1][..3], &[0x51, 0x78, 0xBE]);
        assert_eq!(&sent[2][..3], &[0x51, 0x78, 0xA2]);
        assert!(sent[2][6..54].iter().all(|&b| b == 0)); // lead row is blank
        assert_eq!(sent[3][6], 0x01); // bit-reversed pixel
        assert_eq!(&sent[5][..3], &[0x51, 0x78, 0xA1]); // trailing feed
        assert_eq!(&sent[5][6..8], &[64, 0]); // 64 px LE
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn buffer_full_pauses_until_ready() {
        let bitmap = Bitmap::new(4);
        let mut job = X6PrintJob::new(&bitmap, 3, 0, 0);
        let _ = job.next_action(); // set energy
        let _ = job.next_action(); // apply energy
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
        let mut job = X6PrintJob::new(&bitmap, 3, 0, 0);
        job.on_notification(X6Notification::Ready);
        assert_eq!(job.stats(), JobStats::default());
        // still runs from the start
        let _ = job.next_action(); // set energy
        let _ = job.next_action(); // apply energy
        match job.next_action() {
            Action::Send(p) => assert_eq!(p[2], 0xA2),
            other => panic!("expected scanline, got {other:?}"),
        }
    }

    #[test]
    fn inter_packet_delay_between_scanlines_only() {
        let bitmap = Bitmap::new(2);
        let mut job = X6PrintJob::new(&bitmap, 3, 64, 15);
        // no delay after the two energy setup frames
        assert!(matches!(job.next_action(), Action::Send(_))); // set energy
        assert!(matches!(job.next_action(), Action::Send(_))); // apply energy
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
        let mut job = X6PrintJob::new(&bitmap, 3, 64, 0);
        let _ = job.next_action(); // set energy
        let _ = job.next_action(); // apply energy
        let _ = job.next_action(); // lead row
        let _ = job.next_action(); // row 0
        let _ = job.next_action(); // feed
        assert!(matches!(job.next_action(), Action::WaitMs(SETTLE_MS)));
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn zero_feed_skips_feed_command() {
        let bitmap = Bitmap::new(1);
        let mut job = X6PrintJob::new(&bitmap, 3, 0, 0);
        let sent = drain_sends(&mut job);
        assert_eq!(sent.len(), 4); // energy, apply, lead + row — no 0xA1
        assert!(sent.iter().all(|p| p[2] != 0xA1));
        assert!(sent[2..].iter().all(|p| p[2] == 0xA2));
    }

    #[test]
    fn stats_count_scanlines_not_setup_or_feed() {
        let bitmap = Bitmap::new(3);
        let mut job = X6PrintJob::new(&bitmap, 3, 64, 0);
        drain_sends(&mut job);
        assert_eq!(job.stats().packets_sent, 4); // lead + 3 rows only
        assert_eq!(job.stats().retransmits, 0);
        assert_eq!(job.stats().cooldowns, 0);
    }
}
