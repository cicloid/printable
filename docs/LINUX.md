# Linux ARM64 builds and Raspberry Pi notes

## Build and test

On Debian/Ubuntu ARM64, install Rust stable plus the native dependencies:

```bash
sudo apt-get install build-essential pkg-config libdbus-1-dev libudev-dev
cargo test --locked -p printa-ble-core -p printa-ble --no-default-features
cargo build --locked --release -p printa-ble --no-default-features
```

The `Linux ARM64` workflow repeats those checks on a native Ubuntu 22.04 ARM64
runner for pull requests, pushes to `main`, and manual runs. It packages the
executable, source commit, runtime-library list and SHA-256 checksums as a build
artifact. This supplements the existing macOS workspace and WASM checks.
Release publishing is unchanged.

These builds target aarch64 Linux with glibc 2.35 or newer, including 64-bit
Raspberry Pi OS Bookworm/Trixie. They do not work on 32-bit ARM or musl systems.
The runtime system needs the libraries listed in `LIBRARIES.txt`, notably
`libdbus-1-3`. Optional URL/Chrome printing and CJK fonts are excluded from this
small-device build; the HTTP server and Markdown renderer are included.

Verify the archive checksum before extracting, then run `sha256sum -c SHA256SUMS`
inside the extracted package and inspect `COMMIT`. Artifacts expire after 30 days;
re-run the desired workflow if needed. There is no automated installation or
service restart in this workflow.

## Hardware report and limits

A user ran the printer service successfully on a Raspberry Pi Zero 2 W with
Debian GNU/Linux 13.7 (trixie), BlueZ 5.82, and an X6h. They verified repeated
Markdown printing, QR labels, feed, illustrated clues and a complete game on
2026-09-17. Their tested downstream build also contains separately proposed
Markdown layout, inline-PNG and X6 drain-delay improvements. This hardware report
must not be read as a test of this isolated CI-only branch on the printer.

The tested system used `ControllerMode = le`. Trying `dual` produced
`br-connection-profile-unavailable`; returning to LE restored operation. This is
an observation about that setup, not a requirement to disable Classic Bluetooth
on all Linux machines. Pairing/permissions and mixed-device setups need separate
validation. No Linux LX-D02, Windows, or other ARM board was tested in this report.

Linux device identifiers differ from Bluetooth addresses: on that Pi, the
explicit device filter used a path-shaped ID such as
`hci0/dev_AA_BB_CC_DD_EE_FF`, rather than a colon-separated MAC. Use `printable scan --all` to inspect the identifiers on
your machine. An HTTP health response alone does not prove a live printer link.
