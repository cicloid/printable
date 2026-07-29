# Architecture

How printa-ble is put together, and why. For contributors and for future-you.

The whole project is one idea repeated: **everything becomes a 384 px wide, 1-bit bitmap, and one sans-IO state machine turns that bitmap into BLE packets.** Every surface — CLI, HTTP server, browser — is a different way of feeding those two functions.

## The three crates

| Crate | Binary / artifact | May do I/O | Depends on |
|---|---|---|---|
| `printa-ble-core` | rlib | **No** | `thiserror`, `image`, `fontdue`, `pulldown-cmark`, `qrcode`, `barcoders` |
| `printa-ble` | `printable` (CLI + HTTP server) | Yes: BLE, files, network, Chrome | core + `tokio`, `btleplug`, `axum`, `clap`, `reqwest`, `rand`, `tracing`, `chromiumoxide` (optional), `serde`/`toml`/`dirs` |
| `printa-ble-web` | cdylib (WASM) + rlib | No — JS does it | core + `wasm-bindgen`, `serde-wasm-bindgen`, `serde_bytes`, `image` |

The boundary is enforced by the dependency list, not by convention. `printa-ble-core` has **no tokio, no BLE, no `rand`, no HTTP client, no filesystem access**. Any code that wants to wait, connect, fetch, or roll dice lives in one of the other two crates.

### Why sans-IO

Two payoffs, both load-bearing:

1. **Native tests for everything, including the protocol.** The print flow is a pure state machine: you feed it notification bytes and assert on the packets it wants to send. The core's hundred-plus tests run in under a second with no printer, no Bluetooth adapter, and no `#[ignore]`. A print bug is reproducible on CI.
2. **A WASM build with zero changes.** `printa-ble-core` compiles to `wasm32-unknown-unknown` as-is — no feature flags, no `cfg`, no shims. `printa-ble-web` is a thin `wasm-bindgen` wrapper. The browser gets byte-identical rendering and byte-identical protocol behaviour to the CLI because it is literally the same code.

The cost is real and visible in the API: the core cannot fetch a markdown image, cannot generate an auth nonce, and cannot sleep. Each of those becomes a parameter. See [Randomness injection](#randomness-injection) and the two-pass image flow in [MARKDOWN.md](MARKDOWN.md#images).

## The print FSM

`printa_ble_core::protocol::job::PrintJob` drives the whole LX-D02 flow — hello, auth, density, raster streaming, flow control, finish — without performing any I/O. The contract is three calls:

```rust
let mut job = PrintJob::new(&bitmap, density, challenge, inter_packet_delay_ms)?;
loop {
    match job.next_action() {
        Action::Send(bytes)      => /* write to characteristic 0xFFE1 */,
        Action::WaitMs(ms)       => /* sleep */,
        Action::WaitNotification => /* block on 0xFFE2, then job.on_notification(n) */,
        Action::Done             => break,
    }
}
job.error() // None on success
```

The caller owns *how* to write, sleep, and wait. The job owns *what* and *when*.

### The wire flow

| Step | Packet | Awaited notification |
|---|---|---|
| Hello | `5A 01` (12 B) | `5A 01` + MAC at bytes 4..10 |
| Auth challenge | `5A 0A` + 10 random bytes | `5A 0A` (payload unused) |
| Auth response | `5A 0B` + 10 bytes: per-byte `CRC16/XMODEM(challenge[i] ‖ mac) >> 8` | `5A 0B 01` ok / `5A 0B 00` fail (fatal) |
| Density | `5A 0C <1-7>` | — |
| Print start | `5A 04 <packets:be16> 00 00` | — |
| Raster ×N | `55 <index:be16> <96 B> 00` — two 48-byte rows per packet | flow control, below |
| Print end | `5A 04 <packets:be16> 01 00` | — |

Flow control while streaming (`5A 05/06/07/08`):

| Notification | Effect |
|---|---|
| `LostPacket { index }` | Rewind `send_idx` to `index − 1` and resume — the convention observed in the official app |
| `Hold` | Park in `Holding` until a `LostPacket` resumes or a `Finished` ends the job |
| `Cooldown` | Emit a one-shot `WaitMs(100)` back-off, then carry on |
| `Finished` | The printer decides the job is over, even mid-stream → send print-end |

Everything else (periodic `5A 02` status frames) is ignored by the FSM. Paper and battery checks belong to the transport layer, which sees those frames anyway.

Bounds live in the constructor: more than `u16::MAX` raster packets (131,070 rows) is `JobError::TooLarge`, rejected before a single byte goes out.

### Two transports, one FSM

This is the architectural centerpiece. The same `PrintJob` is pumped by native Rust over btleplug and by JavaScript over Web Bluetooth. Neither transport knows the protocol.

**Native — `crates/printa-ble/src/ble.rs`, the loop inside `Printer::run_job`:**

```
loop {
    while let Ok(n) = notify_rx.try_recv() { check_paper(&n)?; job.on_notification(n); }
    match job.next_action() {
        Send(bytes)      => peripheral.write(&write_char, &bytes, WithoutResponse).await?,
        WaitMs(ms)       => tokio::time::sleep(ms).await,
        WaitNotification => job.on_notification(timeout(10s, notify_rx.recv()).await?),
        Done             => break,
    }
}
```

A background task parses raw 0xFFE2 frames into `Notification`s and pushes them into an unbounded channel. The loop drains that channel *before* every action, so mid-stream `Hold` / `LostPacket` / `Cooldown` reach the FSM even while it is on the fast Send/WaitMs path — and a no-paper status frame aborts the job there rather than later, as a misleading timeout.

**Browser — `web/app.js`, `pump()`:**

```js
while (job) {
  const a = job.next_action();          // {kind:"send"|"waitMs"|"waitNotification"|"done"}
  if (a.kind === "send") await gattWrite(a.bytes);
  else if (a.kind === "waitMs") await sleep(a.ms);
  else if (a.kind === "waitNotification") { armWatchdog(); return; }
  else { finishJob(null); return; }
}
```

JS has no blocking receive, so the shape inverts: `pump()` *returns* on `waitNotification` and the GATT `characteristicvaluechanged` handler calls `job.on_notification(bytes)` and re-enters `pump()`. An `isPumping` flag makes re-entrant calls no-ops — a notification arriving mid-write is absorbed by the running loop's next `next_action()`. A 10 s watchdog replaces the native `timeout`, and a `gattserverdisconnected` event fails the job.

`WasmJob` (`crates/printa-ble-web/src/job.rs`) is the bridge: it serializes `Action` to a tagged JS object and takes raw notification bytes, parsing them with the same `notifications::parse` the CLI uses. One subtlety is pinned in a comment there: `Send.bytes` uses `serde_bytes` so serde-wasm-bindgen emits a `Uint8Array`; a plain `Vec<u8>` would arrive as a JS `Array`, which GATT `writeValue` rejects.

The result: a protocol fix — a flow-control quirk, a retry rule, an auth detail — is made once, in core, and both transports get it. The BLE module holds no protocol knowledge beyond the two characteristic UUIDs (0xFFE1 write, 0xFFE2 notify); the web page holds those plus the service UUID 0xFFE6 and one two-byte peek at `5A 02` frames to show battery percentage in the status chip.

## The rendering pipeline

Everything converges on `Bitmap`: 384 px wide, MSB-first, bit 1 = black, one `[u8; 48]` per row. It knows how to grow (`extend_blank`, for paper feed) and how to chunk itself into 96-byte raster payloads (two rows each, zero-padded).

```
text ────────────► render_text ──┐
markdown ─► lower ─► render_rich ─┤
                                  ├──► Bitmap (384×h, 1-bit) ──► to_raster_payloads ──► PrintJob
image ──► prepare ─► image_to_bitmap ┤                       └──► bitmap_to_png ──► preview
qr / barcode ─────────────────────┘
```

The layers, bottom up:

| Module | Responsibility |
|---|---|
| `raster/bitmap.rs` | The 1-bit canvas and raster chunking |
| `raster/rich.rs` | The typesetter: styled spans → glyphs. Greedy word wrap, mixed sizes on a shared baseline, strikethrough, indent. Owns the three embedded JetBrains Mono faces |
| `raster/text.rs` | Plain text — a thin wrapper: split on `\n`, one `RichLine` each |
| `raster/markdown.rs` | Lowers CommonMark events onto `rich`, plus its own block graphics |
| `raster/dither.rs` | `prepare` (grayscale + Lanczos3 scale to 384 px, height clamped to 4096 rows) and `image_to_bitmap` (Floyd–Steinberg, Atkinson, or threshold) |
| `raster/qr.rs`, `raster/barcode.rs` | Self-contained graphics with their own quiet zones and margins |
| `raster/preview.rs` | `Bitmap` → grayscale PNG, the paperless test path |

### Block stacking

Markdown does not render top-to-bottom in one pass. Lowering produces a `Vec<MdBlock>`:

```rust
enum MdBlock { Lines(Vec<RichLine>), Rule, Tear, Qr(String), Barcode(String), Image(Bitmap) }
```

Each block renders to its own `Bitmap` independently — text through `render_rich`, graphics through their own renderers — and `stack()` concatenates them vertically. `padded()` adds uniform white margins, which is how a QR (16 px of built-in quiet space) and a barcode (none) end up equally spaced: each fence declares what it already draws and is padded *to* a common 24 px, not *by* it.

This is why graphics are full-width and never inherit list or quote indentation (see [MARKDOWN.md](MARKDOWN.md#layout-limitations-worth-knowing)): they are siblings of the text block, not spans inside it. It is also why a failed fence prints its error text with the same margins a successful one would have had — otherwise it would collide with the neighbouring paragraph.

## The four surfaces

| Surface | Entry point | Renders where | Talks to the printer how |
|---|---|---|---|
| CLI | `printable print/qr/scan/status` | Native, in-process | btleplug (native BLE) |
| HTTP API | `printable serve` → `server.rs` | Native, in the server process | btleplug, serialized by one mutex |
| Server UI | `crates/printa-ble/src/server/ui.html`, embedded with `include_str!` | Server-side, via the REST API | Indirectly — it is a thin client |
| Web app | `web/index.html` + `web/app.js` + `web/pkg/` (WASM) | In the browser, WASM | Web Bluetooth, from the page |

What is shared and what is not:

| Concern | Shared in core | Surface-specific |
|---|---|---|
| Text / markdown / QR / barcode rendering | ✅ Identical everywhere | — |
| Image decode, scale, dither | ✅ `prepare` + `image_to_bitmap` | Which bytes get there |
| Print protocol / FSM | ✅ `PrintJob` | The pump (tokio loop vs JS callback) |
| Notification parsing | ✅ `notifications::parse` | Who reads the characteristic |
| **Markdown image resolution** | ❌ Core only lists refs | CLI: files + HTTP · Server: HTTP only · Web: browser `fetch` (CORS) |
| **Randomness** | ❌ Injected | CLI/server: `rand::random()` · Web: `crypto.getRandomValues` |
| **URL → page rendering** | ❌ Not in core at all | CLI/server only, headless Chrome behind the `url` feature |
| Device memory (`config.toml`) | ❌ | CLI/server only; the browser re-picks a device each session |
| Paper/battery gating | ❌ | CLI/server abort on `no_paper`; the web page shows battery and relies on the watchdog |
| Feed, density, copies bounds | ❌ (values passed in) | CLI: clap validators · Server: `PrintOpts::validate` · Web: JS `clamp` + `WasmJob::new` checks |

The CLI and the server share more than the table suggests: both go through `print_service::print_bitmap`, which appends feed, validates the job before touching BLE, connects (explicit `--device` > saved device > any `LX*`), runs one full job per copy over a single connection, and remembers the device. The server adds a mutex so concurrent requests queue instead of fighting over one printer, and maps the marker error types (`NoPrinterFound`, `NoPaper`, `PrintFailure`, `JobError::TooLarge`) to HTTP statuses exactly as the CLI maps them to exit codes 2/3/4/1.

## Randomness injection

`PrintJob::new` takes `challenge: [u8; 10]` instead of generating it. The auth handshake needs 10 random bytes, and the printer answers by CRC-ing each of them against its own MAC — so the bytes must be unpredictable in production but *fixed* in a test.

Injecting them buys three things:

- **Core keeps no RNG dependency.** `rand` pulls in `getrandom`, which on `wasm32-unknown-unknown` needs a JS shim and a feature flag. Adding it would have broken the "compiles to WASM unchanged" property for one 10-byte array.
- **The FSM is fully deterministic.** A test can assert on the exact 12 bytes of the `5A 0B` auth reply, computed independently from a known challenge and MAC (`printa-ble-web/src/job.rs::happy_path_kinds` does exactly that).
- **Each surface uses the right source.** Native takes `rand::random()`; the browser takes `crypto.getRandomValues(new Uint8Array(10))`, which is the correct CSPRNG there and is not reachable from Rust without a shim.

A fresh challenge is drawn per copy, not per connection: `print_bitmap` builds a new `PrintJob` for each copy, and `runJob` in `app.js` does the same.

## Testing strategy

`cargo test --workspace` runs everything natively in about a second. One test is `#[ignore]`d; nothing else needs hardware or network.

| Crate | Notable coverage |
|---|---|
| `printa-ble-core` | Markdown lowering (the largest test module by far), rich-text layout, dithering ratios, QR/barcode geometry, packet bytes, CRC check value, notification parsing, and the FSM |
| `printa-ble` | HTTP handlers via `tower::ServiceExt::oneshot`, error→status mapping, multipart parsing, image resolution against a loopback server, config round-trips, URL scheme validation |
| `printa-ble-web` | WASM wrappers and the `WasmJob` action contract, all through the rlib build |

Techniques worth copying when you add code:

- **The FSM is tested by replaying notification bytes.** `drain_sends(&mut job)` pulls actions until the job blocks, then a test feeds a hand-written frame (`[0x5A, 0x01, 0, 0, mac…]`) and asserts on the next packets. Auth failure, lost-packet rewind, hold/resume, cooldown back-off, and mid-stream `Finished` are all covered without a printer.
- **Rendering is tested on pixels, not on prose.** Helpers like `ink_bbox`, `min_ink_x`, `tallest_run`, and full row-vector comparison assert real geometry: "no ink left of the 24 px list indent", "the tear pattern is 8 on / 8 off starting black at x = 0", "a failed fence keeps ≥ 24 px of white from its neighbours". Where a rendering must equal another, tests compare bitmaps directly (`~~Hi~~` equals struck `Hi`, `[x] task` equals its literal text at indent 24).
- **Preview PNGs replace hardware.** `printable print -f x.md --preview out.png` renders the exact bitmap that would be streamed. Use it to eyeball a change; use pixel assertions to pin it.
- **Network is faked locally.** `md_images` tests spin an ephemeral axum server on `127.0.0.1:0` and serve a real PNG, and probe refusal with port 1. No test reaches the internet.
- **The server tests stop at the BLE boundary by design.** Everything before it — validation, rendering, error mapping, the busy branch of `/status` — is tested; `/print/*` success paths are not, because they would connect to a real printer.

Genuinely requires hardware: BLE scan/connect/subscribe, an end-to-end print, Web Bluetooth GATT, and anything about the physical result (density, paper feed, tear placement). Also unautomated: headless Chrome rendering (`chrome::tests::render_example_com` is `#[ignore]`d — run with `cargo test -- --ignored` when Chrome and network are available).

## Directory map

```
crates/printa-ble-core/          sans-IO: no tokio, no BLE, no rand, no network
  assets/                        JetBrains Mono Regular/Bold/Italic + OFL licence
  src/protocol/
    packets.rs                   command builders (5A 01/04/0A/0B/0C, 55 raster)
    notifications.rs             0xFFE2 frame parser → Notification
    crc.rs                       CRC16/XMODEM (auth only)
    auth.rs                      challenge ⊕ MAC → 10-byte response
    job.rs                       the print FSM: Action / next_action / on_notification
  src/raster/
    bitmap.rs                    384 px 1-bit canvas, raster chunking
    rich.rs                      styled-span typesetter (fonts, wrap, strike)
    text.rs                      plain text → RichLines
    markdown.rs                  CommonMark → MdBlocks → Bitmap
    dither.rs                    scale + Floyd–Steinberg / Atkinson / threshold
    qr.rs, barcode.rs            graphics with their own quiet zones
    preview.rs                   Bitmap → PNG

crates/printa-ble/               the `printable` binary: CLI + HTTP server
  src/main.rs                    command dispatch, input → Bitmap, exit codes
  src/cli.rs                     clap definitions and validators
  src/ble.rs                     btleplug transport; device matching; run_job pump
  src/print_service.rs           shared connect → print → disconnect flow, marker errors
  src/md_images.rs               markdown image resolution (local/remote gates, caps)
  src/server.rs                  axum routes, preview/print handlers, error mapping
  src/server/ui.html             embedded web UI (include_str!)
  src/chrome.rs                  URL → PNG via headless Chrome (feature `url`)
  src/config.rs                  ~/…/printa-ble/config.toml, remembers the printer

crates/printa-ble-web/           WASM bindings (cdylib + rlib so it tests natively)
  src/lib.rs                     render_text / render_markdown_with_images / ImageSet / render_qr / render_image
  src/job.rs                     WasmJob: the FSM bridged to JS

web/                             the static Web Bluetooth page (no build step)
  index.html, app.js             DOM + GATT only; rendering and protocol are WASM
  pkg/                           wasm-pack output (gitignored)

scripts/build-web.sh             wasm-pack build → web/pkg
docs/plans/                      per-phase implementation plans (historical)
```

Note for archaeologists: the plan documents in `docs/plans/` predate a rename and refer to the crates as `lxd2-core` / `lxd2` / `lxd2-web`.

## Design decisions

**Sans-IO core.** Covered above. The one rule to preserve: if a change would add tokio, a socket, a file handle, or an RNG to `printa-ble-core`, it belongs somewhere else. The WASM build and the hardware-free test suite both depend on it.

**Why not CUPS.** Deliberately deferred in the original design ("ValdikSS and rusq cover this niche; revisit later"). A CUPS backend gets you a system print dialog and nothing else this project needs — and it costs a spooler, a PPD, an install step, and platform-specific packaging. A single static binary that also speaks HTTP and compiles to the browser reaches phones and laptops that no CUPS queue would.

**Why headless Chrome for `--url`.** Rendering arbitrary modern HTML/CSS is a browser's job; no Rust HTML renderer would produce a page that looks like the page. The cost is an external dependency and an execution surface, so it is contained: gated behind the default-on `url` feature (`--no-default-features` removes chromiumoxide and the two routes entirely), restricted to `http(s)` by `validate_url` before Chrome launches, and rendered at a 384 px viewport with a full-page screenshot.

**Why no bundler for the web app.** `wasm-pack --target web` emits a plain ES module, so `index.html` loads `app.js` with `<script type="module">` and `app.js` imports the glue directly. No npm, no node_modules, no build step beyond `scripts/build-web.sh`, and `web/` is a static directory you can drop on GitHub Pages. The page is three files and a wasm blob; a bundler would add more configuration than it removes.

**Why `include_str!` instead of rust-embed.** The design called for rust-embed, but the server ships exactly one asset — `ui.html`. `include_str!` gets the same single-binary property with zero dependencies and zero API. Revisit only if the UI grows into a tree of files.

**Why markdown images resolve outside core.** A `HashMap<String, Bitmap>` handed in is the only design that lets three surfaces with three completely different capabilities (filesystem + HTTP, HTTP only, browser fetch under CORS) share one renderer — and it makes "the server must never read local files" a property of one function's arguments rather than a rule scattered through the renderer.

**Why one mutex instead of a job queue.** There is one printer and it is not concurrent. `AppState::print_lock` is held across connect-print-disconnect; `/status` try-locks and reports `{"printing": true}` rather than queueing behind a long job; previews never take it at all.

**Why the CLI prints progress from `print_service`.** A known wart, marked as such in the source: progress lines go straight to stdout/stderr to preserve exact CLI behaviour, which means the server's handlers cannot report them. Moving reporting out to the caller is the fix when someone needs it.

## Where to add things

**A new renderable content type** (say, a sparkline block). Touch, in order:

1. `crates/printa-ble-core/src/raster/<thing>.rs` — the renderer: `fn render_thing(...) -> Result<Bitmap, ThingError>`. Pure, no I/O. Include a quiet zone/margin decision and document it.
2. `crates/printa-ble-core/src/raster/mod.rs` — `pub mod` + re-export.
3. `crates/printa-ble-core/src/raster/markdown.rs` — if it should have a fence: add an `MdBlock` variant, a `Fence` variant, a `fence_kind` arm, and a `render_markdown_with` arm calling `fence_bitmap(render_thing(..), built_in_margin)`. Error paths must render text, never panic.
4. `crates/printa-ble/src/cli.rs` + `main.rs` — a subcommand or file extension, if it deserves one; route it through `dispatch` so `--preview`, `--copies`, and feed keep working.
5. `crates/printa-ble/src/server.rs` — `/preview/<thing>` and `/print/<thing>` (validate → render → `print_and_respond`), plus a tab in `src/server/ui.html`.
6. `crates/printa-ble-web/src/lib.rs` — a `#[wasm_bindgen]` wrapper returning `WasmBitmap` (fallible ones return `Result<_, String>`), then a tab in `web/index.html` and a `renderCurrent()` arm in `web/app.js`.
7. Tests: pixel assertions in core, a 400-path test in the server, a wrapper test in the web crate. Run `cargo test --workspace` — it needs no hardware.

**A protocol change** (new packet, new notification, different retry rule): `protocol/packets.rs`, `protocol/notifications.rs`, and `protocol/job.rs` only. Add a replay test. Both transports inherit it; do not add protocol knowledge to `ble.rs` or `app.js`.

**A new print surface** (Matrix bot, ESC/POS bridge, whatever): depend on `printa-ble-core`, drive `PrintJob` with your own pump, and supply your own randomness. That is the entire contract — the action loops in `ble.rs::run_job` and `app.js::pump` are both complete reference implementations, and each fits on a screen.
