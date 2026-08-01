# AirPrint and CUPS

Expose the LX-D02 as an AirPrint / IPP Everywhere printer, so it shows up in
the macOS print dialog, in `lp`, and on iOS — without a driver, a PPD, or a
CUPS backend.

```sh
cargo build --release
PRINTABLE=./target/release/printable scripts/airprint.sh
```

Then add it as a CUPS queue (no `sudo` needed):

```sh
lpadmin -p printable -E -v ipp://localhost:8631/ipp/print -m everywhere
lp -d printable somefile.txt
```

> **Try it with `--preview` first.** Printing consumes paper, and a
> misconfigured queue can retry a job several times:
>
> ```sh
> AIRPRINT_ARGS="--preview /tmp/job.png" \
>   PRINTABLE=./target/release/printable scripts/airprint.sh
> ```
>
> Every job then renders to `/tmp/job.png` instead of the printer. Open it and
> confirm the layout before switching to a real print.

## Why not a CUPS backend

The obvious approach — a backend in `/usr/libexec/cups/backend/` speaking a
`printable://` URI — does not work on a current macOS:

- the system volume is read-only and SIP is enabled, so the backend directory
  cannot be written to at all;
- working around that means repointing `ServerBin` in `cupsd.conf` and cloning
  every stock backend and filter beside your own;
- Apple has been retiring PPD- and filter-based printing for several releases;
- and it would only ever serve that one Mac. No iPhone, no iPad.

AirPrint is not a CUPS feature. It is IPP over HTTP plus a Bonjour
advertisement. Implement that once and macOS adds the printer *driverlessly*
via IPP Everywhere, which is why the `lpadmin` line above needs no driver and
no elevated privileges.

## How it works

```
 iOS / macOS print dialog
        │  IPP over HTTP, document rasterised by the client
        ▼
 ippeveprinter          IPP server + Bonjour advertisement (ships with macOS)
        │  runs one command per job, spooled file as argv[1]
        ▼
 printable ipp-command  inflate → decode URF → fit → dither → 384 px bitmap
        │
        ▼
 LX-D02 over BLE
```

`ippeveprinter` is a complete IPP Everywhere server included with CUPS. Stage 1
borrows it wholesale, so the only new code is the job-handling command.

### The client rasterises, so we need no PDF renderer

The printer advertises `pdl=image/urf` and **not** `application/pdf`. A client
that cannot send PDF rasterises the document itself and sends Apple Raster
(URF) instead — a simple run-length format that decodes in a few hundred lines
of dependency-free Rust. Advertising PDF would have meant vendoring PDFium or
taking MuPDF's AGPL into the repo.

The decoder lives in `printa-ble-core/src/raster/urf.rs`. Its layout was
derived from a file produced by Apple's own `rastertourf` filter, not from a
specification, and is pinned by a round-trip test: the decoder consumes the
captured fixture exactly, to the byte, with nothing left over.

```text
file header   12 B  "UNIRAST\0" | u32be page_count
page header   32 B  bpp u8 | colorspace u8 | duplex u8 | quality u8
                    | u32be x2 reserved | u32be width | u32be height
                    | u32be dpi | 8 B reserved
page data           per line: u8 repeat (line appears repeat + 1 times),
                    then runs until `width` pixels are covered:
                      c  < 128 -> next pixel repeated c + 1 times
                      c == 128 -> end of row; the rest of the line is blank
                      c  > 128 -> 257 - c literal pixels
```

Note that a literal run encodes *at least two* pixels (`257 - 255`), so a
single odd pixel is always written as a repeat of one.

The `0x80` end-of-row marker was the subtlest part. A page of text never emits
one, so the original fixture could not catch it and it was first documented —
wrongly — as reserved. Its real meaning was recovered by brute-forcing the
candidates against a 3.4 MB page from an iPhone and keeping the only one that
decoded all 6600 rows while consuming the file to its last byte. Treating it
as a 129-pixel repeat or a 129-pixel literal both overrun the first row.

Jobs may also arrive **gzipped** — iOS compresses, macOS does not — so the
command inflates by magic byte before decoding.

### Media: 48 mm continuous roll

`scripts/airprint-receipt.conf` tells the client what the paper actually is:
48 mm wide, continuous, 203 dpi, no margins. That resolution is not arbitrary
— 48 mm at 203 dpi is 383.6 dots, i.e. the 384-dot line the hardware prints,
so a cooperating client rasterises straight to the bitmap width with no
rescaling. Verified on the wire: a job comes back `383x2373 @ 203dpi`.

Continuous length is expressed with the `custom_min_48x25mm` /
`custom_max_48x1000mm` keywords rather than a `rangeOfInteger` y-dimension.
That is not a style choice: a range makes `lpadmin -m everywhere` fail with
"Unable to create PPD file", because a PPD cannot express a variable page
length.

Two further gotchas, both found the hard way:

- **`-a` cannot be combined with `-f`.** Supported formats have to move into
  the attributes file — except that `ippeveprinter` appends its own defaults
  afterwards, so declaring them there produces duplicate attributes that
  `ipptool` flags. The conf therefore leaves formats alone and accepts that
  `image/pwg-raster` is advertised alongside URF. Only URF decodes; a PWG
  Raster job fails with a message naming the format.
- **PWG media names encode their units in the class prefix.** `om_` is metric,
  `oe_` is inches. `oe_receipt_48x297mm` is malformed and breaks PPD
  generation.

### How a page is fitted

The decoder picks its strategy from the page's own physical width:

- **Receipt-width** (≤ 80 mm) — the client honoured the media advertisement,
  so its layout is authoritative. Only leading and trailing blank rows are
  trimmed; on a continuous roll those are paper, not margin.
- **A full sheet** — the client ignored it and sent US Letter. Scaling 5100 px
  onto 384 px is a 13x reduction that renders 12 pt text about 7 px tall, so
  the page is cropped to its ink and *that* is scaled up. Readable, but
  apparent type size then depends on how much content the page holds.

Blank pages are skipped entirely, so a trailing empty page costs no paper.

### macOS `lp` and the 48 mm page

macOS's own CUPS filter chain lays out badly on a 48 mm page: `texttopdf`
clips at the right edge (and adds its own left margin regardless of the
zero-margin advertisement), and `imagetopdf` emits a blank page. This is a
limitation of those filters, not of the printer or the decoder.

iOS never touches them — it rasterises the document itself — so it gets the
48 mm default and the good path. For `lp`, the conf also advertises US Letter
and A4 as an escape hatch:

```sh
lp -d printable -o media=na_letter_8.5x11in file.pdf
```

which lands on the crop path instead. For printing from the Mac, though,
`printable print` is the better tool: it renders for this paper directly.

## Status reporting

`ippeveprinter` treats the command's **stderr as a control channel**. Lines
prefixed `INFO:`, `ERROR:`, `STATE:` and `ATTR:` are parsed and surfaced in the
client's print queue; anything else is only visible under `-vv`.

`ipp-command` uses that to report what the transport is doing:

| Situation | Reported as |
|---|---|
| Job decoded | `ATTR: job-impressions=N`, page geometry as `INFO:` |
| Finished | `INFO: printed N lines in Ns (H holds, C cooldowns, R resends)` |
| Out of paper | `STATE: media-empty` |
| Printer off / not found | `STATE: offline-report` |
| Any failure | `ERROR: <message>` |

The holds and cooldowns matter: thermal flow control routinely pauses this
printer for seconds at a time on dense pages, and without a status line that
looks exactly like a stalled job.

## iOS discovery

The service must be registered under the `_universal` DNS-SD subtype for iOS to
treat it as an AirPrint printer. `scripts/airprint.sh` passes `-r _universal`,
and **the leading underscore is required** — `-r universal` registers on plain
`_ipp._tcp` only, which macOS still finds but iOS does not.

Verify the advertisement:

```sh
dns-sd -B _ipp._tcp,_universal          # should list the service
dns-sd -L printa-ble _ipp._tcp local    # should show URF= and pdl=image/urf
```

## What is verified, and what is not

Verified on macOS 26.5.2 / CUPS 2.3.4:

- the full chain `lp` → CUPS → IPP → `ippeveprinter` → `ipp-command` → a
  correct, legible 384 px render, and a real print from an iPhone photo job;
- 48 mm / 203 dpi media: a job rasterises to `383x2373`, i.e. 47.9 mm wide;
- the URF decoder against a real `rastertourf` capture, byte-exact;
- the Bonjour advertisement, including the `_universal` subtype and a TXT
  record carrying `URF=` and `pdl=image/urf`.

Not yet verified:

- **an end-to-end print from iOS at 48 mm.** Discovery, `Create-Job`,
  `Send-Document` and a real print from an iPhone have all been observed, and
  the 48 mm raster geometry is confirmed on the wire — but not yet both at
  once in a single job.
- **anything but `image/urf`.** PWG Raster is advertised by ippeveprinter's
  defaults and is not implemented.

## Limits

- **CUPS 2.4+ is required for iOS.** macOS 26 still ships CUPS 2.3.4, whose
  `ippeveprinter` rejects the `Create-Job` iOS sends with "Unexpected document
  data following request" — the identical operation from `ipptool` or CUPS
  succeeds, so it is that release mishandling iOS's HTTP framing. Install a
  newer one with `brew install cups`; `scripts/airprint.sh` prefers it
  automatically. Printing from a Mac works on either.
- `ippeveprinter` must stay running; it is a foreground process, not a daemon.
  Nothing here survives a reboot without a launch agent.
- Only `image/urf` is accepted. A client that insists on PDF cannot print.
- Media is whatever the client picks, typically US Letter — hence the crop.
- No authentication. Anyone on the LAN can queue a job, same caveat as
  `printable serve`; see [SECURITY.md](../SECURITY.md).
- One job at a time. Concurrent jobs are not serialised against
  `printable serve`, which holds its own print lock.

Stage 2 — a native `printable airprint` with its own IPP server and Bonjour
registration — would drop the `ippeveprinter` dependency entirely, taking the
CUPS-version requirement and the `-a`/`-f` awkwardness with it. It would also
share the print lock with `serve` and run as a normal background service.

## Regenerating the test fixture

`crates/printa-ble-core/src/raster/testdata/letter_600dpi.urf` was captured
like this:

```sh
mkdir -p /tmp/spool
ippeveprinter -k -d /tmp/spool -f image/urf -p 18631 urf-capture &
lpadmin -p urfcap -E -v ipp://localhost:18631/ipp/print -m everywhere
printf 'printa-ble URF capture test\nline two\n' > /tmp/t.txt
lp -d urfcap /tmp/t.txt
# /tmp/spool/1-t_txt.urf is the capture
lpadmin -x urfcap
```

The output is deterministic: repeating the capture yields a byte-identical
file.
