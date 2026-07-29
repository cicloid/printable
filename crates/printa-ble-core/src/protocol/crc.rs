//! CRC16/XMODEM used by the LX-D02 auth handshake.

/// CRC16/XMODEM: poly 0x1021, init 0x0000, no reflection, no xorout.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
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

    #[test]
    fn crc16_xmodem_check_value() {
        // Standard CRC16/XMODEM check: "123456789" -> 0x31C3
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    #[test]
    fn crc16_xmodem_empty_is_zero() {
        assert_eq!(crc16_xmodem(&[]), 0x0000);
    }
}
