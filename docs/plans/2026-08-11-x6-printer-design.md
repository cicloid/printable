# X6 printer support — design

**Date:** 2026-08-11
**Status:** Validated with maintainer; not yet implemented.

## Goal

Add support for the X6 (a.k.a. X6h) Bluetooth portable thermal printer across
every surface: CLI, HTTP server, AirPrint bridge, and the Web Bluetooth page.
The first version prints 1bpp dithered output only; the X6's 4bpp grayscale
mode is a later phase.

The maintainer has the printer in hand and performs all hardware validation.
Until a real print succeeds, every commit and PR states "not hardware-validated."

## Protocol sources

The X6 belongs to the documented "cat printer" family. Implementation follows
these references, never memory:

- <https://parzivail.github.io/ble-thermal-printer/> — packet framing, commands,
  raster encoding, flow control for the X6h specifically.
- <https://github.com/nazarovmi/tinyprint-x6h> — Python implementation;
  confirms the `X6h-` name prefix and the 16-level grayscale mode.
- <https://github.com/NaitLee/kitty-printer> — Web Bluetooth precedent for the
  same family.

Key facts: BLE print service `0xAE30` with write characteristic `0xAE01` and
notify characteristic `0xAE02`; frames are `51 78` magic, command ID, direction
byte, LE u16 payload length, payload, CRC8 (polynomial `0x07`, payload only),
`FF` trailer; 384-px printhead (48-byte 1bpp scanlines, leftmost pixel in the
least-significant bit); no auth handshake; flow control via a status
notification whose payload signals ready or buffer-full.

## Architecture

A new sans-IO module `crates/printa-ble-core/src/protocol_x6/` sits beside the
untouched, hardware-validated `protocol/` directory:

- `packets.rs` — cat-protocol framing with a named constant for every command
  byte, each verified against the references.
- `crc.rs` — CRC8/0x07. The LX-D02's CRC stays separate; the algorithms differ.
- `job.rs` — `X6PrintJob`, a state machine with the same public shape as the
  existing `PrintJob`: `new(bitmap, options) → next_action() →
on_notification()`. It returns `Action` values, counts the same `JobStats`,
  and has no auth phase. The X6 packetizer owns the LSB-leftmost bit packing,
  which differs from the LX-D02's.

A `PrinterModel` enum (`LxD02`, `X6`) in core carries per-model facts as
values: service and characteristic UUIDs, device-name prefix (`LX` vs `X6h-`),
and which job machine to construct. Transports match on it. Rendering is
untouched: both printheads are 384 px wide, so the existing text, markdown, QR,
dither, and preview pipeline feeds both.

The sans-IO invariant holds: no new dependencies in core, inputs as parameters,
outputs as values.

## Transport and discovery (CLI + server)

`ble.rs` becomes model-aware rather than gaining a parallel transport:

- **Discovery:** the scan matcher accepts both families — `LX*` names are
  LX-D02, `X6h-*` names (case-insensitive) are X6. `printable devices` lists
  both, tagged with model.
- **Targeting:** the `Filter` / `SavedId` / fallback ladder stays. The saved
  device in config gains a `model` field. A new `--model` flag (and config key)
  forces the choice; name-based auto-detection is the default.
- **Connection:** after connecting, the transport selects UUIDs from
  `PrinterModel` and drives whichever job machine core hands it. The five
  existing `ble.rs` timeouts apply to the X6 initially; if hardware shows it
  needs different pacing, the values move into `PrinterModel`.
- **Flow control:** the same drive loop — write while `next_action()` says to,
  feed notify-characteristic notifications into `on_notification()`, pause on
  buffer-full, resume on ready.

`print_service.rs` flows the model through unchanged, so the server's
`/print/*` routes and the AirPrint `ipp-command` hook inherit X6 support once
the service layer is model-aware. The URF-decode → bitmap → job path is
identical for both printers.

## Web Bluetooth page

- `printa-ble-web` gains a wrapper for `X6PrintJob` beside the existing job
  wrapper, with the same JS drive-loop contract. `PrinterModel` is exposed to
  JS so `app.js` asks core for UUIDs instead of hardcoding more constants.
- `requestDevice` uses one filter per family, with both services
  (`0xffe6`, `0xae30`) in `optionalServices`. After connection, `app.js`
  detects which service the device exposes and constructs the matching
  wrapper — service probing is the model detection; the browser never parses
  names.
- The LX-D02 path keeps passing `crypto.getRandomValues` output in for auth;
  the X6 job simply never emits an auth action.
- 1bpp only, matching CLI and server.

## Error handling

The X6 job maps failures into the existing vocabulary (`NoPaper`,
`PrinterNotResponding`, `PrintFailure`) so every surface reports them with the
messages users already see. Unknown status bytes are logged at `-vv` as parsed
frames but are not fatal — the family has undocumented variants. A stall with
no ready notification inside the existing notification timeout ends the job
with `PrinterNotResponding`.

## Testing

All sans-IO; no hardware in the suite:

- **Packet tests:** known payload → exact expected frame bytes, with CRC8
  vectors checked by hand against the reference tables.
- **Job tests:** scripted notification sequences drive `next_action()` through
  connect → print → done, buffer-full pause/resume, and stall → error,
  mirroring the existing `job.rs` test style.
- **Bit-order test:** a known asymmetric 384-px scanline → exact 48-byte
  packing, pinning the LSB-leftmost rule.
- **Matcher tests:** `X6h-Foo` matches as X6, `LX-D02` matches as before,
  plus ambiguity and `--model` override cases.

First hardware print: short text, no `--copies`.

## Documentation

`PROTOCOL.md` gains an X6 section citing the sources above. `CLI.md`,
`API.md`, and the README note the new model and the `--model` flag.

## Later phases (out of scope here)

- 4bpp / 16-level grayscale mode, everywhere at once.
- LZO-compressed scanlines (needs a pure-Rust LZO crate vetted for the sans-IO
  rule).
- Per-model quality, energy, and feed-speed commands.
