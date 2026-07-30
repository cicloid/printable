//! Plain text → 1-bit bitmap rendering: a thin wrapper over [`super::rich`].

use super::bitmap::Bitmap;
use super::rich::{normalize, render_rich, FontStyle, RichLine, Span, Style};

/// Render text to a 1-bit bitmap: greedy word-wrap at 384 px, left-aligned.
///
/// Line height is 1.3 × `size_px`; `\n` forces a hard break. Overlong single
/// words break mid-word. Empty text yields a zero-height bitmap.
///
/// Input is normalized first: `\r\n` and bare `\r` become `\n`, and `\t`
/// expands to four spaces (predictable with a monospace font).
pub fn render_text(text: &str, size_px: f32) -> Bitmap {
    let text = normalize(text);
    if text.is_empty() {
        return Bitmap::new(0);
    }
    let style = Style::new(FontStyle::Regular, size_px);
    let lines: Vec<RichLine> = text
        .split('\n')
        .map(|line| RichLine {
            spans: vec![Span {
                text: line.to_string(),
                style,
            }],
            indent: 0,
        })
        .collect();
    render_rich(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 384;

    /// Line height for a 24 px line, matching `rich`'s 1.3 factor.
    #[cfg(feature = "cjk")]
    const LINE_24: usize = 32;

    fn ink_count(b: &Bitmap) -> usize {
        (0..b.height())
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| b.get(x, y))
            .count()
    }

    #[cfg(feature = "cjk")]
    fn ink_in(b: &Bitmap, xs: std::ops::Range<usize>) -> bool {
        (0..b.height()).any(|y| xs.clone().any(|x| b.get(x, y)))
    }

    #[cfg(feature = "cjk")]
    fn rows(b: &Bitmap) -> Vec<Vec<u8>> {
        (0..b.height()).map(|y| b.row(y).to_vec()).collect()
    }

    #[test]
    fn renders_nonempty_bitmap_with_ink() {
        let b = render_text("Hello", 24.0);
        assert!(b.height() > 0);
        assert!(ink_count(&b) > 0, "expected some black pixels");
    }

    #[test]
    fn wraps_long_lines() {
        let short = render_text("hi", 24.0);
        let long = render_text(&"word ".repeat(40), 24.0);
        assert!(long.height() > short.height() * 3);
    }

    #[test]
    fn empty_text_gives_empty_bitmap() {
        assert_eq!(render_text("", 24.0).height(), 0);
    }

    #[test]
    fn newline_forces_hard_break() {
        let one = render_text("a", 24.0);
        let two = render_text("a\nb", 24.0);
        // Two lines = ceil(2 * line_height), one line = ceil(line_height).
        assert!(
            two.height() > one.height(),
            "{} vs {}",
            two.height(),
            one.height()
        );
        assert!(two.height() >= one.height() * 2 - 1);
    }

    #[test]
    fn overlong_word_breaks_mid_word() {
        let one_line = render_text("M", 24.0);
        let long = render_text(&"M".repeat(60), 24.0);
        assert!(long.height() > one_line.height());
    }

    /// An unassigned code point (plane 10) — no face bundled here has it, so
    /// it always rasterizes as the Latin face's .notdef box.
    #[cfg(feature = "cjk")]
    const TOFU: char = '\u{ABCDE}';

    #[test]
    #[cfg(feature = "cjk")]
    fn japanese_text_renders_ink() {
        let b = render_text("和柄", 24.0);
        let tofu = render_text(&TOFU.to_string().repeat(2), 24.0);
        assert!(ink_count(&tofu) > 0, "tofu baseline should have ink");
        assert_ne!(rows(&b), rows(&tofu), "kanji rendered as tofu boxes");
        // Two kanji at 24 px are far denser than two hollow boxes.
        assert!(
            ink_count(&b) > ink_count(&tofu),
            "kanji ink {} vs tofu ink {}",
            ink_count(&b),
            ink_count(&tofu)
        );
        // Full-width: two glyphs at 1 em each occupy ~48 px.
        assert!(ink_in(&b, 24..48), "second kanji is missing");
        assert!(!ink_in(&b, 50..WIDTH), "ink past the second kanji");
    }

    #[test]
    #[cfg(feature = "cjk")]
    fn mixed_latin_and_japanese_shares_a_line() {
        let b = render_text("Hello 世界", 24.0);
        assert_eq!(b.height(), LINE_24, "mixed text should be one line");
        assert!(ink_in(&b, 0..70), "Latin half has no ink");
        // "Hello " is 6 half-width glyphs (86 px); two full-width kanji then
        // run to ~134 px. Half-width tofu would stop short of 116 px.
        assert!(ink_in(&b, 120..135), "Japanese half has no ink");
        assert!(!ink_in(&b, 140..WIDTH), "ink past the second kanji");
    }

    #[test]
    #[cfg(feature = "cjk")]
    fn cjk_wraps_without_panic() {
        // 200 kanji with no spaces: every character is a break opportunity,
        // so the mid-word break path must split between characters.
        let b = render_text(&"日".repeat(200), 24.0);
        // 384 px / 24 px per glyph = 16 per line, 200 chars = 13 lines.
        let thirteen_lines = (24.0f32 * 1.3 * 13.0).ceil() as usize;
        assert_eq!(b.height(), thirteen_lines, "unexpected line count");
        assert!(ink_count(&b) > 0);
        // Nothing may be clipped off the right edge: the 16th glyph of a
        // full line ends exactly at 384, so the last column stays blank.
        assert!(!ink_in(&b, WIDTH - 1..WIDTH), "ink at the right edge");
    }

    #[test]
    #[cfg(feature = "cjk")]
    fn cjk_advance_is_wider_than_latin() {
        // Twenty Latin monospace glyphs (0.6 em) fit on one 384 px line;
        // twenty full-width kanji (1 em) cannot. This only holds if layout
        // takes the advance from the face that renders the glyph.
        let latin = render_text(&"M".repeat(20), 24.0);
        let kanji = render_text(&"日".repeat(20), 24.0);
        assert_eq!(latin.height(), LINE_24);
        assert!(
            kanji.height() > latin.height(),
            "kanji advance not wider: {} vs {}",
            kanji.height(),
            latin.height()
        );
    }
}
