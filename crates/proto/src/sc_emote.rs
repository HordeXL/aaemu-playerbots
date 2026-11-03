//! `SCEmotionExpressedPacket` (0x12b): somebody in view played an emotion.
//!
//! Server writer: `systems/services/express_text.rs:60` (`bc obj, bc target, u32 emotion`). It is
//! parsed on its own, outside [`crate::sc::ServerEvent`], so the packet is only read by the bots
//! that look at emotes.

use crate::game::LEVEL_GAME;
use crate::opcodes::sc;
use crate::stream::{MarshalError, PacketStream};

/// One emotion somebody played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expressed {
    /// Object id of the unit that played it.
    pub obj: u32,
    /// Object id of the unit it was played at (0 = nobody).
    pub target: u32,
    /// `express_texts` id, see [`crate::cs::emotion`].
    pub emotion: u32,
}

/// Whether a packet of this level and opcode is a `SCEmotionExpressedPacket`.
pub fn is_expressed(level: u8, opcode: u16) -> bool {
    level == LEVEL_GAME && opcode == sc::SCEmotionExpressedPacket
}

/// Reads the body of a `SCEmotionExpressedPacket`.
pub fn read_expressed(body: &[u8]) -> Result<Expressed, MarshalError> {
    let mut s = PacketStream::from_bytes(body);
    Ok(Expressed {
        obj: s.read_bc()?,
        target: s.read_bc()?,
        emotion: s.read_u32()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_server_layout() {
        let mut s = PacketStream::new();
        s.write_bc(0x03_0201).write_bc(0).write_u32(12);
        let e = read_expressed(&s.into_bytes()).unwrap();
        assert_eq!(
            e,
            Expressed {
                obj: 0x03_0201,
                target: 0,
                emotion: 12
            }
        );
        let mut s = PacketStream::new();
        s.write_bc(7).write_bc(0x10_0001).write_u32(62);
        let e = read_expressed(&s.into_bytes()).unwrap();
        assert_eq!((e.obj, e.target, e.emotion), (7, 0x10_0001, 62));
    }

    #[test]
    fn a_short_body_is_an_error_not_a_panic() {
        assert!(read_expressed(&[]).is_err());
        assert!(read_expressed(&[1, 2, 3, 0, 0, 0, 5]).is_err());
    }

    #[test]
    fn only_the_game_level_opcode_counts() {
        assert!(is_expressed(LEVEL_GAME, sc::SCEmotionExpressedPacket));
        assert!(!is_expressed(2, sc::SCEmotionExpressedPacket));
        assert!(!is_expressed(LEVEL_GAME, sc::SCOneUnitMovementPacket));
    }
}
