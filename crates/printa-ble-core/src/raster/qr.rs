//! QR code → 1-bit bitmap rendering.
//!
//! The code is encoded with automatic version and error-correction selection,
//! scaled by the largest integer factor that fits 384 px including a 4-module
//! quiet zone on every side, and centered horizontally with a 16 px white
//! margin above and below. Even a version-40 code (177 modules + 8 quiet =
//! 185) fits at factor 2 (370 px), so the scale factor is always ≥ 2. An
//! optional caption is rendered below via [`render_text`] at 24 px
//! (left-aligned — centering is not supported by the text renderer).

use qrcode::{Color, QrCode};

use super::bitmap::{Bitmap, WIDTH};
use super::text::render_text;

/// White margin above and below the code, in pixels. Visible to the markdown
/// renderer, which pads fence blocks *to* a common margin rather than by one.
pub(crate) const MARGIN: usize = 16;
/// Quiet-zone width on each side, in modules (the QR spec's minimum).
const QUIET_MODULES: usize = 4;
/// Caption text size in pixels.
const CAPTION_SIZE: f32 = 24.0;

/// Errors from QR encoding.
#[derive(Debug, thiserror::Error)]
pub enum QrError {
    /// The payload does not fit in any QR code version.
    #[error("data too long to fit in a QR code")]
    DataTooLong,
    /// Any other encoding failure from the `qrcode` crate.
    #[error("QR encoding failed: {0}")]
    Encode(qrcode::types::QrError),
}

impl From<qrcode::types::QrError> for QrError {
    fn from(e: qrcode::types::QrError) -> Self {
        match e {
            qrcode::types::QrError::DataTooLong => QrError::DataTooLong,
            other => QrError::Encode(other),
        }
    }
}

/// Render `data` as a QR code, optionally with a text caption below.
pub fn render_qr(data: &str, caption: Option<&str>) -> Result<Bitmap, QrError> {
    let code = QrCode::new(data)?;
    let modules = code.width();
    let total_modules = modules + 2 * QUIET_MODULES;
    let scale = WIDTH / total_modules;
    debug_assert!(scale >= 2, "even version 40 scales by at least 2");
    // Full square including the quiet zone, ≤ 384 px. The quiet zone is
    // white: only dark modules are drawn, offset past it.
    let side = scale * total_modules;
    let x0 = (WIDTH - side) / 2 + scale * QUIET_MODULES;
    let y0 = MARGIN + scale * QUIET_MODULES;

    let caption = caption.map(|c| render_text(c, CAPTION_SIZE));
    let qr_height = MARGIN + side + MARGIN;
    let mut out = Bitmap::new(qr_height + caption.as_ref().map_or(0, Bitmap::height));

    let colors = code.to_colors(); // row-major, `modules` per row
    for my in 0..modules {
        for mx in 0..modules {
            if colors[my * modules + mx] == Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        out.set(x0 + mx * scale + dx, y0 + my * scale + dy, true);
                    }
                }
            }
        }
    }

    if let Some(cap) = caption {
        for y in 0..cap.height() {
            for x in 0..WIDTH {
                if cap.get(x, y) {
                    out.set(x, qr_height + y, true);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::bitmap::WIDTH;

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

    #[test]
    fn small_payload_renders_centered_square() {
        let b = render_qr("hello", None).unwrap();
        let (min_x, min_y, max_x, max_y) = ink_bbox(&b).expect("QR render has no ink");
        // "hello" fits version 1: 21 modules + 8 quiet = 29, scale 13 →
        // the quiet zone alone is 52 px; no ink anywhere left of x = 40.
        assert!(min_x >= 40, "ink at x = {min_x}, inside the quiet zone");
        // Horizontally centered: left and right white margins within 2 px.
        let right = WIDTH - 1 - max_x;
        assert!(
            min_x.abs_diff(right) <= 2,
            "not centered: left {min_x}, right {right}"
        );
        // 16 px top margin plus the scaled quiet zone above the code.
        assert!(min_y >= 16, "ink at y = {min_y}, inside the top margin");
        // Ink bounding box roughly square (within 10%).
        let w = max_x - min_x + 1;
        let h = max_y - min_y + 1;
        assert!(
            w.abs_diff(h) * 10 <= w,
            "bounding box {w}x{h} not square within 10%"
        );
    }

    #[test]
    fn caption_adds_height() {
        let plain = render_qr("x", None).unwrap();
        let captioned = render_qr("x", Some("caption")).unwrap();
        assert!(
            captioned.height() > plain.height(),
            "caption {} should be taller than plain {}",
            captioned.height(),
            plain.height()
        );
    }

    #[test]
    fn finder_patterns_present() {
        let b = render_qr("hello", None).unwrap();
        let (min_x, min_y, max_x, max_y) = ink_bbox(&b).expect("QR render has no ink");
        // Finder patterns put black modules at the top-left, top-right, and
        // bottom-left corners of the code's ink bounding box.
        assert!(b.get(min_x, min_y), "no ink at top-left corner");
        assert!(b.get(max_x, min_y), "no ink at top-right corner");
        assert!(b.get(min_x, max_y), "no ink at bottom-left corner");
    }

    #[test]
    fn huge_payload_errors() {
        // 4000 bytes exceeds version 40's byte-mode capacity (2953 at EC L).
        let data = "a".repeat(4000);
        let err = render_qr(&data, None).unwrap_err();
        assert!(matches!(err, QrError::DataTooLong), "got {err:?}");
    }

    #[test]
    fn empty_data_ok() {
        // The qrcode crate accepts empty payloads (an empty segment stream is
        // a valid version-1 code), so this must render without error.
        let b = render_qr("", None).unwrap();
        assert!(ink_bbox(&b).is_some(), "empty-data QR has no ink");
    }
}
