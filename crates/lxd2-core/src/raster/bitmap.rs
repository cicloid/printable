//! 1-bit bitmap for the 384 px wide print head, MSB-first, bit 1 = black.

pub const WIDTH: usize = 384;
pub const BYTES_PER_ROW: usize = WIDTH / 8; // 48

#[derive(Debug, Clone)]
pub struct Bitmap {
    rows: Vec<[u8; BYTES_PER_ROW]>,
}

impl Bitmap {
    pub fn new(height: usize) -> Self {
        Self { rows: vec![[0u8; BYTES_PER_ROW]; height] }
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, y: usize) -> &[u8; BYTES_PER_ROW] {
        &self.rows[y]
    }

    /// Set pixel; x < 384, bit 1 = black, MSB-first within each byte.
    pub fn set(&mut self, x: usize, y: usize, black: bool) {
        let byte = &mut self.rows[y][x / 8];
        let mask = 0x80 >> (x % 8);
        if black { *byte |= mask } else { *byte &= !mask }
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        self.rows[y][x / 8] & (0x80 >> (x % 8)) != 0
    }

    /// Append `rows` blank (white) rows at the bottom, e.g. for feed lines.
    pub fn extend_blank(&mut self, rows: usize) {
        self.rows.resize(self.rows.len() + rows, [0u8; BYTES_PER_ROW]);
    }

    /// 96-byte payloads for raster packets: two rows each, zero-padded.
    pub fn to_raster_payloads(&self) -> Vec<[u8; 2 * BYTES_PER_ROW]> {
        self.rows
            .chunks(2)
            .map(|pair| {
                let mut chunk = [0u8; 2 * BYTES_PER_ROW];
                chunk[..BYTES_PER_ROW].copy_from_slice(&pair[0]);
                if let Some(second) = pair.get(1) {
                    chunk[BYTES_PER_ROW..].copy_from_slice(second);
                }
                chunk
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pixel_packs_msb_first() {
        let mut b = Bitmap::new(2);
        b.set(0, 0, true); // leftmost pixel -> bit 7 of byte 0
        b.set(383, 1, true); // rightmost pixel -> bit 0 of byte 47 of row 1
        assert_eq!(b.row(0)[0], 0b1000_0000);
        assert_eq!(b.row(1)[47], 0b0000_0001);
    }

    #[test]
    fn payloads_pack_two_rows_per_chunk() {
        let mut b = Bitmap::new(4);
        b.set(0, 2, true);
        let chunks = b.to_raster_payloads();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1][0], 0b1000_0000); // row 2 = first row of chunk 1
    }

    #[test]
    fn extend_blank_appends_white_rows() {
        let mut b = Bitmap::new(2);
        b.set(0, 1, true);
        b.extend_blank(3);
        assert_eq!(b.height(), 5);
        assert!(b.get(0, 1)); // existing content untouched
        assert!((2..5).all(|y| (0..384).all(|x| !b.get(x, y))));
    }

    #[test]
    fn odd_height_pads_final_chunk_with_zeros() {
        let b = Bitmap::new(3);
        let chunks = b.to_raster_payloads();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[1][48..].iter().all(|&x| x == 0));
    }
}
