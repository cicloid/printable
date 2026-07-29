# lxd2

A Rust CLI for printing to LX-D02 / LX-D2 Bluetooth thermal printers (the "FunnyPrint" app family) on macOS. These are 58 mm, 203 dpi, 384 px-wide printers made by Shenzhen Xiqi Technology.

## Status

Phases 1 and 2 are done: `scan`, `status`, and `print` (text, images, and markdown) with PNG preview, QR codes via `qr`, multiple copies with `--copies`, and a config file that remembers the last-connected printer. A print server (phase 3) and a Web Bluetooth version (phase 4) are upcoming — see [docs/plans/](docs/plans/).

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
lxd2 print -f notes.md                # markdown: headings, lists, bold/italic, code, rules
lxd2 qr "https://example.com" --caption "scan me"
lxd2 print "hello" --copies 3
```

### Options

| Option | Description |
|---|---|
| `--device <NAME>` | Device name or identifier substring (default: first device named `LX*`) |
| `--density <1-7>` | Print density (default: 3) |
| `--feed <LINES>` | Blank feed lines after printing (default: 40) |
| `--dither <floyd\|atkinson\|threshold>` | Dithering for images (default: floyd; `none` is an alias for `threshold`) |
| `--size <PX>` | Font size for text in pixels (default: 24) |
| `--preview <PATH>` | Render to a PNG file instead of printing |
| `--copies <1-20>` | Number of copies to print (default: 1) |

### QR codes

`lxd2 qr <DATA>` prints a QR code encoding a URL or arbitrary text, centered at the printer's full width. `--caption <TEXT>` prints a caption below the code. The `--device`, `--density`, `--feed`, `--preview`, and `--copies` options work the same as for `print`.

### Markdown

`lxd2 print -f notes.md` (or `.markdown`) renders the file as formatted output rather than plain text. Supported: headings (H1-H3 at decreasing sizes; deeper levels render like H3), **bold** and *italic*, bulleted and ordered lists (including nesting), inline code and code blocks, blockquotes, and horizontal rules. Links render as their text. Tables and images are not supported.

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

## Configuration

After each successful connection, lxd2 saves the printer's identifier and name to a config file — `~/Library/Application Support/lxd2/config.toml` on macOS (the platform config directory elsewhere) — and prefers that printer on later runs. If it is not seen, lxd2 falls back to a device advertising the saved name, or failing that any `LX*` device. `--device` overrides the saved printer, and the newly connected device is saved in its place. Delete the file to forget the saved printer.

## Architecture

The workspace has two crates. `lxd2-core` is a sans-IO crate containing the protocol (packet building, CRC, auth, print-job state machine) and the rendering pipeline (text layout, dithering, raster chunking, PNG preview); it has no Bluetooth dependencies, keeping it WASM-ready for a future Web Bluetooth version. `lxd2` is the CLI, which drives `lxd2-core` over BLE using [btleplug](https://github.com/deviceplug/btleplug).

## Credits

This project builds on protocol work from three reference implementations:

- [rusq/thermoprint](https://github.com/rusq/thermoprint) — Go; protocol reverse-engineering and the print-job state machine
- [ValdikSS/printer-driver-funnyprint](https://github.com/ValdikSS/printer-driver-funnyprint) — Python/CUPS; the de-facto protocol documentation
- [paradon/lxprint](https://github.com/paradon/lxprint) — TypeScript/Web Bluetooth; correct auth implementation (and the [joaquimorg/lxprint](https://github.com/joaquimorg/lxprint) Vue fork)

## License

MIT — see [LICENSE](LICENSE). The embedded JetBrains Mono font is licensed under the SIL Open Font License; see [crates/lxd2-core/assets/OFL.txt](crates/lxd2-core/assets/OFL.txt).
