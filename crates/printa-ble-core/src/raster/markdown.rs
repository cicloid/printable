//! Markdown → 1-bit bitmap rendering, lowered onto the rich-text renderer.
//!
//! Supported mapping (anything else renders as its inner text or is skipped):
//! headings (bold 36/30/26 px, blank line before and after), paragraphs
//! (regular 24 px, blank line after), bold/italic emphasis, inline code
//! (passthrough — the font is monospace anyway), bullet and ordered lists
//! (indent 24 px per nesting level, `• ` / `N. ` prefixes), fenced and
//! indented code blocks (regular 20 px, indent 16 px, exact line breaks
//! preserved), blockquotes (indent 24 px, italic), and horizontal rules
//! (full-width 2 px bar with 12 px margins), strikethrough (`~~text~~`, a
//! 2 px line through the text), task-list checkboxes (`- [ ]` / `- [x]`,
//! rendered as ASCII `[ ]` / `[x]` markers — JetBrains Mono has no ballot-box
//! glyphs), and a tear marker: a thematic break written with interior spaces
//! (`- - -`) renders as a dashed line (8 px on / 8 px off) instead of a solid
//! rule. Links render their inner text only; raw HTML is skipped.
//! Tables render as monospace text blocks (code-style Regular 20 px): each
//! cell's inline content flattens to plain text (bold/italic dropped), columns
//! are padded to their widest cell with two-space gutters, a dashed separator
//! row follows the header, and over-wide tables shrink their widest columns —
//! truncating cells with `…` — to fit the 384 px roll. Left-aligned only;
//! markdown alignment markers are ignored. The shrink has a floor (3 chars per
//! column), so the roll fits **at most six columns**: seven or more need more
//! than the 32-char budget even at the floor, and the rows word-wrap instead,
//! losing column alignment. Nothing panics and no ink leaves the paper — the
//! table just stops being a table.
//!
//! Two fence names turn a code block into a graphic instead of text, matched
//! case-insensitively on the info string's first word (so ` ```QR ` and
//! ` ```barcode utf8 ` both count): ` ```qr ` encodes its trimmed body as a QR
//! code, ` ```barcode ` as a Code128 barcode (printable ASCII only, 28
//! characters max — see [`barcode`](super::barcode)). Every other info string,
//! including none at all, still renders as plain code text. A payload the
//! encoder rejects prints its error message as code text: a bad code never
//! panics or costs the reader the rest of the document.
//!
//! Images are rendered in two passes, because this crate is sans-IO and never
//! fetches anything: [`markdown_image_refs`] lists a document's image
//! destinations, the caller decodes and dithers each one to a [`Bitmap`] its own
//! way, and [`render_markdown_with`] renders the document against that map. A
//! destination present in the map is stacked as its own block with
//! [`IMAGE_MARGIN`] px of white above and below.
//!
//! **Behavior change (Phase 5):** an image whose destination is *not* in the map
//! used to be dropped silently, alt text and all. It now renders an italic
//! placeholder line — `[image: <alt text>]`, falling back to `[image: <dest>]`
//! when there is no alt text — at the surrounding indent, so it nests inside
//! lists and blockquotes. [`render_markdown`] delegates to
//! [`render_markdown_with`] with an empty map, so every caller that has not been
//! taught to supply images now prints placeholders where it used to print
//! nothing.
//!
//! Deviation from the plan's break mapping: a soft break renders as a space
//! (standard markdown behavior, reads better on a 384 px roll); only a hard
//! break starts a new line. Trailing blank space after the last block is
//! trimmed, as are blank lines abutting a horizontal rule (the rule carries
//! its own margins).

use std::collections::{HashMap, HashSet};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::barcode::render_barcode;
use super::bitmap::{Bitmap, WIDTH};
use super::qr::{self, render_qr};
use super::rich::{render_rich, FontStyle, RichLine, Span, Style};

/// Body text size in pixels.
const BODY_SIZE: f32 = 24.0;
/// Code block text size in pixels.
const CODE_SIZE: f32 = 20.0;
/// Indent per list-nesting or blockquote level, in pixels.
const INDENT_STEP: u32 = 24;
/// Extra indent for code blocks, in pixels.
const CODE_INDENT: u32 = 16;
/// White margin above and below a horizontal rule, in pixels.
const RULE_MARGIN: usize = 12;
/// Thickness of a horizontal rule, in pixels.
const RULE_THICKNESS: usize = 2;
/// Dash (and gap) length of a tear marker's dashed line, in pixels.
const TEAR_DASH: usize = 8;
/// Total white margin above and below a rendered ` ```qr ` / ` ```barcode `
/// fence, including whatever margin the renderer draws itself.
const FENCE_MARGIN: usize = 24;
/// White margin above and below a caller-supplied image, in pixels.
const IMAGE_MARGIN: usize = 8;

/// A vertically-stacked unit of lowered markdown.
enum MdBlock {
    Lines(Vec<RichLine>),
    Rule,
    /// A dashed tear-off line (`- - -` in the source).
    Tear,
    /// A ` ```qr ` fence: the payload to encode.
    Qr(String),
    /// A ` ```barcode ` fence: the payload to encode.
    Barcode(String),
    /// A caller-supplied image, already decoded to a full-width bitmap.
    Image(Bitmap),
}

/// Which renderer a code block's info string selects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fence {
    /// Plain code text — the default for indented blocks and every info
    /// string that is not a recognized fence name.
    Code,
    Qr,
    Barcode,
}

/// Classify a fence info string: its first whitespace-separated token, compared
/// case-insensitively. Everything unrecognized (including `rust` and the empty
/// info string of a bare ` ``` ` fence) stays plain code text.
fn fence_kind(info: &str) -> Fence {
    match info
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "qr" => Fence::Qr,
        "barcode" => Fence::Barcode,
        _ => Fence::Code,
    }
}

/// Render markdown to a 1-bit bitmap, with no images available.
///
/// Equivalent to [`render_markdown_with`] against an empty map: every image in
/// the document renders as a placeholder line. Callers that can fetch image
/// bytes should use [`markdown_image_refs`] + [`render_markdown_with`].
///
/// Empty (or whitespace-only) markdown yields a zero-height bitmap.
pub fn render_markdown(md: &str) -> Bitmap {
    render_markdown_with(md, &HashMap::new())
}

/// Every image destination in `md`, in document order, deduplicated.
///
/// This is the first half of the two-pass image flow: the caller resolves each
/// destination to a [`Bitmap`] however it likes (local file, HTTP, browser
/// fetch — this crate does no I/O) and passes the results to
/// [`render_markdown_with`]. Destinations are returned verbatim, exactly as
/// they must appear as keys in that map. A document without images yields an
/// empty vector.
///
/// Only destinations the renderer can actually use are reported, so a caller
/// never wastes a fetch — or, worse, resolves a meaningless path. Two kinds are
/// left out: an empty destination (`![]()`, which has nothing to fetch), and an
/// image nested inside another image's alt text (`![a ![b](b.png)](a.png)`,
/// whose inner events are consumed as alt text and never rendered).
pub fn markdown_image_refs(md: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    // The vector keeps document order; the set answers "seen already?" in
    // constant time, so a document with many images stays linear rather than
    // rescanning the whole vector per image.
    let mut seen: HashSet<String> = HashSet::new();
    let mut depth: u32 = 0;
    for event in Parser::new_ext(md, options()) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                depth += 1;
                let dest = dest_url.into_string();
                // `depth == 1` keeps only the outermost image of a nest — the
                // one the lowering actually looks up.
                if depth == 1 && !dest.is_empty() && seen.insert(dest.clone()) {
                    refs.push(dest);
                }
            }
            Event::End(TagEnd::Image) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    refs
}

/// Render markdown to a 1-bit bitmap, resolving images through `images`.
///
/// Keys are image destinations exactly as [`markdown_image_refs`] reports them.
/// A hit is stacked as its own block with [`IMAGE_MARGIN`] px of white above and
/// below; the bitmap is used as given (every [`Bitmap`] is 384 px wide by
/// construction, so it already matches the roll). A miss renders an italic
/// placeholder line — see the module docs.
pub fn render_markdown_with(md: &str, images: &HashMap<String, Bitmap>) -> Bitmap {
    let blocks = lower(md, images);
    let bitmaps = blocks
        .iter()
        .map(|block| match block {
            MdBlock::Lines(lines) => render_rich(lines),
            MdBlock::Rule => rule_bitmap(),
            MdBlock::Tear => tear_bitmap(),
            // Each fence declares the margin its renderer already draws, so
            // both end up spaced identically on the page.
            MdBlock::Qr(data) => fence_bitmap(render_qr(data, None), qr::MARGIN),
            MdBlock::Barcode(data) => fence_bitmap(render_barcode(data), 0),
            MdBlock::Image(image) => padded(image, IMAGE_MARGIN),
        })
        .collect();
    stack(bitmaps)
}

/// The parser options every entry point shares, so [`markdown_image_refs`] sees
/// exactly the document [`render_markdown_with`] renders.
fn options() -> Options {
    Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES
}

/// A copy of `src` with `margin` px of white above and below.
fn padded(src: &Bitmap, margin: usize) -> Bitmap {
    let mut out = Bitmap::new(2 * margin + src.height());
    for y in 0..src.height() {
        for x in 0..WIDTH {
            if src.get(x, y) {
                out.set(x, margin + y, true);
            }
        }
    }
    out
}

/// Lay out a ` ```qr ` / ` ```barcode ` fence's render.
///
/// On success the code (already 384 px wide and centered) is padded *to*
/// [`FENCE_MARGIN`] px of white above and below, not *by* it: `built_in` is
/// the margin the renderer already draws itself, so a QR (which carries
/// [`qr::MARGIN`]) and a barcode (which carries none) sit equally spaced on
/// the page instead of the barcode looking cramped.
///
/// On failure the encoder's message renders as code-style text instead, padded
/// by the full [`FENCE_MARGIN`] (text carries no built-in margin). A payload
/// that cannot be encoded — too long for any QR version, non-ASCII in a
/// barcode — must never panic or abort the surrounding document: the reader
/// gets a printed diagnostic and the rest of the page. The error keeps the
/// margin a successful fence would have had, because a fence is its own block:
/// `flush_block` trims the blank line around it, so unpadded error text would
/// collide with the neighbouring paragraph.
fn fence_bitmap<E: std::fmt::Display>(rendered: Result<Bitmap, E>, built_in: usize) -> Bitmap {
    let code = match rendered {
        Ok(code) => code,
        Err(e) => return padded(&fence_error(&e.to_string()), FENCE_MARGIN),
    };
    padded(&code, FENCE_MARGIN.saturating_sub(built_in))
}

/// A missing image's placeholder text: its alt text if it has any, else its
/// destination, so the reader at least learns what failed to load. An image
/// with neither (`![]()`) gets a `?` rather than an empty-looking `[image: ]`,
/// which reads as a rendering glitch.
fn placeholder_text(alt: &str, dest: &str) -> String {
    let what = match (alt.trim(), dest.trim()) {
        ("", "") => "?",
        ("", dest) => dest,
        (alt, _) => alt,
    };
    format!("[image: {what}]")
}

/// A failed fence's message, as one code-style line (Regular 20 px, indent 16).
fn fence_error(message: &str) -> Bitmap {
    render_rich(&[RichLine {
        spans: vec![Span {
            text: message.to_string(),
            style: Style::new(FontStyle::Regular, CODE_SIZE),
        }],
        indent: CODE_INDENT,
    }])
}

/// A full-width 2 px black bar with white margins above and below.
fn rule_bitmap() -> Bitmap {
    let mut b = Bitmap::new(2 * RULE_MARGIN + RULE_THICKNESS);
    for y in RULE_MARGIN..RULE_MARGIN + RULE_THICKNESS {
        for x in 0..WIDTH {
            b.set(x, y, true);
        }
    }
    b
}

/// A dashed 2 px tear line (8 px on / 8 px off) with rule margins.
fn tear_bitmap() -> Bitmap {
    let mut b = Bitmap::new(2 * RULE_MARGIN + RULE_THICKNESS);
    for y in RULE_MARGIN..RULE_MARGIN + RULE_THICKNESS {
        for x in (0..WIDTH).filter(|x| (x / TEAR_DASH).is_multiple_of(2)) {
            b.set(x, y, true);
        }
    }
    b
}

/// Stack bitmaps vertically into one bitmap.
fn stack(bitmaps: Vec<Bitmap>) -> Bitmap {
    let total: usize = bitmaps.iter().map(Bitmap::height).sum();
    let mut out = Bitmap::new(total);
    let mut y0 = 0;
    for b in &bitmaps {
        for y in 0..b.height() {
            for x in 0..WIDTH {
                if b.get(x, y) {
                    out.set(x, y0 + y, true);
                }
            }
        }
        y0 += b.height();
    }
    out
}

/// Two-space gutter between table columns.
const TABLE_GUTTER: usize = 2;
/// Chars that fit one 20 px monospace line (~12 px advance in 384 px).
const TABLE_MAX_CHARS: usize = 32;
/// Smallest a column may shrink to when a table overflows the budget.
const TABLE_MIN_COL: usize = 3;

/// Lay a markdown table out as monospace text lines: one padded [`String`] per
/// rendered row (header, a dashed separator, then each body row).
///
/// Cells are already flattened to plain text. Ragged rows (fewer cells than the
/// header) are padded with empty strings; extra cells are dropped. Column
/// widths start at each column's widest cell, then — while the line would
/// exceed [`TABLE_MAX_CHARS`] — the widest column is shrunk one char at a time
/// (down to [`TABLE_MIN_COL`]), and any cell longer than its final width is
/// truncated to `width - 1` chars plus `…`. Cells are left-justified with a
/// [`TABLE_GUTTER`]-space gutter; the separator uses `-` runs. An empty header
/// yields no lines.
///
/// The floor caps the column count: `cols * TABLE_MIN_COL + TABLE_GUTTER *
/// (cols - 1)` stays within [`TABLE_MAX_CHARS`] only up to six columns. Seven
/// or more return lines wider than the budget, which the renderer word-wraps —
/// alignment is lost, but the cells are all still on the paper. Dropping the
/// extra columns instead would silently lose data, which is worse.
fn build_table_lines(header: &[String], rows: &[Vec<String>]) -> Vec<String> {
    let cols = header.len();
    if cols == 0 {
        return Vec::new();
    }

    // Normalize every row to exactly `cols` cells (pad short, drop extra).
    let normalize = |row: &[String]| -> Vec<String> {
        (0..cols)
            .map(|i| row.get(i).cloned().unwrap_or_default())
            .collect()
    };
    let header = normalize(header);
    let rows: Vec<Vec<String>> = rows.iter().map(|r| normalize(r)).collect();

    // Natural width = widest cell per column (at least 1, so the separator
    // always shows a dash).
    let mut widths: Vec<usize> = (0..cols)
        .map(|i| {
            let cell_len = |cells: &[String]| cells[i].chars().count();
            let mut w = cell_len(&header);
            for r in &rows {
                w = w.max(cell_len(r));
            }
            w.max(1)
        })
        .collect();

    // Shrink the widest column until the line fits (or all hit the floor).
    let line_width = |w: &[usize]| w.iter().sum::<usize>() + TABLE_GUTTER * (cols - 1);
    while line_width(&widths) > TABLE_MAX_CHARS {
        let widest = widths.iter().copied().enumerate().max_by_key(|&(_, w)| w);
        match widest {
            Some((idx, w)) if w > TABLE_MIN_COL => widths[idx] = w - 1,
            _ => break,
        }
    }

    // Fit a cell to its column: truncate with `…` when it overflows.
    let fit = |text: &str, w: usize| -> String {
        if text.chars().count() <= w {
            return text.to_string();
        }
        match w {
            0 => String::new(),
            1 => "…".to_string(),
            _ => {
                let kept: String = text.chars().take(w - 1).collect();
                format!("{kept}…")
            }
        }
    };
    let render_row = |cells: &[String]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, &w)| format!("{:<width$}", fit(c, w), width = w))
            .collect::<Vec<_>>()
            .join(&" ".repeat(TABLE_GUTTER))
    };

    let separator = widths
        .iter()
        .map(|&w| "-".repeat(w))
        .collect::<Vec<_>>()
        .join(&" ".repeat(TABLE_GUTTER));

    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(render_row(&header));
    lines.push(separator);
    for r in &rows {
        lines.push(render_row(r));
    }
    lines
}

/// Event-stream lowering state: markdown events → [`MdBlock`]s.
struct Lowering<'a> {
    /// Caller-supplied images, keyed by markdown destination.
    images: &'a HashMap<String, Bitmap>,
    blocks: Vec<MdBlock>,
    /// Lines of the [`MdBlock::Lines`] block being accumulated.
    lines: Vec<RichLine>,
    /// The logical line being built (flushed into `lines`).
    current: RichLine,
    /// Nesting depths; unbalanced end tags saturate at zero.
    bold: u32,
    italic: u32,
    strike: u32,
    quote_depth: u32,
    /// `Some(size_px)` while inside a heading.
    heading_size: Option<f32>,
    /// One entry per open list: `Some(next index)` for ordered, `None` for bullets.
    lists: Vec<Option<u64>>,
    /// True inside a fenced or indented code block.
    in_code: bool,
    /// Which renderer the open code block's info string selected.
    fence: Fence,
    /// Raw content of an open `qr`/`barcode` fence.
    fence_buf: String,
    /// Partial code line, pending its `\n` (or the block's end).
    code_buf: String,
    /// Image nesting depth; while > 0 inner events feed `image_alt` instead of
    /// the page.
    image_depth: u32,
    /// Destination of the open image.
    image_dest: String,
    /// Flattened alt text of the open image (its inner text content).
    image_alt: String,
    /// True while inside a table: cell text collects into `table_cell`.
    in_table: bool,
    /// Header cells (plain text) for the table being collected.
    table_header: Vec<String>,
    /// Body rows (each a Vec of plain-text cells).
    table_rows: Vec<Vec<String>>,
    /// Cells accumulated for the row currently being read.
    table_row: Vec<String>,
    /// Plain-text buffer for the cell currently being read.
    table_cell: String,
}

fn lower(md: &str, images: &HashMap<String, Bitmap>) -> Vec<MdBlock> {
    let mut st = Lowering {
        images,
        blocks: Vec::new(),
        lines: Vec::new(),
        current: RichLine::default(),
        bold: 0,
        italic: 0,
        strike: 0,
        quote_depth: 0,
        heading_size: None,
        lists: Vec::new(),
        in_code: false,
        fence: Fence::Code,
        fence_buf: String::new(),
        code_buf: String::new(),
        image_depth: 0,
        image_dest: String::new(),
        image_alt: String::new(),
        in_table: false,
        table_header: Vec::new(),
        table_rows: Vec::new(),
        table_row: Vec::new(),
        table_cell: String::new(),
    };
    // The offset iterator is only consulted for Rule events: the source text
    // distinguishes a tear marker (`- - -`) from a plain rule (`---`).
    for (event, range) in Parser::new_ext(md, options()).into_offset_iter() {
        match event {
            Event::Rule => st.rule(md[range].trim()),
            _ => st.handle(event),
        }
    }
    st.finish()
}

impl Lowering<'_> {
    /// The style for inline text under the current nesting.
    fn style(&self) -> Style {
        let font = if self.heading_size.is_some() || self.bold > 0 {
            FontStyle::Bold
        } else if self.italic > 0 || self.quote_depth > 0 {
            FontStyle::Italic
        } else {
            FontStyle::Regular
        };
        Style {
            font,
            size_px: self.heading_size.unwrap_or(BODY_SIZE),
            strike: self.strike > 0,
        }
    }

    /// Indent from enclosing blockquotes alone.
    fn quote_indent(&self) -> u32 {
        INDENT_STEP * self.quote_depth
    }

    fn push_span(&mut self, text: &str) {
        self.current.spans.push(Span {
            text: text.to_string(),
            style: self.style(),
        });
    }

    /// Push `current` into `lines` if it has content; keep its indent.
    fn flush_line(&mut self) {
        if !self.current.spans.is_empty() {
            let indent = self.current.indent;
            self.lines.push(std::mem::take(&mut self.current));
            self.current.indent = indent;
        }
    }

    /// Blank separator line; skipped when there is nothing above to separate
    /// from (block start) or the previous line is already blank.
    fn push_blank(&mut self) {
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(RichLine::default());
        }
    }

    fn trim_trailing_blanks(&mut self) {
        while self.lines.last().is_some_and(|l| l.spans.is_empty()) {
            self.lines.pop();
        }
    }

    /// Close the accumulating `Lines` block, if non-empty.
    fn flush_block(&mut self) {
        self.flush_line();
        self.trim_trailing_blanks();
        if !self.lines.is_empty() {
            self.blocks
                .push(MdBlock::Lines(std::mem::take(&mut self.lines)));
        }
    }

    /// Complete a code line (on `\n` or at the block's end).
    fn flush_code_line(&mut self) {
        let indent = self.current.indent;
        self.lines.push(RichLine {
            spans: vec![Span {
                text: std::mem::take(&mut self.code_buf),
                style: Style::new(FontStyle::Regular, CODE_SIZE),
            }],
            indent,
        });
    }

    /// Code block text: preserve exact line breaks, one [`RichLine`] per line.
    fn push_code_text(&mut self, text: &str) {
        for (i, segment) in text.split('\n').enumerate() {
            if i > 0 {
                self.flush_code_line();
            }
            self.code_buf.push_str(segment);
        }
    }

    /// Collect an open image's inner events as alt text; emit the image (or its
    /// placeholder) when the outermost one closes.
    ///
    /// Inner events never reach the page: an image's content is its alt text,
    /// not document text. Emphasis and other inline markup inside the alt text
    /// is flattened away — only the characters survive.
    fn handle_in_image(&mut self, event: Event) {
        match event {
            Event::Start(Tag::Image { .. }) => self.image_depth += 1,
            Event::End(TagEnd::Image) => {
                self.image_depth -= 1;
                if self.image_depth == 0 {
                    self.finish_image();
                }
            }
            Event::Text(text) | Event::Code(text) => self.image_alt.push_str(&text),
            Event::SoftBreak | Event::HardBreak => self.image_alt.push(' '),
            _ => {}
        }
    }

    /// Lower the just-closed image: a supplied bitmap becomes its own stacked
    /// block, an unsupplied one an italic placeholder line at the current
    /// indent (so it nests inside lists and blockquotes).
    fn finish_image(&mut self) {
        let dest = std::mem::take(&mut self.image_dest);
        let alt = std::mem::take(&mut self.image_alt);
        // A table is laid out as monospace text, so nothing can stack inside a
        // cell: a supplied image collapses to the same placeholder text a
        // missing one gets. Escaping here instead would put the image block
        // before the whole table (rows are still buffered) and leave the cell
        // empty — the picture would land in the wrong place either way.
        if self.in_table {
            let text = placeholder_text(&alt, &dest);
            self.table_cell.push_str(&text);
            return;
        }
        match self.images.get(&dest) {
            Some(image) => {
                // Like a rule or a fence, an image interrupts the text flow and
                // stacks on its own. `flush_block` keeps `current.indent`, so
                // whatever follows stays in its list or quote.
                let image = image.clone();
                self.flush_block();
                self.blocks.push(MdBlock::Image(image));
            }
            None => {
                self.current.spans.push(Span {
                    text: placeholder_text(&alt, &dest),
                    style: Style::new(FontStyle::Italic, BODY_SIZE),
                });
            }
        }
    }

    fn handle(&mut self, event: Event) {
        if self.image_depth > 0 {
            self.handle_in_image(event);
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.in_table {
                    self.table_cell.push_str(&text);
                } else if self.in_code {
                    match self.fence {
                        Fence::Code => self.push_code_text(&text),
                        // A qr/barcode fence's body is data, not text to set.
                        Fence::Qr | Fence::Barcode => self.fence_buf.push_str(&text),
                    }
                } else {
                    self.push_span(&text);
                }
            }
            Event::Code(text) => {
                if self.in_table {
                    self.table_cell.push_str(&text);
                } else {
                    self.push_span(&text);
                }
            }
            Event::SoftBreak => {
                if self.in_table {
                    self.table_cell.push(' ');
                } else {
                    self.push_span(" ");
                }
            }
            Event::HardBreak => {
                if self.in_table {
                    self.table_cell.push(' ');
                } else {
                    self.flush_line();
                }
            }
            // Rule events are routed to `rule` (they need the source text).
            Event::Rule => self.rule(""),
            Event::TaskListMarker(checked) => {
                // ASCII markers replace the `• ` bullet prefix pushed by the
                // enclosing Item — JetBrains Mono has no ballot-box glyphs
                // (U+2610/U+2611), pinned in rich::tests.
                let marker = if checked { "[x] " } else { "[ ] " };
                match self.current.spans.last_mut() {
                    Some(span) => span.text = marker.to_string(),
                    None => self.push_span(marker),
                }
            }
            // Raw HTML is skipped; other events are out of scope.
            _ => {}
        }
    }

    /// Lower a thematic break given its trimmed source text: interior
    /// whitespace (`- - -`, `* * *`) marks a tear line, otherwise a rule.
    fn rule(&mut self, source: &str) {
        self.flush_block();
        let tear = source.chars().any(char::is_whitespace);
        self.blocks
            .push(if tear { MdBlock::Tear } else { MdBlock::Rule });
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_line();
                self.push_blank();
                self.heading_size = Some(match level {
                    HeadingLevel::H1 => 36.0,
                    HeadingLevel::H2 => 30.0,
                    _ => 26.0,
                });
                self.current.indent = self.quote_indent();
            }
            Tag::Paragraph => {
                // Inside a list item the paragraph joins the item's line
                // (after the `• `/`N. ` prefix) and keeps the item's indent.
                if self.lists.is_empty() {
                    self.current.indent = self.quote_indent();
                }
            }
            Tag::BlockQuote(_) => self.quote_depth += 1,
            Tag::List(start) => self.lists.push(start),
            Tag::Item => {
                self.flush_line();
                self.current.indent = self.quote_indent() + INDENT_STEP * self.lists.len() as u32;
                let prefix = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let p = format!("{n}. ");
                        *n += 1;
                        p
                    }
                    _ => "• ".to_string(),
                };
                self.push_span(&prefix);
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
                self.in_code = true;
                self.fence = match &kind {
                    CodeBlockKind::Fenced(info) => fence_kind(info),
                    CodeBlockKind::Indented => Fence::Code,
                };
                match self.fence {
                    Fence::Code => self.current.indent = self.quote_indent() + CODE_INDENT,
                    Fence::Qr | Fence::Barcode => self.fence_buf.clear(),
                }
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            // Tables collect plain-text cells here; layout happens at TableEnd
            // (see `build_table_lines`). Alignment markers are ignored.
            Tag::Table(_) => {
                self.flush_line();
                self.push_blank();
                self.in_table = true;
                self.table_header.clear();
                self.table_rows.clear();
                self.table_row.clear();
                self.table_cell.clear();
            }
            // A `TableHead` holds the header cells directly; body rows come as
            // `TableRow`. Either way, start a fresh row buffer.
            Tag::TableHead | Tag::TableRow => self.table_row.clear(),
            Tag::TableCell => self.table_cell.clear(),
            // The image's inner events are its alt text: `handle_in_image`
            // collects them and emits the block when the tag closes.
            Tag::Image { dest_url, .. } => {
                self.image_depth = 1;
                self.image_dest = dest_url.into_string();
                self.image_alt.clear();
            }
            // Links render their inner text; everything else just flows.
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush_line();
                self.push_blank();
                self.heading_size = None;
                self.current.indent = 0;
            }
            TagEnd::Paragraph => {
                self.flush_line();
                if self.lists.is_empty() {
                    self.push_blank();
                    self.current.indent = 0;
                }
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.push_blank();
                    self.current.indent = 0;
                }
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::CodeBlock => {
                self.in_code = false;
                match std::mem::replace(&mut self.fence, Fence::Code) {
                    Fence::Code => {
                        // A fenced block's text ends in `\n`, leaving an empty
                        // buffer; flush any remainder so a missing final
                        // newline still prints.
                        if !self.code_buf.is_empty() {
                            self.flush_code_line();
                        }
                        self.current.indent = 0;
                        self.push_blank();
                    }
                    // A qr/barcode fence is its own stacked block, like a rule.
                    kind => {
                        let data = std::mem::take(&mut self.fence_buf).trim().to_string();
                        self.flush_block();
                        self.blocks.push(if kind == Fence::Qr {
                            MdBlock::Qr(data)
                        } else {
                            MdBlock::Barcode(data)
                        });
                        self.current.indent = 0;
                    }
                }
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::TableCell => {
                self.table_row.push(std::mem::take(&mut self.table_cell));
            }
            TagEnd::TableHead => {
                self.table_header = std::mem::take(&mut self.table_row);
            }
            TagEnd::TableRow => {
                self.table_rows.push(std::mem::take(&mut self.table_row));
            }
            TagEnd::Table => {
                self.in_table = false;
                let header = std::mem::take(&mut self.table_header);
                let body = std::mem::take(&mut self.table_rows);
                for text in build_table_lines(&header, &body) {
                    self.lines.push(RichLine {
                        spans: vec![Span {
                            text,
                            style: Style::new(FontStyle::Regular, CODE_SIZE),
                        }],
                        indent: 0,
                    });
                }
                self.push_blank();
                self.current.indent = 0;
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<MdBlock> {
        self.flush_block();
        self.blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::bitmap::WIDTH;

    fn has_ink(b: &Bitmap) -> bool {
        (0..b.height()).any(|y| (0..WIDTH).any(|x| b.get(x, y)))
    }

    fn ink_before(b: &Bitmap, x_limit: usize) -> bool {
        (0..b.height()).any(|y| (0..x_limit).any(|x| b.get(x, y)))
    }

    fn rows(b: &Bitmap) -> Vec<Vec<u8>> {
        (0..b.height()).map(|y| b.row(y).to_vec()).collect()
    }

    #[test]
    fn heading_taller_than_body() {
        let heading = render_markdown("# Hi");
        let body = render_markdown("Hi");
        assert!(
            heading.height() > body.height(),
            "heading {} should be taller than body {}",
            heading.height(),
            body.height()
        );
    }

    #[test]
    fn bold_differs_from_plain() {
        let bold = render_markdown("**Hi**");
        let plain = render_markdown("Hi");
        assert!(has_ink(&bold), "bold render has no ink");
        assert!(has_ink(&plain), "plain render has no ink");
        assert_ne!(rows(&bold), rows(&plain), "bold should differ from plain");
    }

    #[test]
    fn list_items_indented() {
        let b = render_markdown("- item");
        assert!(has_ink(&b), "list item has no ink");
        assert!(!ink_before(&b, 24), "ink left of the 24 px list indent");
    }

    #[test]
    fn ordered_list_numbers() {
        let two = render_markdown("1. a\n2. b");
        let one = render_markdown("1. a");
        assert!(has_ink(&two), "ordered list has no ink");
        assert!(
            two.height() > one.height(),
            "two items {} should be taller than one {}",
            two.height(),
            one.height()
        );
    }

    #[test]
    fn code_block_preserves_lines() {
        let three = render_markdown("```\na\nb\nc\n```");
        let one = render_markdown("```\na\n```");
        assert!(has_ink(&three), "code block has no ink");
        assert!(
            three.height() > one.height(),
            "three code lines {} should be taller than one {}",
            three.height(),
            one.height()
        );
        // Three code lines at 20 px, 1.3 line height each.
        let expected = (3.0 * 20.0 * 1.3) as usize;
        assert!(
            three.height() >= expected,
            "height {} does not cover three 20 px code lines ({expected})",
            three.height()
        );
    }

    #[test]
    fn hr_renders_full_width_line() {
        let b = render_markdown("---");
        let full_row = (0..b.height()).any(|y| b.get(0, y) && b.get(191, y) && b.get(383, y));
        assert!(full_row, "no full-width black row found for the rule");
    }

    #[test]
    fn empty_markdown_gives_empty_bitmap() {
        assert_eq!(render_markdown("").height(), 0);
    }

    #[test]
    fn paragraph_wraps() {
        let long = render_markdown(&"word ".repeat(30));
        let short = render_markdown("word");
        assert!(
            long.height() > short.height(),
            "long paragraph {} should wrap taller than short {}",
            long.height(),
            short.height()
        );
    }

    #[test]
    fn strikethrough_differs_from_plain() {
        let struck = render_markdown("~~Hi~~");
        let plain = render_markdown("Hi");
        assert!(has_ink(&struck), "struck render has no ink");
        assert!(has_ink(&plain), "plain render has no ink");
        assert_ne!(
            rows(&struck),
            rows(&plain),
            "strikethrough should differ from plain"
        );
        // The tildes must be consumed: ~~Hi~~ is exactly "Hi" struck through.
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "Hi".to_string(),
                style: Style {
                    strike: true,
                    ..Style::default()
                },
            }],
            indent: 0,
        }]);
        assert_eq!(
            rows(&struck),
            rows(&expected),
            "~~Hi~~ should render as struck 'Hi'"
        );
    }

    #[test]
    fn checkbox_checked_differs_from_unchecked() {
        let checked = render_markdown("- [x] task");
        let unchecked = render_markdown("- [ ] task");
        assert!(has_ink(&checked), "checked render has no ink");
        assert!(has_ink(&unchecked), "unchecked render has no ink");
        assert_ne!(
            rows(&checked),
            rows(&unchecked),
            "checked should differ from unchecked"
        );
        // The checked marker is exactly "[x] " (no bullet, no literal "[x]"
        // text run — the marker replaces the bullet prefix).
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "[x] task".to_string(),
                style: Style::default(),
            }],
            indent: 24,
        }]);
        assert_eq!(
            rows(&checked),
            rows(&expected),
            "checked task item should render as '[x] task' at list indent"
        );
    }

    #[test]
    fn checkbox_renders_ascii_marker() {
        // JetBrains Mono lacks the ballot-box glyphs (pinned in rich::tests),
        // so the task marker is ASCII and replaces the bullet: "[ ] task".
        let md = render_markdown("- [ ] task");
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "[ ] task".to_string(),
                style: Style::default(),
            }],
            indent: 24,
        }]);
        assert_eq!(
            rows(&md),
            rows(&expected),
            "task item should render as '[ ] task' at list indent"
        );
    }

    #[test]
    fn tear_renders_dashed_line() {
        let b = render_markdown("- - -");
        // Same footprint as a rule: margins + 2 px line.
        assert_eq!(b.height(), 2 * RULE_MARGIN + RULE_THICKNESS);
        // Dashes: 8 px on / 8 px off, starting black at x = 0.
        for y in RULE_MARGIN..RULE_MARGIN + RULE_THICKNESS {
            for x in 0..WIDTH {
                let want = (x / 8).is_multiple_of(2);
                assert_eq!(b.get(x, y), want, "tear pattern wrong at x={x}, y={y}");
            }
        }
    }

    #[test]
    fn tear_differs_from_solid_rule() {
        let tear = render_markdown("- - -");
        let rule = render_markdown("---");
        assert_ne!(rows(&tear), rows(&rule), "tear should differ from rule");
        let full_row = (0..rule.height()).any(|y| (0..WIDTH).all(|x| rule.get(x, y)));
        assert!(full_row, "solid rule lost its full-width black row");
    }

    #[test]
    fn table_renders_ink() {
        let table = render_markdown("| a | b |\n| --- | --- |\n| c | d |");
        let line = render_markdown("`a  b`");
        assert!(has_ink(&table), "table render has no ink");
        assert!(
            table.height() > line.height(),
            "table {} (header + separator + body) should be taller than one line {}",
            table.height(),
            line.height()
        );
    }

    #[test]
    fn table_has_separator_row() {
        // Header, dashed separator, and one body row → three text rows.
        let table = render_markdown("| a | b |\n|---|---|\n| c | d |");
        let one = render_markdown("| a | b |\n|---|---|");
        assert!(
            table.height() > one.height(),
            "three-row table {} should exceed a header+separator {}",
            table.height(),
            one.height()
        );
        // The separator is the pure-text line the builder emits second.
        let lines = build_table_lines(&["a".into(), "b".into()], &[vec!["c".into(), "d".into()]]);
        assert_eq!(lines.len(), 3, "header, separator, body");
        assert!(
            lines[1].chars().all(|c| c == '-' || c == ' '),
            "separator row is dashes and gutters, got {:?}",
            lines[1]
        );
        assert!(
            lines[1].contains('-'),
            "separator row has dashes, got {:?}",
            lines[1]
        );
    }

    #[test]
    fn build_table_lines_pads_and_gutters() {
        let lines = build_table_lines(
            &["Item".into(), "Qty".into()],
            &[vec!["Coffee".into(), "2".into()]],
        );
        // Column widths: max("Item",6=Coffee)=6, max("Qty",1)=3. Gutter = 2.
        assert_eq!(lines[0], "Item    Qty");
        assert_eq!(lines[1], "------  ---");
        assert_eq!(lines[2], "Coffee  2  ");
        for l in &lines {
            assert_eq!(l.chars().count(), 11, "every row is the same width: {l:?}");
        }
    }

    #[test]
    fn table_truncates_overwide_cells() {
        let long = "x".repeat(50);
        let lines = build_table_lines(&["col".into(), "b".into()], &[vec![long, "y".into()]]);
        for l in &lines {
            assert!(
                l.chars().count() <= TABLE_MAX_CHARS,
                "line {:?} exceeds {TABLE_MAX_CHARS} chars",
                l
            );
        }
        // The over-wide body cell is truncated with an ellipsis.
        assert!(
            lines.last().unwrap().contains('…'),
            "over-wide cell should be truncated with '…', got {:?}",
            lines.last()
        );
        // And it still renders without panicking.
        let md = format!("| col | b |\n|---|---|\n| {} | y |", "x".repeat(50));
        assert!(has_ink(&render_markdown(&md)), "wide table has no ink");
    }

    /// Seven columns already exceed the budget at the 3-char floor, so wide
    /// tables word-wrap instead of staying aligned. This is a documented
    /// ceiling, not a bug — pin it so a change is deliberate.
    #[test]
    fn table_over_six_columns_exceeds_budget_and_wraps() {
        // Six columns still fit: 6*3 + 2*5 = 28 <= 32.
        let six: Vec<String> = (0..6).map(|i| format!("h{i}")).collect();
        let lines = build_table_lines(&six, std::slice::from_ref(&six));
        for l in &lines {
            assert!(
                l.chars().count() <= TABLE_MAX_CHARS,
                "six columns should fit, got {:?} ({} chars)",
                l,
                l.chars().count()
            );
        }

        // Eight do not: every column bottoms out at TABLE_MIN_COL and the line
        // is still 8*3 + 2*7 = 38 chars.
        let eight: Vec<String> = (0..8).map(|i| format!("header{i}")).collect();
        let lines = build_table_lines(&eight, std::slice::from_ref(&eight));
        assert_eq!(
            lines[0].chars().count(),
            8 * TABLE_MIN_COL + TABLE_GUTTER * 7,
            "eight columns bottom out at the floor: {:?}",
            lines[0]
        );
        assert!(
            lines[0].chars().count() > TABLE_MAX_CHARS,
            "eight columns should overflow the budget"
        );

        // The renderer wraps rather than clipping: ink stays on the paper and
        // the block is taller than the three rows it would be if it fit.
        let head = eight
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" | ");
        let md = format!("| {head} |\n|{}\n| {head} |", "---|".repeat(8));
        let wide = render_markdown(&md);
        assert!(has_ink(&wide), "eight-column table has no ink");
        let narrow = render_markdown("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(
            wide.height() > narrow.height(),
            "wrapped table {} should be taller than an aligned one {}",
            wide.height(),
            narrow.height()
        );
    }

    #[test]
    fn table_single_column_ok() {
        let lines = build_table_lines(&["only".into()], &[vec!["one".into()], vec!["two".into()]]);
        assert_eq!(lines, vec!["only", "----", "one ", "two "]);
        assert!(has_ink(&render_markdown(
            "| only |\n|---|\n| one |\n| two |"
        )));
    }

    #[test]
    fn table_ragged_rows_padded() {
        // A body row shorter than the header is padded with empty cells.
        let lines = build_table_lines(&["a".into(), "b".into(), "c".into()], &[vec!["1".into()]]);
        assert_eq!(lines[0], "a  b  c");
        // "1" (col0) + gutter + " " (empty col1) + gutter + " " (empty col2).
        assert_eq!(lines[2], "1      ");
        assert_eq!(lines[2].chars().count(), 7);
        // A row with more cells than the header drops the extras.
        let over = build_table_lines(&["a".into()], &[vec!["1".into(), "2".into()]]);
        assert_eq!(over, vec!["a", "-", "1"]);
        assert!(has_ink(&render_markdown(
            "| a | b | c |\n|---|---|---|\n| 1 |"
        )));
    }

    #[test]
    fn table_empty_header_no_lines() {
        assert!(build_table_lines(&[], &[vec!["x".into()]]).is_empty());
    }

    #[test]
    fn blockquote_indented() {
        let b = render_markdown("> quote");
        assert!(has_ink(&b), "blockquote has no ink");
        assert!(!ink_before(&b, 24), "ink left of the 24 px quote indent");
    }

    /// Bounding box of black pixels: (min_x, min_y, max_x, max_y).
    fn ink_bbox(b: &Bitmap) -> Option<(usize, usize, usize, usize)> {
        let mut bbox: Option<(usize, usize, usize, usize)> = None;
        for y in 0..b.height() {
            for x in 0..WIDTH {
                if b.get(x, y) {
                    bbox = Some(match bbox {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        bbox
    }

    /// Longest run of consecutive black pixels down column `x`.
    fn tallest_run(b: &Bitmap, x: usize) -> usize {
        let (mut best, mut run) = (0, 0);
        for y in 0..b.height() {
            run = if b.get(x, y) { run + 1 } else { 0 };
            best = best.max(run);
        }
        best
    }

    #[test]
    fn qr_fence_renders_block() {
        let b = render_markdown("```qr\nhttps://example.com\n```");
        let (min_x, min_y, max_x, max_y) = ink_bbox(&b).expect("qr fence has no ink");
        let w = max_x - min_x + 1;
        let h = max_y - min_y + 1;
        assert!(
            w.abs_diff(h) * 10 <= w,
            "bbox {w}x{h} is not square within 10%"
        );
        assert!(min_x >= 16, "ink at x = {min_x}, inside the quiet zone");
        assert!(min_y >= 8, "ink at y = {min_y}, inside the top margin");
    }

    #[test]
    fn qr_fence_case_insensitive() {
        let upper = render_markdown("```QR\nhttps://example.com\n```");
        let lower = render_markdown("```qr\nhttps://example.com\n```");
        assert!(has_ink(&upper), "uppercase QR fence has no ink");
        assert_eq!(rows(&upper), rows(&lower), "```QR should match ```qr");
        // An info string with attributes after the name still selects the fence.
        let attrs = render_markdown("```  Qr  extra\nhttps://example.com\n```");
        assert_eq!(rows(&attrs), rows(&lower), "```Qr extra should match ```qr");
    }

    #[test]
    fn qr_fence_too_long_renders_error_not_panic() {
        let md = format!("```qr\n{}\n```", "a".repeat(4000));
        let b = render_markdown(&md);
        assert!(has_ink(&b), "over-long qr fence rendered nothing");
        // Error text, not a code: far shorter than a QR block and left-aligned
        // at the code indent rather than centered in a quiet zone. Both carry
        // the same FENCE_MARGIN, so compare against a real QR fence.
        let real = render_markdown("```qr\nhttps://example.com\n```");
        assert!(
            b.height() * 2 < real.height(),
            "height {} looks like a QR ({}), not error text",
            b.height(),
            real.height()
        );
        assert!(ink_before(&b, 40), "no ink at the 16 px code indent");
    }

    #[test]
    fn barcode_fence_renders_bars() {
        let b = render_markdown("```barcode\nHELLO123\n```");
        assert!(has_ink(&b), "barcode fence has no ink");
        assert!(!ink_before(&b, 16), "ink inside the 16 px quiet zone");
        let tall = (0..WIDTH).filter(|&x| tallest_run(&b, x) >= 60).count();
        assert!(tall >= 20, "only {tall} columns have a 60 px black run");
        // Fence blocks are padded to a common margin, so a barcode gets the
        // same breathing room a QR does rather than sitting cramped against
        // the next block.
        let first_ink = (0..b.height())
            .find(|&y| (0..WIDTH).any(|x| b.get(x, y)))
            .unwrap();
        assert_eq!(first_ink, FENCE_MARGIN, "barcode top margin");
        let last_ink = (0..b.height())
            .rev()
            .find(|&y| (0..WIDTH).any(|x| b.get(x, y)))
            .unwrap();
        assert_eq!(
            b.height() - 1 - last_ink,
            FENCE_MARGIN,
            "barcode bottom margin"
        );
    }

    #[test]
    fn barcode_fence_invalid_renders_error_not_panic() {
        for md in [
            "```barcode\n\n```",
            "```barcode\ncafé ☕\n```",
            "```barcode\nWAY TOO LONG FOR A 384 PIXEL ROLL OF PAPER\n```",
        ] {
            let b = render_markdown(md);
            assert!(has_ink(&b), "invalid barcode {md:?} rendered nothing");
            let tall = (0..WIDTH).filter(|&x| tallest_run(&b, x) >= 60).count();
            assert_eq!(tall, 0, "invalid barcode {md:?} drew bars");
            assert!(ink_before(&b, 40), "no error text at the code indent");
        }
    }

    /// A fence gets the same breathing room whether it encoded or not. A fence
    /// is its own block, so the blank line around it is trimmed; if only the
    /// success branch padded, a failed fence's error text would print flush
    /// against the neighbouring paragraph.
    #[test]
    fn fence_margins_match_in_both_branches() {
        // Padding added by the block = document blanks minus the content's own.
        // The success branch's built-in margin is subtracted, so both branches
        // add the same FENCE_MARGIN of white.
        let pad = |doc: &Bitmap, inner: &Bitmap| {
            let (_, doc_top, _, doc_bot) = ink_bbox(doc).expect("document has no ink");
            let (_, in_top, _, in_bot) = ink_bbox(inner).expect("content has no ink");
            (
                doc_top - in_top,
                (doc.height() - 1 - doc_bot) - (inner.height() - 1 - in_bot),
            )
        };

        let ok_inner = render_barcode("HELLO123").expect("HELLO123 is a valid payload");
        let ok_doc = render_markdown("```barcode\nHELLO123\n```");
        assert_eq!(
            pad(&ok_doc, &ok_inner),
            (FENCE_MARGIN, FENCE_MARGIN),
            "successful fence margins"
        );

        let err = render_barcode("café ☕").expect_err("non-ASCII payload must be rejected");
        let err_inner = fence_error(&err.to_string());
        let err_doc = render_markdown("```barcode\ncafé ☕\n```");
        assert_eq!(
            pad(&err_doc, &err_inner),
            (FENCE_MARGIN, FENCE_MARGIN),
            "failed fence margins"
        );

        // And the visible symptom: with text on both sides, a failed fence
        // keeps at least FENCE_MARGIN of white from its neighbours instead of
        // colliding with them.
        let doc = render_markdown("before\n\n```barcode\ncafé ☕\n```\n\nafter");
        let blank_runs: Vec<usize> = {
            let inked: Vec<bool> = (0..doc.height())
                .map(|y| (0..WIDTH).any(|x| doc.get(x, y)))
                .collect();
            let mut runs = Vec::new();
            let mut run = 0;
            for (i, &ink) in inked.iter().enumerate() {
                if ink {
                    if run > 0 && i > run {
                        runs.push(run);
                    }
                    run = 0;
                } else {
                    run += 1;
                }
            }
            runs
        };
        // The error message wraps, so the interior runs are line gaps; the
        // first and last are the fence's own margins.
        assert!(
            blank_runs.len() >= 2,
            "expected gaps above and below the fence, got {blank_runs:?}"
        );
        for gap in [blank_runs[0], *blank_runs.last().unwrap()] {
            assert!(
                gap >= FENCE_MARGIN,
                "gap of {gap} px around a failed fence is below the {FENCE_MARGIN} px margin"
            );
        }
    }

    #[test]
    fn image_refs_extracts_in_order_deduped() {
        let md = "![a](a.png)\n\n![b](b.png)\n\n![again](a.png)";
        assert_eq!(markdown_image_refs(md), vec!["a.png", "b.png"]);
        assert!(markdown_image_refs("# Just text\n\nno pictures here").is_empty());
        assert!(markdown_image_refs("").is_empty());
    }

    /// Dedupe is by set membership, not a linear rescan, so a document with a
    /// thousand images must still report each destination once, in first-seen
    /// order.
    #[test]
    fn image_refs_dedupe_scales_and_keeps_first_seen_order() {
        let mut md = String::new();
        for i in 0..500 {
            let dest = if i % 2 == 0 { "a.png" } else { "b.png" };
            md.push_str(&format!("![alt]({dest})\n\n"));
        }
        for i in 0..500 {
            md.push_str(&format!("![alt](u{i}.png)\n\n"));
        }

        let refs = markdown_image_refs(&md);
        assert_eq!(refs.len(), 502, "each destination should appear once");

        let mut expected = vec!["a.png".to_string(), "b.png".to_string()];
        expected.extend((0..500).map(|i| format!("u{i}.png")));
        assert_eq!(refs, expected);
    }

    #[test]
    fn image_refs_ignores_links() {
        assert!(markdown_image_refs("[text](page.html)").is_empty());
        // A link wrapping an image still yields the image's destination.
        assert_eq!(
            markdown_image_refs("[![alt](pic.png)](page.html)"),
            vec!["pic.png"]
        );
    }

    #[test]
    fn render_with_supplied_bitmap_stacks_it() {
        let mut pic = Bitmap::new(40);
        pic.set(10, 5, true);
        let images = HashMap::from([("pic.png".to_string(), pic)]);

        let base = render_markdown("# Hi");
        let with = render_markdown_with("# Hi\n\n![p](pic.png)", &images);
        assert_eq!(
            with.height(),
            base.height() + 40 + 2 * IMAGE_MARGIN,
            "image should add its height plus top and bottom margins"
        );
        // The supplied ink lands below the heading and the top margin.
        assert!(
            with.get(10, base.height() + IMAGE_MARGIN + 5),
            "supplied image pixel missing at its stacked offset"
        );
        // ...and nowhere else on that row of the image block.
        assert!(
            !with.get(11, base.height() + IMAGE_MARGIN + 5),
            "supplied image should not smear sideways"
        );
    }

    #[test]
    fn missing_image_renders_placeholder() {
        assert_eq!(placeholder_text("Cat", "cat.png"), "[image: Cat]");
        let b = render_markdown("![Cat](cat.png)");
        assert!(has_ink(&b), "missing image rendered nothing");
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "[image: Cat]".to_string(),
                style: Style::new(FontStyle::Italic, BODY_SIZE),
            }],
            indent: 0,
        }]);
        assert_eq!(
            rows(&b),
            rows(&expected),
            "missing image should render an italic placeholder line"
        );
    }

    #[test]
    fn placeholder_uses_dest_when_no_alt() {
        assert_eq!(placeholder_text("", "x.png"), "[image: x.png]");
        let b = render_markdown("![](x.png)");
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "[image: x.png]".to_string(),
                style: Style::new(FontStyle::Italic, BODY_SIZE),
            }],
            indent: 0,
        }]);
        assert_eq!(
            rows(&b),
            rows(&expected),
            "empty alt should fall back to dest"
        );
    }

    #[test]
    fn placeholder_flattens_styled_alt_text() {
        // Alt text is the image's inner text content, emphasis dropped.
        assert!(markdown_image_refs("![*a* b](x.png)") == vec!["x.png"]);
        let b = render_markdown("![*a* b](x.png)");
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "[image: a b]".to_string(),
                style: Style::new(FontStyle::Italic, BODY_SIZE),
            }],
            indent: 0,
        }]);
        assert_eq!(rows(&b), rows(&expected), "styled alt text should flatten");
    }

    #[test]
    fn image_refs_skips_empty_and_nested_dests() {
        // Nothing to fetch, so nothing to report.
        assert!(markdown_image_refs("![alt]()").is_empty());
        // An image inside another's alt text is consumed as alt text and never
        // rendered, so its destination would be a wasted fetch.
        assert_eq!(
            markdown_image_refs("![a ![b](inner.png)](outer.png)"),
            vec!["outer.png"]
        );
    }

    #[test]
    fn placeholder_marks_a_wholly_empty_image() {
        assert_eq!(placeholder_text("", ""), "[image: ?]");
        assert!(
            has_ink(&render_markdown("![]()")),
            "an empty image should still print a placeholder"
        );
    }

    /// Count the blocks a document lowers to, by kind, for "no stray block"
    /// assertions: (lines blocks, image blocks, other blocks).
    fn block_kinds(md: &str, images: &HashMap<String, Bitmap>) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for block in lower(md, images) {
            match block {
                MdBlock::Lines(_) => counts.0 += 1,
                MdBlock::Image(_) => counts.1 += 1,
                _ => counts.2 += 1,
            }
        }
        counts
    }

    #[test]
    fn image_in_table_cell_renders_inside_the_cell() {
        let md = "| a | b |\n|---|---|\n| 1 | ![Cat](cat.png) |";
        // A table cell is monospace text: the placeholder belongs *in* the
        // cell, not as a stray line after the table.
        assert_eq!(
            block_kinds(md, &HashMap::new()),
            (1, 0, 0),
            "a table with an image should lower to exactly one lines block"
        );
        // The cell holds the placeholder text, so the table renders identically
        // to one whose cell contains that text literally (escaped brackets).
        let literal = "| a | b |\n|---|---|\n| 1 | \\[image: Cat\\] |";
        assert_eq!(
            rows(&render_markdown(md)),
            rows(&render_markdown(literal)),
            "image cell should render as its placeholder text"
        );
    }

    #[test]
    fn supplied_image_in_table_cell_stays_in_the_cell() {
        let md = "| a | b |\n|---|---|\n| 1 | ![Cat](cat.png) |";
        let mut pic = Bitmap::new(40);
        pic.set(10, 5, true);
        let images = HashMap::from([("cat.png".to_string(), pic)]);
        // Nothing can stack inside a monospace cell, so a supplied image
        // collapses to the same placeholder rather than escaping the table —
        // which, with rows still buffered, would emit it *before* the table.
        assert_eq!(
            block_kinds(md, &images),
            (1, 0, 0),
            "a supplied image in a cell must not become its own block"
        );
        assert_eq!(
            rows(&render_markdown_with(md, &images)),
            rows(&render_markdown(md)),
            "a supplied image in a cell renders like a missing one"
        );
    }

    #[test]
    fn image_in_list_keeps_indent() {
        let b = render_markdown("- ![Cat](cat.png)");
        assert!(has_ink(&b), "list image placeholder has no ink");
        assert!(!ink_before(&b, 24), "ink left of the 24 px list indent");
    }

    #[test]
    fn render_markdown_delegates() {
        let md = "# Title\n\n![Cat](cat.png)\n\ntext";
        assert_eq!(
            rows(&render_markdown(md)),
            rows(&render_markdown_with(md, &HashMap::new())),
            "render_markdown should equal render_markdown_with an empty map"
        );
    }

    #[test]
    fn plain_code_fence_unaffected() {
        let expected = render_rich(&[RichLine {
            spans: vec![Span {
                text: "code".to_string(),
                style: Style::new(FontStyle::Regular, CODE_SIZE),
            }],
            indent: CODE_INDENT,
        }]);
        // The classifier matches whole fence names, never prefixes: only
        // exactly `qr` and `barcode` turn into graphics.
        for lang in ["", "rust", "qrcode", "qrs", "barcodes", "barcodex", "bar"] {
            let md = format!("```{lang}\ncode\n```");
            assert_eq!(
                rows(&render_markdown(&md)),
                rows(&expected),
                "{md:?} should still render as code text"
            );
        }
    }
}
