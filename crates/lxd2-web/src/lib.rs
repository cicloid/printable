//! WASM bindings for `lxd2-core`: rendering entry points for the web page.
//!
//! Thin `wasm-bindgen` wrappers around the core rasterizer. The crate also
//! builds natively (rlib) so the wrappers can be unit-tested with plain
//! `cargo test`; fallible functions return `Result<_, String>`, which
//! wasm-bindgen converts to a thrown JS exception carrying the message.

use lxd2_core::raster::{self, Bitmap, Dither};
use wasm_bindgen::prelude::*;

/// Largest accepted font size in pixels, mirroring the server bound.
const MAX_TEXT_SIZE: f32 = 128.0;

/// A rendered 1-bit bitmap, 384 px wide. Opaque to JS: preview via
/// [`WasmBitmap::to_png`], print via the job bridge.
#[wasm_bindgen]
#[derive(Debug)]
pub struct WasmBitmap {
    inner: Bitmap,
}

#[wasm_bindgen]
impl WasmBitmap {
    /// Height in rows (pixels).
    pub fn height(&self) -> usize {
        self.inner.height()
    }

    /// Encode as a PNG for preview (`<img src=blob>`).
    pub fn to_png(&self) -> Vec<u8> {
        raster::bitmap_to_png(&self.inner)
    }

    /// Append `rows` blank rows (paper feed).
    pub fn extend_blank(&mut self, rows: usize) {
        self.inner.extend_blank(rows);
    }
}

impl WasmBitmap {
    fn new(inner: Bitmap) -> Self {
        Self { inner }
    }
}

/// Render plain text at `size` px. Size must be finite, > 0 and ≤ 128.
#[wasm_bindgen]
pub fn render_text(text: &str, size: f32) -> Result<WasmBitmap, String> {
    if !size.is_finite() || size <= 0.0 || size > MAX_TEXT_SIZE {
        return Err(format!(
            "size must be greater than 0 and at most {MAX_TEXT_SIZE}"
        ));
    }
    Ok(WasmBitmap::new(raster::render_text(text, size)))
}

/// Render Markdown (headings, bold/italic, lists, rules).
#[wasm_bindgen]
pub fn render_markdown(md: &str) -> WasmBitmap {
    WasmBitmap::new(raster::render_markdown(md))
}

/// Render `data` as a QR code, optionally with a text caption below.
/// Errors if the payload does not fit in any QR version.
#[wasm_bindgen]
pub fn render_qr(data: &str, caption: Option<String>) -> Result<WasmBitmap, String> {
    raster::render_qr(data, caption.as_deref())
        .map(WasmBitmap::new)
        .map_err(|e| e.to_string())
}

/// Decode `bytes` (PNG/JPEG), scale to the 384 px print width and dither.
/// `dither` is one of `"floyd"`, `"atkinson"`, `"threshold"`.
#[wasm_bindgen]
pub fn render_image(bytes: &[u8], dither: &str) -> Result<WasmBitmap, String> {
    let dither = match dither {
        "floyd" => Dither::FloydSteinberg,
        "atkinson" => Dither::Atkinson,
        "threshold" => Dither::Threshold,
        other => {
            return Err(format!(
                "unknown dither `{other}` (expected floyd, atkinson or threshold)"
            ))
        }
    };
    let img = image::load_from_memory(bytes).map_err(|e| format!("failed to decode image: {e}"))?;
    if img.width() == 0 {
        return Err("image has zero width".to_string());
    }
    Ok(WasmBitmap::new(raster::image_to_bitmap(
        &raster::prepare(&img),
        dither,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];

    /// A small in-memory PNG with a light/dark gradient (exercises dithering).
    fn test_png() -> Vec<u8> {
        let img = image::GrayImage::from_fn(64, 32, |x, _| image::Luma([(x * 4) as u8]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn text_renders_with_png_preview() {
        let bitmap = render_text("hello world", 32.0).unwrap();
        assert!(bitmap.height() > 0);
        let png = bitmap.to_png();
        assert_eq!(&png[..8], &PNG_MAGIC);
    }

    #[test]
    fn text_bad_sizes_error() {
        for size in [0.0, -1.0, 129.0, f32::NAN, f32::INFINITY] {
            let err = render_text("x", size).unwrap_err();
            assert!(err.contains("size"), "unexpected message: {err}");
        }
    }

    #[test]
    fn markdown_heading_taller_than_paragraph() {
        let heading = render_markdown("# Title");
        let paragraph = render_markdown("Title");
        assert!(heading.height() > paragraph.height());
    }

    #[test]
    fn qr_renders() {
        let bitmap = render_qr("https://example.com", Some("caption".to_string())).unwrap();
        assert!(bitmap.height() > 0);
        assert_eq!(&bitmap.to_png()[..8], &PNG_MAGIC);
    }

    #[test]
    fn qr_too_long_errors() {
        let err = render_qr(&"x".repeat(8000), None).unwrap_err();
        assert!(err.contains("too long"), "unexpected message: {err}");
    }

    #[test]
    fn image_renders_with_each_dither() {
        let png = test_png();
        for dither in ["floyd", "atkinson", "threshold"] {
            let bitmap = render_image(&png, dither).unwrap();
            assert!(bitmap.height() > 0, "dither {dither}");
            assert_eq!(&bitmap.to_png()[..8], &PNG_MAGIC, "dither {dither}");
        }
    }

    #[test]
    fn image_bad_dither_errors() {
        let err = render_image(&test_png(), "bogus").unwrap_err();
        assert!(err.contains("unknown dither"), "unexpected message: {err}");
    }

    #[test]
    fn image_bad_bytes_error() {
        let err = render_image(b"not an image", "floyd").unwrap_err();
        assert!(err.contains("decode"), "unexpected message: {err}");
    }

    #[test]
    fn extend_blank_grows_height() {
        let mut bitmap = render_text("x", 24.0).unwrap();
        let before = bitmap.height();
        bitmap.extend_blank(40);
        assert_eq!(bitmap.height(), before + 40);
    }
}
