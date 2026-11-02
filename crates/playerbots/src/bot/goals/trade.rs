//! The player-trade goal (stub).
//!
//! To implement: like the duel goal, driven by a [`PairOrder`] of kind `Trade`: meet at
//! `order.meet_at`, `cs::can_start_trade` / `cs::start_trade`, put items up with
//! `cs::putup_trade_item` and `cs::putup_trade_money`, `cs::trade_lock`, `cs::trade_ok`; cancel with
//! `cs::cancel_trade`. `ctx.state.bag` is the item source; `ctx.gm` can mint items when
//! `bots.gm_bootstrap` is on.

use crate::bot::cmd::PairOrder;
use crate::bot::ctx::Ctx;

use super::{FailReason, GoalStatus};

/// Trade with another bot. Not implemented yet.
#[derive(Debug, Clone)]
pub struct Trade {
    order: Option<PairOrder>,
}

impl Trade {
    /// A goal for the pairing `order` (`None` when picked by the brain on its own).
    pub fn new(order: Option<PairOrder>) -> Trade {
        Trade { order }
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
