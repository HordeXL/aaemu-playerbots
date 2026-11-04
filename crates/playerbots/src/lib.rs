//! Headless ArcheAge 1.2 player bots for the AAEmu Rust server.
//!
//! Every bot is a network client driven by one tokio task ([`bot`]); hundreds run in one process
//! sharing one read-only [`aa_world::World`]. The [`supervisor`] starts and stops them, the
//! [`hub`] publishes them to the operator web UI and routes its commands back, the [`roster`]
//! remembers who they are across restarts.
//!
//! ```text
//!  main ── supervisor ── bot task ── Session (login, lobby, world packets)
//!    │          │            ├─ BotState   (what the bot knows)
//!    │          │            ├─ Motion     (walking, CSMoveUnit)
//!    │          │            ├─ Combat     (target, skills, loot)
//!    │          │            └─ Brain      (goal choice) ── Goal (Wander, Rest, Grind, ...)
//!    ├─ hub ──── web UI (pb_web::BotHub)
//!    └─ coordinator (pairs bots for duels and trades)
//! ```

pub mod archetype;
pub mod bot;
pub mod config;
pub mod contest;
pub mod events;
pub mod gm;
pub mod hub;
pub mod issues;
pub mod names;
pub mod roster;
pub mod secrets;
pub mod supervisor;

#[cfg(test)]
mod testutil;
