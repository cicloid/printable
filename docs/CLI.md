# printable CLI Reference

`printable` prints to two families of BLE thermal printer, both with 384 px-wide print heads: the LX-D02 / LX-D2 (58 mm paper, 203 dpi, hardware-validated) and the X6 / X6h "cat printer" family (**new, and not yet hardware-validated** — see [Printer models](#printer-models)).

```
printable <COMMAND> [OPTIONS]
```

| Command | Purpose |
|---|---|
| [`scan`](#scan) | List nearby supported printers |
| [`status`](#status) | Show battery, paper, density (LX-D02 only) |
| [`print`](#print) | Print text, a file, or a web page |
| [`qr`](#qr) | Print a QR code |
| [`serve`](#serve) | Run the HTTP print server |

Global flags: `-h, --help` (per command too), `-V, --version`, and `-v` for [verbosity](#verbosity-and-logging).

---

## Global behavior

### Printer models

Two printer families are supported, sharing the rendering pipeline (both have 384 px print heads) but speaking completely different wire protocols:

| Family | `--model` value | Advertised name | Status |
|---|---|---|---|
| LX-D02 / LX-D2 | `lx-d02` | starts with `LX` | Hardware-validated |
| X6 / X6h "cat printer" | `x6` | starts with `X6h-` or `x6h-` | **Not yet hardware-validated** |

`X6H-` (capital H) is a different model and is deliberately not matched.

Every command that talks to the printer takes `--model <lx-d02|x6>`. The value is case-insensitive (`--model X6` works); anything else is a usage error naming the two choices. Without the flag the model is detected from the advertised device name, and a successful connection remembers it in the config file, so later runs reconnect with the right protocol automatically.

The flag is a **restriction**, not just a hint: while `--model` is set, a device of the other family — or of no recognizable family — never matches, even under a `--device` filter that would otherwise hit it. Without the flag, reconnecting to the saved device is restricted to the saved model: the point of the saved device is "the same printer as last time", and that includes what kind of printer it was. A `--device` filter alone may still match a device whose name no model claims; such a device is driven as an LX-D02, which is what this tool did before models existed. That LX-D02 is an assumption, not a detection, so it is not saved: the device is remembered with no model, and later flagless runs match it by its saved id and name rather than being restricted to a model its name can never satisfy.

What the X6 does differently:

- **`--density` maps to feed speed (primary) and printhead energy.** The same 1–7 knob drives the X6's `0xBD` SetSpeed command as a speed divisor, `divisor = 8 + 4 × (density − 1)`: density 1 = 8, 3 (the default) = 16, 7 = 32 — kitty-printer's quick/fast/normal presets. On the validated hardware, feed speed is the dominant darkness control (slower prints darker); the `0xAF` SetEnergy / `0xBE` ApplyEnergy pair, driven from the same knob as `energy = 12000 + 6000 × (density − 1)` (density 1 = 12000, 3 = 24000, 7 = 48000 — kitty-printer's low/medium/high "strength" presets), mainly affects banding. [PROTOCOL.md](PROTOCOL.md) §11 has the wire details and provenance.
- **`--feed` is a printer command,** not blank raster lines. The feed does not count toward `Printed <N> lines.`, and a value beyond 65 535 saturates at that maximum. (`--preview` renders before any model is known, so preview output always shows the feed as blank rows.)
- **No status.** The X6 reports no paper, battery, or density. [`status`](#status) fails immediately against one, the pre-print paper check is skipped, and running out of paper mid-job cannot be detected.
- **No liveness probe.** The hello handshake of [Connecting means the printer answered](#connecting-means-the-printer-answered) is LX-only; on an X6, "connected" proves only that the notification subscription is up. On macOS a switched-off X6 can therefore appear to connect (CoreBluetooth answers from its cached GATT database), and the failure surfaces as a print job that stalls in silence — ended by the 10 s notification timeout — rather than as a connect error.

[docs/PROTOCOL.md](PROTOCOL.md) §11 documents the X6 wire protocol and the sources it was reconstructed from.

### Verbosity and logging

`-v` is global: it parses before or after the subcommand, repeats for more detail, and writes to **stderr** only. `RUST_LOG` overrides it entirely when set.

| Flag | Filter | What it is for |
|---|---|---|
| *(none)* | `printable=warn` | This crate's warnings and nothing else |
| `-v` | `printable=info` | Flow control and progress — connection, thermal holds and resumes, retransmit requests, per-copy progress, the server's request log and job summaries |
| `-vv` | `printable=debug` | Parsed protocol frames, device resolution, notification decoding, image-resolution timings |
| `-vvv` | `debug,printable=trace` | Raw hex on the wire, **plus dependency logs** (btleplug, chromiumoxide, reqwest) |

The default filter names this crate and nothing else, which is load-bearing rather than tidy: chromiumoxide logs the websocket frames it fails to deserialize at ERROR, and recent Chrome sends several per screenshot, so a bare `warn` floor made a perfectly successful `print --url` emit two red lines about a connection error. `-vvv` is the rung where you deliberately ask for that noise back — it is the one to reach for when the fault might be in btleplug or Chrome rather than here.

```sh
printable -vv print "test" --preview /tmp/out.png
printable print -v -f long.md                       # watch thermal pauses live
RUST_LOG=printable=trace,btleplug=debug printable status
```

Nothing is logged from `printa-ble-core`: it is sans-IO and has no logger. What it observes leaves as values (`JobStats`) and the transport decides how to report it.

### Device resolution

Every command that talks to the printer resolves a device the same way. `scan` is the exception — it lists everything it sees.

| Rank | Source | Match | When it wins |
|---|---|---|---|
| 1 | `--device <STR>` | Advertised name **or** platform id contains `<STR>` | Immediately, first match |
| 2 | Saved device id (config file) | Exact platform id | Immediately |
| 2a | Saved device *name* | Advertised name equals the saved name | Only at the scan deadline, preferred over 2b |
| 2b | Any supported printer | Advertised name identifies a supported model | Only at the scan deadline |
| 3 | No flag, no saved device | Advertised name identifies a supported model | Immediately, first match |

When a model restriction is in effect — an explicit `--model`, or the saved device's remembered model on a reconnect (see [Printer models](#printer-models)) — every rank matches only devices of that model.

The scan runs up to **10 seconds**, polling every 300 ms. An exact match short-circuits it; the ranked fallbacks are used only if no exact match appears before the deadline. If nothing matches at all, the command fails with `no supported printer found. Is the printer on and in range?` and exit code 2.

After every successful connection the device's id, name, and model are written to the config file, so the next run reconnects to the same printer without a flag. `--device` overrides the saved printer *and* replaces it.

### Connecting means the printer answered

Finding a device is not the same as finding a *live* printer. On macOS, CoreBluetooth caches the GATT database of any peripheral it has paired with before, so connecting and discovering the characteristics both succeed against a printer that is switched off. A connection is therefore only reported once the printer has answered a `5A 01` hello frame of its own accord — the first thing in the flow that only the hardware itself can produce.

This guarantee is **LX-D02 only**. The X6 protocol has no known liveness probe, so an X6 "connection" means only that the subscription is up, and a switched-off X6 fails later and worse — see [Printer models](#printer-models).

A device that is present but silent fails with its own message and its own error type:

```
error: found LX-D02 but it did not respond — is the printer powered on?
```

That is exit code **2** (and `503` on the server): from a caller's point of view there is still no printer to print on, but the wording tells whoever is standing next to it which fault it is. The hello costs one extra round trip and is idempotent — every print job opens with its own hello anyway.

### Timeouts

None of these come from the protocol; they are this implementation's choices.

| Constant | Value | Guards against |
|---|---|---|
| Scan | 10 s | No matching device ever advertises |
| `CONNECT_TIMEOUT` | 15 s | CoreBluetooth's own connect has no deadline and will wait forever for a peripheral that is not there |
| `HELLO_TIMEOUT` | 4 s | A device that connects but never answers (the liveness probe above; LX-D02 only — the X6 has no hello) |
| `NOTIFICATION_TIMEOUT` | 10 s | Total BLE silence mid-job — the link dropped |
| `STALL_TIMEOUT` | 60 s | A printer that keeps talking without taking data |
| `DISCONNECT_TIMEOUT` | 3 s | A teardown that never gets its confirmation callback |
| Status wait | 5 s (`status`), 3 s (pre-print) | No unsolicited status frame arrives (LX-D02 only) |

`NOTIFICATION_TIMEOUT` and `STALL_TIMEOUT` are complementary, and both are needed. The notification deadline measures radio silence and is re-armed by *any* frame, including the periodic unsolicited status heartbeats — so a printer that pauses the stream for thermal reasons and never resumes keeps the deadline alive indefinitely, and the job (and any HTTP client behind it) would wait forever. The stall deadline measures something the printer cannot fake: whether raster data is actually moving. A minute is deliberately generous, since a genuine thermal cooldown resumes in seconds. When it fires after real flow control, the error suggests lowering `--density`:

```
print failed: printer stalled for 60.0s without resuming, 47.3s of this job spent
paused for thermal flow control; the print head may be overheating — try a lower --density
```

### Config file

| Platform | Path |
|---|---|
| macOS | `~/Library/Application Support/printa-ble/config.toml` |
| Linux | `~/.config/printa-ble/config.toml` |
| Other | `<platform config dir>/printa-ble/config.toml` |

```toml
[device]
id = "c0076683-6d1d-5981-7fd2-4292d76b7bd9"
name = "LX-D02"
model = "lx-d02"
```

The file holds nothing else. `model` (`"lx-d02"` or `"x6"`) restricts reconnects to the same printer family; a config written before the field existed still loads, and an unrecognized value is ignored with a warning, falling back to name detection. A missing file is the normal first run and is silent. An unreadable or corrupt file prints a warning and is treated as empty. Delete the file to forget the saved printer. A failed save warns but never fails the command.

### macOS Bluetooth permission

The first BLE access triggers the system Bluetooth permission prompt. The prompt attaches to the **terminal application**, not to the `printable` binary, so approving it once covers every later run from that terminal. If you deny it, scans fail with:

```
failed to start BLE scan; on macOS, grant Bluetooth permission to your terminal in
System Settings > Privacy & Security > Bluetooth
```

Re-enable it under System Settings → Privacy & Security → Bluetooth. Running `printable` from a different terminal app (iTerm vs. Terminal vs. an IDE) prompts again for that app.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error (bad input, unreadable file, oversized job, render failure) |
| 2 | No **usable** printer: none found, or one found that never answered — also `scan` finding nothing, and any command-line usage error (clap's convention) |
| 3 | Printer is out of paper |
| 4 | Print failed (authentication rejected, BLE write failed, printer stopped responding) |

### Output streams

Diagnostics go to stderr, results to stdout. Scripts can read stdout safely.

| Stream | Text |
|---|---|
| stdout | Scan table, status fields, `Printed <N> lines.`, `Printed copy <i>/<N>.`, the preview file path |
| stderr | `Connected to <name>.`, warnings, errors, and everything `-v` turns on |

---

## scan

List every nearby device advertising a supported printer name (`LX*`, `X6h-*`, `x6h-*`).

```
printable scan [--timeout <SECONDS>]
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--timeout` | integer seconds | `5` | Scans for the full duration before printing results |

```console
$ printable scan
NAME                 MODEL    ID
LX-D02               lx-d02   c0076683-6d1d-5981-7fd2-4292d76b7bd9
X6h-1D4A             x6       8f2e11a0-42cb-59d3-88a7-05c1f2a6c3ee
```

The `MODEL` column is the protocol family the name identifies (see [Printer models](#printer-models)). The `ID` column is the platform peripheral identifier — a CoreBluetooth UUID on macOS, a MAC address elsewhere. Pass it (or any substring of it) to `--device`.

With no printers in range, `scan` writes `No supported printers found. Is the printer on?` to stderr and exits **2**.

```sh
printable scan --timeout 15     # slow to advertise, or a crowded 2.4 GHz band
```

---

## status

Connect, read one status frame, disconnect. **LX-D02 only** — the X6 sends no status frames at all, so against one the command fails immediately with `status notifications are not supported on this printer model` (exit 1) rather than sitting out the wait.

```
printable status [--device <DEVICE>] [--model <MODEL>]
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--device` | string | saved device, else first supported printer | Name or id substring |
| `--model` | `lx-d02` \| `x6` | detect from the name | Case-insensitive; see [Printer models](#printer-models) |

```console
$ printable status
Connected to LX-D02.
Battery:  78%
Paper:    OK
Density:  3
Voltage:  4.02 V
```

`Density` and `Voltage` appear only when the printer reports them. `Battery` gains ` (charging)` or ` (charged)` when the printer is on USB power. Two warnings may follow:

```
Warning:  print head is overheating
Warning:  battery is low
```

Status frames arrive unsolicited after subscribing. If none shows up within **5 seconds**, the command fails with `no status received` (exit 1). The connection still counts: the device is saved to the config file before the wait.

---

## print

Print text, a file, or a rendered web page.

```
printable print [OPTIONS] [TEXT]
printable print [OPTIONS] --file <PATH>
printable print [OPTIONS] --url <URL>
echo ... | printable print [OPTIONS]
```

### Input sources

Exactly one source is used. Combining them is an error.

| Source | Rendered as | Notes |
|---|---|---|
| `--url <URL>` | Web page screenshot | Conflicts with `TEXT`, `--file`, and `--markdown` (usage error, exit 2). Requires the `url` build feature. |
| `--file <PATH>` | By extension, see below | Passing `TEXT` too fails: `cannot combine a text argument with --file` |
| `--file -` | Whatever arrives on stdin | A bare `-` is the Unix spelling of "read stdin". `./-` and `-.md` are ordinary filenames. |
| `TEXT` positional | Plain text, or markdown with `-m` | |
| stdin | Plain text, or markdown with `-m` | Used only when there is no `TEXT` and no `--file` |

Markdown is otherwise chosen by file extension, so anything without one is **plain text by default** — piping a document renders its literal source. `-m` / `--markdown` is the only way to say so when there is no filename to give it away.

### `-m, --markdown`

Forces the markdown renderer for input that would otherwise be plain text.

| Input | Effect of `-m` |
|---|---|
| stdin | Renders as markdown |
| `TEXT` positional | Renders as markdown |
| `--file -` | Renders as markdown |
| `--file x.txt` | Renders as markdown |
| `--file x.md` / `.markdown` | Redundant — already markdown, and accepted without comment |
| `--file x.png` / `.jpg` / `.jpeg` | Error: `--markdown does not apply to an image file (…)`, exit 1 |
| `--url` | Usage error at parse time (clap conflict), exit 2 |

```sh
cat notes.md | printable print -m
printable print -m "# Heading\n\n- one\n- two"
printable print -m -f - < notes.md
printable print -m -f notes.txt
```

**Where relative image references anchor depends on how the document arrived.** A `--file` document anchors them to its own directory; markdown that arrived on stdin or as an argument has no file behind it, so they anchor to the **current working directory** — what `![](logo.png)` means to someone piping a document from their shell. (The server anchors nothing and never reads local paths at all; see [API.md](API.md#markdown-images).)

### File extensions

Extension matching is case-insensitive. Anything else fails with `unsupported file type: … (expected .png, .jpg, .jpeg, .txt, .md or .markdown)` and exit 1.

| Extension | Rendering |
|---|---|
| `.txt` | Plain text at `--size`, greedy word-wrap at 384 px |
| `.md`, `.markdown` | Full markdown: headings, emphasis, lists, task lists, tables, code, blockquotes, rules, `qr`, `barcode` and `wagara` fences, images |
| `.png`, `.jpg`, `.jpeg` | Scaled to 384 px wide, dithered with `--dither` |

A `--file` document's image references resolve against **that document's own directory**; both local paths and `http(s)` URLs are fetched. At most 32 references resolve per document, the whole pass gets 30 seconds, each fetch gets 15 seconds and 5 MB. Anything unresolved renders as an italic `[image: alt]` placeholder — a broken image never fails a print.

### Options

| Flag | Type | Default | Range | Applies to |
|---|---|---|---|---|
| `--device <STR>` | string | saved, else first supported printer | — | All |
| `--model <MODEL>` | enum | detect from the name | `lx-d02` \| `x6`, case-insensitive | All |
| `-f, --file <PATH>` | path | — | `-` means stdin | — |
| `-m, --markdown` | flag | off | — | Text input and `.txt`; rejected for images and `--url` |
| `--url <URL>` | string | — | `http://` or `https://` only | — |
| `--density <N>` | integer | `3` | 1–7, enforced by the parser | All (maps to speed and energy on an X6 — see [Printer models](#printer-models)) |
| `--feed <N>` | integer | `40` | ≥ 0, no upper bound | All |
| `--dither <MODE>` | enum | `floyd` | `floyd`, `atkinson`, `threshold` (alias `none`) | Image files and `--url` only |
| `--size <PX>` | float | `24` | > 0, finite, no upper bound | Plain text only (`TEXT`, stdin, `.txt`) |
| `--preview <PATH>` | path | — | — | All |
| `--copies <N>` | integer | `1` | 1–20, enforced by the parser | All |

Out-of-range `--density` or `--copies`, and a non-positive `--size`, are usage errors: exit **2** before anything else runs.

#### `--dither`

| Mode | Behavior |
|---|---|
| `floyd` | Floyd–Steinberg error diffusion — the default, best for photos and gradients |
| `atkinson` | Atkinson error diffusion — higher contrast, lighter mid-tones |
| `threshold` / `none` | Plain threshold at 128, no diffusion — best for line art and screenshots of text |

`--dither` affects `--file photo.png` and `--url` renders. It does **not** affect images embedded in a markdown document: those are always Floyd–Steinberg.

#### `--size`

Font size in pixels for plain text only. Line height is 1.3 × size. `\r\n` and bare `\r` normalize to `\n`, tabs expand to four spaces, lines wrap greedily at 384 px, and an overlong single word breaks mid-word. `--size` is ignored for markdown (the renderer picks per-element sizes), images, and URLs.

#### `--feed`

Blank rows appended after the content, so the paper advances past the tear bar. On an LX-D02 the rows ride along as blank raster lines and count toward the printed line total; on an X6 the feed is a printer command instead, does not count toward `Printed <N> lines.`, and saturates at 65 535. Either way the feed appears in `--preview` output. `40` clears the head on an LX-D02.

#### `--preview`

Renders to a PNG at `<PATH>` and exits without touching the printer or Bluetooth. The path is echoed to stdout. Feed lines are included. With `--copies` above 1 you get `note: preview renders a single copy; --copies is ignored` on stderr and a single-copy image.

#### `--copies`

One BLE connection, one full print job (fresh authentication, on the LX-D02) per copy. Each copy reports `Printed copy <i>/<N>.`; a single copy reports `Printed <lines> lines.` instead.

### Examples

```sh
printable print "Hello world"
printable print "Hello" --size 32 --density 5
echo "from a pipe" | printable print
printable print -f notes.txt --size 28
printable print -f receipt.md
cat receipt.md | printable print -m
printable print -m -f - < receipt.md
printable print -f photo.jpg --dither atkinson
printable print -f screenshot.png --dither none
printable print --url https://example.com
printable print -f flyer.md --copies 3 --feed 60
printable print "draft" --preview /tmp/out.png
printable print "invoice" --device LX-D02
printable print "invoice" --device c0076683      # id substring
printable print "meow" --model x6                # restrict to the X6 family
```

### Failure modes

| Message | Exit | Cause |
|---|---|---|
| `nothing to print` | 1 | Input is empty or whitespace only |
| `cannot combine a text argument with --file` | 1 | Both given |
| `--markdown does not apply to an image file (…)` | 1 | `-m` with `-f photo.png` |
| `unsupported file type: …` | 1 | Extension is not one of the six |
| `failed to open …` / `failed to read …` | 1 | Unreadable file |
| `failed to decode image: …` | 1 | Not a valid PNG or JPEG |
| `cannot print this job: print too large: …` | 1 | Over 65 535 raster packets (more than 131 070 rows). Note the server answers `400` for the same condition |
| `no supported printer found. Is the printer on and in range?` | 2 | Nothing matched within the 10 s scan |
| `found <name> but it did not respond — is the printer powered on?` | 2 | The device was there and connected, but never answered hello |
| `printer is out of paper` | 3 | Pre-print check or a mid-job status frame |
| `print failed: …` | 4 | Auth rejected, BLE write failed, the printer went silent, or the job stalled |

---

## qr

Print a QR code encoding a URL or arbitrary text, centered at the full 384 px width.

```
printable qr <DATA> [OPTIONS]
```

| Argument / Flag | Type | Default | Range |
|---|---|---|---|
| `<DATA>` | string | required | Anything a QR code can hold |
| `--caption <TEXT>` | string | — | Rendered below the code at 24 px, left-aligned |
| `--device <STR>` | string | saved, else first supported printer | — |
| `--model <MODEL>` | enum | detect from the name | `lx-d02` \| `x6`, case-insensitive |
| `--density <N>` | integer | `3` | 1–7 (maps to speed and energy on an X6 — see [Printer models](#printer-models)) |
| `--feed <N>` | integer | `40` | ≥ 0 |
| `--preview <PATH>` | path | — | — |
| `--copies <N>` | integer | `1` | 1–20 |

There is no `--size` and no `--dither`: the version and error-correction level are chosen automatically, and the code is scaled by the largest integer factor that fits 384 px including a 4-module quiet zone, with a 16 px white margin above and below.

Data that fits no QR version fails with `cannot render QR code: data too long to fit in a QR code` and exit 1.

```sh
printable qr "https://example.com"
printable qr "https://example.com/order/42" --caption "Order #42"
printable qr "$(pbpaste)" --preview /tmp/qr.png
printable qr "https://example.com" --copies 5 --density 5
```

To embed a QR code inside a larger document, use a `qr` fence in markdown instead:

````markdown
Scan to reorder:

```qr
https://example.com/order/42
```
````

---

## serve

Run the HTTP print server: REST API plus a phone-friendly web UI. Runs until interrupted.

```
printable serve [OPTIONS]
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--port <PORT>` | integer | `8000` | 0–65535; `0` picks a free port |
| `--bind <ADDR>` | string | `0.0.0.0` | `0.0.0.0` = every interface (LAN printing); `127.0.0.1` = this machine only |
| `--device <STR>` | string | saved, else first supported printer | Pins the printer for every request |
| `--model <MODEL>` | enum | detect from the name | Pins the protocol family, like `--device`; no HTTP route takes a model |
| `--no-remote-images` | flag | off | Never fetch `http(s)` images referenced by markdown |

```console
$ printable serve
Listening on http://0.0.0.0:8000
On your LAN: http://192.168.1.42:8000
```

The LAN hint appears only when `--bind` is exactly `0.0.0.0`.

At the default level the server logs only its own warnings — a failed job, an unreachable printer, and any `5xx` it returns. `-v` turns on the operational log:

```console
$ printable serve -v
Listening on http://0.0.0.0:8000
 INFO POST /preview/markdown -> 200 in 34ms
 INFO print job starting: markdown, 812 lines, density 3, feed 40, 1 copies
 INFO connected to LX-D02, subscribed to notifications
 INFO printer is cooling down
 INFO print job done: markdown, 812 lines in 24310ms (812 packets, 3 holds, 41 cooldowns, 0 resends)
 INFO POST /print/markdown -> 200 in 24544ms
```

Every request gets one line with its method, path, status and elapsed milliseconds. A request that has to queue behind a running job says so going in and reports the wait on the way out — that wait is otherwise indistinguishable from a hang at the client end. URL routes log Chrome's launch-and-render time separately, since it happens before the printer is ever touched and is often the slowest part.

There is **no authentication** — anyone who can reach the port can print, and the markdown and URL endpoints make outbound requests on a caller's behalf. See [SECURITY.md](../SECURITY.md) for the trust model, and [API.md](API.md) for the full endpoint reference.

```sh
printable serve                                  # LAN, port 8000
printable serve --bind 127.0.0.1                 # this Mac only
printable serve --port 9100 --device LX-D02
printable serve --no-remote-images               # no outbound requests at all
```

`--no-remote-images` leaves the server with no outbound request surface. Local file references in posted markdown are refused either way — that is a security boundary, not a setting.

---

## Recipes

### Shopping list

```sh
cat > /tmp/list.md <<'EOF'
# Groceries

- [ ] coffee beans, 250 g
- [ ] oat milk
- [ ] bread

- - -
EOF
printable print -f /tmp/list.md
```

`- - -` (a thematic break with interior spaces) renders as a dashed tear line.

### Wi-Fi credentials as a QR code

Phones join the network by scanning it.

```sh
printable qr 'WIFI:T:WPA;S:MyNetwork;P:hunter2;;' --caption "Guest Wi-Fi"
```

Escape `;`, `,`, `:` and `\` inside the SSID or password with a backslash.

### Pipe command output

```sh
git log --oneline -10 | printable print --size 20
df -h | printable print --size 18
cal | printable print --size 22
uptime | printable print
```

Small sizes fit more columns: 384 px holds roughly 32 monospace characters at 24 px, 42 at 18 px.

### Pipe a markdown document

```sh
cat CHANGELOG.md | printable print -m
glow -s notes.md 2>/dev/null || printable print -m -f notes.md
printf '# Standup\n\n- [ ] deploy\n- [ ] review\n' | printable print -m
```

Without `-m` these print their literal source, `#` and all. Relative image references in a piped document resolve against the **working directory**, so run the command from wherever the images live.

### Print a web page

```sh
printable print --url https://example.com
printable print --url https://news.ycombinator.com --dither atkinson
```

The page renders in headless Chrome at a 384 px viewport and is captured full-page, so a long article prints as a long receipt. Check it first with `--preview`.

### Multiple copies

```sh
printable qr "https://example.com/rsvp" --caption "RSVP" --copies 10
printable print -f ticket.md --copies 4 --feed 60
```

Copies share one BLE connection but run as separate print jobs, so a failure mid-run leaves the earlier copies printed.

### Preview before committing paper

```sh
printable print -f long-report.md --preview /tmp/check.png && open /tmp/check.png
```

### Print from another machine

```sh
printable serve --bind 0.0.0.0                            # on the Mac with the printer
curl -X POST http://192.168.1.42:8000/print/text \
  -H 'Content-Type: application/json' \
  -d '{"content":"Hello from the laptop"}'                 # anywhere on the LAN
```

---

## Troubleshooting

### No printer found (exit 2)

1. Power the printer on and check it is not already connected to a phone — BLE links are exclusive.
2. `found <name> but it did not respond` is a *different* fault from nothing being found, even though both exit 2: the device is right there and connected, but nothing answered. On macOS that is almost always a printer that is switched off, because CoreBluetooth answers connect and discovery out of its cache. Turn it on. (That message is LX-D02 only — an off X6 "connects" successfully and fails later, as a print job that stalls in silence. Same fix: turn it on.)
3. Run `printable scan --timeout 15`. If the printer appears there but commands still fail, pass its id: `printable print "x" --device <ID>`.
4. If `scan` finds nothing, confirm Bluetooth is on. `no Bluetooth adapter found — is Bluetooth turned on?` means the adapter itself is missing or disabled.
5. Connect attempts scan for 10 seconds. A printer that advertises slowly may need a power cycle rather than a longer wait.
6. **A stale saved device costs a full 10 seconds on every command.** The resolver takes an exact id match immediately, but a saved id that no longer exists never matches — so it collects ranked fallbacks (a device with the saved *name*, then any supported printer of the saved model) and only uses one when the scan deadline expires. Every command pays the whole 10 seconds even with the printer sitting right there under a new identifier. Delete the config file, or pass `--device`, to skip it. Run with `-vv` to see which candidate won.

### Bluetooth permission denied

The error names the fix:

```
failed to start BLE scan; on macOS, grant Bluetooth permission to your terminal in
System Settings > Privacy & Security > Bluetooth
```

Permission is granted per terminal application. Toggle your terminal off and on again in that pane and restart it. If the app is not listed at all, the prompt was never triggered — run `printable scan` once and answer it.

### `not a lx-d02 printer?`

```
<name> has no 0xFFE1 write characteristic — not a lx-d02 printer?
```

(or `no 0xAE01 write characteristic — not a x6 printer?`)

`--device` matched something that is not the kind of printer the resolved model expects. Substring matching also matches ids, so a short filter like `--device 0` can catch anything. Use the full name or id from `printable scan` — its `MODEL` column also tells you what to pass to `--model` if the family was guessed wrong.

### Chrome not found

```
could not launch Chrome — is Google Chrome installed?
(build with --no-default-features to disable URL printing)
```

`--url` drives system Google Chrome through the DevTools protocol. Install Chrome, or drop the dependency at build time with `cargo build --release --no-default-features` — `--url` and the server's URL routes then disappear.

If Chrome is installed but the render fails, try the page in a normal browser first: navigation errors, redirects to a login wall, and pages that never finish loading all surface as `failed to load <url>`.

### Print stalls or dies mid-job

| Symptom | Cause |
|---|---|
| `printer went silent (no BLE notification at all for 10s)` (exit 4) | Nothing arrived on the link for 10 seconds. Usually the link dropped — move closer, then retry. |
| `printer stalled for … without resuming` (exit 4) | The printer is still sending frames but has taken no data for 60 seconds. When the job spent time paused for flow control, the message says so and suggests a lower `--density`; when it never paused at all, the head is not the problem and the message does not blame it. |
| Job pauses, then resumes | Normal flow control. The printer asks for a hold when its buffer fills or the head is hot. Run with `-v` to watch the pauses and resumes as they happen. |
| An X6 job goes silent right after connecting | The printer is probably switched off: the X6 has no liveness probe, so a cached connect succeeds against a dead printer and the job dies on the 10 s notification timeout instead. |
| `printer is out of paper` (exit 3) | Checked before the job starts and again on every status frame during it. LX-D02 only — the X6 has no paper signal, so it prints into thin air until the job ends. |
| `warning: printer battery is low` | Printing continues, but density drops on a flat battery. Charge it before a long job. |
| Faint or streaky output | Raise `--density` (up to 7). Long dark jobs trigger thermal cooldown pauses. |
| `cannot print this job: print too large` | Over 131 070 rows. Split the document. |

A failed job leaves the paper where it stopped; re-running reprints from the top.
