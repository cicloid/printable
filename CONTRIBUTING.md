# Contributing to printa-ble

Thanks for your interest in contributing! This document covers setup, the test
workflow, and the architectural rules the codebase depends on.

The single most important thing to know up front: **you never need a printer to
develop this project.** Every rendering path has a `--preview` mode that writes
a PNG instead of sending bytes over Bluetooth, and the whole test suite runs on
a plain laptop with no hardware attached.

## Development Setup

### Prerequisites

- **Rust stable** (the workspace is edition 2021; no MSRV is pinned — a current
  stable toolchain via [rustup](https://rustup.rs) is what CI and the
  maintainer use)
- **`wasm32-unknown-unknown` target** — for the WASM crate:
  `rustup target add wasm32-unknown-unknown`
- **wasm-pack** — only needed to build the web app: `brew install wasm-pack`
- **Google Chrome** (optional) — only for URL printing (`--url`, `/print/url`)
- **An LX-D02 / LX-D2 printer** (optional) — only for hardware validation

macOS is the primary platform: the BLE transport uses
[btleplug](https://github.com/deviceplug/btleplug), which on macOS goes through
CoreBluetooth. btleplug also supports Linux (BlueZ) and Windows, but those paths
are **untested** here — see [Where to Start](#where-to-start).

### Clone and Build

```bash
git clone https://github.com/cicloid/printable.git
cd printable

# Optional but recommended: pre-commit hooks mirroring the CI gate
# (fmt + clippy, plus the wasm32 sans-IO check when core changed;
# skip once with `git commit --no-verify`)
brew install prek   # or: cargo install --locked prek
prek install

# Build the whole workspace
cargo build

# Run the CLI without installing
cargo run -p printa-ble -- scan
cargo run -p printa-ble -- print "hello" --preview out.png

# Install the `printable` binary onto your PATH
cargo install --path crates/printa-ble
```

### Building the Web App

```bash
rustup target add wasm32-unknown-unknown
scripts/build-web.sh                  # wraps wasm-pack, outputs to web/pkg/
python3 -m http.server 8080 -d web
```

Then open <http://localhost:8080>. Web Bluetooth requires a secure context, so
`localhost` or `https` only — a plain `http` LAN address will not work.

## Testing

```bash
# The whole suite — no hardware, no network, no printer
cargo test --workspace

# One crate
cargo test -p printa-ble-core

# Tests matching a pattern
cargo test --workspace markdown
```

This is the one place in the repository that quotes a test count, because it is
the one that goes stale. At the time of writing `cargo test --workspace`
collects **295 tests** — 172 in `printa-ble-core`, 94 unit plus 8 integration in
`printa-ble`, 21 in `printa-ble-web` — of which 294 run and one is `#[ignore]`d.
The whole suite finishes in a couple of seconds with no printer, no Bluetooth
adapter, and no network. If your change adds tests, the number here is expected
to move; count them rather than trusting this line:

```bash
cargo test --workspace 2>&1 | grep 'test result:'
```

### What Needs Hardware, and What Doesn't

| Covered natively | Needs a real LX-D02 |
|---|---|
| Packet construction, CRC, auth handshake | End-to-end print over BLE |
| The print-job state machine, including retransmit/hold/cooldown paths | Real thermal flow control timing |
| Text layout, markdown, dithering, QR, barcodes, PNG preview | Paper feed, density, tear behaviour |
| HTTP server routes and error codes (via `tower::ServiceExt`) | Battery/paper status from the device |
| Image resolution limits and the local-path security boundary | — |

The BLE transport itself (`crates/printa-ble/src/ble.rs`) is the thin,
untestable-without-hardware layer. That is deliberate: everything worth testing
lives behind the sans-IO seam in `printa-ble-core`.

### The Ignored Chrome Test

One test needs Chrome and network access, so it is `#[ignore]`d by default
(`crates/printa-ble/src/chrome.rs`). Run it explicitly:

```bash
cargo test -p printa-ble -- --ignored
```

### The `--preview` Workflow

`--preview <PATH>` renders exactly what would be sent to the printer and writes
it as a PNG instead. Use it constantly:

```bash
cargo run -p printa-ble -- print "hello world" --preview /tmp/out.png
cargo run -p printa-ble -- print -f notes.md --preview /tmp/out.png
cargo run -p printa-ble -- print -m "# heading" --preview /tmp/out.png
cargo run -p printa-ble -- qr "https://example.com" --caption "scan me" --preview /tmp/out.png
open /tmp/out.png
```

`-m` forces markdown rendering for input with no `.md` extension to give it
away, which makes it the quickest way to preview a snippet without writing a
file first.

The server exposes the same thing over HTTP at `/preview/text`,
`/preview/markdown`, `/preview/qr`, `/preview/image`, and `/preview/url` — all
of which return a PNG and never touch the printer. See [docs/API.md](docs/API.md).

### Debugging

The CLI has structured logging on a global `-v` flag (it works before or after
the subcommand). Everything goes to **stderr**; stdout carries the command's
actual output, and scripts parse it.

| Flag | Filter | Shows |
|---|---|---|
| *(none)* | `printable=warn` | This crate's warnings, and nothing else |
| `-v` | `printable=info` | Flow control and progress — connection, thermal holds and resumes, retransmit requests, the server's request log and job summaries |
| `-vv` | `printable=debug` | Parsed protocol frames, device resolution, image-resolution timings |
| `-vvv` | `debug,printable=trace` | Raw hex on the wire, plus dependency logs |

The default filter is **crate-scoped on purpose**, and there is a test pinning
it (`cli::tests::dependency_errors_are_silent_below_the_last_verbosity_rung`).
chromiumoxide logs the websocket frames it fails to deserialize at ERROR, and
recent Chrome sends several per screenshot, so a global `warn` floor made a
perfectly successful `print --url` emit two red lines about a connection error.
Do not add a bare level directive to the first three rungs. `-vvv` is where
dependency noise is deliberately allowed back, for when the fault might be in
btleplug or Chrome rather than here.

`RUST_LOG` overrides `-v` entirely when set, for finer-grained filtering:

```bash
cargo run -p printa-ble -- -vv print "test" --preview /tmp/out.png
RUST_LOG=printable=trace,btleplug=debug cargo run -p printa-ble -- status
```

Logging lives in the CLI crate only. `printa-ble-core` has no logging at all —
see the sans-IO rule below.

## Code Style

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo clippy -p printa-ble-web --target wasm32-unknown-unknown
```

Both clippy invocations must be **warning-free** before you open a PR. The WASM
one matters independently: `printa-ble-web` compiles for a target with no
threads, no filesystem, and no sockets, so it catches portability mistakes the
native build lets through.

Conventions:

- Rust 2021 idioms; default rustfmt settings, no overrides.
- `snake_case` for functions and modules, `PascalCase` for types,
  `SCREAMING_SNAKE_CASE` for constants.
- Branches: `feature/...`, `fix/...`, `docs/...`, `refactor/...`.
- Document *why*, not *what*. The existing doc comments explain protocol
  quirks and design constraints — match that register.
- Constants over magic numbers, especially for protocol byte values and limits.

## Architecture Rules

These are not style preferences. Breaking them breaks builds or security
properties. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) has the full rationale.

### 1. `printa-ble-core` Stays sans-IO

`printa-ble-core` must never depend on `tokio`, `btleplug`, `reqwest`, `rand`,
or anything else that performs I/O, allocates OS resources, or reads a clock.

This is not purity for its own sake — **it is what makes the WASM build
possible.** `printa-ble-web` compiles the same crate to
`wasm32-unknown-unknown`, where there are no sockets, no filesystem, and no
threads. A `tokio` dependency in core would fail to compile for WASM and take
the entire browser app down with it. The `rand` exclusion is the subtle one: the
auth handshake needs randomness, so core takes the random bytes as a
*parameter* and lets each caller supply them (the CLI from `rand`, the browser
from `crypto.getRandomValues`).

The pattern for anything core "needs" from the outside world:

- **Inputs** come in as parameters or values. Markdown image data is resolved by
  the CLI, server, or browser and handed to the renderer as decoded bytes; the
  renderer itself never fetches anything.
- **Outputs** leave as values, not side effects. `JobStats`
  (`packets_sent` / `retransmits` / `holds` / `cooldowns`) is the model here:
  core counts what happened and returns plain data, and the transport layer
  decides whether to log it, print it, or ignore it. **Observability data leaves
  core as values, never as log calls.**

If you find yourself wanting to add a dependency to `printa-ble-core`, that is
the signal to move the work up into `printa-ble` instead.

### 2. The Protocol Layer Is Hardware-Validated

`crates/printa-ble-core/src/protocol/` — packet framing, CRC, the auth
handshake, and the print-job state machine — was reverse-engineered and
confirmed against a physical LX-D02. The magic numbers, the packet ordering, the
inter-raster delays, and the flow-control responses are all load-bearing. A
change that looks like a harmless cleanup can produce garbled output, a hung
job, or wasted paper that no unit test will catch.

Change it only with a specific reason, and say in the PR whether you validated
on hardware. [docs/PROTOCOL.md](docs/PROTOCOL.md) documents the wire format.

### 3. The Server's `allow_local = false` Is a Security Boundary

When the HTTP server resolves markdown image references, it passes
`allow_local = false` so that local filesystem paths are **refused**. Without
it, anyone who can reach the port could read files off the host with
`![x](/etc/hosts)`. The CLI passes `allow_local = true` on purpose — CLI users
already own their filesystem.

This boundary is tested. Do not "simplify" it away, and do not add a flag to
turn it on for the server. See [SECURITY.md](SECURITY.md).

## Test-Driven Development

This project was built test-first, and contributions are expected to follow.

- Write the failing test before the implementation.
- **Rendering changes need a pixel assertion or a preview check.** Assert on
  something concrete — output dimensions, ink coverage, a specific pixel, "the
  heading renders taller than the paragraph" — and attach a `--preview` PNG to
  the PR so a human can see it.
- Protocol changes need a byte-level assertion against known-good frames.
- Cover the failure path too. A malformed QR payload, an oversized barcode, an
  image that fails to decode — none of these should panic, and none should cost
  the user the rest of the document.

## Commits and Pull Requests

Commit messages: short imperative subject, then bullets if there's more to say.

```
Add markdown table rendering support

- Lay out cells as monospace text with two-space gutters
- Truncate overflowing cells with an ellipsis rather than wrapping
- Add tests for the six-column ceiling
```

Your PR description should cover:

- **What behavior changed** and why.
- **Which tests you ran** (`cargo test --workspace`, plus `--ignored` if
  relevant).
- **Which docs you updated** — `README.md` for user-facing changes,
  `docs/` for protocol, CLI, API, markdown, or architecture detail.
- **A `--preview` PNG** for anything that changes rendering. This is the single
  most useful thing you can attach.
- **Whether you validated on hardware**, and with which printer model. Say so
  explicitly either way; "not hardware-tested" is a perfectly good note.

Before requesting review: `cargo fmt --all`, both clippy passes clean, and
`cargo test --workspace` green.

## Where to Start

Good first contributions, roughly in order of self-containedness:

- **New dithering algorithms.** `crates/printa-ble-core/src/raster/dither.rs`
  has Floyd-Steinberg, Atkinson, and threshold. Sierra, Burkes, Stucki, or
  ordered/Bayer dithering would all fit the existing shape, and each is a
  contained change with an obvious preview-based test.
- **Markdown features.** `crates/printa-ble-core/src/raster/markdown.rs` is the
  biggest single file and the most extensible. Footnotes, definition lists, and
  column alignment in tables are all unimplemented, and footnotes in particular
  currently fail in a confusing way (a URL-shaped definition is silently eaten
  as a link reference definition). See [docs/MARKDOWN.md](docs/MARKDOWN.md) for
  what's supported today and its Gotchas section for the sharp edges.
- **Graphic fences.** `qr`, `barcode` and `wagara` show the shape: a pure
  renderer in `raster/`, a `Fence` variant, and an error path that prints text
  rather than panicking. `wagara` is the one to copy if your fence needs
  options as well as a payload.
- **Linux and Windows testing.** btleplug supports both (BlueZ and WinRT), and
  nothing in the codebase is knowingly macOS-specific outside the permission
  prompt. Nobody has tried. Reporting that it works — or exactly how it fails —
  is genuinely valuable. A full Linux contribution would look like: `cargo
  build` against BlueZ, `printable scan --all` seeing real advertisements, one
  hardware-validated print per protocol family, notes on permissions and the
  config path (the code already uses the platform config directory), and an
  `ubuntu-latest` job in `.github/workflows/ci.yml` so the BlueZ backend keeps
  compiling. Partial steps of that list are welcome as separate PRs.
- **Another printer model.** See below.

### Adding a New Printer Model

The sans-IO seam is what makes this tractable. A new model needs changes in
`printa-ble-core/src/protocol/` (packet format, auth, state machine) and
possibly `raster/` if the paper width differs from 384 px; it should need
little or nothing in the BLE transport.

1. Read [docs/PROTOCOL.md](docs/PROTOCOL.md) to understand how the LX-D02
   protocol is structured and where the model-specific parts live.
2. Capture traffic from the vendor app, or find an existing reverse-engineering
   effort — the three implementations credited in the README are how this one
   got started.
3. Write tests against captured byte sequences *first*. The protocol layer is
   entirely testable from recorded frames; you only need the hardware at the
   very end, to confirm the whole thing prints.
4. Keep the new protocol behind the same sans-IO boundary so it works from the
   CLI, the server, and the browser at once.

## Reporting Issues

**Bug reports** should include: OS and version, `rustc --version`, printer model
if hardware is involved, the command you ran (with `-vv` output if you can),
what you expected, and what happened. A `--preview` PNG is worth a lot for
rendering bugs.

**Feature requests** should include the use case, any proposed approach, and
what you considered instead.

Security vulnerabilities do **not** go in public issues — see
[SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions will be licensed under the
MIT License. Two embedded font families are under the SIL Open Font License:
JetBrains Mono (`crates/printa-ble-core/assets/OFL.txt`) and Noto Sans JP
(`crates/printa-ble-core/assets/OFL-NotoSansJP.txt`). If you add or replace a
bundled font, ship its license file alongside it and update both this file and
the README.
