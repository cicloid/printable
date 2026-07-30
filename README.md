# printa-ble

A Rust CLI for printing to LX-D02 / LX-D2 Bluetooth thermal printers (the "FunnyPrint" app family) on macOS. These are 58 mm, 203 dpi, 384 px-wide printers made by Shenzhen Xiqi Technology.

The name **printa-ble** derives from *printa* (the ancestor project) plus *BLE* (Bluetooth Low Energy, how it talks to the printer) — and reads as "printable". It currently supports the LX-D02 / LX-D2 family. The command itself is `printable`.

## Status

All four phases of the [original design](docs/plans/2026-07-27-lxd2-design.md) are delivered: `scan`, `status`, and `print` (text, images, markdown, and web pages via `--url`) with PNG preview, QR codes via `qr`, multiple copies with `--copies`, a config file that remembers the last-connected printer, an HTTP print server with a phone-friendly web UI via `serve`, and a serverless Web Bluetooth page that prints straight from the browser.

A [follow-up phase](docs/plans/2026-07-29-lxd2-phase5-implementation.md) extended the markdown renderer with tables, task-list checkboxes, strikethrough, embedded QR codes and barcodes, images, and a tear marker. Since then it has gained ten `wagara` pattern bands, Japanese text rendering via an embedded CJK fallback face, and `-m` on `print` for rendering markdown that did not arrive as a `.md` file — see [Markdown](#markdown).

## Documentation

This README is the tour. The reference documents go deeper:

| Document | What it covers |
|---|---|
| [docs/CLI.md](docs/CLI.md) | Every command, flag, exit code, failure message, and a recipe section |
| [docs/API.md](docs/API.md) | The HTTP server: endpoints, request and response shapes, limits, errors, concurrency |
| [docs/MARKDOWN.md](docs/MARKDOWN.md) | The markdown dialect — what renders, what doesn't, and the gotchas |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the three crates fit together, and why the core is sans-IO |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | The reverse-engineered LX-D02 wire protocol, byte by byte |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, the test workflow, and the architectural rules |
| [SECURITY.md](SECURITY.md) | Trust model and how to report a vulnerability |

`docs/plans/` holds the original design and phase plans. They are historical records written before the project was renamed — read them for context, not as current behaviour.

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
printable print -f notes.md                # markdown: headings, tables, task lists, QR/barcode/wagara fences, images
cat notes.md | printable print -m          # -m renders piped input as markdown
printable qr "https://example.com" --caption "scan me"
printable print "hello" --copies 3
printable print --url https://example.com    # render a web page via headless Chrome
```

### Options

Five options are shared by `print` and `qr`; the rest belong to `print` alone. `printable <COMMAND> --help` is authoritative, and [docs/CLI.md](docs/CLI.md) has the full reference.

Shared by `print` and `qr`:

| Option | Description |
|---|---|
| `--device <NAME>` | Device name or identifier substring (default: first device named `LX*`) |
| `--density <1-7>` | Print density (default: 3) |
| `--feed <LINES>` | Blank feed lines after printing (default: 40) |
| `--preview <PATH>` | Render to a PNG file instead of printing |
| `--copies <1-20>` | Number of copies to print (default: 1) |

`print` only:

| Option | Description |
|---|---|
| `-f, --file <PATH>` | File to print (`.png`/`.jpg`/`.jpeg`/`.txt`/`.md`/`.markdown`), or `-` to read stdin |
| `-m, --markdown` | Render the input as markdown rather than plain text (see [Markdown](#markdown)) |
| `--dither <floyd\|atkinson\|threshold>` | Dithering for images (default: floyd; `none` is an alias for `threshold`) |
| `--size <PX>` | Font size for text in pixels (default: 24) |
| `--url <URL>` | Web page to render (via headless Chrome) and print; conflicts with a text argument, `--file`, and `--markdown` |

`--dither` applies to a directly printed image (`-f photo.png`) **and** to `--url` renders. It does not apply to images embedded in a markdown document — those are always Floyd–Steinberg. `--size` applies to plain text only.

`-v`/`-vv`/`-vvv` is global and works on every command; see [Logging](#logging).

### QR codes

`printable qr <DATA>` prints a QR code encoding a URL or arbitrary text, centered at the printer's full width. `--caption <TEXT>` prints a caption below the code. `qr` takes only the five shared options above — there is no `--size`, no `--dither`, and no `--url`, because the version, error correction, and scale are all chosen automatically.

### Markdown

`printable print -f notes.md` (or `.markdown`) renders the file as formatted output rather than plain text. The same renderer backs the server's `/print/markdown` and the web app's Markdown tab.

Markdown is chosen by extension, so every other input is **plain text by default** — piping a document without saying so prints its literal source. `-m` / `--markdown` forces the markdown renderer for input that has no `.md` extension to give it away:

```sh
cat notes.md | printable print -m           # stdin
printable print -m "# Heading"              # a text argument
printable print -m -f - < notes.md          # `--file -` also means stdin
printable print -m -f notes.txt             # a .txt file
```

`-m` is redundant (and silently accepted) with `-f notes.md`, rejected for image files, and a usage error alongside `--url`.

Relative image references need a directory to resolve against, and which one depends on how the document arrived: a `--file` document anchors them to **its own directory**, while piped or argument markdown anchors them to the **current working directory** — what `![](logo.png)` means to someone running the command from their shell. The server never resolves local paths at all.

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

```wagara seigaiha
height: 40
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
| Emphasis | `**bold**`, `*italic*`, `~~strikethrough~~` (a 2 px line through the text) — see the note below |
| Lists | Bulleted and ordered, nested; `• ` / `N. ` prefixes |
| Task lists | `- [x]` / `- [ ]` render as ASCII `[x]` / `[ ]` markers (the font has no ballot-box glyphs) |
| Tables | Monospace text blocks; see below |
| Code | Inline code (a passthrough — the font is monospace already) and fenced/indented blocks, exact line breaks preserved |
| Blockquotes | Indented and italic |
| Rules | `---` renders a solid full-width bar |
| Tear marker | A thematic break written with interior spaces — `- - -` or `* * *` — renders a **dashed** line instead, marking where to tear the paper |
| `qr` fence | A fenced code block tagged `qr` — the body is encoded as a QR code |
| `barcode` fence | A fenced code block tagged `barcode` — the body is encoded as a Code128 barcode |
| `wagara` fence | A fenced code block tagged `wagara` — draws a traditional Japanese pattern band, see below |
| Images | `![alt](dest)` — resolved per surface, see below |

Links render as their text; raw HTML is skipped.

**Bold and italic do not compose.** A span gets exactly one font face, resolved in the order heading → bold → italic → regular. So `***x***` renders byte-identical to `**x**`, and `**bold**` inside a blockquote (which is italic) comes out plain bold, not bold-italic. Strikethrough is a separate flag and does compose with any face. Inline code takes the surrounding style, so `` `x` `` renders byte-identical to `x`.

**Japanese text prints.** There is no syntax and no flag: a Noto Sans JP face is embedded alongside JetBrains Mono and used per glyph for anything the Latin face lacks, so 日本語 in a heading, a list or a table cell renders instead of coming out as tofu boxes. It is not free — the face is ~4.5 MB, most of the binary and most of the web bundle — and it has real limits. [docs/MARKDOWN.md](docs/MARKDOWN.md#cjk-text) has the details and the `cjk` opt-out.

[docs/MARKDOWN.md](docs/MARKDOWN.md) is the full dialect reference, including the gotchas — footnotes, front matter, and rules inside list items all have surprising results.

#### Tables

Tables lay out as monospace text (the embedded font is monospace, so column math is exact): cell contents flatten to plain text (bold/italic inside a cell is dropped), columns are padded to their widest cell with two-space gutters, and a dashed separator row follows the header. Everything is left-aligned — markdown alignment markers (`:---:`) are ignored. A row of cells fits 32 display columns across the 384 px roll; a wider table shrinks its widest columns and truncates those cells with `…` rather than overflowing.

Widths count **display columns**, not characters: a full-width CJK character claims two, so a table mixing Japanese and ASCII rows still lines up, and truncation cuts between characters rather than through one. Columns will not shrink below 3 display columns, so **six columns is the practical ceiling**: seven or more need more than 32 even at that floor, and the rows word-wrap instead of staying aligned. Nothing is lost or clipped — every cell still prints — but the table reads as wrapped text rather than a grid. Split a wider table, or transpose it, if alignment matters.

#### QR and barcode fences

A fenced code block whose info string's first word is `qr`, `barcode`, or `wagara` (matched case-insensitively, so `QR` counts) renders as a graphic instead of code text. Every other info string — including none at all — still renders as plain code.

Barcodes are **Code128**, character set B: the payload must be printable ASCII (U+0020 space through U+007E tilde), which covers digits, both letter cases, and punctuation. Anything else (accents, emoji, tabs, newlines) is rejected. The maximum is **28 characters** — beyond that the bars cannot stay at least one pixel wide on 384 px paper. The payload is plain text: there are no escape characters, and the character-set prefix Code128 needs is added for you.

A payload the encoder rejects — too long for any QR version, non-ASCII in a barcode — prints its error message as code text instead. A bad code never panics and never costs you the rest of the document.

#### `wagara` fences

A fence tagged `wagara` draws a traditional Japanese pattern (和柄) as a full-width decorative band — a separator with more character than a rule. The pattern is named in the info string, or failing that on the body's first line:

````markdown
```wagara seigaiha
height: 72
scale: 2
```
````

Ten patterns are drawn:

| Pattern | Kanji | Motif | Aliases |
|---|---|---|---|
| `asanoha` | 麻の葉 | Hemp-leaf star lattice | |
| `ichimatsu` | 市松 | Checkerboard | |
| `kanoko` | 鹿の子 | Fawn spots — the ring-and-speck dapple a shibori tie-dye leaves | |
| `kikkou` | 亀甲 | Tortoise-shell hexagons | `kikko` |
| `sayagata` | 紗綾形 | Key fret — a linked lattice of 卍 forms | |
| `seigaiha` | 青海波 | Overlapping fans, "blue sea waves" | |
| `shippou` | 七宝 | Interlocking circles, "seven treasures" | `shippo` |
| `tatewaku` | 立涌 | Rising steam — paired curves swelling and narrowing | `tachiwaki` |
| `uroko` | 鱗 | Fish scales — solid triangles, alternating rows | |
| `yagasuri` | 矢絣 | Arrow fletching | `yabane` |

Names are matched case-insensitively. Every pattern tiles exactly across the 384 px roll, so a band runs edge to edge with no half-eaten motif.

| Option | Range | Default | Effect |
|---|---|---|---|
| `height` | 16–400 | 56 | Band height in pixels |
| `scale` | 1–4 | 1 | Motif size multiplier |

**Three of them are heavy.** `ichimatsu` and `uroko` are 50% solid ink by construction and `yagasuri` is close behind at 44%, against 14–37% for the line patterns — that is what the motifs are, not a bug. A thermal head lays down what it is told, so those three cost noticeably more heat, paper darkening and battery than the rest. Drop `--density` a step or two when you print them.

**`height` does more for some patterns than others.** `ichimatsu`, `tatewaku`, `uroko` and `yagasuri` divide the band into a whole number of vertical repeats, so the repeat *count* follows `height`: at the 56 px default `uroko` gets three rows of scales and `tatewaku` and `yagasuri` get two, which reads more like a crop than a pattern. At 100–120 px they get four to six and look markedly better. The lattice patterns (`asanoha`, `kikkou`, `shippou`, `sayagata`, `kanoko`) centre a row on the band instead and change much less.

````markdown
```wagara uroko
height: 120
```
````

An unknown pattern name or a malformed option line prints its error message as code text, exactly like a bad QR or barcode payload — the rest of the document still prints.

**Known wart:** `scale` is quantised to the divisors of 384 that keep the band tiling, so for coarse patterns `scale: 3` and `scale: 4` can land on the same motif count and render identically. A large `scale` at the default `height` also shows only a horizontal slice of one motif. Raise `height` alongside `scale`, and check with `--preview`.

#### Images

Image references are resolved by whichever surface is rendering, then handed to the renderer; the rendering core itself never performs I/O. What each surface will fetch differs on purpose:

| Surface | Local paths | `http(s)` URLs |
|---|---|---|
| CLI (`printable print -f notes.md`) | Yes — relative to the document's own directory, or to the working directory when the markdown was piped or passed as an argument | Yes |
| Server (`/print/markdown`, `/preview/markdown`) | **Never** | Yes, unless `--no-remote-images` |
| Web app (Markdown tab) | No — a browser cannot read them | Yes, subject to CORS |

The server refusing local paths is a security boundary, not an omission: without it, anyone on the LAN could read files off the machine running the server by asking for `![x](/etc/hosts)`. Images are PNG or JPEG, scaled to the 384 px roll and dithered with Floyd–Steinberg (`--dither` applies to `-f photo.png` and `--url`, never to images inside a document); remote fetches are capped at 5 MB and 15 s each (CLI and server; the web app hands fetching to the browser and inherits its limits).

Resolution is bounded per document: at most **32 images**, and — on the CLI and server — **30 seconds** for the whole pass. (The web app applies the same 32-image cap but no overall deadline; each fetch is bounded only by the browser.) References past those limits, and any that fail to fetch or decode, are simply left unresolved.

An unresolved reference renders as an italic **`[image: alt text]`** placeholder (falling back to the destination when there is no alt text), so a broken image never fails a print. *This is a behavior change:* markdown images used to render nothing at all.

### macOS Bluetooth permission

The first run triggers a Bluetooth permission prompt for your terminal app. If you deny it, enable it later in System Settings → Privacy & Security → Bluetooth.

### Exit codes

| Code | Meaning |
|---|---|
| 1 | General error (bad input, unreadable file, oversized job) |
| 2 | No usable printer — none found, or one found that never answered |
| 3 | Out of paper |
| 4 | Print failed |

Invalid command-line usage also exits 2 (clap's convention).

### Logging

Every command takes a global `-v`, and `RUST_LOG` overrides it entirely.

| Level | What it is for |
|---|---|
| *(none)* | This crate's warnings only. The default filter is `printable=warn` — crate-scoped, so no dependency can log on your behalf |
| `-v` | Flow control and progress: connection, thermal pauses and resumes, retransmit requests, the server's request log and job summaries |
| `-vv` | Parsed protocol frames, device resolution, image resolution timings |
| `-vvv` | Raw hex on the wire, **plus dependency logs** — btleplug, chromiumoxide, the lot |

The crate-scoped default is deliberate: chromiumoxide reports websocket frames it fails to deserialize at ERROR, and recent Chrome sends several per screenshot, so a global floor made a perfectly successful `print --url` print two red lines about a connection error. Those messages are harmless; `-vvv` is the rung where you ask for them back.

Logs go to stderr, never stdout — scripts read the preview path and the scan table off stdout.

`printable serve` logs one line per request (method, path, status, elapsed ms) at `-v`, plus a line when a job starts, one when it finishes with its counters, one when a request has to queue behind another job and how long it waited, and Chrome's render timing for URL routes. Server errors (5xx) log at warn, so even a default-level server records its own failures.

## Server mode

```
printable serve
```

starts an HTTP print server (REST API + web UI) on `0.0.0.0:8000`. `--port` and `--bind` change the listen address, `--device` pins the printer just like the other commands, and `--no-remote-images` stops the server fetching http(s) images referenced by markdown. Open `http://<mac-ip>:8000` from any device on the LAN — the built-in web UI is phone-friendly and shows a live preview before printing.

### Endpoints

| Method | Path | Body | Result |
|---|---|---|---|
| GET | `/` | — | The web UI (a single self-contained HTML page) |
| GET | `/health` | — | `{"status":"ok","version":…,"url_printing":…}` |
| GET | `/status` | — | Battery, paper, density, charging, voltage as JSON |
| POST | `/preview/text` | JSON `{"content", "size"?}` | PNG |
| POST | `/preview/markdown` | JSON `{"content"}` | PNG |
| POST | `/preview/qr` | JSON `{"data", "caption"?}` | PNG |
| POST | `/preview/image` | multipart: `file`, `dither`? | PNG |
| POST | `/preview/url` | JSON `{"url"}` | PNG |
| POST | `/print/text` | JSON `{"content", "size"?, …}` | Print report (below) |
| POST | `/print/markdown` | JSON `{"content", …}` | Print report |
| POST | `/print/qr` | JSON `{"data", "caption"?, …}` | Print report |
| POST | `/print/image` | multipart: `file`, `dither`?, `density`?, `feed`?, `copies`? | Print report |
| POST | `/print/url` | JSON `{"url", …}` | Print report |

Every `/print/*` endpoint also accepts the optional print options `density` (1-7, default 3), `feed` (blank lines after printing, 0-2000, default 40), and `copies` (1-20, default 1) — flattened into the JSON body, or as text fields in the multipart body. `dither` is **not** one of them: it exists only on the two multipart image endpoints, where it takes `floyd`, `atkinson`, `threshold`, or `none`, like the CLI.

A successful print answers with the same counters the server logs, so a client that never sees the log can still explain a slow job:

```json
{
  "printed_lines": 812,
  "copies": 2,
  "elapsed_ms": 24310,
  "packets_sent": 812,
  "holds": 3,
  "cooldowns": 41,
  "retransmits": 0
}
```

#### Limits the CLI does not have

The server validates what the CLI leaves open, because it accepts input from anyone on the LAN:

| Limit | Server | CLI |
|---|---|---|
| Request body | 20 MiB | — |
| `feed` | 0–2000 | ≥ 0, no upper bound |
| `size` | > 0 and ≤ 128 px | > 0, finite, no upper bound |
| `density` | 1–7 | 1–7 |
| `copies` | 1–20 | 1–20 |

The two surfaces genuinely differ here: `printable print --feed 100000` is accepted and prints a very long blank tail, while `{"feed": 100000}` is a `400`.

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

Errors raised by the handlers come back as `{"error": "message"}` JSON:

| Status | Meaning |
|---|---|
| 400 | Invalid input: out-of-range option, empty `content`, unknown `dither`, undecodable image, non-`http(s)` URL, QR data too long, job over 65 535 raster packets |
| 404 | No such route (including the `url` routes in a build without the feature) |
| 405 | Wrong method for the route (empty body; the `allow` header has the answer) |
| 409 | Printer is out of paper |
| 413 | Request body over 20 MiB on a JSON route — the multipart routes report the same ceiling as a 400 |
| 415 | Missing or wrong `Content-Type` |
| 422 | Body does not match the schema (missing or mistyped field) |
| 500 | Print failed, or an internal render failure |
| 502 | A URL failed to render |
| 503 | No printer found, or the printer never answered |

**Not every non-2xx response is JSON.** Rejections produced by axum's own body extraction — malformed JSON, a missing field, the wrong `Content-Type`, an oversized body — happen before any handler runs and come back as **plain text**, not the `{"error": …}` envelope. That covers 413, 415, 422, and a malformed-JSON 400. Clients must not assume every error parses as JSON. [docs/API.md](docs/API.md#errors) has the exact bodies.

An oversized job is one place the two surfaces disagree on purpose: `JobError::TooLarge` is a `400` on the server (the caller sent something invalid) but exit code **1** on the CLI (a general error), not exit 2.

While a print job is running, `/status` returns `{"printing": true}` immediately instead of waiting for the printer; concurrent print requests queue.

### Trust model

There is no authentication — anyone on the LAN can print. Worst case that's usually wasted paper, but the server also makes outbound requests on a caller's behalf, so run it only on a network you trust:

- **Markdown images.** `/print/markdown` and `/preview/markdown` fetch any `http(s)` URL the body references, so a caller can make the server issue requests to hosts it can reach and you cannot — internal addresses, cloud metadata endpoints, and the like (an SSRF surface), and `/preview/markdown` hands back what was fetched as a 1-bit dithered image. `--no-remote-images` removes the surface entirely. Local file paths are always refused, with or without that flag.
- **URL printing.** `/print/url` and `/preview/url` render a caller-supplied page through headless Chrome, which is the same exposure plus a browser engine. `--no-default-features` removes those routes at build time.

If you'd rather keep the API to yourself, bind it to the Mac only with `--bind 127.0.0.1`.

### URL printing

`/preview/url` and `/print/url` (like the CLI's `--url`) render pages through headless Google Chrome, which must be installed. Only `http://` and `https://` URLs are accepted. Build with `--no-default-features` to disable URL printing entirely; the routes then return 404 and `/health` reports `"url_printing": false`.

One asymmetry with the CLI: `printable print --url … --dither atkinson` honours the dither mode, but the server's URL routes always use Floyd–Steinberg and take no `dither` field.

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

The `wagara` bands are drawn from the geometry of motifs that are centuries old and long out of copyright, with one exception: **`sayagata`** is a specific historical linkage of 卍 forms rather than a lattice with a closed-form rule, so its cell is transcribed from [`Sayagata (line).svg`](https://commons.wikimedia.org/wiki/File:Sayagata_(line).svg) on Wikimedia Commons — a public-domain (CC0) tile by Fred the Oyster. The transcription lives in the `SAYAGATA_U` / `SAYAGATA_V` tables in `crates/printa-ble-core/src/raster/wagara.rs`.

## License

MIT — see [LICENSE](LICENSE). Two font families are embedded, both under the SIL
Open Font License: JetBrains Mono
([OFL.txt](crates/printa-ble-core/assets/OFL.txt)) for Latin text, and Noto Sans
JP ([OFL-NotoSansJP.txt](crates/printa-ble-core/assets/OFL-NotoSansJP.txt)) for
Japanese.
