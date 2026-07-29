# printa-ble

A Rust CLI for printing to LX-D02 / LX-D2 Bluetooth thermal printers (the "FunnyPrint" app family) on macOS. These are 58 mm, 203 dpi, 384 px-wide printers made by Shenzhen Xiqi Technology.

The name **printa-ble** derives from *printa* (the ancestor project) plus *BLE* (Bluetooth Low Energy, how it talks to the printer) — and reads as "printable". It currently supports the LX-D02 / LX-D2 family. The command itself is `printable`.

## Status

All four phases of the [original design](docs/plans/2026-07-27-lxd2-design.md) are delivered: `scan`, `status`, and `print` (text, images, markdown, and web pages via `--url`) with PNG preview, QR codes via `qr`, multiple copies with `--copies`, a config file that remembers the last-connected printer, an HTTP print server with a phone-friendly web UI via `serve`, and a serverless Web Bluetooth page that prints straight from the browser.

A [follow-up phase](docs/plans/2026-07-29-lxd2-phase5-implementation.md) extended the markdown renderer with tables, task-list checkboxes, strikethrough, embedded QR codes and barcodes, images, and a tear marker — see [Markdown](#markdown).

## Install

```
cargo install --path crates/printa-ble
```

or build from source:

```
cargo build --release
```

## Usage

```
printable scan
printable status
echo "hello" | printable print
printable print "hello world"
printable print -f photo.png --dither floyd
printable print -f notes.txt --size 28
printable print "test" --preview out.png   # render without printing
printable print -f notes.md                # markdown: headings, tables, task lists, QR/barcode fences, images
printable qr "https://example.com" --caption "scan me"
printable print "hello" --copies 3
printable print --url https://example.com    # render a web page via headless Chrome
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

`printable qr <DATA>` prints a QR code encoding a URL or arbitrary text, centered at the printer's full width. `--caption <TEXT>` prints a caption below the code. The `--device`, `--density`, `--feed`, `--preview`, and `--copies` options work the same as for `print`.

### Markdown

`printable print -f notes.md` (or `.markdown`) renders the file as formatted output rather than plain text. The same renderer backs the server's `/print/markdown` and the web app's Markdown tab.

````markdown
# Receipt

**Bold**, *italic*, ~~struck through~~.

- [x] beans ground
- [ ] water boiled

| item  | qty |
|-------|-----|
| beans | 250 |
| filter | 1   |

```qr
https://example.com/order/42
```

```barcode
ORDER-42
```

![logo](logo.png)

---

Thanks! Tear here:

- - -
````

#### Supported

| Feature | Notes |
|---|---|
| Headings | H1-H3 at decreasing sizes; deeper levels render like H3 |
| Emphasis | `**bold**`, `*italic*`, `~~strikethrough~~` (a 2 px line through the text); they compose |
| Lists | Bulleted and ordered, nested; `• ` / `N. ` prefixes |
| Task lists | `- [x]` / `- [ ]` render as ASCII `[x]` / `[ ]` markers (the font has no ballot-box glyphs) |
| Tables | Monospace text blocks; see below |
| Code | Inline code and fenced/indented blocks, exact line breaks preserved |
| Blockquotes | Indented and italic |
| Rules | `---` renders a solid full-width bar |
| Tear marker | A thematic break written with interior spaces — `- - -` or `* * *` — renders a **dashed** line instead, marking where to tear the paper |
| `qr` fence | A fenced code block tagged `qr` — the body is encoded as a QR code |
| `barcode` fence | A fenced code block tagged `barcode` — the body is encoded as a Code128 barcode |
| Images | `![alt](dest)` — resolved per surface, see below |

Links render as their text; raw HTML is skipped.

#### Tables

Tables lay out as monospace text (the embedded font is monospace, so column math is exact): cell contents flatten to plain text (bold/italic inside a cell is dropped), columns are padded to their widest cell with two-space gutters, and a dashed separator row follows the header. Everything is left-aligned — markdown alignment markers (`:---:`) are ignored. A row of cells fits 32 characters across the 384 px roll; a wider table shrinks its widest columns and truncates those cells with `…` rather than overflowing.

Columns will not shrink below 3 characters, so **six columns is the practical ceiling**: seven or more need more than 32 characters even at that floor, and the rows word-wrap instead of staying aligned. Nothing is lost or clipped — every cell still prints — but the table reads as wrapped text rather than a grid. Split a wider table, or transpose it, if alignment matters.

#### QR and barcode fences

A fenced code block whose info string's first word is `qr` or `barcode` (matched case-insensitively, so `QR` counts) renders as a graphic instead of code text. Every other info string — including none at all — still renders as plain code.

Barcodes are **Code128**, character set B: the payload must be printable ASCII (U+0020 space through U+007E tilde), which covers digits, both letter cases, and punctuation. Anything else (accents, emoji, tabs, newlines) is rejected. The maximum is **28 characters** — beyond that the bars cannot stay at least one pixel wide on 384 px paper. The payload is plain text: there are no escape characters, and the character-set prefix Code128 needs is added for you.

A payload the encoder rejects — too long for any QR version, non-ASCII in a barcode — prints its error message as code text instead. A bad code never panics and never costs you the rest of the document.

#### Images

Image references are resolved by whichever surface is rendering, then handed to the renderer; the rendering core itself never performs I/O. What each surface will fetch differs on purpose:

| Surface | Local paths | `http(s)` URLs |
|---|---|---|
| CLI (`printable print -f notes.md`) | Yes — relative to the `.md` file's directory | Yes |
| Server (`/print/markdown`, `/preview/markdown`) | **Never** | Yes, unless `--no-remote-images` |
| Web app (Markdown tab) | No — a browser cannot read them | Yes, subject to CORS |

The server refusing local paths is a security boundary, not an omission: without it, anyone on the LAN could read files off the machine running the server by asking for `![x](/etc/hosts)`. Images are PNG or JPEG, scaled to the 384 px roll and dithered with Floyd-Steinberg (`--dither` applies to `-f photo.png`, not to images inside a document); remote fetches are capped at 5 MB and 15 s each (CLI and server; the web app hands fetching to the browser and inherits its limits).

Resolution is bounded per document: at most **32 images**, and — on the CLI and server — **30 seconds** for the whole pass. (The web app applies the same 32-image cap but no overall deadline; each fetch is bounded only by the browser.) References past those limits, and any that fail to fetch or decode, are simply left unresolved.

An unresolved reference renders as an italic **`[image: alt text]`** placeholder (falling back to the destination when there is no alt text), so a broken image never fails a print. *This is a behavior change:* markdown images used to render nothing at all.

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
printable serve
```

starts an HTTP print server (REST API + web UI) on `0.0.0.0:8000`. `--port` and `--bind` change the listen address, `--device` pins the printer just like the other commands, and `--no-remote-images` stops the server fetching http(s) images referenced by markdown. Open `http://<mac-ip>:8000` from any device on the LAN — the built-in web UI is phone-friendly and shows a live preview before printing.

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

There is no authentication — anyone on the LAN can print. Worst case that's usually wasted paper, but the server also makes outbound requests on a caller's behalf, so run it only on a network you trust:

- **Markdown images.** `/print/markdown` and `/preview/markdown` fetch any `http(s)` URL the body references, so a caller can make the server issue requests to hosts it can reach and you cannot — internal addresses, cloud metadata endpoints, and the like (an SSRF surface), and `/preview/markdown` hands back what was fetched as a 1-bit dithered image. `--no-remote-images` removes the surface entirely. Local file paths are always refused, with or without that flag.
- **URL printing.** `/print/url` and `/preview/url` render a caller-supplied page through headless Chrome, which is the same exposure plus a browser engine. `--no-default-features` removes those routes at build time.

If you'd rather keep the API to yourself, bind it to the Mac only with `--bind 127.0.0.1`.

### URL printing

`/preview/url` and `/print/url` (like the CLI's `--url`) render pages through headless Google Chrome, which must be installed. Only `http://` and `https://` URLs are accepted. Build with `--no-default-features` to disable URL printing entirely; the routes then return 404 and `/health` reports `"url_printing": false`.

## Web app (Web Bluetooth)

A static web page that prints directly from the browser — no server, no install. Rendering (text, markdown, QR, images) runs entirely client-side via `printa-ble-core` compiled to WebAssembly, and the page talks to the printer over Web Bluetooth.

Markdown images are fetched by the browser, so only `http(s)` URLs work and only when the host allows cross-origin reads — a server without CORS headers is unreachable from the page. Anything that cannot be fetched renders as an `[image: alt]` placeholder and the page reports how many were skipped.

### Browser support

Chrome and Edge on desktop and Android support Web Bluetooth; Safari (including all of iOS) and Firefox do not. The preview works everywhere — only printing needs Web Bluetooth.

### Build

```
rustup target add wasm32-unknown-unknown
scripts/build-web.sh    # needs wasm-pack
```

This builds `crates/printa-ble-web` with [wasm-pack](https://rustwasm.github.io/wasm-pack/) and puts the WASM package in `web/pkg/`.

### Run locally

```
python3 -m http.server 8080 -d web
```

then open http://localhost:8080. Web Bluetooth requires a secure context — `localhost` or `https` — so plain `http` on a LAN address will not work.

### Hosting

The `web/` directory (with `pkg/` built) is fully static — host it anywhere that serves over `https`, such as GitHub Pages.

## Configuration

After each successful connection, printa-ble saves the printer's identifier and name to a config file — `~/Library/Application Support/printa-ble/config.toml` on macOS (the platform config directory elsewhere) — and prefers that printer on later runs. If it is not seen, printa-ble falls back to a device advertising the saved name, or failing that any `LX*` device. `--device` overrides the saved printer, and the newly connected device is saved in its place. Delete the file to forget the saved printer.

## Architecture

The workspace has three crates. `printa-ble-core` is a sans-IO crate containing the protocol (packet building, CRC, auth, print-job state machine) and the rendering pipeline (text layout, dithering, raster chunking, PNG preview); it has no Bluetooth dependencies. `printa-ble` is the CLI (installed as the `printable` command), which drives `printa-ble-core` over BLE using [btleplug](https://github.com/deviceplug/btleplug). `printa-ble-web` compiles `printa-ble-core` to WebAssembly for the static Web Bluetooth page in `web/`.

## Credits

This project builds on protocol work from three reference implementations:

- [rusq/thermoprint](https://github.com/rusq/thermoprint) — Go; protocol reverse-engineering and the print-job state machine
- [ValdikSS/printer-driver-funnyprint](https://github.com/ValdikSS/printer-driver-funnyprint) — Python/CUPS; the de-facto protocol documentation
- [paradon/lxprint](https://github.com/paradon/lxprint) — TypeScript/Web Bluetooth; correct auth implementation (and the [joaquimorg/lxprint](https://github.com/joaquimorg/lxprint) Vue fork)

## License

MIT — see [LICENSE](LICENSE). The embedded JetBrains Mono font is licensed under the SIL Open Font License; see [crates/printa-ble-core/assets/OFL.txt](crates/printa-ble-core/assets/OFL.txt).
