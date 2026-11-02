//! The duel goal (stub).
//!
//! To implement: the [`PairOrder`] from the coordinator names the partner and a meeting point
//! (`order.meet_at`). The `Initiator` walks there (`ctx.travel`) and sends
//! `cs::challenge_duel(order.partner_char)`; the `Responder` waits for the challenge
//! (`ActivityEvent` duel packets) and answers with `cs::start_duel(.., cs::DUEL_ACCEPT)`. During
//! the fight use `ctx.combat.engage`; announce the arena with `ctx.hub.set_duel_flag` and remove
//! it with `ctx.hub.clear_duel_flag`. Ask for a partner with
//! `ctx.coord.request_partner(PairKind::Duel, ctx.persona.id, (x, y))`.

use crate::bot::cmd::PairOrder;
use crate::bot::ctx::Ctx;

use super::{FailReason, GoalStatus};

/// Duel another bot. Not implemented yet.
#[derive(Debug, Clone)]
pub struct Duel {
    order: Option<PairOrder>,
}

impl Duel {
    /// A goal for the pairing `order` (`None` when picked by the brain on its own).
    pub fn new(order: Option<PairOrder>) -> Duel {
        Duel { order }
    }

    /// The coordinator's pairing, if any.
    pub fn order(&self) -> Option<&PairOrder> {
        self.order.as_ref()
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick: reports that the goal is not implemented.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}
}
