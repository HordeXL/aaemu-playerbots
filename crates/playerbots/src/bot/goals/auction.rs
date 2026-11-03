//! Auction: list surplus goods at the auction house (owned by the auction feature).
//!
//! This file is the hook set the brain and the goal enum call; until the feature fills it in, the
//! goal fails with [`FailReason::NotImplemented`] (the brain then switches it off for the bot) and
//! its [`score`] is 0, so no bot ever picks it. Already routed here:
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table (no temperament, hurt or repeat factors are applied on top; gating, a
//!   cooldown or a `NotImplemented` still zero it). Gate on `ctx.cfg.market.enabled` and
//!   `ctx.cfg.market.auction`.
//! * What the bot may sell: `ctx.state.market.sellable_surplus(ctx.world, &ctx.state.bag)` (the
//!   provenance ledger, [`crate::bot::market::ledger`]: only goods the bot came by honestly) and
//!   `ctx.world.item_facts(template)` for vendor prices, binding and auction categories.
//! * What the goal remembers between runs: `ctx.state.market.inner_mut().auction`
//!   ([`AuctionMemory`](crate::bot::market::auction::AuctionMemory)).

use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// Listing goods at the auction house. See the module docs.
#[derive(Debug, Default)]
pub struct Auction;

impl Auction {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, close the window).
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
        out.push_str("auction (not implemented)");
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
        let g = Auction;
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("auction"), "{text}");
    }

    #[test]
    fn the_stub_has_no_weight() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
    }
}
