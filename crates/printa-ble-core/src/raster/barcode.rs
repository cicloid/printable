//! Code128 barcode → 1-bit bitmap rendering.
//!
//! Callers pass plain text: the Code128 character-set escape the `barcoders`
//! crate requires is added here. Every barcode starts in character-set **B**
//! (`\u{0181}`, `Ɓ`), which covers all printable ASCII — space (U+0020)
//! through tilde (U+007E), i.e. digits, both letter cases, and punctuation.
//! Anything outside that range (accents, emoji, tabs, newlines) is rejected
//! with [`BarcodeError::Charset`] rather than silently mangled.
//!
//! The encoded bar columns are scaled by the largest integer factor that fits
//! 384 px minus a [`QUIET`] px quiet zone on each side, and centered. Bars are
//! [`BAR_HEIGHT`] px tall with no vertical margin (callers add their own) and
//! no human-readable text below. Because the module width must be at least
//! 1 px, roughly 29 characters is the practical limit; longer payloads fail
//! with [`BarcodeError::TooLong`].

use barcoders::sym::code128::Code128;

use super::bitmap::{Bitmap, WIDTH};

/// White quiet zone on each side of the bars, in pixels.
const QUIET: usize = 16;
/// Bar height, in pixels.
const BAR_HEIGHT: usize = 80;
/// Code128 character-set B start escape: the only set covering printable ASCII.
const CHARSET_B: char = '\u{0181}';

/// Errors from Code128 encoding.
#[derive(Debug, thiserror::Error)]
pub enum BarcodeError {
    /// The payload is empty.
    #[error("barcode data is empty")]
    Empty,
    /// The payload contains characters outside printable ASCII.
    #[error("barcode data must be printable ASCII")]
    Charset,
    /// The encoded symbol needs more than 384 px even at one pixel per module.
    #[error("barcode data too long to fit the paper")]
    TooLong,
    /// Any other encoding failure from the `barcoders` crate.
    #[error("barcode encoding failed: {0}")]
    Encode(barcoders::error::Error),
}

/// Render `data` as a Code128 barcode, centered on a [`BAR_HEIGHT`] px bitmap.
pub fn render_barcode(data: &str) -> Result<Bitmap, BarcodeError> {
    if data.is_empty() {
        return Err(BarcodeError::Empty);
    }
    if !data.chars().all(|c| (' '..='~').contains(&c)) {
        return Err(BarcodeError::Charset);
    }

    let mut payload = String::with_capacity(data.len() + CHARSET_B.len_utf8());
    payload.push(CHARSET_B);
    payload.push_str(data);
    let columns = Code128::new(payload)
        .map_err(BarcodeError::Encode)?
        .encode();

    // One bar column per encoded module, scaled to fill the quiet-zone budget.
    let scale = (WIDTH - 2 * QUIET) / columns.len();
    if scale == 0 {
        return Err(BarcodeError::TooLong);
    }
    let x0 = (WIDTH - scale * columns.len()) / 2;

    let mut out = Bitmap::new(BAR_HEIGHT);
    for (i, &module) in columns.iter().enumerate() {
        if module == 1 {
            for dx in 0..scale {
                for y in 0..BAR_HEIGHT {
                    out.set(x0 + i * scale + dx, y, true);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn plain_ascii_needs_no_escape_character() {
        // The point of this module: users never type `Ɓ`.
        let b = render_barcode("TICKET42").expect("plain ASCII must encode");
        assert_eq!(b.height(), BAR_HEIGHT);
        let bars = (0..WIDTH)
            .filter(|&x| tallest_run(&b, x) == BAR_HEIGHT)
            .count();
        assert!(bars > 50, "only {bars} full-height bar columns");
    }

    #[test]
    fn bars_sit_inside_the_quiet_zone_and_are_centered() {
        let b = render_barcode("HELLO123").unwrap();
        let inked: Vec<usize> = (0..WIDTH).filter(|&x| b.get(x, 0)).collect();
        let (&min_x, &max_x) = (inked.first().unwrap(), inked.last().unwrap());
        assert!(min_x >= QUIET, "ink at x = {min_x}, inside the quiet zone");
        assert!(
            max_x < WIDTH - QUIET,
            "ink at x = {max_x}, inside the quiet zone"
        );
        let right = WIDTH - 1 - max_x;
        // Code128 always ends on a dark module, so the ink box is the symbol.
        assert!(
            min_x.abs_diff(right) <= 2,
            "not centered: left {min_x}, right {right}"
        );
    }

    #[test]
    fn printable_ascii_span_encodes() {
        let all: String = (' '..='~').collect();
        // Too long to fit, but it must fail on width, never on charset.
        assert!(matches!(
            render_barcode(&all).unwrap_err(),
            BarcodeError::TooLong
        ));
        for chunk in [
            "abc xyz",
            "0123456789",
            r##"!"#$%&'()*+,-./"##,
            "<=>?@[\\]^_`{|}~",
        ] {
            assert!(render_barcode(chunk).is_ok(), "{chunk:?} should encode");
        }
    }

    #[test]
    fn empty_and_non_ascii_are_rejected() {
        assert!(matches!(
            render_barcode("").unwrap_err(),
            BarcodeError::Empty
        ));
        for bad in ["café", "☕", "a\tb", "a\nb"] {
            assert!(
                matches!(render_barcode(bad).unwrap_err(), BarcodeError::Charset),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn single_character_encodes() {
        // `Code128::new` rejects payloads under 2 bytes; the charset escape is
        // 2 bytes on its own, so a 1-char payload must still work.
        assert!(render_barcode("7").is_ok());
    }
}
