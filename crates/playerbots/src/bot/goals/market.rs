//! Market: sell goods in trade chat, meet the buyer (in town or on Mirage Isle) and trade safely.
//!
//! The work is split in two:
//!
//! * the **duty** ([`duty()`], every brain tick, whatever the bot does): tells the board where the
//!   bot is, lets [`AdState`](crate::bot::market::ads::AdState) read the shouts, trade and
//!   group-finding chat and the whispers, post advertisements within the limits, negotiate (price,
//!   one counter, the place) and say the lines that are due. When a sale is agreed it asks the
//!   brain to start this goal.
//! * the **goal** ([`Market`]): does the meeting of an agreed sale. [`machine`] is its pure state
//!   machine (go, enter the island through the portal doodad, wait, trade, leave through the exit
//!   doodad), [`run`] the glue that reads the tick and carries out its acts, [`meet`] the choice of
//!   the place and [`report`] the issue reports (the island's silence, a trade that moved nothing,
//!   a refused whisper). The trade itself is the trade goal under an agreed
//!   [`Deal`](super::trade::Deal): the bot locks and confirms only a window that holds exactly the
//!   agreement.
//!
//! Started by the operator without a deal the goal posts an advertisement at once and listens for
//! buyers for a few minutes. Everything is behind `[market] enabled` and `chat_ads`; selling to
//! players that are not our bots also needs `sell_to_players` (never with `gm_bootstrap`).

mod duty;
mod machine;
mod meet;
mod report;
mod run;

use pb_web::Activity;

use crate::bot::ctx::Ctx;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

pub use duty::duty;

/// How much the goal weighs when a deal is waiting. The duty starts the goal itself, this only
/// keeps the brain from picking something else in the same tick.
const DEAL_WEIGHT: f32 = 50.0;

/// The meeting of an agreed sale. See the module docs.
#[derive(Debug, Default)]
pub struct Market(Option<Box<run::Run>>);

impl Market {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        if !(ctx.cfg.market.enabled && ctx.cfg.market.chat_ads) {
            return;
        }
        let mut r = Box::<run::Run>::default();
        r.start(ctx);
        self.0 = Some(r);
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        match self.0.as_mut() {
            Some(r) => r.tick(ctx),
            None => GoalStatus::Failed(FailReason::Other("the market is switched off")),
        }
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, cancel an open trade).
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        if let Some(mut r) = self.0.take() {
            r.stop(ctx);
        }
    }

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
            Activity::Trading
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        match self.0.as_ref() {
            Some(r) => r.describe(out),
            None => out.push_str("market"),
        }
    }
}

/// The goal's weight before the situation is looked at (`brain::base_weights` calls this): 0,
/// the goal is never drawn by weight. It runs when a sale is agreed (the duty starts it) or when
/// the operator orders it.
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The score of the goal for this tick, given its `base` weight: high while a sale waits for its
/// meeting, else 0 (the duty advertises without the goal).
pub fn score(ctx: &Ctx<'_>, base: f32) -> f32 {
    let cfg = &ctx.cfg.market;
    if !(cfg.enabled && cfg.chat_ads) {
        return 0.0;
    }
    let waiting = ctx
        .state
        .market
        .get()
        .is_some_and(|i| i.ads.deal().is_some());
    if waiting {
        base.max(1.0) * DEAL_WEIGHT
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goal_shows_as_trading_traveling_or_fighting() {
        let g = Market::default();
        assert_eq!(g.activity(false, false), Activity::Trading);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("market"), "{text}");
    }

    #[test]
    fn the_goal_is_never_drawn_by_weight() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
    }

    #[test]
    fn the_goal_stays_small() {
        assert_eq!(
            std::mem::size_of::<Market>(),
            std::mem::size_of::<usize>(),
            "a pointer, the run is boxed"
        );
    }
}
