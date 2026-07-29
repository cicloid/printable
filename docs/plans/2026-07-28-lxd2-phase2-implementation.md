# lxd2 Phase 2 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Markdown printing, QR codes, `--copies`, and a config file that remembers the printer, on top of the completed phase 1.

**Architecture:** All rendering stays in sans-IO `lxd2-core`: a new styled-text renderer (multiple fonts/sizes per line) that the markdown module lowers into, and a QR module producing `Bitmap`s. The CLI gains `.md` handling, a `qr` subcommand, `--copies` (one BLE connection, one `PrintJob` per copy), and a small TOML config for device memory.

**Tech Stack:** `pulldown-cmark` 0.12 (default features off), `qrcode` 0.14 (default features off), `serde`/`toml`/`dirs` in the CLI crate only. JetBrains Mono Bold + Italic TTFs embedded alongside the existing Regular.

**State:** Phase 1 complete at `d75d348`: `lxd2-core` (protocol + bitmap/dither/text/preview, re-exports in `raster::`), `lxd2` CLI (scan/status/print, preview, exit codes 2/3/4). 43 tests green. Design doc: `docs/plans/2026-07-27-lxd2-design.md`.

---

### Task 1: Styled text renderer (rich.rs)

**Files:**
- Create: `crates/lxd2-core/src/raster/rich.rs`; wire `pub mod rich;` + re-exports into `raster/mod.rs`
- Add fonts: `crates/lxd2-core/assets/JetBrainsMono-Bold.ttf`, `JetBrainsMono-Italic.ttf` (same v2.304 release zip as Regular; OFL.txt already present)
- Modify: `crates/lxd2-core/src/raster/text.rs` — refactor to delegate to rich.rs (keep `render_text` signature)

**API:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle { Regular, Bold, Italic }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub font: FontStyle,
    pub size_px: f32,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// One logical line (may wrap to several rendered lines).
#[derive(Debug, Clone, Default)]
pub struct RichLine {
    pub spans: Vec<Span>,
    /// Left indent in pixels (lists, blockquotes, code).
    pub indent: u32,
}

pub fn render_rich(lines: &[RichLine]) -> Bitmap
```

Behavior: greedy word-wrap at 384 px minus indent; wrapped continuation lines keep the indent; per-rendered-line height = 1.3 × max size on that line, baseline = max ascent; mixed styles on one line share the baseline. Empty `lines` → height 0. An empty RichLine (no spans) renders as a blank line of its indent's default… simplest: height 1.3 × 24 (document it). `render_text(text, size)` becomes a thin wrapper: split on `\n` → one RichLine per line with a single Regular span (normalization stays).

**Tests (TDD, write first):** bold span renders differently from regular (compare ink patterns of "Hello" regular vs bold — assert bitmaps differ and both have ink); mixed-size line uses one baseline (render "Ag" 24px + "Ag" 36px in one line; assert height ≈ 1.3×36 rounded, and both have ink); indent shifts ink right (min ink x ≥ indent); wrap respects indent (long text with indent 40 wraps into more lines than indent 0… simpler: assert no ink in columns < indent on any row); existing `render_text` tests keep passing unchanged.

Steps: red → implement → green → full suite → clippy → commit `"Add styled text renderer with bold and italic fonts"`.

### Task 2: Markdown → bitmap

**Files:**
- Create: `crates/lxd2-core/src/raster/markdown.rs`; wire + re-export `render_markdown`
- Modify: `crates/lxd2-core/Cargo.toml` — `pulldown-cmark = { version = "0.12", default-features = false }`

**API:** `pub fn render_markdown(md: &str) -> Bitmap` — lowers markdown to `Vec<RichLine>` then `render_rich`.

Mapping (YAGNI — this list only):
| Markdown | Style |
|---|---|
| H1 / H2 / H3+ | Bold 36 / 30 / 26 px, blank line before+after |
| Paragraph | Regular 24 px, blank line after |
| **bold** / *italic* | Bold / Italic span, inherits size |
| `inline code` | Regular (monospace anyway) — pass through |
| Bullet list item | indent 24, `• ` prefix span |
| Ordered list item | indent 24, `N. ` prefix |
| Nested list | +24 indent per level |
| Fenced/indented code block | Regular 20 px, indent 16, preserve line breaks |
| Blockquote | indent 24, Italic |
| Horizontal rule | full-width black line 2 px tall with 12 px margins (draw directly on a marker RichLine — see note) |
| Soft/hard break | new RichLine |

Note on HR: simplest is a special-case: lower to a sentinel (e.g. `RichLine` with a span text `"\u{0}HR"`)… NO — cleaner: make the lowering produce `Vec<MdBlock>` where `MdBlock::Lines(Vec<RichLine>)` or `MdBlock::Rule`, render blocks sequentially into one Bitmap by stacking sub-bitmaps (add a small private `stack(bitmaps: Vec<Bitmap>) -> Bitmap` helper). Tables/images/links: render inner text only (link text without URL), skip images.

**Tests:** heading is taller than body text (render `# Hi` vs `Hi`, compare heights); bold emphasis produces different ink than plain; list items indented (no ink left of indent); code block preserves blank-line-free stacking; HR produces a full-width run of black pixels; empty string → height 0; plain paragraph wraps.

Steps: red → implement → green → clippy → commit `"Add markdown rendering"`.

### Task 3: QR codes

**Files:**
- Create: `crates/lxd2-core/src/raster/qr.rs`; wire + re-export
- Modify: `crates/lxd2-core/Cargo.toml` — `qrcode = { version = "0.14", default-features = false }`

**API:** `pub fn render_qr(data: &str, caption: Option<&str>) -> Result<Bitmap, QrError>` — `qrcode::QrCode::new(data)` (auto version/EC), scale each module by the largest integer factor fitting 320 px (leave quiet zone), center horizontally in 384, 16 px white margin top/bottom; caption rendered below via `render_text` at 24 px, centered is not supported by render_text → left-aligned is fine (document). `QrError` wraps data-too-long.

**Tests:** small payload renders square ink region ≥ 100 px wide with quiet margins (no ink in x<16); caption adds height vs no caption; huge payload (> 3 KB) returns Err; roundtrip sanity — decode is out of scope (no decoder dep), instead assert finder pattern: top-left 7-module square has black at its corners… keep simple: assert some ink and correct symmetry of the three finder corners (ink at mirrored coordinates).

Steps: red → implement → green → clippy → commit `"Add QR code rendering"`.

### Task 4: Config file + device memory

**Files:**
- Create: `crates/lxd2/src/config.rs`
- Modify: `crates/lxd2/Cargo.toml` — add `serde = { version = "1", features = ["derive"] }`, `toml = "0.8"`, `dirs = "5"`
- Modify: `crates/lxd2/src/ble.rs`, `main.rs`

**API:**
```rust
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub device: Option<SavedDevice>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice { pub id: String, pub name: String }

impl Config {
    pub fn path() -> Option<PathBuf>       // dirs::config_dir()/lxd2/config.toml
    pub fn load() -> Config                 // missing/corrupt file → Default (corrupt: eprintln warning)
    pub fn save(&self) -> anyhow::Result<()> // create dirs, write TOML
}
```

Behavior in `connect()` flow (main.rs): resolution order = `--device` flag > saved device id (try connect by id; on failure fall back to scan-by-name) > first `LX*`. After any successful connect, save id+name back to config (only if changed). `Printer` needs to expose its peripheral id as a string (add method).

**Tests:** config unit tests with a temp dir (redirect path via `#[cfg(test)]` helper or make `load_from(path)`/`save_to(path)` the testable core, `load()/save()` thin wrappers): roundtrip save→load; corrupt TOML → Default; missing file → Default. BLE resolution order is manual-verify only.

Steps: red → implement → green → clippy → commit `"Add config file with device memory"`.

### Task 5: CLI wiring — .md files, qr subcommand, --copies

**Files:**
- Modify: `crates/lxd2/src/cli.rs`, `main.rs`

Changes:
- `print -f notes.md` (or `.markdown`) → `render_markdown(&contents)`; stdin/text-arg stay plain text
- New subcommand: `Qr { data: String, #[arg(long)] caption: Option<String>, #[command(flatten)] device: DeviceArgs, #[arg(long, default_value_t = 3)] density: u8 (range 1-7), #[arg(long, default_value_t = 40)] feed: usize, #[arg(long)] preview: Option<PathBuf>, #[arg(long, default_value_t = 1)] copies: u16 (range 1..=20) }` — share a common print-dispatch helper with `cmd_print` (bitmap → preview-or-print pipeline) instead of duplicating it
- `--copies N` on `Print` too (default 1, range 1..=20): one connection, then N sequential `PrintJob::new(...)` runs (fresh random challenge each; auth re-runs per job — acceptable; the printer expects a full session per job)
- Feed rows appended once per copy (each job carries its own feed)

**Verify (no hardware):** `--preview` for: a markdown file exercising headings/lists/code/hr/bold; `lxd2 qr "https://example.com" --caption "scan me" --preview qr.png`; eyeball both PNGs. `--copies 0` rejected by clap. `--help` sane. Full suite + clippy.

Steps: implement → verify → commit `"Add markdown printing, qr command, and copies"`.

### Task 6: Wrap-up

- `cargo fmt --all`, `cargo clippy --workspace --all-targets`, full test suite
- README: add markdown/QR examples, `--copies`, config-file section (path, what's stored, how to reset)
- Commit `"Update README for markdown, QR, and config"`

---

## Notes for the implementer

- Core stays sans-IO: `pulldown-cmark`/`qrcode` are pure — fine in core; `serde`/`toml`/`dirs` are CLI-only.
- Do not break `render_text`'s public signature — text.rs delegates to rich.rs internally.
- Fonts: verify SHA of downloaded TTFs against the official v2.304 release like phase 1 did.
- Markdown inline code/links render as plain text — do not add syntax highlighting or link footnotes (YAGNI).
- One BLE connection for all copies; a fresh PrintJob (and auth) per copy.
- Commit messages: never mention Claude.

## Post-review addenda

- **Atkinson dithering**: `--dither` now accepts `floyd|atkinson|threshold` (`none` aliases `threshold`), matching the design doc's CLI sketch. Floyd–Steinberg and Atkinson share one kernel-parameterized error-diffusion helper in `dither.rs`.
- **Saved-device fallback**: when the saved id is not seen before the scan deadline, connect now prefers a device advertising the saved *name* over any other `LX*` device (ranked fallback in `ble.rs`).
