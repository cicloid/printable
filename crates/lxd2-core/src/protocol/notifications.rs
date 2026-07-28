//! Parser for printer notifications received on BLE characteristic 0xFFE2.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub battery_pct: u8,
    pub no_paper: bool,
    pub charging: bool,
    pub charged: bool,
    pub overheat: bool,
    pub low_battery: bool,
    pub density: Option<u8>,
    pub voltage_mv: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    Hello { mac: [u8; 6] },
    Status(Status),
    AuthChallengeReply,
    AuthResult { ok: bool },
    LostPacket { index: u16 },
    Finished { num_packets: u16 },
    Cooldown,
    Hold,
}

pub fn parse(data: &[u8]) -> Option<Notification> {
    if data.len() < 2 || data[0] != 0x5A {
        return None;
    }
    match data[1] {
        0x01 if data.len() >= 10 => {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&data[4..10]);
            Some(Notification::Hello { mac })
        }
        0x02 if data.len() >= 5 => Some(Notification::Status(Status {
            battery_pct: data[2],
            no_paper: data[3] != 0,
            charging: data[4] == 1,
            charged: data[4] == 2,
            overheat: data.get(5).is_some_and(|&b| b != 0),
            low_battery: data.get(6).is_some_and(|&b| b != 0),
            density: data.get(7).copied(),
            voltage_mv: data.get(8..10).map(|v| u16::from_be_bytes([v[0], v[1]])),
        })),
        0x05 if data.len() >= 4 => Some(Notification::LostPacket {
            index: u16::from_be_bytes([data[2], data[3]]),
        }),
        0x06 if data.len() >= 4 => Some(Notification::Finished {
            num_packets: u16::from_be_bytes([data[2], data[3]]),
        }),
        0x07 => Some(Notification::Cooldown),
        0x08 => Some(Notification::Hold),
        0x0A => Some(Notification::AuthChallengeReply),
        0x0B if data.len() >= 3 => Some(Notification::AuthResult {
            ok: data[2] == 0x01,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hello_reply_mac() {
        let n = [0x5A, 0x01, 0, 0, 0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33, 0, 0];
        assert_eq!(
            parse(&n),
            Some(Notification::Hello {
                mac: [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33]
            })
        );
    }

    #[test]
    fn parses_status() {
        // battery 80%, no_paper, charging, overheat=0, low_batt=0, density 3
        let n = [0x5A, 0x02, 80, 1, 1, 0, 0, 3, 0x0F, 0xA0];
        assert_eq!(
            parse(&n),
            Some(Notification::Status(Status {
                battery_pct: 80,
                no_paper: true,
                charging: true,
                charged: false,
                overheat: false,
                low_battery: false,
                density: Some(3),
                voltage_mv: Some(4000),
            }))
        );
    }

    #[test]
    fn parses_short_status_without_extended_fields() {
        let n = [0x5A, 0x02, 55, 0, 2];
        let parsed = parse(&n);
        match parsed {
            Some(Notification::Status(s)) => {
                assert_eq!(s.battery_pct, 55);
                assert!(!s.no_paper);
                assert!(s.charged);
                assert_eq!(s.density, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_flow_control() {
        assert_eq!(
            parse(&[0x5A, 0x05, 0x01, 0x40]),
            Some(Notification::LostPacket { index: 0x0140 })
        );
        assert_eq!(
            parse(&[0x5A, 0x06, 0x01, 0x40]),
            Some(Notification::Finished {
                num_packets: 0x0140
            })
        );
        assert_eq!(parse(&[0x5A, 0x07]), Some(Notification::Cooldown));
        assert_eq!(parse(&[0x5A, 0x08]), Some(Notification::Hold));
    }

    #[test]
    fn parses_auth_results() {
        assert_eq!(
            parse(&[0x5A, 0x0B, 0x01]),
            Some(Notification::AuthResult { ok: true })
        );
        assert_eq!(
            parse(&[0x5A, 0x0B, 0x00]),
            Some(Notification::AuthResult { ok: false })
        );
        // 5A 0A reply payload is unused garbage; still recognized
        let n = [0x5A, 0x0A, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(parse(&n), Some(Notification::AuthChallengeReply));
    }

    #[test]
    fn unknown_or_short_returns_none() {
        assert_eq!(parse(&[0x5A]), None);
        assert_eq!(parse(&[0x42, 0x00]), None);
    }

    #[test]
    fn truncated_guarded_variants_return_none() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0x5A, 0x02, 80]), None); // status needs >= 5 bytes
        assert_eq!(parse(&[0x5A, 0x05, 0x01]), None); // lost-packet needs >= 4 bytes
        assert_eq!(parse(&[0x5A, 0x0B]), None); // auth result needs >= 3 bytes
    }
}
