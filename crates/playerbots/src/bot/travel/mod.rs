//! Portal travel: the teleport book, recall points and gates (owned by the portals feature).
//!
//! [`TravelState`] is the bot's memory of what the server told it about portals: the private
//! teleport book, the visited recall districts and the bound one, and the gates used around it.
//! [`BotState`](super::state::BotState) owns one (`state.travel`) and forwards every portal event
//! to [`TravelState::on_event`], in arrival order, from `BotState::apply` (never from a goal).
//! The events are [`WorldEvent::PortalBook`], [`WorldEvent::ReturnDistricts`],
//! [`WorldEvent::PortalSaved`], [`WorldEvent::PortalDeleted`] and [`WorldEvent::PortalUsed`].
//!
//! The per-bot budget is tight (`size_of::<Bot>() < 3200` is asserted, with 48 bytes to spare when
//! the travel hooks went in): keep this struct within [`MAX_INLINE_BYTES`] and put anything that
//! grows behind a lazily created `Box`, like `BotState::quest_mem`.

use aa_proto::sc_world::WorldEvent;

/// The most bytes [`TravelState`] may take inline in every bot (checked by a test).
pub const MAX_INLINE_BYTES: usize = 16;

/// What a bot knows about portals. See the module docs.
#[derive(Debug, Default)]
pub struct TravelState {}

impl TravelState {
    /// A portal event of the server (see the module docs for which). Does nothing yet.
    pub fn on_event(&mut self, _ev: &WorldEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_stays_within_its_share_of_the_bot() {
        assert!(std::mem::size_of::<TravelState>() <= MAX_INLINE_BYTES);
    }
}
