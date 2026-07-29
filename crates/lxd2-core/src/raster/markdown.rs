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
//! rule. Links render their inner text only; images and raw HTML are skipped.
//! Table parsing is enabled but layout is deferred (Task 2): each table row
//! flattens to a plain space-separated text line for now.
//!
//! Deviation from the plan's break mapping: a soft break renders as a space
//! (standard markdown behavior, reads better on a 384 px roll); only a hard
//! break starts a new line. Trailing blank space after the last block is
//! trimmed, as are blank lines abutting a horizontal rule (the rule carries
//! its own margins).

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::bitmap::{Bitmap, WIDTH};
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

/// A vertically-stacked unit of lowered markdown.
enum MdBlock {
    Lines(Vec<RichLine>),
    Rule,
    /// A dashed tear-off line (`- - -` in the source).
    Tear,
}

/// Render markdown to a 1-bit bitmap.
///
/// Empty (or whitespace-only) markdown yields a zero-height bitmap.
pub fn render_markdown(md: &str) -> Bitmap {
    let blocks = lower(md);
    let bitmaps = blocks
        .iter()
        .map(|block| match block {
            MdBlock::Lines(lines) => render_rich(lines),
            MdBlock::Rule => rule_bitmap(),
            MdBlock::Tear => tear_bitmap(),
        })
        .collect();
    stack(bitmaps)
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

/// Event-stream lowering state: markdown events → [`MdBlock`]s.
struct Lowering {
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
    /// Partial code line, pending its `\n` (or the block's end).
    code_buf: String,
    /// Image nesting depth; while > 0 all events are skipped.
    image_depth: u32,
}

fn lower(md: &str) -> Vec<MdBlock> {
    let mut st = Lowering {
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
        code_buf: String::new(),
        image_depth: 0,
    };
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES;
    // The offset iterator is only consulted for Rule events: the source text
    // distinguishes a tear marker (`- - -`) from a plain rule (`---`).
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        match event {
            Event::Rule => st.rule(md[range].trim()),
            _ => st.handle(event),
        }
    }
    st.finish()
}

impl Lowering {
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

    fn handle(&mut self, event: Event) {
        // Inside an image: swallow everything (alt text included) until it ends.
        if self.image_depth > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => self.image_depth += 1,
                Event::End(TagEnd::Image) => self.image_depth -= 1,
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.in_code {
                    self.push_code_text(&text);
                } else {
                    self.push_span(&text);
                }
            }
            Event::Code(text) => self.push_span(&text),
            Event::SoftBreak => self.push_span(" "),
            Event::HardBreak => self.flush_line(),
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
            Tag::CodeBlock(_) => {
                self.flush_line();
                self.in_code = true;
                self.current.indent = self.quote_indent() + CODE_INDENT;
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            // Task 2 adds real table layout; for now each row (header
            // included) flattens to a plain space-separated text line.
            Tag::Table(_) => {
                self.flush_line();
                self.current.indent = self.quote_indent();
            }
            Tag::TableHead | Tag::TableRow => self.flush_line(),
            Tag::TableCell => {
                if !self.current.spans.is_empty() {
                    self.push_span(" ");
                }
            }
            Tag::Image { .. } => self.image_depth = 1,
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
                // A fenced block's text ends in `\n`, leaving an empty buffer;
                // flush any remainder so a missing final newline still prints.
                if !self.code_buf.is_empty() {
                    self.flush_code_line();
                }
                self.in_code = false;
                self.current.indent = 0;
                self.push_blank();
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Table => {
                self.flush_line();
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
    fn table_flattens_to_text_lines() {
        // Task 2 adds real table layout; until then cell text flattens to
        // plain space-separated lines (one per row, pipes and the alignment
        // row consumed) so nothing is silently lost.
        let b = render_markdown("| a | b |\n| --- | --- |\n| c | d |");
        let expected = render_rich(&[
            RichLine {
                spans: vec![Span {
                    text: "a b".to_string(),
                    style: Style::default(),
                }],
                indent: 0,
            },
            RichLine {
                spans: vec![Span {
                    text: "c d".to_string(),
                    style: Style::default(),
                }],
                indent: 0,
            },
        ]);
        assert!(has_ink(&b), "table render has no ink");
        assert_eq!(
            rows(&b),
            rows(&expected),
            "table should flatten to one plain line per row"
        );
    }

    #[test]
    fn blockquote_indented() {
        let b = render_markdown("> quote");
        assert!(has_ink(&b), "blockquote has no ink");
        assert!(!ink_before(&b, 24), "ink left of the 24 px quote indent");
    }
}
