//! Sail: summon a boat and cross the water (owned by the sailing feature).
//!
//! This file is the hook set the brain and the goal enum call; until the feature fills it in, the
//! goal fails with [`FailReason::NotImplemented`] (the brain then switches it off for the bot) and
//! its [`score`] is 0, so no bot ever picks it. Already routed here:
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table (no temperament, hurt or repeat factors are applied on top; gating, a
//!   cooldown or a `NotImplemented` still zero it). Gate on `ctx.cfg.behavior.sailing`.
//! * `ctx.state.vehicle` (the slave and seat events, ship movement; see
//!   [`crate::bot::vehicle`]) and `ctx.state.place`.
//! * `World::boat_scrolls()`, `World::boat_for_scroll`, `World::slave_template` for the static
//!   data.

use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// Summoning a boat and sailing it. See the module docs.
#[derive(Debug, Default)]
pub struct Sail;

impl Sail {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, leave the boat).
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}

    /// Whether the goal drives the combat controller itself (see [`super::Goal::owns_combat`]).
    pub fn owns_combat(&self) -> bool {
        false
    }

    /// The activity the web UI shows while the goal runs.
    pub fn activity(&self, moving: bool, fighting: bool) -> Activity {
        if fighting {
            Activity::Fighting
        } else if moving {
            Activity::Traveling
        } else {
            Activity::Idle
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        out.push_str("sail (not implemented)");
    }
}

/// The goal's weight before the situation is looked at, from the configured weights and the
/// bot's temperament (`brain::base_weights` calls this; 0 means "not scored by weight").
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The score of the goal for this tick, given its `base` weight. 0 keeps the bot from picking it.
/// Called whenever the brain scores the goals, so keep it cheap.
pub fn score(_ctx: &Ctx<'_>, _base: f32) -> f32 {
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_shows_as_idle_or_traveling() {
        let g = Sail;
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
    }
}
