# The printa-ble markdown dialect

Reference for the markdown renderer in `crates/printa-ble-core/src/raster/markdown.rs`. The same renderer backs every surface: `printable print -f notes.md`, the server's `/preview/markdown` and `/print/markdown`, and the web app's Markdown tab. Output is always a 384 px wide, 1-bit bitmap — the width of 58 mm paper at 203 dpi.

Parsing is [pulldown-cmark](https://docs.rs/pulldown-cmark) 0.12 (CommonMark) with exactly three extensions enabled: strikethrough, task lists, and tables. Everything else in CommonMark parses; the lowering then maps each event to a bitmap block or drops it.

## Reaching the markdown renderer

The renderer is not the default for text — plain text is. Which path input takes depends on the surface:

| Input | Renders as |
|---|---|
| `printable print -f notes.md` / `.markdown` | Markdown, by extension |
| `printable print -m …` | Markdown, forced — for stdin, a text argument, `--file -`, or a `.txt` file |
| `printable print` with anything else | **Plain text**, literal source and all |
| `POST /print/markdown`, `/preview/markdown` | Markdown |
| The web app's Markdown tab | Markdown |

Piping a document without `-m` prints its `#` and `**` verbatim. See [CLI.md](CLI.md#-m---markdown) for what `-m` accepts and rejects, and where relative image references anchor in each case.

## The canvas

| Property | Value |
|---|---|
| Width | 384 px, fixed |
| Colour | 1 bit; a glyph pixel is black at ≥ 128/255 coverage |
| Font | JetBrains Mono (Regular / Bold / Italic), embedded in the binary. Glyph coverage and any fallback are described in the crate's `raster/rich.rs` |
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

Indent +24 px per level, and the body renders italic. No vertical bar is drawn — the indent and the italic face are the whole treatment. Nesting compounds: `> > x` sits at 48 px. Headings inside a quote keep their bold face and size and pick up the indent, and so does `**bold**` (see [Emphasis](#emphasis)).

### Rules and tear markers

Both are 2 px tall with 12 px of white above and below (a 26 px block), full width, ignoring any indent.

| Source | Renders |
|---|---|
| `---`, `***`, `___`, `----` | Solid full-width bar |
| `- - -`, `* * *`, `_ _ _`, `-  -  -` | Dashed line: 8 px on, 8 px off, starting black at x = 0 |

The distinction is the *source text* of the thematic break: **any interior whitespace makes it a tear marker.** The intent is physical. A solid rule is a typographic divider inside the document; a dashed line is a cut guide — print several receipts in one job and tear them apart on the dashes. Nothing else in the dialect can express "the paper ends here", and thermal rolls have no page breaks.

The blockquote marker does not count as interior whitespace: `> ---` is still a solid rule.

**`- ---` is a tear marker, not a list item.** It looks like a rule nested in a bullet, but CommonMark reads the whole line as four `-` characters separated by a space — a thematic break — and the space makes it dashed. It renders byte-identical to `- - -`. A break that genuinely *is* inside a list item (`- ***`, where the characters differ so the outer `-` really is a bullet) comes out **solid** instead, because the nested path never sees the source text and cannot detect interior whitespace. Two lines that read the same way to a person produce opposite results; write `- - -` when you mean a tear.

### Tables

Tables are laid out as monospace text, not drawn boxes. The font makes the column arithmetic exact.

Every width below is a **display column**, not a character. A full-width East-Asian character — CJK ideographs and their compatibility forms, kana, CJK punctuation, hangul syllables, and the fullwidth halves of Halfwidth and Fullwidth Forms — counts as two; everything else counts as one. Counting raw characters would let a Japanese cell overrun its column and push every column to its right out of line with the ASCII rows.

- Cells flatten to plain text. Bold, italic, strikethrough, inline code, and links inside a cell lose their formatting; an image inside a cell collapses to its `[image: alt]` placeholder text.
- Column width starts at the widest cell in that column (minimum 1).
- Columns are joined by a **2-space gutter**, left-justified, and the header is followed by a dashed separator row.
- The whole line must fit **32 display columns** (a 20 px monospace line at 384 px). While it does not, the widest column is shrunk one column at a time, down to a floor of **3**. A cell wider than its final width is truncated to `width − 1` columns plus `…`.
- **Truncation never splits a full-width character.** A character is kept only if it fits whole, so a cut that would land mid-glyph drops it and the leftover column becomes padding — a cell can therefore come out one column short of its budget rather than half a glyph over it.
- Ragged rows are padded with empty cells; cells past the header count are dropped. A table with an empty header row renders nothing.
- **Alignment markers (`:---:`, `---:`) are ignored.** Everything is left-aligned.

The 3-column floor caps the column count. `cols × 3 + 2 × (cols − 1) ≤ 32` holds up to **six columns** (6 × 3 + 2 × 5 = 28). Seven or more overflow the budget even at the floor, and the rows word-wrap: every cell still prints, but the grid stops lining up. Dropping the extra columns would silently lose data, which is worse. Split or transpose a wider table.

Column *alignment* is exact; column *pixel* positions are close but not exact when CJK is involved. The mono advance is 0.6 em and a CJK glyph advances 1 em, not 1.2, so each full-width character in a cell pulls the columns after it about 4 px left at 20 px. Two columns of drift per Japanese cell is visible if you look for it and invisible if you do not; counting characters instead, as the layout used to, drifts twice as far in the other direction.

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

A fenced code block whose info string's **first whitespace-separated token** is `qr`, `barcode`, or `wagara` — compared case-insensitively, so ` ```QR ` and ` ```Barcode utf8 ` both match — renders as a graphic instead of code text. Every other info string, including none at all, stays plain code: `rust`, `qrcode`, `barcodes`, and `bar` are all just code.

Each fence kind is stacked as its own block. `qr` and `barcode` are padded to a uniform 24 px of white above and below; a `wagara` band is a separator and takes the same padding.

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

### ` ```wagara `

Draws a traditional Japanese pattern (和柄, *wagara*) as a full-width decorative band — a separator with more character than a rule. All the motifs are centuries old and long out of copyright; nothing here traces an existing drawing.

````markdown
```wagara seigaiha
height: 72
scale: 2
```
````

The pattern name comes from the info string's **second token**. Failing that, it comes from the body's **first non-empty line** — but only if that line contains no `:`, so a fence that forgot its name reports the missing name rather than blaming the first option:

````markdown
```wagara
asanoha
height: 40
```
````

Ten patterns are drawn. Names are matched case-insensitively. Two of the aliases are romanisations that differ only in a long vowel (`shippo`, `kikko`); the other two are a second Japanese name for the same motif — the fletching band is `yagasuri` (矢絣) after the weave or `yabane` (矢羽根) after the feather, and 立涌 is read `tatewaku` in modern usage but `tachiwaki` in the court vocabulary it comes from.

| Name | Kanji | Motif | Also accepts | Ink |
|---|---|---|---|---|
| `asanoha` | 麻の葉 | Hemp-leaf star lattice | | 37% |
| `ichimatsu` | 市松 | Checkerboard | | **50%** |
| `kanoko` | 鹿の子 | Fawn spots — the ring-and-speck dapple a shibori tie-dye leaves behind | | 22% |
| `kikkou` | 亀甲 | Tortoise-shell hexagons | `kikko` | 14% |
| `sayagata` | 紗綾形 | Key fret — a linked lattice of 卍 forms | | 31% |
| `seigaiha` | 青海波 | Overlapping fans, "blue sea waves" | | 32% |
| `shippou` | 七宝 | Interlocking circles, "seven treasures" | `shippo` | 25% |
| `tatewaku` | 立涌 | Rising steam — paired curves swelling and narrowing | `tachiwaki` | 19% |
| `uroko` | 鱗 | Fish scales — solid triangles, alternate rows offset and inverted | | **50%** |
| `yagasuri` | 矢絣 | Arrow fletching | `yabane` | **44%** |

The remaining body lines are `key: value` options. Blank lines are ignored, keys are case-insensitive, and spacing is free.

| Option | Range | Default | Effect |
|---|---|---|---|
| `height` | 16–400 | 56 | Band height in pixels |
| `scale` | 1–4 | 1 | Motif size multiplier |

Anything else is an error rather than a silent default — a band that quietly ignored `heigth: 80` would just look wrong with no way to tell why.

**Three patterns are half solid ink.** The `Ink` column is the share of the band a pattern blacks in at the default height and scale (a test pins every pattern into a 10–52% window). `ichimatsu` and `uroko` are 50% *by construction* — a checkerboard and a field of solid scales are half-and-half, that is the motif — and `yagasuri` is 44%, its chevrons broken only by the hairline left for each arrow's shaft. The line patterns sit between 14% and 37%. A thermal head lays down exactly what it is told, so coverage is also the band's cost in heat, paper darkening and battery life. Print those three a step or two below your usual `--density`; a 50% band at `--density 6` is a lot of black for a separator.

**`height` changes some patterns more than others.** `ichimatsu`, `tatewaku`, `uroko` and `yagasuri` have a free vertical rhythm, so the band's height is divided by the whole number of repeats nearest the traditional proportion and the repeat *count* follows `height` directly:

| Pattern | Repeats at `height: 56` | at `100` | at `120` |
|---|---|---|---|
| `ichimatsu` | 2 | 4 | 5 |
| `tatewaku` | 2 | 3 | 4 |
| `uroko` | 3 | 5 | 6 |
| `yagasuri` | 2 | 3 | 4 |

At the 56 px default `uroko`, `yagasuri` and `tatewaku` get two or three repeats, which reads as a crop of a pattern rather than as the pattern. Give them 100–120 px:

````markdown
```wagara uroko
height: 120
```
````

The lattice patterns — `asanoha`, `kikkou`, `shippou`, `sayagata`, `kanoko` — cannot be divided that way without shearing, so they centre a row on the band and a taller band simply shows more of the same lattice.

**How the tiling works.** A band is a separator, so it must run edge to edge with no margin and no half-eaten motif at the paper's edge. Every pattern picks a horizontal period that divides 384 exactly and draws one motif past each edge, so the rendered band is genuinely periodic: column *x* equals column *x + period*. Arcs and diagonals are drawn into a 3× oversampled buffer and collapsed by majority vote, so a stroke lands within a third of a pixel of where the maths puts it; strokes are 2 px on paper — thin enough to read as a pattern, heavy enough to survive a thermal head. `seigaiha` is the only pattern whose motifs overlap, and each row erases its own half-discs before stroking its arcs, painter's-algorithm style; without that the arcs cross and the pattern reads as noise.

**Known wart: `scale` is quantised.** Because the period must divide 384, `scale` only nudges a target motif width and the nearest usable count wins. For coarse patterns that means `scale: 3` and `scale: 4` can land on the same count and render **identically**. A large `scale` at the default 56 px `height` also crops the motif — you see one horizontal slice of a shape that wants far more room. Raise `height` alongside `scale`, and check the result with `--preview`.

### When a fence fails

A payload the encoder rejects prints its error message as code text (Regular 20 px at the 16 px indent), padded with the same 24 px margins a successful fence would have had:

| Failure | Printed message |
|---|---|
| QR payload too large | `data too long to fit in a QR code` |
| Barcode empty | `barcode data is empty (after trimming whitespace)` |
| Barcode non-ASCII | `barcode data must be printable ASCII` |
| Barcode too long | `barcode data too long to fit the paper` |
| Wagara unknown pattern | `unknown wagara pattern "…" (valid: asanoha, ichimatsu, kanoko, kikkou, sayagata, seigaiha, shippou, tatewaku, uroko, yagasuri)` |
| Wagara malformed option | `wagara option "…" is not a \`key: value\` line (valid keys: height, scale)` |
| Wagara unknown option | `unknown wagara option "…" (valid: height, scale)` |
| Wagara non-numeric value | `wagara height must be a whole number, got "…"` |
| Wagara value out of range | `wagara scale must be between 1 and 4, got 9` |

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
| Footnotes (`[^1]`) | Not an extension here, and what happens depends on the definition — see [Gotchas](#gotchas) |
| YAML / `+++` front matter | Not recognized, and a trap — see [Gotchas](#gotchas) |
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

## Gotchas

Places where valid markdown prints something a reader would not predict. All of these were confirmed by rendering, not by reading the code.

### Footnotes can silently disappear

Footnotes are not an enabled extension, so `[^1]` is just a link label. What that produces depends entirely on whether the "definition" happens to parse as a CommonMark **link reference definition**:

| Source | What prints |
|---|---|
| `See note[^1]` + `[^1]: https://example.com/note` | `See note^1` — the definition **vanishes** and the marker loses its brackets. Byte-identical to typing `See note^1`. |
| `See note[^1]` + `[^1]: The note body.` | `See note[^1]` then `[^1]: The note body.` as an ordinary paragraph — prose after the colon is not a valid link destination, so nothing is consumed |
| `See note[^1]` alone | `See note[^1]`, marker and all |

The first row is the dangerous one: a URL-shaped footnote is eaten whole, with no warning and no visible gap. There is no superscripting, no renumbering, and no collected footnote section in any case. Move the content inline before printing.

### Front matter is a trap

YAML front matter is not recognized, and it does not render as text either — it renders as *layout*:

```
---
title: Hi
---

Body text.
```

The opening `---` becomes a solid horizontal rule, and `title: Hi` followed by the closing `---` is a **setext H2**, so the document opens with a rule and a large bold heading reading `title: Hi`. That output is byte-identical to writing `---`, then `## title: Hi`, then the body. Strip front matter before printing.

### `- ---` is a tear marker, not a nested rule

Covered under [Rules and tear markers](#rules-and-tear-markers): CommonMark reads it as a thematic break with interior whitespace, so it comes out dashed. A break that really is inside a list item comes out solid.

### Code blocks escape their list item

A fenced or indented code block inside a list item does **not** inherit the list indent — only blockquote depth adds to it. The block sits at the flat 16 px code indent, which is *further left* than the bullet's text at 24 px, so the code visibly dedents out from under its own bullet. Nesting inside a blockquote works as expected: 24 px per level, on top of the code indent.

### Blockquotes are italic, and bold overrides that

A blockquote body is set in the italic face rather than marked with a bar, so `*emphasis*` inside a quote is invisible — it renders byte-identical to the unmarked text. `**bold**` inside a quote comes out upright bold, not bold-italic, because a span gets exactly one face. See [Emphasis](#emphasis).

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
