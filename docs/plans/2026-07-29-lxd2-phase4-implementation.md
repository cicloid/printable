# lxd2 Phase 4 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** A static web page that prints to the LX-D02 directly from Chrome/Edge via Web Bluetooth — no server, no install — reusing `lxd2-core` compiled to WASM.

**Architecture:** New `crates/lxd2-web` cdylib wraps `lxd2-core` with `wasm-bindgen`: rendering entry points (text/markdown/QR/image → preview PNG + raster payloads) and a `WasmJob` bridging the sans-IO print FSM to JS. A static page (`web/`) adapted from the phase-3 UI owns Web Bluetooth: request device (name prefix `LX`, service `0xFFE6`), write `0xFFE1`, subscribe `0xFFE2`, and pump `WasmJob` actions. Verified: `lxd2-core` already compiles to `wasm32-unknown-unknown` unchanged.

**Tech Stack:** `wasm-bindgen` 0.2, `wasm-pack` (`--target web`, needs `cargo install wasm-pack` or brew), vanilla JS/HTML (no bundler), `serde-wasm-bindgen` for action objects. Browser support: Chrome/Edge desktop + Android (Web Bluetooth; not Safari/iOS — documented).

**State:** Phases 1-3 complete at `a9cd1d6`. 92 tests + 1 ignored. Design doc: `docs/plans/2026-07-27-lxd2-design.md`. wasm32 target installed; wasm-pack NOT installed.

---

### Task 1: lxd2-web crate — rendering API

**Files:**

- Create: `crates/lxd2-web/Cargo.toml`, `crates/lxd2-web/src/lib.rs`
- Modify: root `Cargo.toml` workspace members

Cargo.toml:

```toml
[package]
name = "lxd2-web"
version = "0.1.0"
edition.workspace = true
license.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
lxd2-core = { path = "../lxd2-core" }
wasm-bindgen = "0.2"
serde = { version = "1", features = ["derive"] }
serde-wasm-bindgen = "0.6"
```

API (all `#[wasm_bindgen]`):

```rust
#[wasm_bindgen]
pub struct WasmBitmap { inner: Bitmap }   // opaque to JS

#[wasm_bindgen]
impl WasmBitmap {
    pub fn height(&self) -> usize;
    pub fn to_png(&self) -> Vec<u8>;                 // preview <img src=blob>
    pub fn extend_blank(&mut self, rows: usize);      // feed
}

#[wasm_bindgen] pub fn render_text(text: &str, size: f32) -> WasmBitmap;
#[wasm_bindgen] pub fn render_markdown(md: &str) -> WasmBitmap;
#[wasm_bindgen] pub fn render_qr(data: &str, caption: Option<String>) -> Result<WasmBitmap, JsError>;
#[wasm_bindgen] pub fn render_image(bytes: &[u8], dither: &str) -> Result<WasmBitmap, JsError>;
// dither: "floyd" | "atkinson" | "threshold" — image decoded by the image crate inside WASM
```

Errors → `JsError` (thrown as JS exceptions with the message).

**Tests:** the wrappers are thin; test natively (rlib): `cargo test -p lxd2-web` with plain `#[test]`s (constructors/PNG magic/height >0/bad dither errors). Keep `wasm-bindgen` attr-compatible with native tests (it compiles to no-ops off-wasm — but `JsError` doesn't exist natively; use `Result<WasmBitmap, JsError>` only under `#[cfg(target_arch = "wasm32")]`?? — NO: simpler, make fallible fns return `Result<WasmBitmap, String>`, which wasm-bindgen converts to a JS throw too; String works natively). Verify signatures compile BOTH native and wasm32: `cargo build -p lxd2-web --target wasm32-unknown-unknown` + `cargo test -p lxd2-web`.

Install wasm-pack (`brew install wasm-pack` else `cargo install wasm-pack`), then `wasm-pack build crates/lxd2-web --target web --out-dir ../../web/pkg` succeeds. Add `web/pkg/` to `.gitignore` (build artifact; a build script regenerates it).

Commit: `"Add lxd2-web WASM crate with rendering API"`.

### Task 2: WasmJob — the print FSM bridge

**Files:**

- Modify: `crates/lxd2-web/src/lib.rs` (or new `job.rs` module)

```rust
#[wasm_bindgen]
pub struct WasmJob { inner: PrintJob }

#[wasm_bindgen]
impl WasmJob {
    /// challenge: exactly 10 bytes from crypto.getRandomValues
    #[wasm_bindgen(constructor)]
    pub fn new(bitmap: &WasmBitmap, density: u8, challenge: &[u8]) -> Result<WasmJob, String>;
    /// {kind:"send", bytes:Uint8Array} | {kind:"waitMs", ms} | {kind:"waitNotification"} | {kind:"done"}
    pub fn next_action(&mut self) -> JsValue;   // serde-wasm-bindgen of a tagged enum
    /// Feed raw 0xFFE2 notification bytes; unparseable frames are ignored.
    pub fn on_notification(&mut self, data: &[u8]);
    pub fn error(&self) -> Option<String>;
}
```

- density validated 1-7 → Err(String); challenge length must be 10 → Err
- Inter-packet delay: hardcode 15 ms (same as CLI) inside `PrintJob::new` call
- `on_notification` parses via `notifications::parse` then feeds the FSM (mirrors ble.rs)
- Serialize actions with `#[derive(Serialize)] #[serde(tag = "kind", rename_all = "camelCase")]` enum → `serde_wasm_bindgen::to_value`; bytes as `serde_bytes`/Vec<u8> → ensure it lands as Uint8Array (serde-wasm-bindgen serializes Vec<u8> to Uint8Array with `serde_bytes::ByteBuf` — verify; if it comes out as Array, use ByteBuf)

**Tests (native, mirror the FSM contract):** happy path — drive a 3-row bitmap job feeding synthetic notification BYTES (e.g. hello reply `[0x5A,0x01,0,0,1,2,3,4,5,6,0,0]`, `[0x5A,0x0A,...]`, `[0x5A,0x0B,0x01]`, finished `[0x5A,0x06,0,2]`) and assert the action sequence kinds; bad challenge length errors; bad density errors; auth-fail sets error(). (Actions natively: can't use JsValue — factor the tagged enum + a `next_action_inner() -> ActionMsg` that native tests call; the wasm method wraps it.)

`cargo test -p lxd2-web` green, wasm32 build green, full workspace suite still green (expect ~99), clippy (native target) + fmt.

Commit: `"Add WASM print job bridge"`.

### Task 3: The web page

**Files:**

- Create: `web/index.html` (single file, adapted from `crates/lxd2/src/server/ui.html` — same look/tabs/options, NO URL tab)
- Create: `web/app.js` (module script: imports `./pkg/lxd2_web.js`)
- Create: `scripts/build-web.sh` (`#!/bin/sh -e`: wasm-pack build + echo serve instructions)

Page structure (reuse ui.html's CSS wholesale):

- Header: "lxd2 web" + Connect button + status chip (disconnected / connected LX-D02 / battery from first 5A 02 notification)
- Tabs: Text | Markdown | Image | QR (client-side rendering via WASM — no server involved)
- Options: density 1-7, feed, copies 1-20
- Preview button → WASM render → `to_png()` → blob URL → <img> (pure client-side, works without a printer)
- Print button → connect flow if not connected → job pump
- Unsupported-browser banner when `!navigator.bluetooth` (Safari/iOS/Firefox) — page still previews, print disabled

`app.js` core:

```js
import init, {
  render_text,
  render_markdown,
  render_qr,
  render_image,
  WasmJob,
} from "./pkg/lxd2_web.js";

const SERVICE = 0xffe6,
  WRITE = 0xffe1,
  NOTIFY = 0xffe2;

async function connect() {
  const device = await navigator.bluetooth.requestDevice({
    filters: [{ namePrefix: "LX" }],
    optionalServices: [SERVICE],
  });
  const server = await device.gatt.connect();
  const svc = await server.getPrimaryService(SERVICE);
  writeChar = await svc.getCharacteristic(WRITE);
  notifyChar = await svc.getCharacteristic(NOTIFY);
  await notifyChar.startNotifications();
  notifyChar.addEventListener("characteristicvaluechanged", onNotify);
  device.addEventListener("gattserverdisconnected", onDisconnect);
}

function onNotify(e) {
  const bytes = new Uint8Array(e.target.value.buffer);
  latestStatusMaybe(bytes); // update battery chip on 5A 02
  if (job) {
    job.on_notification(bytes);
    pump();
  } // wake pump if waiting
}

async function pump() {
  while (job) {
    const a = job.next_action();
    if (a.kind === "send") await writeChar.writeValueWithoutResponse(a.bytes);
    else if (a.kind === "waitMs") await sleep(a.ms);
    else if (a.kind === "waitNotification")
      return; // onNotify re-enters pump
    else {
      finishJob();
      return;
    } // done: check error()
  }
}
```

Careful points (document in code): re-entrancy — guard `pump()` with an `isPumping` flag so onNotify during an in-flight write doesn't double-pump; copies loop = sequential jobs (fresh `crypto.getRandomValues(new Uint8Array(10))` challenge each); 10 s watchdog on waitNotification → error toast; `writeValueWithoutResponse` fallback to `writeValue` if unavailable.

**Verification (no printer):** `scripts/build-web.sh`, then `python3 -m http.server 8080 -d web` + Playwright (python3, installed): page loads, WASM initializes, each tab's Preview renders a PNG <img> (assert natural width 384), unsupported-browser banner logic (Playwright's chromium HAS navigator.bluetooth? headless usually yes-but-unavailable — assert banner state matches `!!navigator.bluetooth`, whatever it is, and report), no console errors. Screenshot light+dark to scratchpad. Kill server. Real print: user validation (needs the physical printer + a permission chooser click).

Commit: `"Add Web Bluetooth page"`.

### Task 4: Wrap-up

- fmt/clippy/test sweep (all targets incl. `cargo build -p lxd2-web --target wasm32-unknown-unknown`)
- README: "Web app (Web Bluetooth)" section — what it is (static page, no server), browser support matrix (Chrome/Edge desktop+Android; not iOS), build (`scripts/build-web.sh`), serve locally (`python3 -m http.server 8080 -d web`), the https-or-localhost secure-context requirement, hosting note (any static host / GitHub Pages)
- Design-doc roadmap check: all four phases delivered
- Commit `"Update README for web app"`

---

## Notes for the implementer

- `lxd2-core` must remain untouched. If a core API gap appears (e.g. missing getter), STOP and report rather than patching core silently.
- wasm-bindgen fallible constructors: `Result<WasmJob, String>` works (throws in JS). Avoid `JsError`/`JsValue` in signatures compiled natively.
- serde-wasm-bindgen + `Vec<u8>`: confirm actions carry Uint8Array (use `serde_bytes::ByteBuf` if needed — add `serde_bytes` dep only if required).
- The page must remain a static site: no fetch() to any backend, everything client-side.
- Web Bluetooth needs a secure context: localhost is fine for dev; note https for hosting.
- Commit messages: never mention Claude.
