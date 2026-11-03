//! Dungeon: form a party with other bots and clear an instance dungeon (owned by the dungeons
//! feature).
//!
//! This file is the hook set the brain and the goal enum call; until the feature fills it in, the
//! goal fails with [`FailReason::NotImplemented`] (the brain then switches it off for the bot) and
//! its [`score`] is 0, so no bot ever picks it. Everything the feature needs from the outside is
//! already routed here:
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table (no temperament, hurt or repeat factors are applied on top; gating, a
//!   cooldown or a `NotImplemented` still zero it). Gate on `ctx.cfg.behavior.dungeons`.
//! * [`Dungeon::new`]: the party order (`BotCmd::Party`, see [`crate::bot::party`]) that started
//!   the goal, if any.
//! * `ctx.state.place` ([`crate::bot::place::Place`]): which world the bot is in and whether an
//!   instance is loading; the instance packets keep it current.
//! * `World::dungeons()` / `World::instance_world_by_zone_key` for the static data.

mod clear;
mod entry;
mod machine;
mod route;

use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::bot::party::PartyOrder;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// Clearing an instance dungeon with a party. See the module docs.
#[derive(Debug, Default)]
pub struct Dungeon {
    /// The order that started the goal; `None` when the operator forced the goal or the score
    /// picked it.
    order: Option<Box<PartyOrder>>,
}

impl Dungeon {
    /// A goal started by `order` (`None` for an operator command or the bot's own choice).
    pub fn new(order: Option<Box<PartyOrder>>) -> Dungeon {
        Dungeon { order }
    }

    /// The party order the goal was started with.
    pub fn order(&self) -> Option<&PartyOrder> {
        self.order.as_deref()
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, drop targets).
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
        out.push_str("dungeon (not implemented)");
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
    use crate::bot::party::PartyPurpose;

    #[test]
    fn the_order_that_started_the_goal_is_kept() {
        assert!(Dungeon::default().order().is_none());
        let order = PartyOrder {
            party_id: 1,
            leader: 2,
            members: [2, 3].into_iter().collect(),
            purpose: PartyPurpose::Dungeon { zone_group: 5 },
        };
        let g = Dungeon::new(Some(Box::new(order.clone())));
        assert_eq!(g.order(), Some(&order));
        assert!(!g.owns_combat());
    }

    #[test]
    fn the_stub_shows_as_idle_or_traveling() {
        let g = Dungeon::default();
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
    }
}
