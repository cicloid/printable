# The printa-ble markdown dialect

Reference for the markdown renderer in `crates/printa-ble-core/src/raster/markdown.rs`. The same renderer backs every surface: `printable print -f notes.md`, the server's `/preview/markdown` and `/print/markdown`, and the web app's Markdown tab. Output is always a 384 px wide, 1-bit bitmap — the width of 58 mm paper at 203 dpi.

Parsing is [pulldown-cmark](https://docs.rs/pulldown-cmark) 0.12 (CommonMark) with exactly three extensions enabled: strikethrough, task lists, and tables. Everything else in CommonMark parses; the lowering then maps each event to a bitmap block or drops it.

## The canvas

| Property | Value |
|---|---|
| Width | 384 px, fixed |
| Colour | 1 bit; a glyph pixel is black at ≥ 128/255 coverage |
| Font | JetBrains Mono (Regular / Bold / Italic), embedded in the binary |
| Line height | 1.3 × the largest font size on the rendered line |
| Wrapping | Greedy word wrap at `384 − indent`; an overlong word breaks mid-word |
| Alignment | Left, always. Nothing centres except QR codes and barcodes |
| Normalization | `\r\n` and `\r` → `\n`; tab → four spaces |

Because the font is monospace, character counts are exact: the advance is 0.6 em, so a full-width line holds 26 characters at 24 px and 32 characters at 20 px.

## Block elements

### Headings

Bold, at a fixed size per level, with a blank line above and below (trimmed at the start and end of the document).

| Level | Size | Rendered block height |
|---|---|---|
| `#` H1 | 36 px | 47 px |
| `##` H2 | 30 px | 39 px |
| `###` and deeper | 26 px | 34 px |

H4-H6 are not distinguished from H3. Setext headings (`===` / `---` underlines) work and are identical to their ATX equivalents. A heading forces the bold face, so `*italic*` inside a heading has no visible effect.

### Paragraphs

Regular 24 px, one blank line after (32 px). A soft break (a plain newline in the source) renders as a space — standard markdown, and the right call on a 384 px roll. A hard break (two trailing spaces or a backslash) starts a new line.

### Emphasis

| Source | Rendering |
|---|---|
| `**bold**` | Bold face |
| `*italic*` | Italic face |
| `***both***` | **Bold** — see below |
| `~~struck~~` | A 2 px line at 0.35 × the font size above the baseline |
| `` `code` `` | Same face and size as the surrounding text |

**Bold and italic do not compose.** There is one font face per span, and the resolution order is: heading → bold → italic → regular. So `***x***` renders byte-identical to `**x**`, and bold text inside a blockquote (which is italic) renders bold. Strikethrough is a separate flag and composes with any face.

The strike line spans each glyph's full advance, including the spaces between words, so a struck phrase gets one continuous line.

Inline code takes the surrounding style — the font is monospace already, so there is no distinct code face. `` `x` `` renders byte-identical to `x`.

### Lists

Bullets get a `• ` prefix, ordered items `N. ` — the `start` attribute is honoured (`5.` starts at five) and increments per item. Indent is **24 px per nesting level**, plus 24 px for every enclosing blockquote:

```
- level 1          → indent 24
  - level 2        → indent 48
    - level 3      → indent 72
```

A wrapped line keeps its item's indent. A second paragraph inside an item keeps it too. Loose and tight lists render identically — no extra blank line between loose items.

A fenced or indented code block inside a list item does **not** inherit the list indent; it sits at the code indent of 16 px (see below).

### Task lists

`- [x]` / `- [ ]` render as ASCII `[x] ` / `[ ] `. The marker *replaces* the list prefix rather than following it, so `- [x] done` renders exactly as the text `[x] done` at indent 24.

ASCII, not `☑`/`☐`, because JetBrains Mono has no glyph for U+2610 BALLOT BOX or U+2611 BALLOT BOX WITH CHECK — they would rasterize as blanks. This is pinned by a test (`rich::tests::font_lacks_ballot_box_glyphs`), so the fallback cannot silently rot if the font is ever swapped.

Because the marker overwrites the prefix, **an ordered task item loses its number**: `1. [ ] t` and `- [ ] t` produce identical bitmaps.

### Code blocks

Fenced and indented blocks both render as Regular **20 px** at indent **16 px** (plus 24 px per enclosing blockquote). Line breaks are preserved exactly, one rendered line per source line, 26 px each. Tabs expand to four spaces. A line too long for the width wraps mid-word rather than clipping.

### Blockquotes

Indent +24 px per level, and the body renders italic. Nesting compounds: `> > x` sits at 48 px. Headings inside a quote keep their bold face and size and pick up the indent.

### Rules and tear markers

Both are 2 px tall with 12 px of white above and below (a 26 px block), full width, ignoring any indent.

| Source | Renders |
|---|---|
| `---`, `***`, `___`, `----` | Solid full-width bar |
| `- - -`, `* * *`, `_ _ _`, `-  -  -` | Dashed line: 8 px on, 8 px off, starting black at x = 0 |

The distinction is the *source text* of the thematic break: **any interior whitespace makes it a tear marker.** The intent is physical. A solid rule is a typographic divider inside the document; a dashed line is a cut guide — print several receipts in one job and tear them apart on the dashes. Nothing else in the dialect can express "the paper ends here", and thermal rolls have no page breaks.

The blockquote marker does not count as interior whitespace: `> ---` is still a solid rule.

### Tables

Tables are laid out as monospace text, not drawn boxes. The font makes the column arithmetic exact.

- Cells flatten to plain text. Bold, italic, strikethrough, inline code, and links inside a cell lose their formatting; an image inside a cell collapses to its `[image: alt]` placeholder text.
- Column width starts at the widest cell in that column (minimum 1).
- Columns are joined by a **2-space gutter**, left-justified, and the header is followed by a dashed separator row.
- The whole line must fit **32 characters** (a 20 px monospace line at 384 px). While it does not, the widest column is shrunk one character at a time, down to a floor of **3 characters**. A cell longer than its final width is truncated to `width − 1` characters plus `…`.
- Ragged rows are padded with empty cells; cells past the header count are dropped. A table with an empty header row renders nothing.
- **Alignment markers (`:---:`, `---:`) are ignored.** Everything is left-aligned.

The 3-character floor caps the column count. `cols × 3 + 2 × (cols − 1) ≤ 32` holds up to **six columns** (6 × 3 + 2 × 5 = 28). Seven or more overflow the budget even at the floor, and the rows word-wrap: every cell still prints, but the grid stops lining up. Dropping the extra columns would silently lose data, which is worse. Split or transpose a wider table.

```
| Item          | Qty | Price |
|---------------|-----|-------|
| Espresso      | 2   | 5.00  |
| Oat flat white| 1   | 4.20  |
```

renders as

```
Item            Qty  Price
--------------  ---  -----
Espresso        2    5.00
Oat flat white  1    4.20
```

## Graphic fences

A fenced code block whose info string's **first whitespace-separated token** is `qr` or `barcode` — compared case-insensitively, so ` ```QR ` and ` ```Barcode utf8 ` both match — renders as a graphic instead of code text. Every other info string, including none at all, stays plain code: `rust`, `qrcode`, `barcodes`, and `bar` are all just code.

Both fence kinds are stacked as their own block, padded to a uniform 24 px of white above and below.

### ` ```qr `

````markdown
```qr
https://example.com/order/42
```
````

The trimmed body is encoded with automatic version and error-correction selection, given a 4-module quiet zone, scaled by the largest integer factor that fits 384 px, and centred. Even a version-40 code (177 modules + 8 quiet = 185) scales by 2, so the code always fills most of the width. Block height is `side + 48` px: 425 px for a version-1 code, 418 px for a typical short URL.

An empty payload is valid (the encoder accepts it) and produces a version-1 code. There is no caption from markdown — captions exist only on `printable qr --caption` and the QR API/tab.

### ` ```barcode `

````markdown
```barcode
ORDER-42
```
````

Code128, **character set B**. Accepted characters are printable ASCII, U+0020 space through U+007E tilde: digits, both letter cases, and punctuation. You write plain text; the character-set escape Code128 needs is prepended for you.

The limit is **28 characters**. A Code128-B payload of *n* characters encodes to `11n + 35` modules (start, data, checksum, stop). Modules must be at least one pixel wide, and the budget is 384 px minus a 16 px quiet zone per side = 352 px. 28 characters need 343 modules; 29 need 354, one past the paper. The bars are 80 px tall, centred, with no human-readable text below. Block height is a flat 128 px.

Rejected payloads: empty or whitespace-only, anything outside printable ASCII (accents, emoji, tabs, newlines), and anything over the width budget.

### When a fence fails

A payload the encoder rejects prints its error message as code text (Regular 20 px at the 16 px indent), padded with the same 24 px margins a successful fence would have had:

| Failure | Printed message |
|---|---|
| QR payload too large | `data too long to fit in a QR code` |
| Barcode empty | `barcode data is empty (after trimming whitespace)` |
| Barcode non-ASCII | `barcode data must be printable ASCII` |
| Barcode too long | `barcode data too long to fit the paper` |

A bad code never panics and never costs the reader the rest of the document. The margins match on both branches deliberately: a fence is its own block, so the surrounding blank lines are trimmed, and unpadded error text would collide with the neighbouring paragraph.

## Images

```markdown
![alt text](https://example.com/logo.png)
```

The rendering core is sans-IO — it never opens a file or a socket. Images therefore resolve in two passes: `markdown_image_refs(md)` lists the destinations a document uses (in document order, deduplicated), the surface fetches what it can and decodes each to a bitmap, and `render_markdown_with(md, &images)` renders against that map.

### What each surface will fetch

| Surface | Local paths | `http(s)` URLs |
|---|---|---|
| CLI — `printable print -f notes.md` | Yes, relative to the `.md` file's directory (absolute paths as given) | Yes |
| Server — `/preview/markdown`, `/print/markdown` | **Never** | Yes, unless started with `--no-remote-images` |
| Web app — Markdown tab | No; a browser cannot read them | Yes, via `fetch`, subject to CORS |

The server's refusal is a security boundary, not an omission: it listens on the LAN by default, and without it `![x](/etc/hosts)` would let any client read files off the host. The resolver does not even stat the path when local access is off. `--no-remote-images` additionally removes the outbound fetch, leaving the server with no request surface at all (no SSRF, no fetch amplification).

### Rendering

A resolved image is decoded (PNG or JPEG), scaled to exactly 384 px wide with Lanczos3 preserving aspect ratio, clamped to 4096 rows (~0.5 m of paper), and dithered with **Floyd–Steinberg — always**, regardless of `--dither` or the API's `dither` field. Those knobs apply to a directly printed image (`-f photo.png`, `/print/image`, the Image tab); a document has no per-image control. It is then stacked as its own full-width block with 8 px of white above and below.

An unresolved reference renders an italic placeholder line at the surrounding indent, so it nests inside lists and quotes:

| Case | Placeholder |
|---|---|
| `![Cat](cat.png)` | `[image: Cat]` |
| `![](cat.png)` | `[image: cat.png]` |
| `![]()` | `[image: ?]` |

Alt-text markup is flattened: `![*a* b](x.png)` gives `[image: a b]`. A broken image never fails a print.

### Bounds

| Limit | Value | Applies to |
|---|---|---|
| Image references resolved per document | 32 | CLI, server, web |
| Whole-pass budget | 30 s | CLI, server (the web app has none; each fetch is bounded by the browser) |
| Per-fetch timeout | 15 s | CLI, server |
| Maximum download | 5 MB | CLI, server (checked against `Content-Length` *and* while streaming) |

Fetches are sequential on purpose — resolving them concurrently would multiply the outbound traffic one request can trigger. References past the cap, images that time out, 404, exceed the size limit, or fail to decode are simply left unresolved and get placeholders; the CLI and server warn on stderr, the web app reports a count in its toast.

Two kinds of reference are never reported and never fetched: an empty destination (`![]()`), and an image nested inside another image's alt text (`![a ![b](b.png)](a.png)` — the inner one is consumed as alt text).

## Not supported

Verified against the enabled parser options (`ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS | ENABLE_TABLES`):

| Feature | What happens instead |
|---|---|
| Footnotes (`[^1]`) | Not an extension here. `[^1]: note` is parsed as a CommonMark *link reference definition* and vanishes; the reference renders as the bare text `^1` |
| YAML / `+++` front matter | Not recognized. A leading `---` is a thematic break, and the block that follows usually becomes a setext H2 — strip front matter before printing |
| Definition lists | `term` / `  : def` renders as one wrapped paragraph |
| Math (`$…$`) | Renders literally |
| GFM alerts (`> [!NOTE]`) | Renders as a normal blockquote with the literal `[!NOTE]` text |
| Smart punctuation | Off. Quotes, `--` and `...` print as typed |
| Heading attributes (`# t {#id}`) | Printed as part of the heading text |
| HTML blocks | Dropped entirely, content and all: `<div>hi</div>` prints nothing |
| Inline HTML | Tags dropped, text kept: `a <b>c</b> d` renders `a c d` |
| Link destinations | Link text renders; the URL is discarded. Autolinks (`<http://x>`) render as their text. Use a ` ```qr ` fence to make a URL actionable |
| Table alignment, colspans, cell markup | Ignored / flattened (see Tables) |
| Colour, background, centred text | The canvas is 1-bit and left-aligned |

## Layout limitations worth knowing

- **Graphic and image blocks are full-width and never indented.** A QR fence, barcode fence, or resolved image inside a list item or blockquote interrupts the text flow and stacks at x = 0. The item's bullet still prints on its own line above it. Only the *placeholder* for an unresolved image respects the indent.
- **Images inside table cells collapse to placeholder text**, even when the surface resolved them. A cell is monospace text; nothing can stack inside it, and escaping the table would put the picture before the whole table (rows are still buffered) and leave the cell empty.
- **Tables wider than six columns lose their alignment** (see Tables).
- **The document has no page breaks.** Height is unbounded until the protocol's 16-bit packet index runs out at 131,070 rows.
- **Font size is fixed.** `--size` / the API's `size` apply to plain-text printing only; markdown always uses the sizes in this document.

## A complete example

````markdown
# Cafe Aurora

**Order 42** — *table 6* — ~~takeaway~~

## Items

| Item          | Qty | Price |
|---------------|-----|-------|
| Espresso      | 2   | 5.00  |
| Oat flat white| 1   | 4.20  |

### Notes

- [x] beans ground
- [ ] water boiled
  - filtered, 94 C

> Reheat within 20 minutes.

Pickup code:

```barcode
ORDER-42
```

```qr
https://example.com/order/42
```

```
tare: 312 g
net:  188 g
```

![receipt logo](logo.png)

---

Thanks! Tear below.

- - -
````

Printed, top to bottom:

1. `Cafe Aurora` in bold 36 px.
2. A 24 px paragraph reading `Order 42 — table 6 — takeaway`, with `Order 42` bold, `table 6` italic, and `takeaway` struck through. It runs past 26 characters, so it wraps after the second dash and `takeaway` prints struck on the second line.
3. `Items` in bold 30 px.
4. A four-line monospace table at 20 px: header, dashed separator, two rows. Columns are 14 / 3 / 5 characters wide with 2-space gutters — 26 characters total, comfortably inside the budget.
5. `Notes` in bold 26 px.
6. Three list lines: `[x] beans ground` and `[ ] water boiled` at indent 24, then `• filtered, 94 C` at indent 48.
7. `Reheat within 20 minutes.` in italic at indent 24.
8. `Pickup code:` as a normal paragraph.
9. An 80 px Code128 barcode, centred, with 24 px of white above and below.
10. A QR code filling most of the width, centred, same 24 px margins — 418 px tall for this payload.
11. Two 20 px code lines at indent 16, line breaks intact.
12. `[image: receipt logo]` in italic 24 px, because `logo.png` was not found next to the document (the CLI warns on stderr). Had it resolved, a full-width dithered image would appear here instead.
13. A solid 2 px rule.
14. `Thanks! Tear below.`
15. A dashed 2 px tear guide.

Render it yourself without touching the printer:

```
printable print -f example.md --preview example.png
```
