# lxd2 Phase 3 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `lxd2 serve` — a printa-style HTTP API plus a small embedded web UI so any device on the LAN can print, and URL-to-print via headless Chrome (server and CLI).

**Architecture:** The print pipeline moves out of `main.rs` into a shared `print_service` module used by both the CLI and the new axum server. The server serializes print jobs with a tokio `Mutex` (one printer, one job at a time) and connects to the printer per job. Chrome rendering lives in its own feature-gated module (`url` feature, on by default) driving system Chrome via `chromiumoxide` at a 384-px viewport.

**Tech Stack:** `axum` 0.8 (with `multipart`), `tower` (tests via `oneshot`), `serde_json`, `chromiumoxide` 0.7 (tokio runtime), embedded UI via `include_str!`. No new deps in `lxd2-core` — phase 3 is entirely CLI-crate work.

**State:** Phases 1-2 complete at `147a075`: core (protocol FSM + bitmap/dither/text/rich/markdown/qr/preview), CLI (scan/status/print/qr, config, copies). 69 tests. Design doc: `docs/plans/2026-07-27-lxd2-design.md`.

---

### Task 1: Extract shared print service (pure refactor)

**Files:**
- Create: `crates/lxd2/src/print_service.rs`
- Modify: `crates/lxd2/src/main.rs` (shrinks), `crates/lxd2/src/cli.rs` (untouched or trivial)

Move from `main.rs` into `print_service.rs`:
```rust
#[derive(Debug, Clone, Copy)]
pub struct PrintOptions {
    pub density: u8,   // 1-7
    pub feed: usize,
    pub copies: u16,   // 1-20
}

/// Append feed, validate, connect (resolution: explicit > saved > any LX), run
/// `copies` jobs over one connection, remember device. Returns total lines printed.
pub async fn print_bitmap(
    mut bitmap: Bitmap,
    explicit_device: Option<&str>,
    opts: PrintOptions,
) -> anyhow::Result<usize>
```
Also move `build_bitmap`-style helpers that the server will reuse: `pub fn bitmap_from_image_bytes(bytes: &[u8], dither: Dither) -> anyhow::Result<Bitmap>` (decode via `image::load_from_memory`, zero-width guard, prepare, dither) — refactor cmd_print's file/image path to call it. Preview stays in main.rs (CLI-only concern). `dispatch()` in main.rs becomes a thin wrapper: preview short-circuit else `print_service::print_bitmap`.

**Verification:** pure refactor — `cargo test --workspace` still 69, clippy/fmt clean, `print --preview` byte-identical output (hash before/after), no behavior change. NO hardware.

Commit: `"Extract shared print service"`.

### Task 2: Headless Chrome URL rendering + CLI --url

**Files:**
- Create: `crates/lxd2/src/chrome.rs`
- Modify: `crates/lxd2/Cargo.toml`, `cli.rs`, `main.rs`

Cargo.toml:
```toml
[features]
default = ["url"]
url = ["dep:chromiumoxide"]

[dependencies]
chromiumoxide = { version = "0.7", default-features = false, features = ["tokio-runtime"], optional = true }
```

`chrome.rs` (whole module `#![cfg(feature = "url")]`… use `#[cfg(feature = "url")] mod chrome;` in main):
```rust
/// Render a URL to a full-page PNG at 384 px width using system Chrome.
pub async fn render_url_png(url: &str) -> anyhow::Result<Vec<u8>>
```
- `BrowserConfig::builder().window_size(384, 800).arg("--hide-scrollbars")` — chromiumoxide auto-detects the Chrome binary; map launch failure to "Chrome not found — install Google Chrome or build without the `url` feature"
- Spawn browser + handler task, `browser.new_page(url)`, wait for navigation, 500 ms settle sleep, screenshot with `CaptureScreenshotParams` full-page (`capture_beyond_viewport(true)`), PNG format
- Always close the browser (also on error paths — use a scopeguard-style explicit close before `?` returns, or a helper that owns cleanup)
- Validate scheme: only `http://` / `https://` (reject `file://` etc. — the server will expose this endpoint on the LAN)

CLI: `PrintArgs` gains `#[arg(long, conflicts_with_all = ["text", "file"])] url: Option<String>`; pipeline: `render_url_png` → `bitmap_from_image_bytes(bytes, dither)`. When built without the feature, `--url` isn't present (cfg on the field + build_bitmap arm).

**Tests:** unit-test scheme validation (pure fn `validate_url`). Chrome integration: one `#[ignore]`d tokio test `render_example_com` (runs only via `cargo test -- --ignored`) — the implementer SHOULD run it once locally (Chrome is installed on this Mac) and eyeball the PNG via `--preview`. Also verify `cargo build --no-default-features` compiles without chromiumoxide.

Commit: `"Add URL printing via headless Chrome"`.

### Task 3: Server skeleton — health, status, previews

**Files:**
- Create: `crates/lxd2/src/server.rs`
- Modify: `Cargo.toml` (axum/tower/serde_json), `cli.rs` (Serve command), `main.rs`

Cargo.toml: `axum = { version = "0.8", features = ["multipart"] }`, `serde_json = "1"`, dev-dep `tower = { version = "0.5", features = ["util"] }`, plus `http-body-util` dev-dep for reading test bodies.

CLI: `Serve { #[arg(long, default_value_t = 8000)] port: u16, #[arg(long, default_value = "0.0.0.0")] bind: String, #[command(flatten)] device: DeviceArgs }` — bind default 0.0.0.0 (LAN printing is the point; README documents the trust model: anyone on the LAN can print).

`server.rs`:
```rust
pub struct AppState {
    pub device: Option<String>,          // --device flag at serve time
    pub print_lock: tokio::sync::Mutex<()>, // one job at a time
}
pub fn router(state: Arc<AppState>) -> axum::Router
pub async fn serve(bind: String, port: u16, device: Option<String>) -> anyhow::Result<()>
```

Endpoints this task:
- `GET /health` → `{"status":"ok","version":env!("CARGO_PKG_VERSION")}`
- `GET /status` → connect + `wait_status` → JSON of the Status fields; 503 `{"error":...}` if no printer
- `POST /preview/text` body `{"content": "...", "size": 24.0?}` → `image/png` bytes (bitmap_to_png of render_text)
- `POST /preview/markdown` `{"content"}` → PNG
- `POST /preview/qr` `{"data", "caption"?}` → PNG (400 on QrError)
- `POST /preview/image` multipart field `file` (+ optional `dither` field: floyd|atkinson|threshold) → PNG
- Errors: JSON `{"error": "..."}` with 400 (bad input/render), 503 (printer unreachable), 500 (other)

Shared request structs with serde defaults (`density` 3, `feed` 40, `copies` 1, validated 1-7 / 1-20 → 400 out of range — write a small `validate()` helper, unit-tested).

**Tests (tower oneshot, no BLE/no Chrome — write first):**
```rust
#[tokio::test] async fn health_ok()                  // 200, body contains "ok"
#[tokio::test] async fn preview_text_returns_png()   // 200, content-type image/png, body starts with PNG magic \x89PNG
#[tokio::test] async fn preview_markdown_returns_png()
#[tokio::test] async fn preview_qr_returns_png()
#[tokio::test] async fn preview_qr_too_long_is_400()
#[tokio::test] async fn preview_text_empty_is_400()
#[tokio::test] async fn density_out_of_range_is_400() // {"content":"x","density":9}
```

Commit: `"Add serve command with status and preview endpoints"`.

### Task 4: Print endpoints

**Files:**
- Modify: `crates/lxd2/src/server.rs`

- `POST /print/text` `{"content", "size"?, "density"?, "feed"?, "copies"?}` → render → `print_lock.lock().await` → `print_service::print_bitmap` → `{"printed_lines": N, "copies": M}`
- `POST /print/markdown`, `POST /print/qr` — same pattern
- `POST /print/image` — multipart like preview + density/feed/copies fields
- `POST /print/url` `{"url", ...}` (cfg feature url): `render_url_png` → bitmap → print; 400 invalid scheme, 502 chrome failure
- Error mapping from print_service: no printer → 503, no paper → 409 `{"error":"printer is out of paper"}`, print failure → 500, TooLarge → 400 (downcast the marker types from ble.rs/main errors — move markers `NoPrinterFound/NoPaper/PrintFailure` into print_service.rs so server and CLI share them; CLI exit-code mapping keeps working)
- Serialization: lock held across the whole connect+print; concurrent requests queue (document in code; no explicit timeout — BLE layer already has its own)

**Tests:** oneshot tests for validation-only paths (missing content → 400/422, bad url scheme → 400 — the handler validates before touching BLE). Real print paths: NOT testable without hardware — leave for hardware validation. Ensure the print handlers' pre-BLE validation is factored so tests exercise it (e.g. render+validate happens before lock/connect).

Commit: `"Add print endpoints"`.

### Task 5: Embedded web UI

**Files:**
- Create: `crates/lxd2/src/server/ui.html` (include_str! from server.rs)
- Modify: `server.rs` — `GET /` serves it (`Html<&'static str>`)

Single self-contained HTML (vanilla JS, no external assets — must work offline):
- Tabs: Text | Markdown | Image | QR | URL (URL tab hidden if the server was built without the feature — expose feature flag in /health response `{"url_printing": bool}` and hide via JS)
- Controls: density (1-7 slider), feed, copies; textarea for text/markdown; file input for image (+ dither select); data+caption for QR; url input
- Buttons: **Preview** (fetch POST /preview/* → blob → show `<img>`, 384px wide, bordered) and **Print** (POST /print/* → show result/error toast)
- Status footer: fetch /status on load — battery/paper/density; degrade gracefully on 503 ("printer unreachable")
- Styling: minimal clean CSS inline, dark-mode friendly (prefers-color-scheme), mobile-first (this is used from a phone)
- No framework, no build step

**Tests:** oneshot `GET /` → 200 text/html containing "lxd2". Manual: `lxd2 serve` + curl the API + open browser — implementer does curl checks only (no printer; preview endpoints fine), report findings.

Commit: `"Add embedded web UI"`.

### Task 6: Wrap-up

- fmt/clippy/test sweep (expect ~80 tests)
- README: Server section — serve usage, endpoint table with curl examples (mirror printa's README style), UI screenshot omitted (no binary assets), LAN trust-model note, Chrome requirement + `--no-default-features` opt-out
- Design-doc check: phase 3 scope delivered (REST + UI + URL printing)
- Commit `"Update README for server mode"`

---

## Notes for the implementer

- `lxd2-core` must remain untouched this phase (except nothing — all server work is CLI-crate).
- The protocol/BLE layer (`ble.rs` internals, FSM) must not change; `print_service` only re-homes existing main.rs logic.
- Marker error types move to `print_service.rs`; re-check CLI exit codes 2/3/4 still work after the move (the downcast is type-based — moving the type is fine as long as both sides use the same one).
- Chrome tests: run the `#[ignore]` test manually once; don't wire Chrome into the default test run (CI-hostile).
- UI: keep it genuinely dependency-free (no CDN links — must work on a LAN without internet).
- Commit messages: never mention Claude.
