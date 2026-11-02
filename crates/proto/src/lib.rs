//! ArcheAge 1.2 (r208022) client-side wire protocol for the AAEmu Rust server.
//!
//! `codec`, `stream`, `pos` and `opcodes` are vendored from the AAEmu-1.2-Rust workspace
//! (`aaemu-commons`, `aaemu-game`), so this crate does not depend on the server crates.
//!
//! - [`login`]: login server client (Trion ticket auth, world list, world cookie).
//! - [`game`]: game server connection (framing, DD04 batches, send/recv).
//! - [`cs`]: client → server packet builders.
//! - [`sc`]: server → client packet parsers ([`sc::ServerEvent`]).

pub mod codec;
pub mod cs;
pub mod game;
pub mod login;
pub mod opcodes;
pub mod pos;
pub mod sc;
pub mod sc_activity;
pub mod sc_world;
pub mod stream;

pub use stream::{MarshalError, PacketStream};
