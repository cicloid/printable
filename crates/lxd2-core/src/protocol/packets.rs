//! Command packet builders for the LX-D02 wire protocol.

pub const RASTER_DATA_LEN: usize = 96; // two 48-byte print lines

pub fn hello() -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x01;
    p
}

pub fn set_density(level: u8) -> [u8; 3] {
    [0x5A, 0x0C, level]
}

pub fn print_start(num_packets: u16) -> [u8; 6] {
    let [hi, lo] = num_packets.to_be_bytes();
    [0x5A, 0x04, hi, lo, 0x00, 0x00]
}

pub fn print_end(num_packets: u16) -> [u8; 6] {
    let [hi, lo] = num_packets.to_be_bytes();
    [0x5A, 0x04, hi, lo, 0x01, 0x00]
}

pub fn auth_challenge(challenge: &[u8; 10]) -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x0A;
    p[2..].copy_from_slice(challenge);
    p
}

pub fn auth_reply(response: &[u8; 10]) -> [u8; 12] {
    let mut p = [0u8; 12];
    p[0] = 0x5A;
    p[1] = 0x0B;
    p[2..].copy_from_slice(response);
    p
}

pub fn raster(index: u16, data: &[u8; RASTER_DATA_LEN]) -> [u8; 100] {
    let mut p = [0u8; 100];
    p[0] = 0x55;
    p[1..3].copy_from_slice(&index.to_be_bytes());
    p[3..99].copy_from_slice(data);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_packet_bytes() {
        assert_eq!(hello(), [0x5A, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn density_packet_bytes() {
        assert_eq!(set_density(3), [0x5A, 0x0C, 3]);
    }

    #[test]
    fn print_start_end_encode_length_big_endian() {
        assert_eq!(print_start(0x0142), [0x5A, 0x04, 0x01, 0x42, 0x00, 0x00]);
        assert_eq!(print_end(0x0142), [0x5A, 0x04, 0x01, 0x42, 0x01, 0x00]);
    }

    #[test]
    fn auth_challenge_packet() {
        let c = [9u8; 10];
        let p = auth_challenge(&c);
        assert_eq!(&p[..2], &[0x5A, 0x0A]);
        assert_eq!(&p[2..], &c);
    }

    #[test]
    fn auth_reply_packet() {
        let r = [7u8; 10];
        let p = auth_reply(&r);
        assert_eq!(&p[..2], &[0x5A, 0x0B]);
        assert_eq!(&p[2..], &r);
    }

    #[test]
    fn raster_packet_layout() {
        let data = [0xFFu8; 96];
        let p = raster(0x0203, &data);
        assert_eq!(p.len(), 100);
        assert_eq!(&p[..3], &[0x55, 0x02, 0x03]);
        assert_eq!(&p[3..99], &data[..]);
        assert_eq!(p[99], 0x00);
    }
}
