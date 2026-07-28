# lxd2

A Rust CLI for printing to LX-D02 / LX-D2 Bluetooth thermal printers (the "FunnyPrint" app family) on macOS. These are 58 mm, 203 dpi, 384 px-wide printers made by Shenzhen Xiqi Technology.

## Status

Phase 1: `scan`, `status`, and `print` (text and images) with PNG preview. Markdown rendering, QR codes, a print server, and a Web Bluetooth version are planned — see [docs/plans/](docs/plans/).

## Install

```
cargo install --path crates/lxd2
```

or build from source:

```
cargo build --release
```

## Usage

```
lxd2 scan
lxd2 status
echo "hello" | lxd2 print
lxd2 print "hello world"
lxd2 print -f photo.png --dither floyd
lxd2 print -f notes.txt --size 28
lxd2 print "test" --preview out.png   # render without printing
```

### Options

| Option | Description |
|---|---|
| `--device <NAME>` | Device name or identifier substring (default: first device named `LX*`) |
| `--density <1-7>` | Print density (default: 3) |
| `--feed <LINES>` | Blank feed lines after printing (default: 40) |
| `--dither <floyd\|threshold>` | Dithering for images (default: floyd) |
| `--size <PX>` | Font size for text in pixels (default: 24) |
| `--preview <PATH>` | Render to a PNG file instead of printing |

### macOS Bluetooth permission

The first run triggers a Bluetooth permission prompt for your terminal app. If you deny it, enable it later in System Settings → Privacy & Security → Bluetooth.

### Exit codes

| Code | Meaning |
|---|---|
| 1 | General error |
| 2 | No printer found |
| 3 | Out of paper |
| 4 | Print failed |

Invalid command-line usage also exits 2 (clap's convention).

## Architecture

The workspace has two crates. `lxd2-core` is a sans-IO crate containing the protocol (packet building, CRC, auth, print-job state machine) and the rendering pipeline (text layout, dithering, raster chunking, PNG preview); it has no Bluetooth dependencies, keeping it WASM-ready for a future Web Bluetooth version. `lxd2` is the CLI, which drives `lxd2-core` over BLE using [btleplug](https://github.com/deviceplug/btleplug).

## Credits

This project builds on protocol work from three reference implementations:

- [rusq/thermoprint](https://github.com/rusq/thermoprint) — Go; protocol reverse-engineering and the print-job state machine
- [ValdikSS/printer-driver-funnyprint](https://github.com/ValdikSS/printer-driver-funnyprint) — Python/CUPS; the de-facto protocol documentation
- [paradon/lxprint](https://github.com/paradon/lxprint) — TypeScript/Web Bluetooth; correct auth implementation (and the [joaquimorg/lxprint](https://github.com/joaquimorg/lxprint) Vue fork)

## License

MIT — see [LICENSE](LICENSE). The embedded JetBrains Mono font is licensed under the SIL Open Font License; see [crates/lxd2-core/assets/OFL.txt](crates/lxd2-core/assets/OFL.txt).
