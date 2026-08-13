# Repository Guidelines

## Project Structure & Module Organization

- `crates/printa-ble-core`: sans-IO library — protocol and rendering, no I/O deps.
  - `src/protocol/`: packets, CRC, auth, print-job state machine (`job.rs`, `JobStats`).
  - `src/raster/`: text, markdown, dither, QR, barcode, wagara, bitmap, PNG preview, URF (Apple Raster) decoding.
  - `assets/`: embedded font faces and their SIL OFL licences.
- `crates/printa-ble`: CLI + HTTP server; builds the `printable` binary.
  - `ble.rs` (btleplug transport), `server.rs` (+ `server/ui.html`), `cli.rs`, `md_images.rs`, `chrome.rs`, `config.rs`, `print_service.rs`, `ipp_command.rs` (AirPrint job hook).
- `crates/printa-ble-web`: WASM bindings for the static Web Bluetooth page.
- `web/`: static page (`index.html`, `app.js`); `web/pkg/` is generated, gitignored.
- `docs/`: `PROTOCOL.md`, `CLI.md`, `API.md`, `MARKDOWN.md`, `ARCHITECTURE.md`, `AIRPRINT.md`; `docs/plans/` is historical. The `README.md` Documentation section indexes them all.

## Build, Test, and Development Commands

- Build: `cargo build`; release: `cargo build --release`.
- Run CLI: `cargo run -p printa-ble -- <cmd>`; install: `cargo install --path crates/printa-ble`.
- Preview without printing: `cargo run -p printa-ble -- print "hi" --preview out.png`; add `-m` to render the input as markdown.
- Server: `cargo run -p printa-ble -- serve --bind 127.0.0.1`.
- Web app: `rustup target add wasm32-unknown-unknown && scripts/build-web.sh` then `python3 -m http.server 8080 -d web`.
- Test: `cargo test --workspace`; Chrome test: `cargo test -p printa-ble -- --ignored`.
- Lint/format: `cargo fmt --all` and `cargo clippy --workspace --all-targets`, plus `cargo clippy -p printa-ble-web --target wasm32-unknown-unknown`.
- Debug: global `-v` / `-vv` / `-vvv` (flow control / parsed frames / raw hex plus dependency logs); the default filter is crate-scoped (`printable=warn`) and `RUST_LOG` overrides it.

## Coding Style & Naming Conventions

- Rust 2021, stable toolchain, no MSRV pin; default rustfmt, no overrides.
- `snake_case` functions/modules, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants; branches `feature/...`, `fix/...`.
- Named constants over magic numbers, especially protocol bytes and limits.
- Doc comments explain _why_ (protocol quirks, constraints), not _what_.
- Both clippy invocations must be warning-free before review.

## Testing Guidelines

- Unit tests live in `mod tests` next to the code; integration tests in `crates/printa-ble/tests/`.
- The whole suite runs with no printer, no adapter, no network; exactly one `#[ignore]`d test needs Chrome. `CONTRIBUTING.md` holds the only test count in the repo — count them rather than quoting one from elsewhere.
- Test-first. Rendering changes need a pixel/dimension assertion or a `--preview` check; protocol changes need byte-level assertions against known-good frames.
- Cover failure paths: bad QR payloads, oversized barcodes, undecodable images must not panic or abort the document.
- Server routes are tested in-process via `tower::ServiceExt`; no live socket or printer required.

## Commit & Pull Request Guidelines

- Commits: short imperative subject plus bullet summary (e.g., `Add markdown table rendering support`).
- No AI attribution or co-author trailers in commit messages.
- PRs: describe behavior change, tests run, and doc updates; attach a `--preview` PNG for rendering changes.
- State explicitly whether you validated on hardware, and with which printer.
- `cargo fmt --all`, both clippy passes, and `cargo test --workspace` green before requesting review.

## Security & Configuration Tips

- `printa-ble-core` must never depend on `tokio`, `btleplug`, `reqwest`, or `rand` — it compiles to WASM, where those break the build. Randomness and fetched bytes come in as parameters; `JobStats` shows observability leaving as values, not log calls.
- `crates/printa-ble-core/src/protocol/` is hardware-validated against a real LX-D02. Do not refactor it casually; see `docs/PROTOCOL.md`.
- The server resolves markdown images with `allow_local = false` — a tested boundary preventing local file reads. The CLI uses `true` by design. Do not "unify" them.
- `serve` has no auth and binds `0.0.0.0` by default; it fetches caller-supplied URLs (SSRF). Trusted LAN only; see `SECURITY.md`.
- Config is written to `~/Library/Application Support/printa-ble/config.toml` (platform config dir elsewhere); it holds only a device name and identifier, no secrets.
- Printing consumes physical paper — default to `--preview` unless a real print is explicitly requested.
