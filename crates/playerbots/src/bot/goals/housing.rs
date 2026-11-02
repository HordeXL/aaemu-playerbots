//! The housing goal (stub).
//!
//! To implement: pick a design (`ctx.world.housing_designs()`) and a plot inside an allowed area
//! (`ctx.world.housing_area_at` / `housing_area_allows`), `cs::construct_house_tax` then
//! `cs::create_house`, keep building with the doodad skills, and mark the house for the map with
//! `ctx.hub.set_house`. `ctx.state.houses` lists the bot's houses.

use crate::bot::ctx::Ctx;

use super::{FailReason, GoalStatus};

/// Build and use a house. Not implemented yet.
#[derive(Debug, Default, Clone)]
pub struct Housing {}

impl Housing {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick: reports that the goal is not implemented.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}
}
