//! X6/X6h ("cat printer" family) wire protocol.
//!
//! Reverse-engineering sources are pinned in docs/PROTOCOL.md; do not adjust
//! constants from memory. Unlike the LX-D02 protocol this family has no auth
//! handshake, uses CRC8 (poly 0x07) over the payload only, and streams one
//! 48-byte scanline per packet.

pub mod crc;
pub mod job;
pub mod notifications;
pub mod packets;
