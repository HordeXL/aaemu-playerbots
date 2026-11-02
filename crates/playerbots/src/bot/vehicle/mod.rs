//! Vehicles: boats and other slaves the bot owns or rides (owned by the sailing feature).
//!
//! [`VehicleState`] is the bot's memory of its slave: the one it summoned, the one it sits on,
//! its seats and, for a ship, its heading, speed and helm. [`BotState`](super::state::BotState)
//! owns one (`state.vehicle`) and forwards every slave event to [`VehicleState::on_event`], in
//! arrival order, from `BotState::apply` (never from a goal). The events are
//! [`WorldEvent::SlaveCreated`], [`WorldEvent::MySlave`], [`WorldEvent::SlaveBound`],
//! [`WorldEvent::UnitAttached`], [`WorldEvent::UnitDetached`], [`WorldEvent::SlaveDespawned`],
//! [`WorldEvent::SlaveRemoved`] and [`WorldEvent::ShipMoved`] (the last one is made from the
//! same packet as a ship's [`WorldEvent::UnitMoved`], see `session::decode_checked`).
//!
//! The per-bot budget is tight (`size_of::<Bot>() < 3200` is asserted, with 48 bytes to spare when
//! the travel hooks went in): keep this struct within [`MAX_INLINE_BYTES`] and put anything that
//! grows behind a lazily created `Box`, like `BotState::quest_mem`.

use aa_proto::sc_world::WorldEvent;

/// The most bytes [`VehicleState`] may take inline in every bot (checked by a test).
pub const MAX_INLINE_BYTES: usize = 16;

/// What a bot knows about its boat. See the module docs.
#[derive(Debug, Default)]
pub struct VehicleState {}

impl VehicleState {
    /// A slave event of the server (see the module docs for which); `my_obj` is the bot's own
    /// object id (0 before it spawned). Does nothing yet.
    pub fn on_event(&mut self, _ev: &WorldEvent, _my_obj: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_stays_within_its_share_of_the_bot() {
        assert!(std::mem::size_of::<VehicleState>() <= MAX_INLINE_BYTES);
    }
}
