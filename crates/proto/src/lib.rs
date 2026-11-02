//! ArcheAge 1.2 (r208022) client-side wire protocol for the AAEmu Rust server.
//!
//! `codec`, `stream`, `pos` and `opcodes` are vendored from the AAEmu-1.2-Rust workspace
//! (`aaemu-commons`, `aaemu-game`), so this crate does not depend on the server crates.

pub mod codec;
pub mod opcodes;
pub mod pos;
pub mod stream;

pub use stream::{MarshalError, PacketStream};
