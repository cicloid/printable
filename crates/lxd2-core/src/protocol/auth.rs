//! Challenge-response auth keyed on the printer's MAC address.

use crate::protocol::crc::crc16_xmodem;

/// Compute the 10-byte `5A 0B` auth payload from our challenge bytes and the
/// printer's MAC (learned from the `5A 01` hello reply, bytes 4..10).
pub fn auth_response(challenge: &[u8; 10], mac: &[u8; 6]) -> [u8; 10] {
    let mut out = [0u8; 10];
    for (i, &c) in challenge.iter().enumerate() {
        let mut buf = [0u8; 7];
        buf[0] = c;
        buf[1..].copy_from_slice(mac);
        out[i] = (crc16_xmodem(&buf) >> 8) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::crc::crc16_xmodem;

    const MAC: [u8; 6] = [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33];

    #[test]
    fn auth_response_matches_manual_crc() {
        let challenge = [0u8; 10];
        let resp = auth_response(&challenge, &MAC);
        // Every byte identical for an all-zero challenge (ValdikSS's shortcut)
        let mut buf = vec![0u8];
        buf.extend_from_slice(&MAC);
        let expected = (crc16_xmodem(&buf) >> 8) as u8;
        assert_eq!(resp, [expected; 10]);
    }

    #[test]
    fn auth_response_uses_each_challenge_byte() {
        let challenge = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let resp = auth_response(&challenge, &MAC);
        for (i, &c) in challenge.iter().enumerate() {
            let mut buf = vec![c];
            buf.extend_from_slice(&MAC);
            assert_eq!(resp[i], (crc16_xmodem(&buf) >> 8) as u8);
        }
    }
}
