//! Parser for X6 frames received on characteristic 0xAE02.
//!
//! Only the 0xAE device-status frames are understood; everything else (battery
//! frames, device info, models that prefix frames with 0x12) parses to `None`
//! and is logged by the transport as an unparseable frame rather than treated
//! as fatal — the family has many undocumented variants.

const CMD_DEVICE_STATUS: u8 = 0xAE;
const PRINTER_TO_HOST: u8 = 0x01;
const STATUS_BUFFER_FULL: u8 = 0x10;
const STATUS_READY: u8 = 0x00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X6Notification {
    /// RX buffer full: stop sending scanlines.
    BufferFull,
    /// Buffer drained: sending may resume.
    Ready,
}

pub fn parse(data: &[u8]) -> Option<X6Notification> {
    // 51 78 | cmd | dir | len LE u16 | payload | crc | FF
    if data.len() < 9 || data[0] != 0x51 || data[1] != 0x78 {
        return None;
    }
    if data[3] != PRINTER_TO_HOST || data[2] != CMD_DEVICE_STATUS {
        return None;
    }
    let len = u16::from_le_bytes([data[4], data[5]]) as usize;
    if len != 1 || data.len() < 6 + len + 2 {
        return None;
    }
    match data[6] {
        STATUS_BUFFER_FULL => Some(X6Notification::BufferFull),
        STATUS_READY => Some(X6Notification::Ready),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two flow-control frames as captured (parzivail, verbatim hex).
    #[test]
    fn parses_captured_status_frames() {
        let full = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x10, 0x70, 0xFF];
        let ready = [0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x00, 0x00, 0xFF];
        assert_eq!(parse(&full), Some(X6Notification::BufferFull));
        assert_eq!(parse(&ready), Some(X6Notification::Ready));
    }

    #[test]
    fn rejects_wrong_magic_direction_or_command() {
        // wrong magic
        assert_eq!(
            parse(&[0x5A, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x10, 0x70, 0xFF]),
            None
        );
        // host->printer direction byte
        assert_eq!(
            parse(&[0x51, 0x78, 0xAE, 0x00, 0x01, 0x00, 0x10, 0x70, 0xFF]),
            None
        );
        // unknown command id: not ours to interpret
        assert_eq!(
            parse(&[0x51, 0x78, 0xBA, 0x01, 0x01, 0x00, 0x63, 0x00, 0xFF]),
            None
        );
        // unknown status payload value
        assert_eq!(
            parse(&[0x51, 0x78, 0xAE, 0x01, 0x01, 0x00, 0x42, 0x00, 0xFF]),
            None
        );
    }

    #[test]
    fn rejects_truncated_frames() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x51]), None);
        assert_eq!(parse(&[0x51, 0x78, 0xAE, 0x01, 0x01, 0x00]), None);
    }
}
