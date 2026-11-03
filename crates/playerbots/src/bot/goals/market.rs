//! Market: advertise surplus goods in trade chat and sell them to buyers (owned by the market
//! feature).
//!
//! This file is the hook set the brain and the goal enum call; until the feature fills it in, the
//! goal fails with [`FailReason::NotImplemented`] (the brain then switches it off for the bot) and
//! its [`score`] is 0, so no bot ever picks it. Already routed here:
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table (no temperament, hurt or repeat factors are applied on top; gating, a
//!   cooldown or a `NotImplemented` still zero it). Gate on `ctx.cfg.market.enabled` and
//!   `ctx.cfg.market.chat_ads`; selling to real players also needs
//!   [`Config::may_sell_to_players`](crate::config::Config::may_sell_to_players).
//! * What the bot may sell: `ctx.state.market.sellable_surplus(ctx.world, &ctx.state.bag)` (the
//!   provenance ledger, [`crate::bot::market::ledger`]: only goods the bot came by honestly) and
//!   `ctx.world.item_facts(template)`.
//! * What the bot hears: shouts, trade and group-finding chat and whispers reach
//!   [`AdState::on_chat`](crate::bot::market::ads::AdState::on_chat) from the social duty.
//! * What the goal remembers between runs: `ctx.state.market.inner_mut().ads`
//!   ([`AdState`](crate::bot::market::ads::AdState)); the chat types for what it says are in
//!   `aa_proto::cs::chat_type`, the trade window is `ctx.state.trade`
//!   ([`super::trade`]).

use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// Advertising and selling goods. See the module docs.
#[derive(Debug, Default)]
pub struct Market;

impl Market {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, _ctx: &mut Ctx<'_>) {}

    /// One tick.
    pub fn tick(&mut self, _ctx: &mut Ctx<'_>) -> GoalStatus {
        GoalStatus::Failed(FailReason::NotImplemented)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, cancel an open trade).
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
        out.push_str("market (not implemented)");
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
        let g = Market;
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("market"), "{text}");
    }

    #[test]
    fn the_stub_has_no_weight() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
    }
}
