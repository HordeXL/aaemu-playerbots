//! The quest goal (stub).
//!
//! To implement: pick a quest from `ctx.world.quests()` that suits the bot's level and race, walk
//! to the giver (`ctx.travel`), `cs::start_quest_context`, then follow the compiled steps
//! (`aa_world::QuestStep`) with `cs::quest_talk_made`, kills (`ctx.combat.engage`), doodad use
//! (`cs::start_skill_doodad`) and `cs::complete_quest_context`. `ctx.state.quests` holds the
//! server's view; `StateError::Quest` records refusals, which should quarantine the quest.

use crate::bot::ctx::Ctx;

use super::{FailReason, GoalStatus};

/// Do quests. Not implemented yet.
#[derive(Debug, Default, Clone)]
pub struct Quest {}

impl Quest {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick: reports that the goal is not implemented.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}
}
