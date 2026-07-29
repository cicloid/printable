//! Styled text → 1-bit bitmap rendering: mixed fonts and sizes per line.

use std::sync::OnceLock;

use fontdue::Font;

use super::bitmap::{Bitmap, WIDTH};

const REGULAR_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Regular.ttf");
const BOLD_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Bold.ttf");
const ITALIC_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Italic.ttf");

/// Glyph coverage at or above this value (of 255) is printed black.
const COVERAGE_THRESHOLD: u8 = 128;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_FACTOR: f32 = 1.3;

/// Line-height basis for a [`RichLine`] with no spans (a blank line).
const DEFAULT_SIZE_PX: f32 = 24.0;

/// Height of the strikethrough line above the baseline, as a fraction of the
/// font size.
const STRIKE_FACTOR: f32 = 0.35;

/// Thickness of the strikethrough line, in pixels.
const STRIKE_THICKNESS: usize = 2;

/// Which embedded JetBrains Mono face to render with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    Regular,
    Bold,
    Italic,
}

/// Font face plus pixel size for a span of text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub font: FontStyle,
    pub size_px: f32,
    /// Draw a 2 px strikethrough line across the span.
    pub strike: bool,
}

impl Style {
    /// A plain (non-struck) style with the given face and size.
    pub fn new(font: FontStyle, size_px: f32) -> Self {
        Style {
            font,
            size_px,
            strike: false,
        }
    }
}

impl Default for Style {
    /// Regular face, 24 px, no strikethrough.
    fn default() -> Self {
        Style::new(FontStyle::Regular, DEFAULT_SIZE_PX)
    }
}

/// A run of text rendered in a single style.
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

fn font_for(style: FontStyle) -> &'static Font {
    static REGULAR: OnceLock<Font> = OnceLock::new();
    static BOLD: OnceLock<Font> = OnceLock::new();
    static ITALIC: OnceLock<Font> = OnceLock::new();
    let (lock, bytes) = match style {
        FontStyle::Regular => (&REGULAR, REGULAR_BYTES),
        FontStyle::Bold => (&BOLD, BOLD_BYTES),
        FontStyle::Italic => (&ITALIC, ITALIC_BYTES),
    };
    lock.get_or_init(|| {
        Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("embedded font is valid")
    })
}

/// Normalize text for rendering: `\r\n` and bare `\r` become `\n`, and `\t`
/// expands to four spaces (predictable with a monospace font).
pub(crate) fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
}

/// A glyph placed by the layout pass, prior to rasterization.
struct PlacedGlyph {
    ch: char,
    pen_x: f32,
    line: usize,
    style: Style,
}

/// Layout state for one rendered (post-wrap) line.
struct RenderedLine {
    indent: u32,
    /// Largest glyph size placed on this line; 0 until a glyph lands here.
    max_size: f32,
    /// Height basis when no glyph lands here (blank or fully swallowed line).
    default_size: f32,
    /// Largest ascent among the styles placed on this line.
    ascent: f32,
}

/// Render styled lines to a 1-bit bitmap: greedy word-wrap, left-aligned.
///
/// Each [`RichLine`] wraps greedily at 384 px minus its `indent`, and wrapped
/// continuation lines keep the indent. A rendered line is 1.3 × the largest
/// `size_px` on it, and all its glyphs share one baseline placed at the
/// largest ascent among its styles. Overlong single words break mid-word.
///
/// An empty `lines` slice yields a zero-height bitmap; a [`RichLine`] with no
/// spans renders as a blank line of height 1.3 × 24 px.
///
/// Span text is normalized (`\r\n`/`\r` → `\n`, `\t` → four spaces), but
/// spans are expected to hold single logical lines: a remaining `\n` is not a
/// line break here — it renders as an ordinary (blank) glyph.
pub fn render_rich(lines: &[RichLine]) -> Bitmap {
    let mut placed: Vec<PlacedGlyph> = Vec::new();
    let mut rendered: Vec<RenderedLine> = Vec::new();

    // Layout pass: assign each glyph a pen position and rendered-line index.
    for rich_line in lines {
        let default_size = rich_line
            .spans
            .iter()
            .map(|s| s.style.size_px)
            .reduce(f32::max)
            .unwrap_or(DEFAULT_SIZE_PX);
        let new_line = || RenderedLine {
            indent: rich_line.indent,
            max_size: 0.0,
            default_size,
            ascent: 0.0,
        };
        let max_x = WIDTH as f32 - rich_line.indent as f32;
        let advance = |ch: char, style: Style| {
            font_for(style.font)
                .metrics(ch, style.size_px)
                .advance_width
        };

        // Flatten the spans into one styled character sequence, then split it
        // into space-separated words (a word may mix styles across spans).
        let chars: Vec<(char, Style)> = rich_line
            .spans
            .iter()
            .flat_map(|s| {
                normalize(&s.text)
                    .chars()
                    .map(|ch| (ch, s.style))
                    .collect::<Vec<_>>()
            })
            .collect();
        let words: Vec<&[(char, Style)]> = chars.split(|&(ch, _)| ch == ' ').collect();
        let space_styles: Vec<Style> = chars
            .iter()
            .filter(|&&(ch, _)| ch == ' ')
            .map(|&(_, style)| style)
            .collect();

        rendered.push(new_line());
        let mut pen_x = 0.0f32;
        for (j, word) in words.iter().enumerate() {
            let word_width: f32 = word.iter().map(|&(ch, style)| advance(ch, style)).sum();
            if j > 0 {
                // Wrap before the word if it (plus its leading space) overflows;
                // the space is swallowed at the break.
                let space_w = advance(' ', space_styles[j - 1]);
                if pen_x + space_w + word_width > max_x && pen_x > 0.0 {
                    rendered.push(new_line());
                    pen_x = 0.0;
                } else {
                    // Place the space too: it blits nothing, but a struck
                    // style draws its strike line across the space's advance.
                    placed.push(PlacedGlyph {
                        ch: ' ',
                        pen_x,
                        line: rendered.len() - 1,
                        style: space_styles[j - 1],
                    });
                    pen_x += space_w;
                }
            }
            for &(ch, style) in word.iter() {
                let adv = advance(ch, style);
                // Break overlong single words mid-word rather than overflowing.
                if pen_x + adv > max_x && pen_x > 0.0 {
                    rendered.push(new_line());
                    pen_x = 0.0;
                }
                placed.push(PlacedGlyph {
                    ch,
                    pen_x,
                    line: rendered.len() - 1,
                    style,
                });
                let line = rendered.last_mut().expect("at least one rendered line");
                line.max_size = line.max_size.max(style.size_px);
                let ascent = font_for(style.font)
                    .horizontal_line_metrics(style.size_px)
                    .map(|m| m.ascent)
                    .unwrap_or(style.size_px);
                line.ascent = line.ascent.max(ascent);
                pen_x += adv;
            }
        }
    }

    // Vertical pass: stack rendered-line heights and compute the total.
    let mut y = 0.0f32;
    let mut offsets = Vec::with_capacity(rendered.len());
    for line in &mut rendered {
        if line.max_size == 0.0 {
            line.max_size = line.default_size;
        }
        offsets.push(y);
        y += line.max_size * LINE_HEIGHT_FACTOR;
    }
    let height = y.ceil() as usize;
    let mut bitmap = Bitmap::new(height);

    // Raster pass: blit each glyph's coverage onto the bitmap.
    for g in &placed {
        let line = &rendered[g.line];
        let (metrics, coverage) = font_for(g.style.font).rasterize(g.ch, g.style.size_px);
        let baseline = offsets[g.line] + line.ascent;
        let x0 = line.indent as i64 + (g.pen_x + metrics.xmin as f32).round() as i64;
        let y0 = (baseline - metrics.ymin as f32).round() as i64 - metrics.height as i64;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                if coverage[row * metrics.width + col] < COVERAGE_THRESHOLD {
                    continue;
                }
                let x = x0 + col as i64;
                let y = y0 + row as i64;
                if (0..WIDTH as i64).contains(&x) && (0..height as i64).contains(&y) {
                    bitmap.set(x as usize, y as usize, true);
                }
            }
        }
        // Strikethrough: a bar across the glyph's full advance (pen to
        // pen + advance), so consecutive struck glyphs form a continuous line.
        if g.style.strike {
            let advance = font_for(g.style.font)
                .metrics(g.ch, g.style.size_px)
                .advance_width;
            let y_top = (baseline - STRIKE_FACTOR * g.style.size_px).round() as i64;
            let x_start = line.indent as i64 + g.pen_x.round() as i64;
            let x_end = line.indent as i64 + (g.pen_x + advance).round() as i64;
            for y in y_top..y_top + STRIKE_THICKNESS as i64 {
                for x in x_start..x_end {
                    if (0..WIDTH as i64).contains(&x) && (0..height as i64).contains(&y) {
                        bitmap.set(x as usize, y as usize, true);
                    }
                }
            }
        }
    }
    bitmap
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(font: FontStyle, size_px: f32) -> Style {
        Style::new(font, size_px)
    }

    fn span(text: &str, font: FontStyle, size_px: f32) -> Span {
        Span {
            text: text.to_string(),
            style: style(font, size_px),
        }
    }

    fn line(spans: Vec<Span>, indent: u32) -> RichLine {
        RichLine { spans, indent }
    }

    fn has_ink(b: &Bitmap) -> bool {
        (0..b.height()).any(|y| (0..WIDTH).any(|x| b.get(x, y)))
    }

    fn min_ink_x(b: &Bitmap) -> Option<usize> {
        (0..WIDTH).find(|&x| (0..b.height()).any(|y| b.get(x, y)))
    }

    fn rows(b: &Bitmap) -> Vec<Vec<u8>> {
        (0..b.height()).map(|y| b.row(y).to_vec()).collect()
    }

    #[test]
    fn bold_differs_from_regular() {
        let regular = render_rich(&[line(vec![span("Hello", FontStyle::Regular, 24.0)], 0)]);
        let bold = render_rich(&[line(vec![span("Hello", FontStyle::Bold, 24.0)], 0)]);
        assert!(has_ink(&regular), "regular render has no ink");
        assert!(has_ink(&bold), "bold render has no ink");
        assert_ne!(
            rows(&regular),
            rows(&bold),
            "bold should differ from regular"
        );
    }

    #[test]
    fn mixed_sizes_share_baseline() {
        let small = style(FontStyle::Regular, 24.0);
        let b = render_rich(&[line(
            vec![
                span("Ag", FontStyle::Regular, 24.0),
                span("Ag", FontStyle::Regular, 36.0),
            ],
            0,
        )]);
        let expected = (36.0f32 * LINE_HEIGHT_FACTOR).ceil() as usize;
        assert_eq!(b.height(), expected);
        // Split at the end of the first span: both spans must have ink.
        let font = font_for(small.font);
        let w1: f32 = "Ag"
            .chars()
            .map(|c| font.metrics(c, small.size_px).advance_width)
            .sum();
        let split = w1.ceil() as usize;
        let ink_left = (0..b.height()).any(|y| (0..split).any(|x| b.get(x, y)));
        let ink_right = (0..b.height()).any(|y| (split..WIDTH).any(|x| b.get(x, y)));
        assert!(ink_left, "24 px span has no ink");
        assert!(ink_right, "36 px span has no ink");
    }

    #[test]
    fn indent_shifts_ink_right() {
        let b = render_rich(&[line(vec![span("Hello", FontStyle::Regular, 24.0)], 40)]);
        assert!(has_ink(&b));
        assert!(
            min_ink_x(&b) >= Some(40),
            "ink left of indent: {:?}",
            min_ink_x(&b)
        );
    }

    #[test]
    fn wrap_keeps_indent() {
        let text = "word ".repeat(30);
        let one_line = render_rich(&[line(vec![span("word", FontStyle::Regular, 24.0)], 40)]);
        let b = render_rich(&[line(vec![span(&text, FontStyle::Regular, 24.0)], 40)]);
        assert!(
            b.height() > one_line.height(),
            "long text should wrap to multiple lines"
        );
        assert!(has_ink(&b));
        assert!(
            min_ink_x(&b) >= Some(40),
            "ink left of indent: {:?}",
            min_ink_x(&b)
        );
    }

    #[test]
    fn strike_differs_from_plain() {
        let struck_style = Style {
            strike: true,
            ..Style::default()
        };
        let plain = render_rich(&[line(vec![span("abc", FontStyle::Regular, 24.0)], 0)]);
        let struck = render_rich(&[line(
            vec![Span {
                text: "abc".to_string(),
                style: struck_style,
            }],
            0,
        )]);
        assert!(has_ink(&plain), "plain render has no ink");
        assert!(has_ink(&struck), "struck render has no ink");
        assert_ne!(
            rows(&plain),
            rows(&struck),
            "strike should differ from plain"
        );
    }

    #[test]
    fn strike_has_line_at_strike_height() {
        let struck_style = Style {
            strike: true,
            ..Style::default()
        };
        let b = render_rich(&[line(
            vec![Span {
                text: "ab cd".to_string(),
                style: struck_style,
            }],
            0,
        )]);
        let font = font_for(struck_style.font);
        let ascent = font
            .horizontal_line_metrics(struck_style.size_px)
            .expect("font has line metrics")
            .ascent;
        let y = (ascent - 0.35 * struck_style.size_px).round() as usize;
        let width: f32 = "ab cd"
            .chars()
            .map(|c| font.metrics(c, struck_style.size_px).advance_width)
            .sum();
        // The line must be continuous across the full advance width — no
        // breaks in inter-glyph gaps or at the word space.
        for x in 0..width.floor() as usize {
            assert!(b.get(x, y), "strike line broken at x={x}, y={y}");
        }
    }

    #[test]
    fn font_lacks_ballot_box_glyphs() {
        // Pins the task-list marker decision: JetBrains Mono has no glyphs
        // for U+2610 BALLOT BOX or U+2611 BALLOT BOX WITH CHECK, so markdown
        // checkboxes fall back to ASCII "[ ] " / "[x] ".
        let font = font_for(FontStyle::Regular);
        assert_eq!(font.lookup_glyph_index('\u{2610}'), 0);
        assert_eq!(font.lookup_glyph_index('\u{2611}'), 0);
    }

    #[test]
    fn empty_lines_gives_empty_bitmap() {
        assert_eq!(render_rich(&[]).height(), 0);
    }

    #[test]
    fn spanless_line_is_blank() {
        let b = render_rich(&[RichLine::default()]);
        let expected = (24.0f32 * LINE_HEIGHT_FACTOR).ceil() as usize;
        assert_eq!(b.height(), expected);
        assert!(!has_ink(&b), "blank line should have no ink");
    }
}
