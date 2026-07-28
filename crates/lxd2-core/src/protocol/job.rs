//! Sans-IO print job state machine.
//!
//! Drives the full LX-D02 print flow (hello, auth, density, raster streaming,
//! flow control, finish) without doing any I/O. The caller asks for the next
//! [`Action`], performs it, and feeds incoming notifications back in via
//! [`PrintJob::on_notification`].

use crate::protocol::auth::auth_response;
use crate::protocol::notifications::Notification;
use crate::protocol::packets::{self, RASTER_DATA_LEN};
use crate::raster::bitmap::Bitmap;

/// How long to back off when the printer reports a thermal cooldown.
const COOLDOWN_MS: u64 = 100;

/// What the caller should do next.
#[derive(Debug)]
pub enum Action {
    /// Write these bytes to characteristic 0xFFE1.
    Send(Vec<u8>),
    /// Sleep for this many milliseconds, then call `next_action()` again.
    WaitMs(u64),
    /// Block on 0xFFE2 until a notification is fed via `on_notification()`.
    WaitNotification,
    /// The job is over. Check [`PrintJob::error`] for whether it failed.
    Done,
}

/// Fatal print job errors.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The printer rejected our `5A 0B` auth response.
    #[error("printer rejected authentication")]
    AuthFailed,
    /// The bitmap needs more raster packets than the protocol's 16-bit
    /// packet index can address.
    #[error("print too large: {packets} raster packets exceeds the maximum of {max}", max = u16::MAX)]
    TooLarge { packets: usize },
}

/// Internal state of the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SendHello,
    AwaitHello,
    SendChallenge,
    AwaitChallengeReply,
    SendAuthResponse,
    AwaitAuthResult,
    SendDensity,
    SendStart,
    /// Streaming raster packets; `send_idx` tracks the next one to send.
    Streaming,
    /// Printer asked us to pause; resumes on `LostPacket`, or ends when a
    /// `Finished` arrives.
    Holding,
    /// All packets sent; waiting for `Finished`.
    AwaitFinish,
    SendEnd,
    Done,
}

/// Sans-IO driver for one print job.
#[derive(Debug)]
pub struct PrintJob {
    state: State,
    payloads: Vec<[u8; RASTER_DATA_LEN]>,
    density: u8,
    challenge: [u8; 10],
    /// Printer MAC, learned from the hello reply.
    mac: [u8; 6],
    /// Index of the next raster packet to send while `Streaming`.
    send_idx: u16,
    inter_packet_delay_ms: u64,
    /// One-shot wait to emit before the next action (inter-packet delay or
    /// cooldown back-off).
    pending_wait_ms: Option<u64>,
    error: Option<JobError>,
}

impl PrintJob {
    /// Create a job that prints `bitmap` once the caller drives it.
    ///
    /// * `density` — print darkness, valid range 1-7.
    /// * `challenge` — caller-supplied randomness for the `5A 0A` auth
    ///   challenge. Injecting it keeps this crate free of RNG dependencies
    ///   and makes the state machine fully deterministic in tests.
    /// * `inter_packet_delay_ms` — pause between raster packet sends;
    ///   15 ms is the recommended value for real hardware, 0 disables the
    ///   delay entirely.
    ///
    /// Errors with [`JobError::TooLarge`] if the bitmap needs more raster
    /// packets than the protocol's 16-bit packet index can address
    /// (i.e. more than 131,070 rows).
    pub fn new(
        bitmap: &Bitmap,
        density: u8,
        challenge: [u8; 10],
        inter_packet_delay_ms: u64,
    ) -> Result<Self, JobError> {
        let payloads = bitmap.to_raster_payloads();
        if payloads.len() > u16::MAX as usize {
            return Err(JobError::TooLarge {
                packets: payloads.len(),
            });
        }
        Ok(Self {
            state: State::SendHello,
            payloads,
            density,
            challenge,
            mac: [0u8; 6],
            send_idx: 0,
            inter_packet_delay_ms,
            pending_wait_ms: None,
            error: None,
        })
    }

    fn num_packets(&self) -> u16 {
        self.payloads.len() as u16
    }

    /// Returns the next action the caller should perform.
    ///
    /// Once a fatal error has been recorded this returns [`Action::Done`];
    /// callers must check [`PrintJob::error`] after the loop exits to tell
    /// success from failure.
    #[must_use]
    pub fn next_action(&mut self) -> Action {
        if self.error.is_some() {
            return Action::Done;
        }
        if let Some(ms) = self.pending_wait_ms.take() {
            return Action::WaitMs(ms);
        }
        match self.state {
            State::SendHello => {
                self.state = State::AwaitHello;
                Action::Send(packets::hello().to_vec())
            }
            State::SendChallenge => {
                self.state = State::AwaitChallengeReply;
                Action::Send(packets::auth_challenge(&self.challenge).to_vec())
            }
            State::SendAuthResponse => {
                self.state = State::AwaitAuthResult;
                let resp = auth_response(&self.challenge, &self.mac);
                Action::Send(packets::auth_reply(&resp).to_vec())
            }
            State::SendDensity => {
                self.state = State::SendStart;
                Action::Send(packets::set_density(self.density).to_vec())
            }
            State::SendStart => {
                self.state = State::Streaming;
                Action::Send(packets::print_start(self.num_packets()).to_vec())
            }
            State::Streaming => {
                let idx = self.send_idx;
                match self.payloads.get(idx as usize) {
                    Some(data) => {
                        self.send_idx += 1;
                        // Inter-packet delay applies between raster sends; a
                        // zero delay is skipped entirely.
                        if self.inter_packet_delay_ms > 0
                            && (self.send_idx as usize) < self.payloads.len()
                        {
                            self.pending_wait_ms = Some(self.inter_packet_delay_ms);
                        }
                        Action::Send(packets::raster(idx, data).to_vec())
                    }
                    None => {
                        self.state = State::AwaitFinish;
                        Action::WaitNotification
                    }
                }
            }
            State::SendEnd => {
                self.state = State::Done;
                Action::Send(packets::print_end(self.num_packets()).to_vec())
            }
            State::AwaitHello
            | State::AwaitChallengeReply
            | State::AwaitAuthResult
            | State::Holding
            | State::AwaitFinish => Action::WaitNotification,
            State::Done => Action::Done,
        }
    }

    /// Feed a parsed notification from 0xFFE2 into the state machine.
    ///
    /// Notifications that make no sense in the current state are ignored.
    pub fn on_notification(&mut self, n: Notification) {
        match (self.state, n) {
            (State::AwaitHello, Notification::Hello { mac }) => {
                self.mac = mac;
                self.state = State::SendChallenge;
            }
            (State::AwaitChallengeReply, Notification::AuthChallengeReply) => {
                self.state = State::SendAuthResponse;
            }
            (State::AwaitAuthResult, Notification::AuthResult { ok }) => {
                if ok {
                    self.state = State::SendDensity;
                } else {
                    self.error = Some(JobError::AuthFailed);
                    self.state = State::Done;
                }
            }
            // Flow control, valid both mid-stream and after the last packet.
            (
                State::Streaming | State::Holding | State::AwaitFinish,
                Notification::LostPacket { index },
            ) => {
                // Resend from one packet before the reported index — the
                // convention observed in the official app (per rusq fsm.go).
                self.send_idx = index.saturating_sub(1);
                self.pending_wait_ms = None;
                self.state = State::Streaming;
            }
            (State::Streaming | State::AwaitFinish, Notification::Hold) => {
                self.state = State::Holding;
            }
            (State::Streaming | State::AwaitFinish, Notification::Cooldown) => {
                self.pending_wait_ms = Some(COOLDOWN_MS);
            }
            // The printer decides when the job is complete, even if we think
            // we are still streaming.
            (
                State::Streaming | State::Holding | State::AwaitFinish,
                Notification::Finished { .. },
            ) => {
                self.pending_wait_ms = None;
                self.state = State::SendEnd;
            }
            // Everything else (e.g. periodic Status frames) is ignored; paper
            // and battery checks live in the BLE layer, not this FSM.
            _ => {}
        }
    }

    /// The fatal error, if the job failed.
    pub fn error(&self) -> Option<&JobError> {
        self.error.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::notifications::Notification;

    const MAC: [u8; 6] = [1, 2, 3, 4, 5, 6];
    const CHALLENGE: [u8; 10] = [7u8; 10];

    fn hello_reply() -> Notification {
        Notification::Hello { mac: MAC }
    }

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
        PrintJob::new(&bitmap, 3, CHALLENGE, 0).unwrap()
    }

    /// Fast-forward through hello + auth exchange, stopping right before the
    /// auth result so tests can feed a pass or a failure.
    fn complete_handshake(job: &mut PrintJob) {
        drain_sends(job);
        job.on_notification(hello_reply());
        drain_sends(job);
        job.on_notification(Notification::AuthChallengeReply);
        drain_sends(job);
    }

    /// A two-packet job that has passed the handshake and is ready to stream.
    fn authed_job() -> PrintJob {
        let mut job = two_packet_job();
        complete_handshake(&mut job);
        job.on_notification(Notification::AuthResult { ok: true });
        job
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
        let mut job = authed_job();
        drain_sends(&mut job); // all packets streamed

        job.on_notification(Notification::LostPacket { index: 1 });
        let sent = drain_sends(&mut job);
        // resent from index 0 (= 1 - 1): packets 0 and 1 again
        assert_eq!(&sent[0][..3], &[0x55, 0x00, 0x00]);
        assert_eq!(&sent[1][..3], &[0x55, 0x00, 0x01]);
    }

    #[test]
    fn hold_pauses_until_lost_packet_resumes() {
        let mut job = authed_job();

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
    fn oversized_bitmap_errors() {
        // 131,073 rows -> 65,537 packets, one more than a u16 index allows.
        let bitmap = crate::raster::bitmap::Bitmap::new(131_073);
        assert!(matches!(
            PrintJob::new(&bitmap, 3, CHALLENGE, 0),
            Err(JobError::TooLarge { packets: 65_537 })
        ));
    }

    #[test]
    fn auth_failure_is_fatal() {
        let mut job = two_packet_job();
        complete_handshake(&mut job);
        job.on_notification(Notification::AuthResult { ok: false });
        assert!(job.error().is_some());
    }

    #[test]
    fn cooldown_waits_100ms_then_resumes() {
        let mut job = authed_job();

        let _ = job.next_action(); // density
        let _ = job.next_action(); // start
        let _ = job.next_action(); // raster 0
        job.on_notification(Notification::Cooldown);
        assert!(matches!(job.next_action(), Action::WaitMs(COOLDOWN_MS)));
        match job.next_action() {
            Action::Send(p) => assert_eq!(&p[..3], &[0x55, 0x00, 0x01]),
            other => panic!("expected resumed send, got {other:?}"),
        }
    }

    #[test]
    fn lost_packet_index_zero_rewinds_to_zero() {
        let mut job = authed_job();
        drain_sends(&mut job); // all packets streamed

        job.on_notification(Notification::LostPacket { index: 0 });
        let sent = drain_sends(&mut job);
        assert_eq!(&sent[0][..3], &[0x55, 0x00, 0x00]);
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn finished_while_streaming_moves_to_print_end() {
        let mut job = authed_job();

        let _ = job.next_action(); // density
        let _ = job.next_action(); // start
        let _ = job.next_action(); // raster 0

        // printer claims completion before we sent everything
        job.on_notification(Notification::Finished { num_packets: 2 });
        let sent = drain_sends(&mut job);
        assert_eq!(&sent[0], &[0x5A, 0x04, 0x00, 0x02, 0x01, 0x00]);
        assert!(matches!(job.next_action(), Action::Done));
    }

    #[test]
    fn inter_packet_delay_emits_wait_between_rasters() {
        let bitmap = crate::raster::bitmap::Bitmap::new(3);
        let mut job = PrintJob::new(&bitmap, 3, CHALLENGE, 15).unwrap();
        complete_handshake(&mut job);
        job.on_notification(Notification::AuthResult { ok: true });

        let _ = job.next_action(); // density
        let _ = job.next_action(); // start
        match job.next_action() {
            Action::Send(p) => assert_eq!(&p[..3], &[0x55, 0x00, 0x00]),
            other => panic!("expected raster 0, got {other:?}"),
        }
        assert!(matches!(job.next_action(), Action::WaitMs(15)));
        match job.next_action() {
            Action::Send(p) => assert_eq!(&p[..3], &[0x55, 0x00, 0x01]),
            other => panic!("expected raster 1, got {other:?}"),
        }
    }
}
