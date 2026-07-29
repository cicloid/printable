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
    let style = Style {
        font: FontStyle::Regular,
        size_px,
    };
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

    #[test]
    fn renders_nonempty_bitmap_with_ink() {
        let b = render_text("Hello", 24.0);
        assert!(b.height() > 0);
        let ink = (0..b.height())
            .flat_map(|y| (0..384).map(move |x| (x, y)))
            .any(|(x, y)| b.get(x, y));
        assert!(ink, "expected some black pixels");
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
}
