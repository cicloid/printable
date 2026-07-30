# CLAUDE.md

Instructions for Claude Code working in this repository.

**Read [AGENTS.md](AGENTS.md) first** — it has the project layout, build and test
commands, style conventions, and PR expectations. This file covers only what an
AI agent specifically needs to know beyond that, and it is mostly a list of ways
to do real damage here.

## Never print without asking

**Printing consumes physical paper on a real device in someone's home.** There is
no undo.

- Default to `--preview <PATH>` for anything that renders. Every command that
  prints has a preview mode, and the HTTP server mirrors it at `/preview/*`.
- Run a real print **only** when the user explicitly asks for one in the current
  request. "Test that it works" means `--preview`. So does "try it".
- Never run `printable print`, `printable qr`, or a `/print/*` request
  speculatively, in a loop, or to check your own work.
- `--copies` multiplies the damage. Do not raise it on your own initiative.
- Verify rendering by opening the preview PNG and looking at it, or by asserting
  on its dimensions and pixels in a test.

## The sans-IO invariant

`printa-ble-core` **must not** depend on `tokio`, `btleplug`, `reqwest`, `rand`,
`std::fs`, or anything that performs I/O or reads a clock.

This is a build constraint, not a preference. `printa-ble-web` compiles the same
crate to `wasm32-unknown-unknown`, a target with no sockets, no filesystem, and
no threads. **Adding an I/O dependency to core breaks the WASM build and takes
the entire browser app down with it** — and you will not notice from a native
`cargo build`, because native compiles fine.

If you touch `printa-ble-core`'s dependencies or add an `use` that reaches
outward, verify with:

```bash
cargo clippy -p printa-ble-web --target wasm32-unknown-unknown
```

The two established patterns for anything core seems to "need":

- **Inputs arrive as parameters.** Auth randomness is passed in (the CLI supplies
  it from `rand`, the browser from `crypto.getRandomValues`). Markdown image
  bytes are fetched by the CLI, server, or browser and handed to the renderer
  already decoded.
- **Outputs leave as values.** `JobStats` in `protocol/job.rs` is the reference
  example: core counts `packets_sent` / `retransmits` / `holds` / `cooldowns`
  and returns plain data; the transport layer decides whether to log it.
  **Observability data leaves core as values, never as log calls.** There is no
  `tracing` in core, and there should not be.

When a change wants an I/O dependency in core, the answer is to move that work
up into `printa-ble`, not to relax the rule. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Don't casually refactor the protocol layer

`crates/printa-ble-core/src/protocol/` was reverse-engineered and validated
against a physical LX-D02. Packet framing, CRC, the auth handshake, magic byte
values, packet ordering, and inter-raster delays are all load-bearing, and the
tests encode observed hardware behavior rather than a specification.

A change that reads as an obvious cleanup — collapsing a delay, reordering
writes, "simplifying" a constant — can produce garbled output, a hung job, or a
meter of wasted paper, and **no test will catch it**, because the tests cannot
reach the hardware.

Do not restructure this layer as a side effect of some other task. If the user
asks for a protocol change, make the minimal change, keep the byte-level tests
passing, and note in the commit or PR that it is not hardware-validated unless
the user says they validated it.

## Verify protocol bytes, don't assume

Never infer a protocol constant, packet layout, CRC polynomial, or command byte
from memory, from analogy with ESC/POS, or from what "should" be there. This is
a proprietary, reverse-engineered protocol and its details are frequently
unintuitive.

Before writing or changing any protocol byte:

1. Read the actual value in `crates/printa-ble-core/src/protocol/`.
2. Cross-check against [docs/PROTOCOL.md](docs/PROTOCOL.md).
3. If neither is conclusive, say so rather than guessing. "The docs don't
   specify this and I couldn't confirm it in the source" is a correct answer.

The same rule applies to the rendering and transport limits (384 px width, 32
images, 5 MB and 15 s per fetch, 30 s budget, 20 MiB body, 28-character
barcodes, `feed` ≤ 2000 and `size` ≤ 128 on the server but unbounded on the
CLI, and the five timeouts in `ble.rs`): grep for the constant rather than
recalling it. Several of these were changed after the README was first written,
and the CLI and server deliberately differ on some of them.

## The server's `allow_local = false`

The HTTP server resolves markdown images with `allow_local = false`; the CLI
uses `true`. This asymmetry is a **tested security boundary**, not duplication to
be tidied up — it is what stops a LAN caller reading `/etc/passwd` off the host.
Do not unify the two call sites, and do not add a server flag to enable local
paths. See [SECURITY.md](SECURITY.md).

## Debugging

The CLI has a global `-v` flag: `-v` for flow-control events, `-vv` for parsed
frames, `-vvv` for raw hex **plus dependency logs**. `RUST_LOG` overrides it.
Prefer this over adding `println!` calls, and never add logging to
`printa-ble-core`.

The default filter is `printable=warn` — crate-scoped, with no bare level
directive, so dependencies are disabled outright rather than merely quiet. That
is load-bearing and pinned by a test: chromiumoxide reports harmless websocket
deserialization failures at ERROR, and a global floor made a successful
`print --url` look like it had failed. Do not "simplify" `cli::log_filter` into
a plain level ladder.

## Plan documents are historical

`docs/plans/` contains the original design and phase-implementation documents.
They are **historical records, not current specification** — they were written
before the project was renamed, so they are named `lxd2-*` and refer to the
project as `lxd2` throughout (the repo directory is still `lxd2`; the project is
`printa-ble` and the binary is `printable`).

Read them for context on why something was built a certain way. Do not treat
them as describing current behavior, and do not update them to match new work —
the live documentation is `README.md` and `docs/{PROTOCOL,CLI,API,MARKDOWN,ARCHITECTURE}.md`.

## Commits

- Short imperative subject, bullets below if needed.
- **No AI attribution.** No `Co-Authored-By: Claude`, no "Generated with Claude
  Code", no robot emoji. The commit history should read as the maintainer's.
- Do not commit unless the user asks you to.
- `web/pkg/` and `target/` are gitignored build output — never add them.

## Quick verification loop

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo clippy -p printa-ble-web --target wasm32-unknown-unknown   # if core changed
cargo test --workspace
```

Everything should pass, with exactly 1 ignored (it needs Chrome and network).
Do not quote a test count from memory — [CONTRIBUTING.md](CONTRIBUTING.md#testing)
holds the only one in the repository, and even that is a snapshot. If your
change touched rendering, generate a `--preview` PNG and actually look at it
before claiming the change works.
