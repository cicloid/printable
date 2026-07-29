# lxd2

A Rust CLI for printing to LX-D02 / LX-D2 Bluetooth thermal printers (the "FunnyPrint" app family) on macOS. These are 58 mm, 203 dpi, 384 px-wide printers made by Shenzhen Xiqi Technology.

## Status

All four phases of the [original design](docs/plans/2026-07-27-lxd2-design.md) are delivered: `scan`, `status`, and `print` (text, images, markdown, and web pages via `--url`) with PNG preview, QR codes via `qr`, multiple copies with `--copies`, a config file that remembers the last-connected printer, an HTTP print server with a phone-friendly web UI via `serve`, and a serverless Web Bluetooth page that prints straight from the browser.

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
lxd2 print --url https://example.com    # render a web page via headless Chrome
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
| `--url <URL>` | Web page to render (via headless Chrome) and print; conflicts with a text argument and `--file` |

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

## Server mode

```
lxd2 serve
```

starts an HTTP print server (REST API + web UI) on `0.0.0.0:8000`. `--port` and `--bind` change the listen address, and `--device` pins the printer just like the other commands. Open `http://<mac-ip>:8000` from any device on the LAN — the built-in web UI is phone-friendly and shows a live preview before printing.

### Endpoints

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/health` | — | `{"status":"ok","version":…,"url_printing":…}` |
| GET | `/status` | — | Battery, paper, density, charging, voltage as JSON |
| POST | `/preview/text` | JSON `{"content", "size"?}` | PNG |
| POST | `/preview/markdown` | JSON `{"content"}` | PNG |
| POST | `/preview/qr` | JSON `{"data", "caption"?}` | PNG |
| POST | `/preview/image` | multipart: `file`, `dither`? | PNG |
| POST | `/preview/url` | JSON `{"url"}` | PNG |
| POST | `/print/text` | JSON `{"content", "size"?, …}` | `{"printed_lines", "copies"}` |
| POST | `/print/markdown` | JSON `{"content", …}` | `{"printed_lines", "copies"}` |
| POST | `/print/qr` | JSON `{"data", "caption"?, …}` | `{"printed_lines", "copies"}` |
| POST | `/print/image` | multipart: `file`, `dither`?, `density`?, `feed`?, `copies`? | `{"printed_lines", "copies"}` |
| POST | `/print/url` | JSON `{"url", …}` | `{"printed_lines", "copies"}` |

Every `/print/*` JSON body also accepts the optional print options `density` (1-7, default 3), `feed` (blank lines after printing, default 40), and `copies` (1-20, default 1). `dither` takes `floyd`, `atkinson`, `threshold`, or `none`, like the CLI.

### Examples

```sh
# Print markdown
curl -X POST http://localhost:8000/print/markdown \
  -H 'Content-Type: application/json' \
  -d '{"content": "# Shopping\n\n- milk\n- eggs", "copies": 2}'

# Print a QR code with a caption
curl -X POST http://localhost:8000/print/qr \
  -H 'Content-Type: application/json' \
  -d '{"data": "https://example.com", "caption": "scan me"}'

# Preview an image (returns a PNG, no printing)
curl -X POST http://localhost:8000/preview/image \
  -F file=@photo.png -F dither=atkinson -o preview.png
```

### Errors

Errors come back as `{"error": "message"}` JSON: 400 for invalid input, 409 when the printer is out of paper, 502 when a URL failed to render, and 503 when no printer is found. While a print job is running, `/status` returns `{"printing": true}` immediately instead of waiting for the printer; concurrent print requests queue.

### Trust model

There is no authentication — anyone on the LAN can print. Worst case that's wasted paper, but if you'd rather keep the API to yourself, bind it to the Mac only with `--bind 127.0.0.1`.

### URL printing

`/preview/url` and `/print/url` (like the CLI's `--url`) render pages through headless Google Chrome, which must be installed. Only `http://` and `https://` URLs are accepted. Build with `--no-default-features` to disable URL printing entirely; the routes then return 404 and `/health` reports `"url_printing": false`.

## Web app (Web Bluetooth)

A static web page that prints directly from the browser — no server, no install. Rendering (text, markdown, QR, images) runs entirely client-side via `lxd2-core` compiled to WebAssembly, and the page talks to the printer over Web Bluetooth.

### Browser support

Chrome and Edge on desktop and Android support Web Bluetooth; Safari (including all of iOS) and Firefox do not. The preview works everywhere — only printing needs Web Bluetooth.

### Build

```
rustup target add wasm32-unknown-unknown
scripts/build-web.sh    # needs wasm-pack
```

This builds `crates/lxd2-web` with [wasm-pack](https://rustwasm.github.io/wasm-pack/) and puts the WASM package in `web/pkg/`.

### Run locally

```
python3 -m http.server 8080 -d web
```

then open http://localhost:8080. Web Bluetooth requires a secure context — `localhost` or `https` — so plain `http` on a LAN address will not work.

### Hosting

The `web/` directory (with `pkg/` built) is fully static — host it anywhere that serves over `https`, such as GitHub Pages.

## Configuration

After each successful connection, lxd2 saves the printer's identifier and name to a config file — `~/Library/Application Support/lxd2/config.toml` on macOS (the platform config directory elsewhere) — and prefers that printer on later runs. If it is not seen, lxd2 falls back to a device advertising the saved name, or failing that any `LX*` device. `--device` overrides the saved printer, and the newly connected device is saved in its place. Delete the file to forget the saved printer.

## Architecture

The workspace has three crates. `lxd2-core` is a sans-IO crate containing the protocol (packet building, CRC, auth, print-job state machine) and the rendering pipeline (text layout, dithering, raster chunking, PNG preview); it has no Bluetooth dependencies. `lxd2` is the CLI, which drives `lxd2-core` over BLE using [btleplug](https://github.com/deviceplug/btleplug). `lxd2-web` compiles `lxd2-core` to WebAssembly for the static Web Bluetooth page in `web/`.

## Credits

This project builds on protocol work from three reference implementations:

- [rusq/thermoprint](https://github.com/rusq/thermoprint) — Go; protocol reverse-engineering and the print-job state machine
- [ValdikSS/printer-driver-funnyprint](https://github.com/ValdikSS/printer-driver-funnyprint) — Python/CUPS; the de-facto protocol documentation
- [paradon/lxprint](https://github.com/paradon/lxprint) — TypeScript/Web Bluetooth; correct auth implementation (and the [joaquimorg/lxprint](https://github.com/joaquimorg/lxprint) Vue fork)

## License

MIT — see [LICENSE](LICENSE). The embedded JetBrains Mono font is licensed under the SIL Open Font License; see [crates/lxd2-core/assets/OFL.txt](crates/lxd2-core/assets/OFL.txt).
