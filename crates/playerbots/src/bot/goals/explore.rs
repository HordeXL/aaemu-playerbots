//! Explore: see new places: zones the bot has not visited, landmarks, the view from a hill (owned by the presence and exploring feature).
//!
//! This file is the hook set the brain and the goal enum call; until the feature fills it in, the
//! goal fails with [`FailReason::NotImplemented`] (the brain then switches it off for the bot) and
//! its [`score`] is 0, so no bot ever picks it. Already routed here:
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table (no temperament, hurt or repeat factors are applied on top; gating, a
//!   cooldown or a `NotImplemented` still zero it).
//! * [`base_weight`]: the weight before the situation is looked at (`brain::base_weights`).
//! * The arguments the LLM director, the web UI or the agent API may start it with
//!   ([`GoalArgs`]: `zone` to head for a zone, `dest` for a point).
//! * The switches to gate on: `ctx.cfg.explore` (`enabled`), the bot's `persona.profile.explorer`, its journal (`ctx.hub.journal`, the zones it has seen).

use pb_web::Activity;

use crate::bot::cmd::GoalArgs;
use crate::bot::ctx::Ctx;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// Explore: see new places: zones the bot has not visited, landmarks, the view from a hill. See the module docs.
#[derive(Debug, Default)]
pub struct Explore {
    /// What the goal was started with (`None`: the bot's own choice).
    args: Option<Box<GoalArgs>>,
}

impl Explore {
    /// A fresh goal (the brain starts it with [`Goal::with_args`](super::Goal::with_args)).
    pub fn new(args: Option<Box<GoalArgs>>) -> Explore {
        Explore { args }
    }

    /// The arguments it was started with.
    pub fn args(&self) -> Option<&GoalArgs> {
        self.args.as_deref()
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, close windows).
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
        out.push_str("explore (not implemented)");
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
        let g = Explore::new(None);
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        assert!(g.args().is_none());
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("explore"), "{text}");
    }

    #[test]
    fn the_stub_has_no_weight() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
    }
}
