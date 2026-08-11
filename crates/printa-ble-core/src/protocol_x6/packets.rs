//! Command frame builders for the X6 wire protocol.
//!
//! Frame layout (see docs/PROTOCOL.md, X6 section):
//! `51 78 | cmd | 00 | len LE u16 | payload | crc8(payload) | FF`.

use super::crc::crc8;
use crate::raster::bitmap::BYTES_PER_ROW;

const MAGIC: [u8; 2] = [0x51, 0x78];
const HOST_TO_PRINTER: u8 = 0x00;
const TRAILER: u8 = 0xFF;

const CMD_FEED_PAPER: u8 = 0xA1;
const CMD_RAW_SCANLINE: u8 = 0xA2;

/// Build one framed command.
pub fn frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u16;
    let mut p = Vec::with_capacity(8 + payload.len());
    p.extend_from_slice(&MAGIC);
    p.push(cmd);
    p.push(HOST_TO_PRINTER);
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(payload);
    p.push(crc8(payload));
    p.push(TRAILER);
    p
}

/// Feed `pixels` rows of blank paper (0xA1).
pub fn feed_paper(pixels: u16) -> Vec<u8> {
    frame(CMD_FEED_PAPER, &pixels.to_le_bytes())
}

/// One uncompressed 1bpp scanline (0xA2).
///
/// `row` is a [`crate::raster::bitmap::Bitmap`] row: MSB-first, bit 1 =
/// black. The X6 wants the leftmost pixel in the least-significant bit, so
/// each byte is bit-reversed on the way out.
pub fn raw_scanline(row: &[u8; BYTES_PER_ROW]) -> Vec<u8> {
    let wire: Vec<u8> = row.iter().map(|b| b.reverse_bits()).collect();
    frame(CMD_RAW_SCANLINE, &wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout check against the parzivail worked example
    /// `51 78 A4 00 01 00 35 8B FF` (command 0xA4, payload [0x35]).
    #[test]
    fn frame_matches_documented_example() {
        assert_eq!(
            frame(0xA4, &[0x35]),
            vec![0x51, 0x78, 0xA4, 0x00, 0x01, 0x00, 0x35, 0x8B, 0xFF]
        );
    }

    #[test]
    fn feed_paper_encodes_pixels_le() {
        // 0x0140 = 320 pixels; CRC over [0x40, 0x01] is 0x5C (Task 1 vector).
        assert_eq!(
            feed_paper(0x0140),
            vec![0x51, 0x78, 0xA1, 0x00, 0x02, 0x00, 0x40, 0x01, 0x5C, 0xFF]
        );
    }

    #[test]
    fn raw_scanline_frame_shape() {
        let row = [0u8; 48];
        let p = raw_scanline(&row);
        assert_eq!(p.len(), 2 + 1 + 1 + 2 + 48 + 1 + 1); // 56
        assert_eq!(&p[..6], &[0x51, 0x78, 0xA2, 0x00, 0x30, 0x00]);
        assert_eq!(p[54], 0x00); // CRC of 48 zero bytes is 0
        assert_eq!(p[55], 0xFF);
    }

    /// Bitmap is MSB-first (leftmost pixel = 0x80); the X6 wants the leftmost
    /// pixel in the least-significant bit, so every byte is bit-reversed.
    #[test]
    fn raw_scanline_reverses_bit_order() {
        let mut row = [0u8; 48];
        row[0] = 0x80; // pixel x=0 black
        row[1] = 0x40; // pixel x=9 black
        let p = raw_scanline(&row);
        assert_eq!(p[6], 0x01);
        assert_eq!(p[7], 0x02);
    }
}
