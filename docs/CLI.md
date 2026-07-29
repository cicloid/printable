# printable CLI Reference

`printable` prints to LX-D02 / LX-D2 BLE thermal printers: 58 mm paper, 203 dpi, 384 px wide.

```
printable <COMMAND> [OPTIONS]
```

| Command | Purpose |
|---|---|
| [`scan`](#scan) | List nearby LX printers |
| [`status`](#status) | Show battery, paper, density |
| [`print`](#print) | Print text, a file, or a web page |
| [`qr`](#qr) | Print a QR code |
| [`serve`](#serve) | Run the HTTP print server |

Global flags: `-h, --help` (per command too) and `-V, --version`.

---

## Global behavior

### Device resolution

Every command that talks to the printer resolves a device the same way. `scan` is the exception — it lists everything it sees.

| Rank | Source | Match | When it wins |
|---|---|---|---|
| 1 | `--device <STR>` | Advertised name **or** platform id contains `<STR>` | Immediately, first match |
| 2 | Saved device id (config file) | Exact platform id | Immediately |
| 2a | Saved device *name* | Advertised name equals the saved name | Only at the scan deadline, preferred over 2b |
| 2b | Any `LX*` | Advertised name starts with `LX` | Only at the scan deadline |
| 3 | No flag, no saved device | Advertised name starts with `LX` | Immediately, first match |

The scan runs up to **10 seconds**, polling every 300 ms. An exact match short-circuits it; the ranked fallbacks are used only if no exact match appears before the deadline. If nothing matches at all, the command fails with `no LX printer found. Is the printer on and in range?` and exit code 2.

After every successful connection the device's id and name are written to the config file, so the next run reconnects to the same printer without a flag. `--device` overrides the saved printer *and* replaces it.

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
```

The file holds nothing else. A missing file is the normal first run and is silent. An unreadable or corrupt file prints a warning and is treated as empty. Delete the file to forget the saved printer. A failed save warns but never fails the command.

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
| 2 | No printer found — also `scan` finding nothing, and any command-line usage error (clap's convention) |
| 3 | Printer is out of paper |
| 4 | Print failed (authentication rejected, BLE write failed, printer stopped responding) |

### Output streams

Diagnostics go to stderr, results to stdout. Scripts can read stdout safely.

| Stream | Text |
|---|---|
| stdout | Scan table, status fields, `Printed <N> lines.`, `Printed copy <i>/<N>.`, the preview file path |
| stderr | `Connected to <name>.`, warnings, errors |

---

## scan

List every nearby device advertising a name that starts with `LX`.

```
printable scan [--timeout <SECONDS>]
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--timeout` | integer seconds | `5` | Scans for the full duration before printing results |

```console
$ printable scan
NAME                 ID
LX-D02               c0076683-6d1d-5981-7fd2-4292d76b7bd9
```

The `ID` column is the platform peripheral identifier — a CoreBluetooth UUID on macOS, a MAC address elsewhere. Pass it (or any substring of it) to `--device`.

With no printers in range, `scan` writes `No LX printers found. Is the printer on?` to stderr and exits **2**.

```sh
printable scan --timeout 15     # slow to advertise, or a crowded 2.4 GHz band
```

---

## status

Connect, read one status frame, disconnect.

```
printable status [--device <DEVICE>]
```

| Flag | Type | Default | Notes |
|---|---|---|---|
| `--device` | string | saved device, else first `LX*` | Name or id substring |

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
| `--url <URL>` | Web page screenshot | Conflicts with `TEXT` and `--file` (usage error, exit 2). Requires the `url` build feature. |
| `--file <PATH>` | By extension, see below | Passing `TEXT` too fails: `cannot combine a text argument with --file` |
| `TEXT` positional | Plain text | |
| stdin | Plain text | Used only when there is no `TEXT` and no `--file` |

Note that stdin and the positional argument are always **plain text**, never markdown — piping a `.md` file renders its literal source. Use `--file` for markdown.

### File extensions

Extension matching is case-insensitive. Anything else fails with `unsupported file type: … (expected .png, .jpg, .jpeg, .txt, .md or .markdown)` and exit 1.

| Extension | Rendering |
|---|---|
| `.txt` | Plain text at `--size`, greedy word-wrap at 384 px |
| `.md`, `.markdown` | Full markdown: headings, emphasis, lists, task lists, tables, code, blockquotes, rules, `qr` and `barcode` fences, images |
| `.png`, `.jpg`, `.jpeg` | Scaled to 384 px wide, dithered with `--dither` |

Markdown image references resolve against the document's own directory; both local paths and `http(s)` URLs are fetched. At most 32 references resolve per document, the whole pass gets 30 seconds, each fetch gets 15 seconds and 5 MB. Anything unresolved renders as an italic `[image: alt]` placeholder — a broken image never fails a print.

### Options

| Flag | Type | Default | Range | Applies to |
|---|---|---|---|---|
| `--device <STR>` | string | saved, else first `LX*` | — | All |
| `-f, --file <PATH>` | path | — | — | — |
| `--url <URL>` | string | — | `http://` or `https://` only | — |
| `--density <N>` | integer | `3` | 1–7, enforced by the parser | All |
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

Blank rows appended after the content, so the paper advances past the tear bar. The rows count toward the printed line total and appear in `--preview` output. `40` clears the head on an LX-D02.

#### `--preview`

Renders to a PNG at `<PATH>` and exits without touching the printer or Bluetooth. The path is echoed to stdout. Feed lines are included. With `--copies` above 1 you get `note: preview renders a single copy; --copies is ignored` on stderr and a single-copy image.

#### `--copies`

One BLE connection, one full print job (fresh authentication) per copy. Each copy reports `Printed copy <i>/<N>.`; a single copy reports `Printed <lines> lines.` instead.

### Examples

```sh
printable print "Hello world"
printable print "Hello" --size 32 --density 5
echo "from a pipe" | printable print
printable print -f notes.txt --size 28
printable print -f receipt.md
printable print -f photo.jpg --dither atkinson
printable print -f screenshot.png --dither none
printable print --url https://example.com
printable print -f flyer.md --copies 3 --feed 60
printable print "draft" --preview /tmp/out.png
printable print "invoice" --device LX-D02
printable print "invoice" --device c0076683      # id substring
```

### Failure modes

| Message | Exit | Cause |
|---|---|---|
| `nothing to print` | 1 | Input is empty or whitespace only |
| `cannot combine a text argument with --file` | 1 | Both given |
| `unsupported file type: …` | 1 | Extension is not one of the six |
| `failed to open …` / `failed to read …` | 1 | Unreadable file |
| `failed to decode image: …` | 1 | Not a valid PNG or JPEG |
| `cannot print this job: print too large: …` | 1 | Over 65 535 raster packets (more than 131 070 rows) |
| `no LX printer found. Is the printer on and in range?` | 2 | Nothing matched within the 10 s scan |
| `printer is out of paper` | 3 | Pre-print check or a mid-job status frame |
| `print failed: …` | 4 | Auth rejected, BLE write failed, or the printer stopped responding |

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
| `--device <STR>` | string | saved, else first `LX*` | — |
| `--density <N>` | integer | `3` | 1–7 |
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
| `--device <STR>` | string | saved, else first `LX*` | Pins the printer for every request |
| `--no-remote-images` | flag | off | Never fetch `http(s)` images referenced by markdown |

```console
$ printable serve
Listening on http://0.0.0.0:8000
On your LAN: http://192.168.1.42:8000
```

The LAN hint appears only when `--bind` is exactly `0.0.0.0`.

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
2. Run `printable scan --timeout 15`. If the printer appears there but commands still fail, pass its id: `printable print "x" --device <ID>`.
3. If `scan` finds nothing, confirm Bluetooth is on. `no Bluetooth adapter found — is Bluetooth turned on?` means the adapter itself is missing or disabled.
4. Connect attempts scan for 10 seconds. A printer that advertises slowly may need a power cycle rather than a longer wait.
5. A stale saved device slows things down: the resolver waits the full 10 seconds for the saved id before falling back. Delete the config file, or pass `--device`, to skip that.

### Bluetooth permission denied

The error names the fix:

```
failed to start BLE scan; on macOS, grant Bluetooth permission to your terminal in
System Settings > Privacy & Security > Bluetooth
```

Permission is granted per terminal application. Toggle your terminal off and on again in that pane and restart it. If the app is not listed at all, the prompt was never triggered — run `printable scan` once and answer it.

### `not an LX printer?`

```
<name> has no 0xFFE1 write characteristic — not an LX printer?
```

`--device` matched something that is not an LX printer. Substring matching also matches ids, so a short filter like `--device 0` can catch anything. Use the full name or id from `printable scan`.

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
| `printer stopped responding` (exit 4) | No notification for 10 seconds. Usually the link dropped — move closer, then retry. |
| Job pauses, then resumes | Normal flow control. The printer asks for a hold when its buffer fills or the head is hot. |
| `printer is out of paper` (exit 3) | Checked before the job starts and again on every status frame during it. |
| `warning: printer battery is low` | Printing continues, but density drops on a flat battery. Charge it before a long job. |
| Faint or streaky output | Raise `--density` (up to 7). Long dark jobs trigger thermal cooldown pauses. |
| `cannot print this job: print too large` | Over 131 070 rows. Split the document. |

A failed job leaves the paper where it stopped; re-running reprints from the top.
