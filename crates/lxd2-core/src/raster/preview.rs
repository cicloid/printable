//! PNG preview export for 1-bit bitmaps.

use super::bitmap::{Bitmap, WIDTH};

/// Encode a bitmap as an 8-bit grayscale PNG (black = 0, white = 255).
///
/// A zero-height bitmap yields a 384x1 all-white PNG, since the image crate
/// cannot encode zero-height images.
pub fn bitmap_to_png(b: &Bitmap) -> Vec<u8> {
    let height = b.height().max(1) as u32;
    let img = image::GrayImage::from_fn(WIDTH as u32, height, |x, y| {
        let black = (y as usize) < b.height() && b.get(x as usize, y as usize);
        image::Luma([if black { 0u8 } else { 255u8 }])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("in-memory PNG encoding cannot fail");
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::bitmap::Bitmap;

    #[test]
    fn roundtrips_through_png() {
        let mut b = Bitmap::new(2);
        b.set(0, 0, true);
        let png = bitmap_to_png(&b);
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!(img.width(), 384);
        assert_eq!(img.height(), 2);
        assert_eq!(img.get_pixel(0, 0).0[0], 0);     // black
        assert_eq!(img.get_pixel(1, 0).0[0], 255);   // white
    }

    #[test]
    fn zero_height_bitmap_gives_white_384x1_png() {
        let png = bitmap_to_png(&Bitmap::new(0));
        let img = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!(img.width(), 384);
        assert_eq!(img.height(), 1);
        assert!(img.pixels().all(|p| p.0[0] == 255));
    }
}
