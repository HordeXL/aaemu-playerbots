//! The trade-run goal (stub).
//!
//! To implement: choose a pack craft (`ctx.world.pack_crafts()`), find a bench
//! (`ctx.world.pack_crafts_at`), `cs::execute_craft`, carry the pack to a buyer
//! (`ctx.world.specialty_buyers`) and `cs::sell_backpack_goods`. Labor and materials come from
//! `ctx.state` and, with `bots.gm_bootstrap`, from GM commands (`ctx.gm`).

use crate::bot::ctx::Ctx;

use super::{FailReason, GoalStatus};

/// Craft, carry and sell trade packs. Not implemented yet.
#[derive(Debug, Default, Clone)]
pub struct TradeRun {}

impl TradeRun {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick: reports that the goal is not implemented.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}
}
