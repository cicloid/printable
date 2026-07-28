//! Text → 1-bit bitmap rendering with an embedded monospace font.

use std::sync::OnceLock;

use fontdue::Font;

use super::bitmap::{Bitmap, WIDTH};

const FONT_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Regular.ttf");

/// Glyph coverage at or above this value (of 255) is printed black.
const COVERAGE_THRESHOLD: u8 = 128;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_FACTOR: f32 = 1.3;

fn font() -> &'static Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("embedded font is valid")
    })
}

/// A glyph placed by the layout pass, prior to rasterization.
struct PlacedGlyph {
    ch: char,
    pen_x: f32,
    line: usize,
}

/// Render text to a 1-bit bitmap: greedy word-wrap at 384 px, left-aligned.
///
/// Line height is 1.3 × `size_px`; `\n` forces a hard break. Overlong single
/// words break mid-word. Empty text yields a zero-height bitmap.
///
/// Input is normalized first: `\r\n` and bare `\r` become `\n`, and `\t`
/// expands to four spaces (predictable with a monospace font).
pub fn render_text(text: &str, size_px: f32) -> Bitmap {
    let text = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ");
    if text.is_empty() {
        return Bitmap::new(0);
    }

    let font = font();
    let advance = |ch: char| font.metrics(ch, size_px).advance_width;
    let space_w = advance(' ');
    let max_x = WIDTH as f32;

    // Layout pass: assign each glyph a pen position and line index.
    let mut placed = Vec::new();
    let mut line = 0usize;
    for (i, hard_line) in text.split('\n').enumerate() {
        if i > 0 {
            line += 1;
        }
        let mut pen_x = 0.0f32;
        for (j, word) in hard_line.split(' ').enumerate() {
            let word_width: f32 = word.chars().map(advance).sum();
            if j > 0 {
                // Wrap before the word if it (plus its leading space) overflows;
                // the space is swallowed at the break.
                if pen_x + space_w + word_width > max_x && pen_x > 0.0 {
                    line += 1;
                    pen_x = 0.0;
                } else {
                    pen_x += space_w;
                }
            }
            for ch in word.chars() {
                let adv = advance(ch);
                // Break overlong single words mid-word rather than overflowing.
                if pen_x + adv > max_x && pen_x > 0.0 {
                    line += 1;
                    pen_x = 0.0;
                }
                placed.push(PlacedGlyph { ch, pen_x, line });
                pen_x += adv;
            }
        }
    }

    let line_height = size_px * LINE_HEIGHT_FACTOR;
    let ascent = font
        .horizontal_line_metrics(size_px)
        .map(|m| m.ascent)
        .unwrap_or(size_px);
    let height = ((line + 1) as f32 * line_height).ceil() as usize;
    let mut bitmap = Bitmap::new(height);

    // Raster pass: blit each glyph's coverage onto the bitmap.
    for g in &placed {
        let (metrics, coverage) = font.rasterize(g.ch, size_px);
        let baseline = g.line as f32 * line_height + ascent;
        let x0 = (g.pen_x + metrics.xmin as f32).round() as i64;
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
    }
    bitmap
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
        assert!(two.height() > one.height(), "{} vs {}", two.height(), one.height());
        assert!(two.height() >= one.height() * 2 - 1);
    }

    #[test]
    fn overlong_word_breaks_mid_word() {
        let one_line = render_text("M", 24.0);
        let long = render_text(&"M".repeat(60), 24.0);
        assert!(long.height() > one_line.height());
    }
}
