//! WASM bridge for the sans-IO print job state machine.
//!
//! [`WasmJob`] wraps [`lxd2_core::protocol::job::PrintJob`] for the Web
//! Bluetooth page: JS asks for the next action, performs it (GATT write,
//! sleep, or wait), and feeds raw 0xFFE2 notification bytes back in. The
//! action enum and [`WasmJob::next_action_inner`] are plain Rust so the
//! whole contract is unit-testable natively.

use lxd2_core::protocol::job::{Action, PrintJob};
use lxd2_core::protocol::notifications;
use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::WasmBitmap;

/// Pause between raster packet sends; the value used by the CLI for real
/// hardware.
const INTER_PACKET_DELAY_MS: u64 = 15;

/// One step of the print job, serialized for JS as a tagged object:
/// `{kind:"send", bytes:Uint8Array} | {kind:"waitMs", ms} |
/// {kind:"waitNotification"} | {kind:"done"}`.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActionMsg {
    Send {
        // `serde_bytes` is load-bearing: serde-wasm-bindgen routes it through
        // `serialize_bytes`, which emits a JS `Uint8Array` (its default;
        // verified in serde-wasm-bindgen 0.6.5 `ser.rs`). A plain `Vec<u8>`
        // would go through `serialize_seq` and land as a JS `Array` of
        // numbers, which GATT `writeValue*` does not accept.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    WaitMs {
        ms: u64,
    },
    WaitNotification,
    Done,
}

impl From<Action> for ActionMsg {
    fn from(action: Action) -> Self {
        match action {
            Action::Send(bytes) => ActionMsg::Send { bytes },
            Action::WaitMs(ms) => ActionMsg::WaitMs { ms },
            Action::WaitNotification => ActionMsg::WaitNotification,
            Action::Done => ActionMsg::Done,
        }
    }
}

/// One print job driven from JS. Create per copy, with a fresh random
/// challenge each time.
#[wasm_bindgen]
#[derive(Debug)]
pub struct WasmJob {
    inner: PrintJob,
}

#[wasm_bindgen]
impl WasmJob {
    /// Start a job for `bitmap` at `density` (1-7).
    ///
    /// `challenge` must be exactly 10 bytes of caller-supplied randomness
    /// (`crypto.getRandomValues(new Uint8Array(10))`), used for the 5A 0A
    /// auth exchange.
    #[wasm_bindgen(constructor)]
    pub fn new(bitmap: &WasmBitmap, density: u8, challenge: &[u8]) -> Result<WasmJob, String> {
        if !(1..=7).contains(&density) {
            return Err(format!("density must be 1-7, got {density}"));
        }
        let challenge: [u8; 10] = challenge.try_into().map_err(|_| {
            format!(
                "challenge must be exactly 10 bytes, got {}",
                challenge.len()
            )
        })?;
        // An empty bitmap would run the whole hello/auth/start/finish dance
        // just to feed no paper; reject it up front instead.
        if bitmap.height() == 0 {
            return Err("nothing to print: bitmap is empty".to_string());
        }
        PrintJob::new(&bitmap.inner, density, challenge, INTER_PACKET_DELAY_MS)
            .map(|inner| WasmJob { inner })
            .map_err(|e| e.to_string())
    }

    /// Next step for the JS pump, as one of four tagged objects:
    ///
    /// - `{kind: "send", bytes: Uint8Array}` — write `bytes` to 0xFFE1
    /// - `{kind: "waitMs", ms: number}` — sleep `ms` milliseconds
    /// - `{kind: "waitNotification"}` — wait for a 0xFFE2 notification,
    ///   feed it to `on_notification`, then call `next_action` again
    /// - `{kind: "done"}` — job finished; check `error()`
    pub fn next_action(&mut self) -> JsValue {
        // Serializing ActionMsg cannot fail: it is a closed enum of
        // primitives and bytes, with no maps or non-string keys.
        serde_wasm_bindgen::to_value(&self.next_action_inner()).unwrap()
    }

    /// Feed raw notification bytes from characteristic 0xFFE2.
    /// Unparseable frames are ignored, mirroring the CLI's BLE layer.
    pub fn on_notification(&mut self, data: &[u8]) {
        if let Some(n) = notifications::parse(data) {
            self.inner.on_notification(n);
        }
    }

    /// The fatal error message, if the job failed. Check after `done`.
    pub fn error(&self) -> Option<String> {
        self.inner.error().map(|e| e.to_string())
    }
}

impl WasmJob {
    /// Native-testable core of [`WasmJob::next_action`].
    pub fn next_action_inner(&mut self) -> ActionMsg {
        self.inner.next_action().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxd2_core::protocol::auth::auth_response;
    use lxd2_core::protocol::packets;
    use lxd2_core::raster::Bitmap;

    const CHALLENGE: [u8; 10] = [9u8; 10];
    const MAC: [u8; 6] = [1, 2, 3, 4, 5, 6];
    /// Hello reply carrying `MAC` at bytes 4..10.
    const HELLO_REPLY: [u8; 12] = [0x5A, 0x01, 0, 0, 1, 2, 3, 4, 5, 6, 0, 0];
    const CHALLENGE_REPLY: [u8; 2] = [0x5A, 0x0A];
    const AUTH_OK: [u8; 3] = [0x5A, 0x0B, 0x01];
    const AUTH_FAIL: [u8; 3] = [0x5A, 0x0B, 0x00];
    /// Finished, num_packets = 2.
    const FINISHED: [u8; 4] = [0x5A, 0x06, 0x00, 0x02];

    /// A 3-row bitmap job (=> 2 raster packets of 2 rows each).
    fn three_row_job() -> WasmJob {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(3),
        };
        WasmJob::new(&bitmap, 3, &CHALLENGE).unwrap()
    }

    /// Pull actions until the job blocks on a notification (or is done),
    /// returning everything emitted including the terminator.
    fn drain(job: &mut WasmJob) -> Vec<ActionMsg> {
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
    fn happy_path_kinds() {
        let mut job = three_row_job();

        // Hello, then block for the reply.
        let actions = drain(&mut job);
        let sends = sent_bytes(&actions);
        assert_eq!(sends.len(), 1);
        assert_eq!(&sends[0][..2], [0x5A, 0x01]);
        assert!(matches!(actions.last(), Some(ActionMsg::WaitNotification)));

        job.on_notification(&HELLO_REPLY);
        let actions = drain(&mut job); // auth challenge
        assert_eq!(&sent_bytes(&actions)[0][..2], [0x5A, 0x0A]);

        job.on_notification(&CHALLENGE_REPLY);
        let actions = drain(&mut job); // auth response
        let sends = sent_bytes(&actions);
        assert_eq!(&sends[0][..2], [0x5A, 0x0B]);
        // Full auth response must match the core's computation for the
        // challenge and the MAC learned from the hello reply.
        let expected = packets::auth_reply(&auth_response(&CHALLENGE, &MAC));
        assert_eq!(sends[0], expected);

        job.on_notification(&AUTH_OK);
        let actions = drain(&mut job);
        let sends = sent_bytes(&actions);
        // density, start, two raster packets — then wait for Finished
        assert_eq!(sends.len(), 4);
        assert_eq!(&sends[0][..2], [0x5A, 0x0C]);
        assert_eq!(&sends[1][..4], [0x5A, 0x04, 0x00, 0x02]);
        assert_eq!(sends[2][0], 0x55);
        assert_eq!(sends[3][0], 0x55);
        assert!(matches!(actions.last(), Some(ActionMsg::WaitNotification)));

        job.on_notification(&FINISHED);
        let actions = drain(&mut job); // print end, then done
        assert_eq!(
            sent_bytes(&actions)[0],
            [0x5A, 0x04, 0x00, 0x02, 0x01, 0x00]
        );
        assert!(matches!(actions.last(), Some(ActionMsg::Done)));
        assert_eq!(job.error(), None);
    }

    #[test]
    fn wait_ms_between_rasters() {
        let mut job = three_row_job();
        drain(&mut job);
        job.on_notification(&HELLO_REPLY);
        drain(&mut job);
        job.on_notification(&CHALLENGE_REPLY);
        drain(&mut job);
        job.on_notification(&AUTH_OK);

        let actions = drain(&mut job);
        // raster 0, WaitMs(15), raster 1
        let raster_start = actions
            .iter()
            .position(|a| matches!(a, ActionMsg::Send { bytes } if bytes[0] == 0x55))
            .unwrap();
        assert_eq!(actions[raster_start + 1], ActionMsg::WaitMs { ms: 15 });
        assert!(
            matches!(&actions[raster_start + 2], ActionMsg::Send { bytes } if bytes[0] == 0x55)
        );
    }

    #[test]
    fn bad_challenge_len_errors() {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(3),
        };
        for len in [9, 11] {
            let err = WasmJob::new(&bitmap, 3, &vec![0u8; len]).unwrap_err();
            assert!(err.contains("challenge"), "unexpected message: {err}");
        }
    }

    #[test]
    fn bad_density_errors() {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(3),
        };
        for density in [0, 8] {
            let err = WasmJob::new(&bitmap, density, &CHALLENGE).unwrap_err();
            assert!(err.contains("density"), "unexpected message: {err}");
        }
    }

    #[test]
    fn empty_bitmap_errors() {
        let bitmap = WasmBitmap {
            inner: Bitmap::new(0),
        };
        let err = WasmJob::new(&bitmap, 3, &CHALLENGE).unwrap_err();
        assert!(
            err.contains("nothing to print"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn auth_fail_sets_error() {
        let mut job = three_row_job();
        drain(&mut job);
        job.on_notification(&HELLO_REPLY);
        drain(&mut job);
        job.on_notification(&CHALLENGE_REPLY);
        drain(&mut job);
        job.on_notification(&AUTH_FAIL);

        assert!(matches!(job.next_action_inner(), ActionMsg::Done));
        let err = job.error().unwrap();
        assert!(err.contains("auth"), "unexpected message: {err}");
    }

    #[test]
    fn unparseable_notification_ignored() {
        let mut job = three_row_job();
        drain(&mut job);

        // Garbage mid-stream: wrong magic, truncated frames, empty.
        job.on_notification(&[0x42, 0x00, 0x01]);
        job.on_notification(&[0x5A]);
        job.on_notification(&[]);

        // Still cleanly waiting for the hello reply, and the real one works.
        assert!(matches!(
            job.next_action_inner(),
            ActionMsg::WaitNotification
        ));
        job.on_notification(&HELLO_REPLY);
        let actions = drain(&mut job);
        assert_eq!(&sent_bytes(&actions)[0][..2], [0x5A, 0x0A]);
        assert_eq!(job.error(), None);
    }
}
