# lxd2 Phase 5 Implementation Plan — Markdown Extensions

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Markdown gains embedded QR codes, barcodes, images (local + remote), task-list checkboxes, strikethrough, tables, and a tear marker — across CLI, server, and web.

**Architecture:** All rendering stays in sans-IO `lxd2-core`. Images use a two-pass design: `markdown_image_refs(md)` lists refs, each surface fetches bytes its own way (CLI: files + HTTP; server: HTTP only; web: browser fetch), then `render_markdown_with(md, &images)` renders with a `HashMap<String, Bitmap>`. Fences (`qr, `barcode) and the tear marker are pure core. The lowering switches to `Parser::into_offset_iter` so the tear marker can inspect rule source text.

**Tech Stack:** pulldown-cmark extensions (TABLES, STRIKETHROUGH, TASKLISTS), `barcoders` (pure Rust, no default features) for Code128, `reqwest` 0.12 (rustls, no default features) in the CLI crate for remote images.

**State:** Phases 1-4 complete at `8387ddd`, 109 tests. Key files: `crates/lxd2-core/src/raster/{markdown,rich,qr}.rs`, `crates/lxd2/src/{main,server,print_service}.rs`, `crates/lxd2-web/src/lib.rs`, `web/app.js`.

---

### Task 1: Core — strikethrough, checkboxes, tear marker

- `rich.rs`: `Style` gains `pub strike: bool` (breaking for struct literals — check all constructors; add `Style::new(font, size)` helper or `..Default::default()` pattern; `Default` for Style = Regular 24 false). Rendering: after blitting each glyph of a struck span, draw a 2px horizontal line across that glyph's advance at ~0.35 × size above baseline. Test: struck text differs from plain; a struck span has a continuous black run at strike height.
- `markdown.rs`: enable `Options::ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS` (+ ENABLE_TABLES now, used in Task 2). Strikethrough event → strike style (composes with bold/italic). TaskListMarker(checked) → prefix span `"[x] "` / `"[ ] "` — UNLESS JetBrains Mono has ☐ U+2610 / ☑ U+2611: check via fontdue `lookup_glyph_index != 0` at runtime… decide at implementation time with a quick test; prefer the real glyphs if present, else ASCII fallback, and PIN the choice in a test.
- Tear marker: switch lowering to `Parser::new_ext(md, opts).into_offset_iter()`; on `Tag/Event::Rule`, slice the source range — if the trimmed source contains an interior space (`- - -`, `* * *`), lower to `MdBlock::Tear` (dashed: repeating 8px-on/8px-off 2px line, same margins as Rule); else `MdBlock::Rule` as today. Tests: `---` solid (existing test still passes), `- - -` produces dashed (assert alternating runs on the line row), checkbox tests, strikethrough test.
- Commit: `"Add strikethrough, checkboxes, and tear marker to markdown"`.

### Task 2: Core — tables

- Monospace layout (the font is monospace — char-count math is exact): collect table cells as plain text (inline styling flattened), compute per-column max char width, total = cols + separators (`|` → 3 chars, no outer borders... simpler: `col1  col2` two-space gutters). Budget: code-style 20px → advance ≈ 12px → 32 chars/line. If total exceeds budget, shrink widest columns (truncate cells with `…`). Render as code-block-style lines (Regular 20px, indent 0) with a full-width underline row after the header (use box-drawing `─`? — NO, draw a thin rule bitmap line via a Rule-like block scaled to table width… simplest: a text row of `-` chars per column). Alignment: left only (ignore alignment markers). Tests: 2-col table renders (ink), header separator row present, overwide cells truncated with …, table wider than budget still ≤384px ink.
- Commit: `"Add table rendering to markdown"`.

### Task 3: Core — qr and barcode fences

- `markdown.rs`: fenced code blocks with info string `qr` → `MdBlock::Qr(String)` (trimmed content); `barcode` → `MdBlock::Barcode(String)`. Render: Qr → `qr::render_qr(data, None)` centered (already 384-wide) — on error, render the error message as code-style text instead (document; a bad QR shouldn't kill the whole doc). Barcode: `barcoders` crate, Code128 — content charset limited; on encode error render error text. Bars: height 80px, module width = max integer scale fitting 384 − 2×16px quiet margins, centered; data text NOT printed below (YAGNI).
- Cargo.toml (core): `barcoders = { version = "2", default-features = false }`.
- Tests: qr fence produces scannable-shaped block (finder corners like qr.rs tests); qr fence with 4000 chars renders error text not panic; barcode fence renders vertical bars (columns with tall black runs); invalid barcode chars → error text; normal ``` code fence unaffected.
- Commit: `"Add qr and barcode fences to markdown"`.

### Task 4: Core — images in markdown

- `pub fn markdown_image_refs(md: &str) -> Vec<String>` — parse (same Options), collect `Tag::Image` dest URLs in order, deduped.
- `pub fn render_markdown_with(md: &str, images: &HashMap<String, Bitmap>) -> Bitmap` — Image tag → look up dest: hit → `MdBlock::Image(Bitmap)` (already 384-wide, caller's responsibility via prepare+dither) stacked with 8px margins; miss → italic placeholder line `[image: <alt or dest>]`. `render_markdown(md)` delegates to `render_markdown_with(md, &HashMap::new())` — existing behavior changes: previously images were skipped SILENTLY; now missing → placeholder. Update the old swallow-test accordingly (intentional, document).
- Tests: refs extraction (order, dedupe, empty), render with a supplied bitmap stacks it (height grows by image height + margins), missing ref → placeholder ink, alt-text used.
- Commit: `"Add image support to markdown rendering"`.

### Task 5: CLI + server wiring

- New `crates/lxd2/src/md_images.rs`: `pub async fn resolve(md: &str, base_dir: Option<&Path>, allow_local: bool) -> HashMap<String, Bitmap>` — for each ref: `http(s)://` → reqwest GET (timeout 15s, cap 5 MB via content-length + streamed limit, non-2xx → skip w/ eprintln warning) → `bitmap_from_image_bytes(bytes, Dither::FloydSteinberg)`; else if allow_local → read path relative to base_dir → same; failures warn + skip (→ placeholder). Cargo: `reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }`.
- CLI `build_bitmap` md arm: `resolve(md, md_file_parent_dir, allow_local: true)` → `render_markdown_with`. stdin markdown? stdin is plain text (unchanged) — only `-f x.md` hits this.
- Server `/print/markdown` + `/preview/markdown`: `resolve(md, None, allow_local: false)` — LOCAL FILES MUST STAY OFF (LAN callers must not read server filesystem — document as security boundary + test: a markdown body with `![x](/etc/hosts)` previews with placeholder, add oneshot test asserting 200 + no panic; can't assert file not read directly — assert placeholder present is enough via… PNG can't be inspected easily; just assert 200 and add a unit test on resolve() that allow_local=false never touches fs (pass a path, expect empty map)).
- Tests: resolve unit tests (local file happy path w/ tempdir, allow_local=false skips paths, bad URL skips), server oneshot as above. Existing suites green.
- Commit: `"Resolve markdown images in CLI and server"`.

### Task 6: Web + wrap-up

- `lxd2-web`: export `markdown_image_refs(md) -> Vec<String>` (wasm-bindgen: `Vec<String>` → `string[]`); `render_markdown_with_images(md: &str, names: Vec<String>, images: Vec<js_sys::Uint8Array>… ` — avoid js-sys if possible: accept `names: Vec<String>`, `buffers: JsValue`?? SIMPLEST: `add_image(&mut self…)` no — do: `render_markdown_with_images(md, names: Vec<String>, concat: &[u8], lengths: Vec<u32>, dither: &str) -> Result<WasmBitmap, String>` is ugly. Cleaner: a small builder: `#[wasm_bindgen] pub struct ImageSet { … } impl { new(), add(name, bytes: &[u8], dither: &str) -> Result<(), String>, }` then `render_markdown_with(md, &ImageSet)`. Choose the builder. Decode+dither inside `add`.
- `web/app.js`: markdown preview/print path: `markdown_image_refs(text)` → for http(s) refs `fetch` (try/catch, CORS failures → skip w/ toast note), build ImageSet, render. Non-http refs skipped (→ placeholder).
- README: document all extensions with a markdown example block (qr/barcode fence syntax, `- - -` tear, checkboxes, tables, images incl. the server no-local-files rule and web CORS caveat).
- Full sweep: fmt, clippy (workspace + wasm target), all tests, build-web.sh, Playwright smoke of the web markdown tab with a qr fence + checkbox doc. Hardware validation at the end (one kitchen-sink print).
- Commit: `"Add markdown image support to web app"` + `"Update README for markdown extensions"`.

---

## Notes for the implementer

- `Style` gaining a field breaks struct literals in markdown.rs/tests — prefer `Style::new()` + `with_strike()` or `..Style::default()` to keep future fields cheap.
- Tear detection depends on `into_offset_iter` — keep the lowering readable; the offset is only consulted for Rule events.
- barcoders: verify its Code128 API (it may require charset prefix like `\u{0181}` for charset B — READ ITS DOCS; encode("...") returns Vec<u8> of 0/1 columns).
- Server allow_local=false is a security boundary — comment it as such.
- reqwest must not leak into lxd2-core or lxd2-web.
- Commit messages: never mention Claude.
