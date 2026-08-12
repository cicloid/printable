# printa-ble API Reference

`printable serve` exposes a REST API and a web UI for a supported BLE thermal printer — an LX-D02 / LX-D2, or an X6 / X6h (**not yet hardware-validated**) — over the LAN. Preview endpoints render to PNG without touching the printer; print endpoints run the same rendering through the BLE print pipeline.

The printer model is server configuration, not request data: `printable serve --model x6` restricts the server to that family, exactly like `--device` pins a device, and no HTTP route takes a model. Without the flag the model is detected from the device name (see [CLI.md](CLI.md#printer-models)).

## Base URL

```
http://localhost:8000
```

`--port` and `--bind` change it. The default bind is `0.0.0.0`, so the server is reachable from every interface; `--bind 127.0.0.1` keeps it on the machine. See [CLI.md](CLI.md#serve) for the command's flags.

## Authentication

None. Anyone who can reach the port can print, read printer status, and — through the markdown and URL endpoints — make the server issue outbound HTTP requests on their behalf. Run it only on a network you trust. [SECURITY.md](../SECURITY.md) has the full trust model.

## Content types

| Direction | Type | Used by |
|---|---|---|
| Request | `application/json` | Everything except the image endpoints |
| Request | `multipart/form-data` | `POST /preview/image`, `POST /print/image` |
| Response | `image/png` | All `/preview/*` endpoints |
| Response | `application/json` | All `/print/*` endpoints, `/health`, `/status`, and every error the handlers raise |
| Response | `text/html` | `GET /` |

Request bodies are capped at **20 MiB** (20 971 520 bytes) on every route. On the JSON routes a larger body gets `413 Payload Too Large` with a plain-text message; on the multipart routes it surfaces as a `400` in the JSON envelope instead (see [Errors](#the-plain-text-exception)).

## Endpoints

| Method | Path | Body | Success |
|---|---|---|---|
| GET | `/` | — | `200` HTML web UI |
| GET | `/health` | — | `200` JSON |
| GET | `/status` | — | `200` JSON |
| POST | `/preview/text` | JSON | `200` PNG |
| POST | `/preview/markdown` | JSON | `200` PNG |
| POST | `/preview/qr` | JSON | `200` PNG |
| POST | `/preview/image` | multipart | `200` PNG |
| POST | `/preview/url` | JSON | `200` PNG |
| POST | `/print/text` | JSON | `200` JSON |
| POST | `/print/markdown` | JSON | `200` JSON |
| POST | `/print/qr` | JSON | `200` JSON |
| POST | `/print/image` | multipart | `200` JSON |
| POST | `/print/url` | JSON | `200` JSON |

The two `/…/url` routes exist only in builds with the `url` feature (on by default). Without it they return `404` and `/health` reports `"url_printing": false`.

---

## Shared print options

Every `/print/*` endpoint accepts three job options. In JSON bodies they are flattened into the top level alongside the content fields, not nested under an object. In multipart bodies they are ordinary text fields.

| Field | Type | Default | Valid range |
|---|---|---|---|
| `density` | integer | `3` | 1–7 |
| `feed` | integer | `40` | 0–2000 |
| `copies` | integer | `1` | 1–20 |

```json
{ "content": "Hello", "density": 5, "feed": 60, "copies": 2 }
```

Options are validated before rendering and before the printer is touched, so an out-of-range value costs nothing:

```json
{ "error": "density must be between 1 and 7" }
```

`feed` appends blank rows after the content so the paper clears the tear bar; on an LX-D02 they ride along as blank raster rows and count toward `printed_lines`, while an X6 receives the feed as a printer command instead and does **not** count it. `density` is honored by the LX-D02 and accepted-but-ignored on an X6, whose quality commands are not implemented. `copies` runs one full print job per copy over a single BLE connection.

Unknown fields are ignored. A value of the wrong JSON type, or an integer outside its numeric type (`"density": 300` for a `u8`), is rejected by the deserializer with `422` and a plain-text body — see [Errors](#errors).

**`dither` is not a shared option.** It exists only on the two multipart image endpoints (`/preview/image`, `/print/image`), where it is a text field. No JSON body has a `dither` field: text, markdown and QR have nothing to dither, and the URL routes are fixed to Floyd–Steinberg (the CLI's `--url` does honour `--dither` — that asymmetry is real).

These bounds are the server's, not the renderer's, and the CLI does not share all of them:

| Field | Server | CLI equivalent |
|---|---|---|
| `density` | 1–7 | `--density`, 1–7 |
| `copies` | 1–20 | `--copies`, 1–20 |
| `feed` | 0–2000 | `--feed`, ≥ 0 with **no upper bound** |
| `size` | > 0, ≤ 128 | `--size`, > 0 and finite, **no upper bound** |
| Request body | 20 MiB | — (the CLI reads a local file of any size) |

The server caps what the CLI leaves open because it takes input from anyone who can reach the port; a local user asking for a 100 000-line feed is only wasting their own paper.

---

## Health and status

### GET /health

Liveness plus build capabilities. Never touches Bluetooth.

```bash
curl http://localhost:8000/health
```

```json
{
  "status": "ok",
  "version": "0.1.0",
  "url_printing": true
}
```

| Field | Type | Notes |
|---|---|---|
| `status` | string | Always `"ok"` |
| `version` | string | Crate version of the running binary |
| `url_printing` | boolean | Whether `/preview/url` and `/print/url` exist |

### GET /status

Connect over BLE, wait for one status frame, disconnect. **LX-D02 only**: the X6 sends no status frames at all, so against one the connect itself succeeds but the route answers `503` immediately with `{"error": "status notifications are not supported on this printer model"}` instead of sitting out the 5-second wait.

```bash
curl http://localhost:8000/status
```

```json
{
  "battery_pct": 78,
  "no_paper": false,
  "charging": false,
  "charged": false,
  "overheat": false,
  "low_battery": false,
  "density": 3,
  "voltage_mv": 4021
}
```

| Field | Type | Notes |
|---|---|---|
| `battery_pct` | integer | 0–100 |
| `no_paper` | boolean | `true` when the paper sensor is clear |
| `charging` | boolean | On USB power, still charging |
| `charged` | boolean | On USB power, full |
| `overheat` | boolean | Print head over temperature |
| `low_battery` | boolean | |
| `density` | integer | Present only when the printer reports it |
| `voltage_mv` | integer | Millivolts; present only when the printer reports it |

While a print job is running, `/status` returns immediately with a single field instead of queueing behind the job or opening a second BLE connection:

```json
{ "printing": true }
```

| Status | Cause |
|---|---|
| `200` | Status read, or a print is in progress |
| `503` | No printer found within the 10 s scan, no status frame within 5 s, or the printer is an X6 (no status support) |

The device is saved to the config file on a successful connection, exactly as the CLI does.

---

## Preview endpoints

Previews return a PNG of what would print, at 384 px wide. They never connect to the printer and never take the print lock, so they work while a job is running. They do **not** accept `density`, `feed`, or `copies` — the returned image is content only, with no feed rows.

### POST /preview/text

| Field | Type | Default | Validation |
|---|---|---|---|
| `content` | string | required | Must not be empty or whitespace only |
| `size` | number | `24` | Finite, > 0, ≤ 128 (pixels) |

```bash
curl -X POST http://localhost:8000/preview/text \
  -H 'Content-Type: application/json' \
  -d '{"content": "Hello world", "size": 32}' \
  -o preview.png
```

Text wraps greedily at 384 px, line height is 1.3 × `size`, `\n` forces a break, tabs expand to four spaces.

### POST /preview/markdown

| Field | Type | Default | Validation |
|---|---|---|---|
| `content` | string | required | Must not be empty or whitespace only |

```bash
curl -X POST http://localhost:8000/preview/markdown \
  -H 'Content-Type: application/json' \
  -d '{"content": "# Receipt\n\n| item | qty |\n|---|---|\n| beans | 250 |"}' \
  -o preview.png
```

Supports headings, emphasis, strikethrough, lists, task lists, tables, code blocks, blockquotes, rules, `qr`, `barcode` and `wagara` fences, and images — the full dialect in [MARKDOWN.md](MARKDOWN.md). Image handling is a security boundary — see [Markdown images](#markdown-images).

### POST /preview/qr

| Field | Type | Default | Validation |
|---|---|---|---|
| `data` | string | required | Must fit some QR version |
| `caption` | string | none | Rendered below the code at 24 px |

```bash
curl -X POST http://localhost:8000/preview/qr \
  -H 'Content-Type: application/json' \
  -d '{"data": "https://example.com", "caption": "scan me"}' \
  -o qr.png
```

Version and error correction are chosen automatically. Data too large for any version returns `400 {"error": "data too long to fit in a QR code"}`.

### POST /preview/image

`multipart/form-data`.

| Field | Type | Default | Notes |
|---|---|---|---|
| `file` | file | required | PNG or JPEG bytes |
| `dither` | text | `floyd` | `floyd`, `atkinson`, `threshold`, or `none` (alias for `threshold`) |

```bash
curl -X POST http://localhost:8000/preview/image \
  -F file=@photo.png -F dither=atkinson \
  -o preview.png
```

The image is scaled to 384 px wide and reduced to 1 bit. Unknown fields are ignored; a missing `file` field is `400 {"error": "missing \`file\` field"}`.

### POST /preview/url

Requires the `url` feature.

| Field | Type | Default | Validation |
|---|---|---|---|
| `url` | string | required | Must start with `http://` or `https://` (case-insensitive) |

```bash
curl -X POST http://localhost:8000/preview/url \
  -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com"}' \
  -o page.png
```

The page renders in system headless Chrome at a 384 px viewport, settles for 500 ms, and is captured full-page, then dithered with Floyd–Steinberg. The dither mode is not configurable here.

| Status | Cause |
|---|---|
| `400` | Scheme is not `http`/`https` — checked before Chrome launches |
| `502` | Chrome could not launch, navigate, or capture |
| `500` | The screenshot could not be decoded |

---

## Print endpoints

Print endpoints render exactly like their preview counterparts, then take the print lock and drive the printer. They all answer with the same body:

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

| Field | Type | Notes |
|---|---|---|
| `printed_lines` | integer | (content rows + `feed`) × `copies` on an LX-D02. On an X6 it is content rows × `copies`: the feed is a command, not rows, and the protocol's blank lead row is not counted either |
| `copies` | integer | Echo of the requested copy count |
| `elapsed_ms` | integer | Wall clock from the start of the connect to the last copy finishing — connect, hello, auth, streaming and all |
| `packets_sent` | integer | Raster packets written, summed over every copy (on an X6: one scanline per row, plus one blank lead row per copy) |
| `holds` | integer | Times the printer paused the stream — `5A 08` on an LX-D02, a buffer-full notification on an X6 |
| `cooldowns` | integer | Times the printer asked for a thermal back-off (`5A 07`); always 0 on an X6, whose protocol has no such event |
| `retransmits` | integer | Times the printer asked for a resend from a given packet index (`5A 05`); always 0 on an X6 |

The last five are the same counters the server writes to its log, repeated here for clients that never see it. They are what distinguishes a slow print from a stuck one: an `elapsed_ms` far larger than `packets_sent` × 15 ms means the difference was spent paused, and `holds` and `cooldowns` say so. Zero across the board is a clean job. The counters come out of the sans-IO core as plain values (`JobStats`); the server only formats them.

### POST /print/text

| Field | Type | Default | Validation |
|---|---|---|---|
| `content` | string | required | Must not be empty or whitespace only |
| `size` | number | `24` | Finite, > 0, ≤ 128 |
| `density`, `feed`, `copies` | — | — | [Shared print options](#shared-print-options) |

```bash
curl -X POST http://localhost:8000/print/text \
  -H 'Content-Type: application/json' \
  -d '{"content": "Table 4 — order up", "size": 32, "density": 5}'
```

### POST /print/markdown

| Field | Type | Default | Validation |
|---|---|---|---|
| `content` | string | required | Must not be empty or whitespace only |
| `density`, `feed`, `copies` | — | — | [Shared print options](#shared-print-options) |

```bash
curl -X POST http://localhost:8000/print/markdown \
  -H 'Content-Type: application/json' \
  -d '{"content": "# Shopping\n\n- [ ] milk\n- [ ] eggs", "copies": 2}'
```

### POST /print/qr

| Field | Type | Default | Validation |
|---|---|---|---|
| `data` | string | required | Must fit some QR version |
| `caption` | string | none | |
| `density`, `feed`, `copies` | — | — | [Shared print options](#shared-print-options) |

```bash
curl -X POST http://localhost:8000/print/qr \
  -H 'Content-Type: application/json' \
  -d '{"data": "WIFI:T:WPA;S:MyNetwork;P:hunter2;;", "caption": "Guest Wi-Fi"}'
```

### POST /print/image

`multipart/form-data`. Field names are exact.

| Field | Type | Default | Notes |
|---|---|---|---|
| `file` | file | required | PNG or JPEG bytes |
| `dither` | text | `floyd` | `floyd`, `atkinson`, `threshold`, `none` |
| `density` | text | `3` | 1–7 |
| `feed` | text | `40` | 0–2000 |
| `copies` | text | `1` | 1–20 |

```bash
curl -X POST http://localhost:8000/print/image \
  -F file=@photo.jpg \
  -F dither=atkinson \
  -F density=5 \
  -F copies=2
```

An unparseable numeric field is `400 {"error": "invalid density \`x\`"}`. Unknown fields are ignored.

### POST /print/url

Requires the `url` feature.

| Field | Type | Default | Validation |
|---|---|---|---|
| `url` | string | required | `http://` or `https://` only |
| `density`, `feed`, `copies` | — | — | [Shared print options](#shared-print-options) |

```bash
curl -X POST http://localhost:8000/print/url \
  -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com", "density": 4}'
```

The scheme is checked before Chrome launches and before any BLE work, so a bad URL never reaches either.

---

## Concurrency

The server holds one printer, so it prints one job at a time.

- A mutex is held across the whole connect-print-disconnect flow of every `/print/*` request. Concurrent print requests **queue**; there is no queue timeout, so a second caller waits as long as the first job takes. Requests are served in arrival order at the lock.
- `/status` never queues. It try-locks, and on failure returns `{"printing": true}` with `200` immediately rather than waiting or opening a second BLE connection.
- `/preview/*`, `/health` and `/` never take the lock and stay responsive throughout a print.
- `--copies` is one connection and N jobs, all inside a single lock hold, so copies cannot be interleaved with another caller's job.

---

## Markdown images

`/preview/markdown` and `/print/markdown` resolve `![alt](dest)` references before rendering. What they will fetch is deliberately narrower than the CLI:

| Reference | Server behavior |
|---|---|
| `http://…`, `https://…` | Fetched, unless the server was started with `--no-remote-images` |
| `/etc/hosts`, `./logo.png`, any other path | **Never** read. Not opened, not stat-ed. |

Refusing local paths is a security boundary: without it anyone on the LAN could read files off the host by posting `![x](/etc/hosts)` and looking at the returned PNG. A posted document also has no directory of its own, so relative references have nothing to resolve against.

Resolution is bounded:

| Limit | Value |
|---|---|
| References resolved per document | 32 (the rest render as placeholders) |
| Total time for the whole pass | 30 s |
| Timeout per fetch | 15 s |
| Bytes per image | 5 MiB (`Content-Length` checked up front, body checked while streaming) |

Fetches run sequentially on purpose — resolving them concurrently would multiply the outbound traffic one request can trigger.

Nothing here can fail a request. An unreachable, oversized, or undecodable image logs a warning on the server's stderr and renders as an italic `[image: alt]` placeholder in its place.

`printable serve --no-remote-images` skips remote fetches entirely, leaving the server with no outbound request surface at all.

---

## Errors

Errors raised by the handlers use a JSON envelope:

```json
{ "error": "density must be between 1 and 7" }
```

| Status | Meaning | Examples |
|---|---|---|
| `400` | Invalid input | Out-of-range option, empty `content`, `size` over 128, unknown `dither`, missing `file` field, undecodable image, non-`http(s)` URL, QR data too long, job over 65 535 raster packets |
| `404` | No such route | Also the `url` routes in a build without the feature |
| `405` | Wrong method | e.g. `GET /print/text`. **Empty body** — no JSON, no text; the `allow` header carries the answer |
| `409` | Printer is out of paper | Detected before the job starts or from a mid-job status frame (LX-D02 only — the X6 has no paper signal) |
| `413` | Body over 20 MiB on a JSON route | Plain text, not JSON. The multipart routes report the same condition as `400` — see below |
| `415` | Wrong `Content-Type` | Plain text, not JSON |
| `422` | Body does not match the schema | Plain text, not JSON |
| `500` | Print failed, or an internal render failure | Auth rejected, BLE write failed, printer stopped responding |
| `502` | URL rendering failed | Chrome missing, page unreachable |
| `503` | No printer found, no printer that answered, or no status frame | Nothing matched within the 10 s scan; or a device was found but never answered the hello probe (`found <name> but it did not respond — is the printer powered on?`) |

### The plain-text exception

Rejections produced by axum's own body extraction — before any handler runs — are plain text, not the `{"error": …}` envelope. Clients must not assume every non-2xx response parses as JSON.

```console
$ curl -i -X POST localhost:8000/print/text -H 'Content-Type: application/json' -d '{bad'
HTTP/1.1 400 Bad Request
content-type: text/plain; charset=utf-8

Failed to parse the request body as JSON: key must be a string at line 1 column 2

$ curl -i -X POST localhost:8000/print/text -H 'Content-Type: application/json' -d '{}'
HTTP/1.1 422 Unprocessable Entity
content-type: text/plain; charset=utf-8

Failed to deserialize the JSON body into the target type: missing field `content` at line 1 column 2

$ curl -i -X POST localhost:8000/print/text -d 'hello'
HTTP/1.1 415 Unsupported Media Type
content-type: text/plain; charset=utf-8

Expected request with `Content-Type: application/json`
```

| Trigger | Status | Body |
|---|---|---|
| Malformed JSON | `400` | plain text |
| Missing required field, or a field of the wrong type | `422` | plain text |
| Missing or wrong `Content-Type` | `415` | plain text |
| Body over the 20 MiB limit, JSON route | `413` | plain text |
| Body over the 20 MiB limit, multipart route | `400` | **JSON** — see below |
| Wrong method for an existing route | `405` | **empty** |

Everything a handler rejects — every validation rule documented above — comes back as JSON.

Two rows in that table surprise people:

**An oversized multipart body is a `400`, not a `413`.** The body limit trips while the handler is streaming a field rather than before it runs, so the handler sees a field-read failure and reports it in the normal envelope:

```console
$ curl -X POST localhost:8000/preview/image -F file=@21mb.png
{"error":"failed to read file: Error parsing `multipart/form-data` request"}
```

The same 20 MiB ceiling applies either way; only the reporting differs. Since the image endpoints are the ones an oversized body actually reaches in practice, a client that treats `413` as "too big" and everything else as "malformed" will misreport the common case.

**A `405` has no body at all** — not JSON, not text. The `allow` header names the methods the route does accept.

---

## Logging

The server writes to **stderr** under the same `-v` ladder as the rest of the CLI (see [CLI.md](CLI.md#verbosity-and-logging)). With no flag it logs only its own warnings — a failed print job, an unreachable printer, and every `5xx` it returns — so a default-level server still records its own failures and nothing else.

`printable serve -v` adds the operational log:

| Event | Line |
|---|---|
| Every request | `POST /print/markdown -> 200 in 24544ms` — method, path, status, elapsed ms |
| Job start | `print job starting: markdown, 812 lines, density 3, feed 40, 1 copies` |
| Job finish | `print job done: markdown, 812 lines in 24310ms (812 packets, 3 holds, 41 cooldowns, 0 resends)` |
| Queueing | `printer is busy with another job; this request is queued`, then `printer free; queued for 18402ms` |
| Flow control | `printer paused the stream (print head too hot); waiting to resume…`, `printer is cooling down` |
| URL rendering | `rendering <url> with headless Chrome`, then `rendered <url> to 214 KiB in 2841ms` |

The request line alone cannot say whether `/print/markdown` was two lines or two thousand, which is why the job lines name the content kind and the size. The queue lines exist because a request stuck behind another job is indistinguishable from a hang at the client end. Chrome's timing is separate because it happens before the printer is ever touched and is often the slowest part of a URL print.

`-vv` adds parsed protocol frames and image-resolution timings; `-vvv` adds raw hex and dependency logs. `RUST_LOG` overrides the flag entirely.

---

## Web UI

`GET /` serves a single self-contained HTML page with no external assets. It is phone-friendly and drives the same endpoints: tabs for text, markdown, QR and image, a live preview through `/preview/*`, and a print button through `/print/*`. Open `http://<host>:8000` from any device on the LAN.
