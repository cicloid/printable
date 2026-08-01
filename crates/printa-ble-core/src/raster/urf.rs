//! Apple Raster (URF) decoding.
//!
//! URF is what an AirPrint client sends when the printer advertises
//! `image/urf` and does not advertise PDF: the client rasterises the document
//! itself, which is why this crate needs no PDF renderer. See
//! [docs/AIRPRINT.md](../../../../docs/AIRPRINT.md).
//!
//! The layout below was **not** taken from a specification — it was derived
//! from a file produced by Apple's own `rastertourf` filter and confirmed by a
//! round trip: the decoder consumes the fixture exactly, to the byte, with no
//! trailing padding (`decodes_captured_letter_page`). Treat it the same way as
//! `protocol/`: verified against a real producer, not assumed.
//!
//! ```text
//! file header   12 B  "UNIRAST\0" | u32be page_count
//! page header   32 B  bpp u8 | colorspace u8 | duplex u8 | quality u8
//!                     | u32be x2 reserved | u32be width | u32be height
//!                     | u32be dpi | 8 B reserved
//! page data           per line: u8 repeat (line appears repeat + 1 times),
//!                     then runs until `width` pixels are covered:
//!                       c  < 128 -> next pixel repeated c + 1 times
//!                       c == 128 -> reserved, rejected
//!                       c  > 128 -> 257 - c literal pixels
//! ```
//!
//! Every pixel count is `bpp / 8` bytes wide.

use super::bitmap::{Bitmap, WIDTH};
use super::dither::{image_to_bitmap, prepare, Dither};
use image::{GrayImage, Luma};

/// Magic at the head of every URF stream.
const MAGIC: &[u8; 8] = b"UNIRAST\0";
/// Shared prefix of the PWG Raster sync words (`RaSt`, `RaS2`, `RaS3` and
/// their byte-swapped forms). Used only to turn "bad magic" into a message
/// that says which format arrived; nothing here decodes PWG Raster.
const PWG_RASTER_PREFIX: &[u8] = b"RaS";
/// File header: magic plus the page count.
const FILE_HEADER_LEN: usize = 12;
/// Fixed-size per-page header.
const PAGE_HEADER_LEN: usize = 32;
/// Run byte 0x80 ends the row early; the rest of the line stays blank.
///
/// This is not a guess. It was recovered by brute-forcing the candidate
/// meanings against a 3.4 MB page produced by iOS and keeping the only one
/// that decoded all 6600 rows while consuming the file to the last byte.
/// Treating it as a 129-pixel repeat or a 129-pixel literal both overrun the
/// first row immediately.
const END_OF_ROW: u8 = 0x80;

/// Refuse absurd geometry before allocating. A4 at 1200 dpi is ~9900 x 14000,
/// so this leaves generous headroom while keeping a hostile or corrupt header
/// from asking for a multi-gigabyte buffer.
const MAX_DIMENSION: u32 = 30_000;
/// Total pixels per page, independent of the per-axis cap above.
const MAX_PIXELS: u64 = 128 << 20;
/// A print job with more pages than this is not a receipt.
const MAX_PAGES: u32 = 1_000;

/// Why a URF stream could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrfError {
    #[error("not an Apple Raster stream (bad UNIRAST magic)")]
    BadMagic,
    #[error("this looks like PWG Raster, which is not supported — only Apple Raster (URF) is")]
    PwgRaster,
    #[error("URF stream ended mid-{0}")]
    Truncated(&'static str),
    #[error("unsupported URF depth: {0} bits per pixel (expected 8 or 24)")]
    UnsupportedDepth(u8),
    #[error("URF page {0} has zero width or height")]
    EmptyPage(u32),
    #[error("URF page {0} is {1}x{2}, beyond the decoder's limits")]
    PageTooLarge(u32, u32, u32),
    #[error("URF stream declares {0} pages, more than the {MAX_PAGES} allowed")]
    TooManyPages(u32),
    #[error("URF run overruns row width (page {0}, row {1})")]
    RowOverrun(u32, u32),
}

/// One decoded page, already flattened to 8-bit grayscale.
///
/// `dpi` and the original pixel dimensions are kept because they are the only
/// clue to the page's physical size; the caller decides whether to scale.
#[derive(Debug, Clone)]
pub struct UrfPage {
    /// Pixel width as declared in the page header.
    pub width: u32,
    /// Pixel height as declared in the page header.
    pub height: u32,
    /// Resolution in dots per inch.
    pub dpi: u32,
    /// Decoded pixels, 255 = white.
    pub gray: GrayImage,
}

/// A cursor that turns every short read into [`UrfError::Truncated`].
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], UrfError> {
        let end = self.pos.checked_add(n).ok_or(UrfError::Truncated(what))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(UrfError::Truncated(what))?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, UrfError> {
        Ok(self.take(1, what)?[0])
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, UrfError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Rec. 601 luma, matching what the `image` crate uses for RGB to grayscale.
fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

/// Decode every page of a URF stream.
///
/// Returns pages in document order. An empty stream (`page_count` of zero) is
/// not an error — it decodes to no pages, and the caller decides whether that
/// is worth printing.
pub fn decode_urf(bytes: &[u8]) -> Result<Vec<UrfPage>, UrfError> {
    let mut r = Reader::new(bytes);
    let magic = r.take(MAGIC.len(), "magic")?;
    if magic != MAGIC {
        // PWG Raster shares URF's run encoding but has a different, larger
        // header, so it would mis-decode rather than fail cleanly if anyone
        // ever wired it up. Naming it is purely for a better error message:
        // an unrecognised prefix still falls through to `BadMagic`.
        if magic.starts_with(PWG_RASTER_PREFIX) {
            return Err(UrfError::PwgRaster);
        }
        return Err(UrfError::BadMagic);
    }
    let page_count = r.u32("page count")?;
    if page_count > MAX_PAGES {
        return Err(UrfError::TooManyPages(page_count));
    }
    debug_assert_eq!(r.pos, FILE_HEADER_LEN);

    let mut pages = Vec::new();
    for index in 0..page_count {
        pages.push(decode_page(&mut r, index)?);
    }
    Ok(pages)
}

fn decode_page(r: &mut Reader<'_>, index: u32) -> Result<UrfPage, UrfError> {
    let header = r.pos;
    let bpp = r.u8("page header")?;
    let _colorspace = r.u8("page header")?;
    let _duplex = r.u8("page header")?;
    let _quality = r.u8("page header")?;
    let _reserved1 = r.u32("page header")?;
    let _reserved2 = r.u32("page header")?;
    let width = r.u32("page header")?;
    let height = r.u32("page header")?;
    let dpi = r.u32("page header")?;
    r.take(8, "page header")?;
    debug_assert_eq!(r.pos - header, PAGE_HEADER_LEN);

    // Only the depths an AirPrint client actually produces for `W8` (8-bit
    // grayscale) and `SRGB24` (24-bit colour). Anything else would be a silent
    // mis-decode, so refuse it loudly.
    let bytes_per_pixel = match bpp {
        8 => 1,
        24 => 3,
        other => return Err(UrfError::UnsupportedDepth(other)),
    };
    if width == 0 || height == 0 {
        return Err(UrfError::EmptyPage(index));
    }
    if width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(UrfError::PageTooLarge(index, width, height));
    }

    let mut gray = GrayImage::from_pixel(width, height, Luma([255]));
    let mut row = vec![255u8; width as usize];
    let mut y = 0u32;

    while y < height {
        // The repeat byte counts *additional* copies, so a line always appears
        // at least once.
        let repeat = r.u8("line repeat")?;
        decode_row(r, &mut row, bytes_per_pixel, index, y)?;

        // Clamp rather than reject: a producer that over-repeats the final
        // line should not cost the user a whole print job.
        let copies = u32::from(repeat).saturating_add(1).min(height - y);
        for _ in 0..copies {
            for (x, &v) in row.iter().enumerate() {
                gray.put_pixel(x as u32, y, Luma([v]));
            }
            y += 1;
        }
    }

    Ok(UrfPage {
        width,
        height,
        dpi,
        gray,
    })
}

/// Decode exactly one line's worth of runs into `row`.
fn decode_row(
    r: &mut Reader<'_>,
    row: &mut [u8],
    bytes_per_pixel: usize,
    page: u32,
    y: u32,
) -> Result<(), UrfError> {
    let width = row.len();
    let mut x = 0usize;

    while x < width {
        let code = r.u8("run")?;
        if code == END_OF_ROW {
            // Nothing more is coded for this line. Blank the remainder rather
            // than leaving whatever the previous line put in the scratch
            // buffer, which would smear the last row across the page.
            row[x..].fill(255);
            return Ok(());
        }

        if code < END_OF_ROW {
            // Repeat: one pixel, `code + 1` times.
            let count = code as usize + 1;
            if x + count > width {
                return Err(UrfError::RowOverrun(page, y));
            }
            let px = r.take(bytes_per_pixel, "repeated pixel")?;
            let v = pixel_luma(px);
            row[x..x + count].fill(v);
            x += count;
        } else {
            // Literal: `257 - code` distinct pixels.
            let count = 257 - code as usize;
            if x + count > width {
                return Err(UrfError::RowOverrun(page, y));
            }
            let px = r.take(count * bytes_per_pixel, "literal pixels")?;
            for (i, chunk) in px.chunks_exact(bytes_per_pixel).enumerate() {
                row[x + i] = pixel_luma(chunk);
            }
            x += count;
        }
    }
    Ok(())
}

fn pixel_luma(px: &[u8]) -> u8 {
    match px {
        [v] => *v,
        [r, g, b] => luma(*r, *g, *b),
        // `bytes_per_pixel` is 1 or 3, so `take` always yields one of the
        // above; keep a sane value rather than panicking on a logic slip.
        _ => 255,
    }
}

/// Grey level at or below which a pixel counts as content.
///
/// Not 254: anti-aliased glyph edges and JPEG-ish ringing leave near-white
/// pixels all over an otherwise blank margin, and treating those as ink would
/// make the crop below a no-op on exactly the pages it exists to fix.
const INK_THRESHOLD: u8 = 240;

/// White border kept around cropped content, as a fraction of its width.
///
/// Applied before scaling so it survives the resize as a proportional margin
/// rather than a handful of pixels.
const CROP_MARGIN_RATIO: u32 = 50; // 1/50 = 2%

/// Blank rows between the pages of a multi-page job.
const PAGE_GAP: usize = 24;

/// Paper this wide or narrower was laid out *for* a receipt printer.
///
/// A client that honours a 48 mm media advertisement sends a page already the
/// width of the paper; one that ignores it sends US Letter at 215.9 mm. The
/// gap between those is enormous, so this only has to fall somewhere sensible
/// in between — 80 mm also covers the common 58 mm and 80 mm receipt widths.
const RECEIPT_MAX_WIDTH_MM: u32 = 80;

/// Physical page width in millimetres, when the header declares a usable dpi.
fn width_mm(page: &UrfPage) -> Option<u32> {
    (page.dpi > 0).then(|| (u64::from(page.width) * 254 / (u64::from(page.dpi) * 10)) as u32)
}

/// Drop entirely blank rows from the top and bottom, keeping the full width.
///
/// The horizontal position is left alone: on a page already sized for this
/// paper, the client's layout *is* the intended layout, and re-centring it
/// would undo that. Only the leading and trailing whitespace goes, because on
/// a continuous roll that whitespace is literally paper.
fn trim_blank_rows(img: &GrayImage) -> Option<GrayImage> {
    let (_, y0, _, y1) = ink_bounds(img)?;
    Some(image::imageops::crop_imm(img, 0, y0, img.width(), y1 - y0).to_image())
}

/// Bounding box of non-white pixels as `(x0, y0, x1, y1)`, end-exclusive.
///
/// Returns `None` for a page with no content at all, which the caller should
/// skip rather than print as blank paper.
fn ink_bounds(img: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (x, y, px) in img.enumerate_pixels() {
        if px.0[0] <= INK_THRESHOLD {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0).then_some((x0, y0, x1, y1))
}

/// Crop a page to its content, with a proportional white margin.
fn crop_to_ink(img: &GrayImage) -> Option<GrayImage> {
    let (x0, y0, x1, y1) = ink_bounds(img)?;
    let margin = ((x1 - x0) / CROP_MARGIN_RATIO).max(1);
    let x0 = x0.saturating_sub(margin);
    let y0 = y0.saturating_sub(margin);
    let x1 = (x1 + margin).min(img.width());
    let y1 = (y1 + margin).min(img.height());
    Some(image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image())
}

/// Turn decoded URF pages into one printable 384 px bitmap.
///
/// How a page is fitted depends on whether the client laid it out for this
/// printer, which the page's own physical width reveals:
///
/// - **Already receipt-width** (see [`RECEIPT_MAX_WIDTH_MM`]) — the client
///   honoured our 48 mm media advertisement, so its layout is authoritative.
///   Only leading and trailing blank rows are trimmed; on a continuous roll
///   those are paper, not margin.
/// - **A full sheet** — the client ignored the advertisement and sent US
///   Letter. Scaling 5100 px whole onto 384 px is a 13x reduction that renders
///   12 pt text about 7 px tall: legible on screen, grey mush on a thermal
///   head. So the page is cropped to its ink and *that* is scaled up, which
///   keeps a short document readable at the cost of making apparent type size
///   depend on how much content the page holds.
///
/// The first branch is the good one, and it is why `scripts/airprint.sh`
/// advertises real media — see `docs/AIRPRINT.md`. The second exists because
/// we cannot make every client cooperate.
///
/// Blank pages are skipped entirely, so a trailing empty page costs no paper.
pub fn pages_to_bitmap(pages: &[UrfPage], dither: Dither) -> Bitmap {
    let rendered: Vec<Bitmap> = pages
        .iter()
        .filter_map(|page| {
            if width_mm(page).is_some_and(|mm| mm <= RECEIPT_MAX_WIDTH_MM) {
                trim_blank_rows(&page.gray)
            } else {
                crop_to_ink(&page.gray)
            }
        })
        .map(|fitted| {
            let prepared = prepare(&image::DynamicImage::ImageLuma8(fitted));
            image_to_bitmap(&prepared, dither)
        })
        .collect();

    let gaps = PAGE_GAP * rendered.len().saturating_sub(1);
    let total: usize = rendered.iter().map(Bitmap::height).sum::<usize>() + gaps;
    let mut out = Bitmap::new(total);
    let mut top = 0;
    for page in &rendered {
        for y in 0..page.height() {
            for x in 0..WIDTH {
                if page.get(x, y) {
                    out.set(x, top + y, true);
                }
            }
        }
        top += page.height() + PAGE_GAP;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from macOS's own `rastertourf`; see `testdata/README.md`.
    const LETTER: &[u8] = include_bytes!("testdata/letter_600dpi.urf");

    /// Build a minimal single-page stream around pre-encoded row data.
    fn stream(bpp: u8, width: u32, height: u32, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC);
        v.extend_from_slice(&1u32.to_be_bytes());
        v.push(bpp);
        v.extend_from_slice(&[0, 1, 0]); // colorspace, duplex, quality
        v.extend_from_slice(&[0; 8]); // two reserved words
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&203u32.to_be_bytes());
        v.extend_from_slice(&[0; 8]);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn decodes_captured_letter_page() {
        let pages = decode_urf(LETTER).expect("real capture must decode");
        assert_eq!(pages.len(), 1);
        let p = &pages[0];
        // US Letter at 600 dpi.
        assert_eq!((p.width, p.height, p.dpi), (5100, 6600, 600));
        assert_eq!(p.gray.dimensions(), (5100, 6600));
    }

    /// The strongest evidence that the format is understood correctly: the
    /// decoder consumes the real file exactly, with nothing left over.
    #[test]
    fn captured_page_consumes_every_byte() {
        let mut r = Reader::new(LETTER);
        r.take(8, "magic").unwrap();
        assert_eq!(r.u32("pages").unwrap(), 1);
        decode_page(&mut r, 0).unwrap();
        assert_eq!(r.pos, LETTER.len(), "trailing bytes left undecoded");
    }

    /// A text page must be mostly white with real black ink somewhere.
    #[test]
    fn captured_page_has_ink_on_white() {
        let pages = decode_urf(LETTER).unwrap();
        let px = pages[0].gray.pixels();
        let total = pages[0].gray.pixels().count();
        let dark = px.filter(|p| p.0[0] < 128).count();
        assert!(dark > 0, "captured text page decoded as blank");
        assert!(
            dark * 100 < total,
            "text page should be well under 1% ink, got {dark}/{total}"
        );
    }

    #[test]
    fn decodes_repeat_and_literal_runs() {
        // Row of 4: two black (repeat), then two literals 0x40 and 0x80.
        let body = [
            0x00, // line appears once
            0x01, 0x00, // repeat 0x00 twice
            0xFF, 0x40, 0x80, // 257-255 = 2 literals
        ];
        let pages = decode_urf(&stream(8, 4, 1, &body)).unwrap();
        let row: Vec<u8> = pages[0].gray.pixels().map(|p| p.0[0]).collect();
        assert_eq!(row, vec![0x00, 0x00, 0x40, 0x80]);
    }

    #[test]
    fn line_repeat_duplicates_rows() {
        // repeat = 2 means the line appears three times.
        let body = [0x02, 0x03, 0x11];
        let pages = decode_urf(&stream(8, 4, 3, &body)).unwrap();
        assert_eq!(pages[0].gray.dimensions(), (4, 3));
        assert!(pages[0].gray.pixels().all(|p| p.0[0] == 0x11));
    }

    #[test]
    fn over_repeated_final_line_is_clamped_not_rejected() {
        // Claims 200 more copies of the line but only 2 rows exist.
        let body = [0xC8, 0x03, 0x22];
        let pages = decode_urf(&stream(8, 4, 2, &body)).expect("should clamp");
        assert_eq!(pages[0].gray.dimensions(), (4, 2));
    }

    #[test]
    fn rgb_pages_flatten_to_luma() {
        // 0xFF is the *longest* literal run at 257 - 255 = 2 pixels, so a
        // literal can never encode a single pixel — that needs a repeat of 1.
        // Here: two literals (red, green), then white repeated twice.
        let body = [
            0x00, // line appears once
            0xFF, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, // literal red, green
            0x01, 0xFF, 0xFF, 0xFF, // repeat white twice
        ];
        let pages = decode_urf(&stream(24, 4, 1, &body)).unwrap();
        let row: Vec<u8> = pages[0].gray.pixels().map(|p| p.0[0]).collect();
        assert_eq!(row[0], luma(255, 0, 0));
        assert_eq!(row[1], luma(0, 255, 0));
        assert_eq!(&row[2..], &[255, 255]);
    }

    #[test]
    fn pwg_raster_is_named_rather_than_called_bad_magic() {
        let mut v = b"RaS2".to_vec();
        v.extend_from_slice(&[0u8; 32]);
        assert_eq!(decode_urf(&v).unwrap_err(), UrfError::PwgRaster);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bad = LETTER.to_vec();
        bad[0] = b'X';
        assert_eq!(decode_urf(&bad).unwrap_err(), UrfError::BadMagic);
    }

    /// 0x80 ends the line; the rest must come out blank, not inherit the
    /// previous row's pixels out of the reusable scratch buffer.
    #[test]
    fn end_of_row_marker_blanks_the_remainder() {
        // Row 0: 4 black pixels. Row 1: 1 black pixel, then end-of-row.
        let body = [
            0x00, 0x03, 0x00, // row 0: repeat black x4
            0x00, 0x00, 0x00, END_OF_ROW, // row 1: one black, then stop
        ];
        let pages = decode_urf(&stream(8, 4, 2, &body)).unwrap();
        let px: Vec<u8> = pages[0].gray.pixels().map(|p| p.0[0]).collect();
        assert_eq!(&px[..4], &[0, 0, 0, 0], "row 0 should be all black");
        assert_eq!(
            &px[4..],
            &[0, 255, 255, 255],
            "row 1 must blank after the marker, not smear row 0"
        );
    }

    #[test]
    fn rejects_run_past_end_of_row() {
        // Repeat 10 pixels into a 4-pixel row.
        let body = [0x00, 0x09, 0x00];
        assert_eq!(
            decode_urf(&stream(8, 4, 1, &body)).unwrap_err(),
            UrfError::RowOverrun(0, 0)
        );
    }

    #[test]
    fn rejects_unsupported_depth() {
        assert_eq!(
            decode_urf(&stream(1, 4, 1, &[0x00, 0x03, 0x00])).unwrap_err(),
            UrfError::UnsupportedDepth(1)
        );
    }

    #[test]
    fn rejects_absurd_geometry_without_allocating() {
        let err = decode_urf(&stream(8, 40_000, 40_000, &[])).unwrap_err();
        assert!(matches!(err, UrfError::PageTooLarge(0, 40_000, 40_000)));
    }

    #[test]
    fn rejects_zero_sized_page() {
        assert_eq!(
            decode_urf(&stream(8, 0, 10, &[])).unwrap_err(),
            UrfError::EmptyPage(0)
        );
    }

    #[test]
    fn rejects_truncation_mid_row() {
        // Declares 4 rows but supplies data for one.
        let body = [0x00, 0x03, 0x00];
        assert!(matches!(
            decode_urf(&stream(8, 4, 4, &body)),
            Err(UrfError::Truncated(_))
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(
            decode_urf(&LETTER[..20]),
            Err(UrfError::Truncated(_))
        ));
    }

    #[test]
    fn rejects_implausible_page_count() {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&100_000u32.to_be_bytes());
        assert_eq!(decode_urf(&v).unwrap_err(), UrfError::TooManyPages(100_000));
    }

    /// Build a page whose only ink is a filled rectangle.
    fn page_with_box(w: u32, h: u32, bx: u32, by: u32, bw: u32, bh: u32) -> UrfPage {
        let mut gray = GrayImage::from_pixel(w, h, Luma([255]));
        for y in by..by + bh {
            for x in bx..bx + bw {
                gray.put_pixel(x, y, Luma([0]));
            }
        }
        UrfPage {
            width: w,
            height: h,
            dpi: 203,
            gray,
        }
    }

    #[test]
    fn ink_bounds_finds_the_content_box() {
        let page = page_with_box(100, 80, 10, 20, 30, 40);
        assert_eq!(ink_bounds(&page.gray), Some((10, 20, 40, 60)));
    }

    #[test]
    fn ink_bounds_is_none_for_a_blank_page() {
        let blank = GrayImage::from_pixel(50, 50, Luma([255]));
        assert_eq!(ink_bounds(&blank), None);
    }

    #[test]
    fn near_white_speckle_does_not_defeat_the_crop() {
        // A margin full of 250-grey noise must not count as content, or the
        // crop silently degrades to "the whole page".
        let mut page = page_with_box(200, 200, 80, 80, 20, 20);
        page.gray.put_pixel(2, 2, Luma([250]));
        page.gray.put_pixel(197, 197, Luma([250]));
        let (x0, y0, x1, y1) = ink_bounds(&page.gray).unwrap();
        assert_eq!((x0, y0, x1, y1), (80, 80, 100, 100));
    }

    #[test]
    fn blank_pages_are_skipped_entirely() {
        let blank = UrfPage {
            width: 100,
            height: 100,
            dpi: 203,
            gray: GrayImage::from_pixel(100, 100, Luma([255])),
        };
        let bmp = pages_to_bitmap(&[blank], Dither::Threshold);
        assert_eq!(bmp.height(), 0, "a blank page must not cost paper");
    }

    /// A page the client already sized for 48 mm paper keeps its layout: the
    /// small mark stays small instead of being blown up to fill the width.
    #[test]
    fn receipt_width_page_is_not_cropped_to_ink() {
        // 383 px at 203 dpi = 47.9 mm, exactly what a cooperating client sends.
        let mut page = page_with_box(383, 2000, 10, 500, 40, 40);
        page.dpi = 203;
        assert_eq!(width_mm(&page), Some(47));

        let bmp = pages_to_bitmap(std::slice::from_ref(&page), Dither::Threshold);
        // Blank rows gone (2000 -> ~40), but the mark was not scaled up to
        // fill 384 px, which ink-cropping would have done.
        assert!(
            bmp.height() < 100,
            "expected blank rows trimmed to about the mark height, got {}",
            bmp.height()
        );
        let inked_cols = (0..WIDTH)
            .filter(|&x| (0..bmp.height()).any(|y| bmp.get(x, y)))
            .count();
        assert!(
            inked_cols < WIDTH / 2,
            "a 40 px mark must stay narrow, not be stretched across the page"
        );
    }

    /// The same geometry sent as US Letter still gets the crop treatment.
    #[test]
    fn letter_page_still_crops_to_ink() {
        let mut page = page_with_box(5100, 6600, 100, 100, 400, 200);
        page.dpi = 600;
        assert_eq!(width_mm(&page), Some(215));
        let bmp = pages_to_bitmap(std::slice::from_ref(&page), Dither::Threshold);
        let inked_cols = (0..WIDTH)
            .filter(|&x| (0..bmp.height()).any(|y| bmp.get(x, y)))
            .count();
        assert!(
            inked_cols > WIDTH / 2,
            "a Letter page should be cropped and scaled up to fill the width"
        );
    }

    #[test]
    fn zero_dpi_falls_back_to_cropping() {
        let mut page = page_with_box(383, 2000, 10, 500, 40, 40);
        page.dpi = 0;
        assert_eq!(width_mm(&page), None);
        // Must not divide by zero, and must still produce something printable.
        assert!(pages_to_bitmap(&[page], Dither::Threshold).height() > 0);
    }

    #[test]
    fn cropping_beats_scaling_the_whole_page() {
        // Small mark on a big page: cropped output is far taller (and so far
        // more legible) than the ~1 px the full-page scale would leave.
        let page = page_with_box(4000, 5000, 100, 100, 400, 200);
        let bmp = pages_to_bitmap(&[page], Dither::Threshold);
        assert!(
            bmp.height() > 100,
            "expected the crop to fill the width, got {} rows",
            bmp.height()
        );
    }

    #[test]
    fn multiple_pages_stack_with_a_gap() {
        let a = page_with_box(400, 400, 50, 50, 100, 100);
        let b = page_with_box(400, 400, 50, 50, 100, 100);
        let one = pages_to_bitmap(std::slice::from_ref(&a), Dither::Threshold);
        let two = pages_to_bitmap(&[a, b], Dither::Threshold);
        assert_eq!(two.height(), one.height() * 2 + PAGE_GAP);
    }

    /// The whole Stage 1 path in one assertion: real macOS bytes in, a
    /// printable 384 px bitmap with actual ink out.
    #[test]
    fn captured_page_renders_to_a_printable_bitmap() {
        let pages = decode_urf(LETTER).unwrap();
        let bmp = pages_to_bitmap(&pages, Dither::FloydSteinberg);
        assert!(bmp.height() > 0, "captured page produced no output");
        let ink = (0..bmp.height())
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| bmp.get(x, y))
            .count();
        assert!(ink > 0, "captured page rendered without ink");
    }

    #[test]
    fn empty_stream_decodes_to_no_pages() {
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&0u32.to_be_bytes());
        assert!(decode_urf(&v).unwrap().is_empty());
    }
}
