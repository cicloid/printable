//! CRC8, polynomial 0x07, init 0, no reflection — matches the checksum table
//! in tinyprint-x6h `encoding.py` and the frames captured by parzivail.

/// CRC8 over `data`: polynomial 0x07, init 0, MSB-first, no final XOR.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every vector is lifted from a captured frame, not computed by us:
    /// `51 78 A4 00 01 00 35 8B FF` (quality packet, parzivail's example),
    /// `51 78 AE 01 01 00 10 70 FF` (buffer full),
    /// `51 78 AE 01 01 00 00 00 FF` (ready).
    #[test]
    fn matches_captured_frames() {
        assert_eq!(crc8(&[0x35]), 0x8B);
        assert_eq!(crc8(&[0x10]), 0x70);
        assert_eq!(crc8(&[0x00]), 0x00);
    }

    #[test]
    fn empty_payload_is_zero() {
        assert_eq!(crc8(&[]), 0x00);
    }

    #[test]
    fn multi_byte_payload() {
        // Feed-paper payload 0x0140 pixels, LE: 40 01.
        // Hand-walked through the tinyprint table: table[0x40]=0xC7,
        // table[0xC7 ^ 0x01]=table[0xC6]=0x5C.
        assert_eq!(crc8(&[0x40, 0x01]), 0x5C);
    }
}
