//! WASM bridge for the sans-IO X6 print job state machine.
//!
//! [`WasmX6Job`] wraps [`printa_ble_core::protocol_x6::job::X6PrintJob`] for
//! the Web Bluetooth page, with the same drive contract as
//! [`crate::job::WasmJob`]: JS asks for the next action, performs it (GATT
//! write, sleep, or wait), and feeds raw 0xAE02 notification bytes back in.
//! The X6 flow is much simpler — no auth, no completion notification — so
//! the constructor takes only a bitmap, a density and a feed length. The
//! density maps to the X6's feed-speed and printhead-energy commands in
//! core.

use printa_ble_core::protocol_x6::job::X6PrintJob;
use printa_ble_core::protocol_x6::notifications;
use wasm_bindgen::prelude::*;

use crate::job::{ActionMsg, INTER_PACKET_DELAY_MS};
use crate::WasmBitmap;

/// One X6 print job driven from JS. Create per copy.
#[wasm_bindgen]
#[derive(Debug)]
pub struct WasmX6Job {
    inner: X6PrintJob,
}

#[wasm_bindgen]
impl WasmX6Job {
    /// Start a job that sets the feed speed and printhead energy from
    /// `density` (1-7, same knob as the LX-D02's), prints `bitmap`, then
    /// feeds `feed_px` rows of blank paper via the 0xA1 feed command
    /// (0 skips the feed).
    ///
    /// Unlike the LX-D02 job there is no auth challenge.
    #[wasm_bindgen(constructor)]
    pub fn new(bitmap: &WasmBitmap, density: u8, feed_px: u16) -> Result<WasmX6Job, String> {
        // Core clamps an out-of-range density; reject it here instead,
        // with the same message as WasmJob.
        if !(1..=7).contains(&density) {
            return Err(format!("density must be 1-7, got {density}"));
        }
        // An empty bitmap would send nothing but the blank lead row and the
        // feed; reject it up front, with the same message as WasmJob.
        if bitmap.height() == 0 {
            return Err("nothing to print: bitmap is empty".to_string());
        }
        Ok(WasmX6Job {
            inner: X6PrintJob::new(&bitmap.inner, density, feed_px, INTER_PACKET_DELAY_MS),
        })
    }

    /// Next step for the JS pump, as the same four tagged objects as
    /// [`crate::job::WasmJob::next_action`]:
    ///
    /// - `{kind: "send", bytes: Uint8Array}` — write `bytes` to 0xAE01
    /// - `{kind: "waitMs", ms: number}` — sleep `ms` milliseconds
    /// - `{kind: "waitNotification"}` — wait for a 0xAE02 notification,
    ///   feed it to `on_notification`, then call `next_action` again
    /// - `{kind: "done"}` — job finished
    pub fn next_action(&mut self) -> JsValue {
        // Serializing ActionMsg cannot fail: it is a closed enum of
        // primitives and bytes, with no maps or non-string keys.
        serde_wasm_bindgen::to_value(&self.next_action_inner()).unwrap()
    }

    /// Feed raw notification bytes from characteristic 0xAE02.
    /// Unparseable frames are ignored, mirroring the CLI's BLE layer.
    pub fn on_notification(&mut self, data: &[u8]) {
        if let Some(n) = notifications::parse(data) {
            self.inner.on_notification(n);
        }
    }

    /// Always `None`: the X6 job has no fatal-error path (no auth to fail).
    /// Present so the page's shared job teardown can call it on either
    /// wrapper.
    pub fn error(&self) -> Option<String> {
        None
    }
}

impl WasmX6Job {
    /// Native-testable core of [`WasmX6Job::next_action`].
    pub fn next_action_inner(&mut self) -> ActionMsg {
        self.inner.next_action().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::ActionMsg;
    use crate::WasmBitmap;
    use printa_ble_core::raster::Bitmap;

    /// The two 0xAE02 flow-control frames as captured from hardware
    /// (parzivail, verbatim hex) — the same bytes core's notification
    /// parser tests pin.
    const BUFFER_FULL: [u8; 9] = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x10, 0x70, 0xFF];
    const READY: [u8; 9] = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x00, 0x00, 0xFF];

    /// A 2-row bitmap job at density 3 with a 64 px trailing feed.
    fn two_row_job() -> WasmX6Job {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(2),
        };
        WasmX6Job::new(&bitmap, 3, 64).unwrap()
    }

    /// Pull sends until the job has streamed its speed/energy setup frames
    /// and the blank lead row, leaving it mid-scanline where flow control
    /// applies.
    fn past_setup_and_lead(job: &mut WasmX6Job) {
        for _ in 0..4 {
            assert!(matches!(job.next_action_inner(), ActionMsg::Send { .. }));
        }
    }

    /// Pull actions until the job blocks on a notification (or is done),
    /// returning everything emitted including the terminator.
    fn drain(job: &mut WasmX6Job) -> Vec<ActionMsg> {
        let mut actions = vec![];
        loop {
            let a = job.next_action_inner();
            let stop = matches!(a, ActionMsg::WaitNotification | ActionMsg::Done);
            actions.push(a);
            if stop {
                return actions;
            }
        }
    }

    fn sent_bytes(actions: &[ActionMsg]) -> Vec<&[u8]> {
        actions
            .iter()
            .filter_map(|a| match a {
                ActionMsg::Send { bytes } => Some(bytes.as_slice()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn happy_path_streams_scanlines_feed_settle_done() {
        let mut job = two_row_job();

        // No hello/auth: the job runs to done without ever needing a
        // notification.
        let actions = drain(&mut job);
        let sends = sent_bytes(&actions);

        // Speed and energy setup frames, blank artifact-guard lead row + 2
        // bitmap rows, then the feed.
        assert_eq!(sends.len(), 7);
        assert_eq!(&sends[0][..3], [0x51, 0x78, 0xBD]);
        assert_eq!(sends[0][6], 16); // density 3 = divisor 16
        assert_eq!(&sends[1][..3], [0x51, 0x78, 0xAF]);
        assert_eq!(&sends[1][6..8], [0xC0, 0x5D]); // density 3 = 24000 LE
        assert_eq!(&sends[2][..3], [0x51, 0x78, 0xBE]);
        for (i, send) in sends[3..6].iter().enumerate() {
            assert_eq!(&send[..3], [0x51, 0x78, 0xA2], "scanline {i}");
        }
        assert_eq!(&sends[6][..3], [0x51, 0x78, 0xA1]);

        // ... then a settle wait, then done.
        let n = actions.len();
        assert!(matches!(actions[n - 2], ActionMsg::WaitMs { .. }));
        assert!(matches!(actions[n - 1], ActionMsg::Done));
        assert_eq!(job.error(), None);
    }

    #[test]
    fn inter_packet_delay_matches_the_lx_wrapper() {
        let mut job = two_row_job();
        let actions = drain(&mut job);
        assert!(
            actions.contains(&ActionMsg::WaitMs { ms: 15 }),
            "expected a 15 ms inter-scanline delay in {actions:?}"
        );
    }

    #[test]
    fn empty_bitmap_errors() {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(0),
        };
        let err = WasmX6Job::new(&bitmap, 3, 64).unwrap_err();
        assert_eq!(err, "nothing to print: bitmap is empty");
    }

    /// Same message as WasmJob's density check.
    #[test]
    fn bad_density_errors() {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(2),
        };
        for density in [0, 8] {
            let err = WasmX6Job::new(&bitmap, density, 64).unwrap_err();
            assert_eq!(err, format!("density must be 1-7, got {density}"));
        }
    }

    #[test]
    fn buffer_full_pauses_and_ready_resumes() {
        let mut job = two_row_job();

        // Stream up to the lead row, then the printer reports BufferFull.
        past_setup_and_lead(&mut job);
        job.on_notification(&BUFFER_FULL);
        assert!(matches!(
            job.next_action_inner(),
            ActionMsg::WaitNotification
        ));
        assert!(matches!(
            job.next_action_inner(),
            ActionMsg::WaitNotification
        ));

        // Ready resumes streaming scanlines...
        job.on_notification(&READY);
        match job.next_action_inner() {
            ActionMsg::Send { bytes } => assert_eq!(&bytes[..3], [0x51, 0x78, 0xA2]),
            other => panic!("expected resumed scanline, got {other:?}"),
        }

        // ... and the job still runs to completion.
        let actions = drain(&mut job);
        assert!(matches!(actions.last(), Some(ActionMsg::Done)));
        assert_eq!(job.error(), None);
    }

    #[test]
    fn garbage_notifications_ignored() {
        let mut job = two_row_job();
        past_setup_and_lead(&mut job);
        job.on_notification(&BUFFER_FULL);

        // Garbage while paused: wrong magic, truncated frames, empty.
        job.on_notification(&[0x42, 0x00, 0x01]);
        job.on_notification(&[0x51]);
        job.on_notification(&[]);

        // Still cleanly paused, and the real Ready works.
        assert!(matches!(
            job.next_action_inner(),
            ActionMsg::WaitNotification
        ));
        job.on_notification(&READY);
        assert!(matches!(job.next_action_inner(), ActionMsg::Send { .. }));
    }
}
