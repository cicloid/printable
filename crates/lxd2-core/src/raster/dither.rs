//! Grayscale image → 1-bit bitmap conversion: scaling and dithering.

use super::bitmap::{Bitmap, WIDTH};

/// Dithering algorithm used when reducing 8-bit grayscale to 1-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    /// Floyd–Steinberg error diffusion.
    FloydSteinberg,
    /// Plain threshold at 128.
    Threshold,
}

/// Maximum output height of [`prepare`] in rows: about half a meter of
/// paper at 203 dpi.
const MAX_PREPARE_HEIGHT: u64 = 4096;

/// Grayscale + resize to the 384 px print width, preserving aspect ratio.
///
/// The output height is clamped to 4096 rows (~0.5 m of paper); taller
/// results are squashed to fit rather than erroring. A zero-width input
/// cannot be scaled and yields a 384x1 all-white image.
pub fn prepare(img: &image::DynamicImage) -> image::GrayImage {
    if img.width() == 0 {
        return image::GrayImage::from_pixel(WIDTH as u32, 1, image::Luma([255]));
    }
    let height = ((WIDTH as u64 * img.height() as u64) / img.width() as u64)
        .clamp(1, MAX_PREPARE_HEIGHT) as u32;
    img.resize_exact(WIDTH as u32, height, image::imageops::FilterType::Lanczos3)
        .to_luma8()
}

/// Convert a grayscale image to a 1-bit bitmap (bit 1 = black).
///
/// Images narrower than 384 px leave the right margin white. Images wider
/// than 384 px are truncated on the right — call [`prepare`] first to scale.
pub fn image_to_bitmap(img: &image::GrayImage, dither: Dither) -> Bitmap {
    let width = (img.width() as usize).min(WIDTH);
    let height = img.height() as usize;
    let mut bitmap = Bitmap::new(height);
    match dither {
        Dither::Threshold => {
            for y in 0..height {
                for x in 0..width {
                    let v = img.get_pixel(x as u32, y as u32).0[0];
                    bitmap.set(x, y, v < 128);
                }
            }
        }
        Dither::FloydSteinberg => {
            let mut buf: Vec<f32> = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| f32::from(img.get_pixel(x as u32, y as u32).0[0]))
                .collect();
            for y in 0..height {
                for x in 0..width {
                    let old = buf[y * width + x];
                    let black = old < 128.0;
                    bitmap.set(x, y, black);
                    let new = if black { 0.0 } else { 255.0 };
                    let err = old - new;
                    if x + 1 < width {
                        buf[y * width + x + 1] += err * 7.0 / 16.0;
                    }
                    if y + 1 < height {
                        if x > 0 {
                            buf[(y + 1) * width + x - 1] += err * 3.0 / 16.0;
                        }
                        buf[(y + 1) * width + x] += err * 5.0 / 16.0;
                        if x + 1 < width {
                            buf[(y + 1) * width + x + 1] += err * 1.0 / 16.0;
                        }
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
    use image::{GrayImage, Luma};

    #[test]
    fn threshold_maps_dark_to_black() {
        let mut img = GrayImage::new(384, 2);
        for p in img.pixels_mut() {
            *p = Luma([10]);
        } // dark
        let b = image_to_bitmap(&img, Dither::Threshold);
        assert!(b.get(0, 0) && b.get(383, 1)); // black everywhere
    }

    #[test]
    fn threshold_maps_light_to_white() {
        let mut img = GrayImage::new(384, 2);
        for p in img.pixels_mut() {
            *p = Luma([250]);
        }
        let b = image_to_bitmap(&img, Dither::Threshold);
        assert!(!b.get(0, 0) && !b.get(383, 1));
    }

    #[test]
    fn floyd_steinberg_mid_gray_is_half_black() {
        let mut img = GrayImage::new(384, 100);
        for p in img.pixels_mut() {
            *p = Luma([128]);
        }
        let b = image_to_bitmap(&img, Dither::FloydSteinberg);
        let black: usize = (0..100)
            .flat_map(|y| (0..384).map(move |x| (x, y)))
            .filter(|&(x, y)| b.get(x, y))
            .count();
        let ratio = black as f64 / (384.0 * 100.0);
        assert!((0.4..0.6).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn prepare_scales_to_384_wide() {
        let img = image::DynamicImage::new_luma8(768, 200);
        let g = prepare(&img);
        assert_eq!(g.width(), 384);
        assert_eq!(g.height(), 100);
    }

    #[test]
    fn prepare_clamps_height_to_4096() {
        // 10x1000 would scale to 384x38400; clamped to the paper cap.
        let img = image::DynamicImage::new_luma8(10, 1000);
        let g = prepare(&img);
        assert_eq!(g.width(), 384);
        assert_eq!(g.height(), 4096);
    }

    #[test]
    fn prepare_zero_width_gives_white_384x1() {
        let img = image::DynamicImage::new_luma8(0, 10);
        let g = prepare(&img);
        assert_eq!((g.width(), g.height()), (384, 1));
        assert!(g.pixels().all(|p| p.0[0] == 255));
    }
}
