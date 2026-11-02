//! Goals: what a bot does for a few minutes at a time.
//!
//! A goal is a small state machine the [`Brain`](super::brain::Brain) starts, ticks and stops.
//! The set is closed (an enum, no allocation per goal): [`Wander`], [`Rest`] and [`Grind`] are
//! implemented; [`Quest`], [`Duel`], [`Trade`], [`TradeRun`] and [`Housing`] are stubs that report
//! [`FailReason::NotImplemented`] on their first tick so the brain stops considering them.
//!
//! # Writing a goal
//!
//! * Put the state in the goal struct; keep it small (it lives as long as the bot task).
//! * `start` runs once when the brain picks the goal, `tick` every brain tick (250 ms while the
//!   bot moves or fights, 1.5 s while it stands), `stop` when the goal ends or is interrupted
//!   (stop motion, drop targets).
//! * Everything the goal may touch is in [`Ctx`]: the world data, the bot's state, movement
//!   ([`Ctx::travel`], `ctx.motion`), combat (`ctx.combat.engage`), packets (`ctx.send`,
//!   `ctx.say`, `ctx.gm`), the pairing service (`ctx.coord`) and the hub (map markers).
//! * Return [`GoalStatus::Running`] while busy, [`GoalStatus::Done`] when finished and
//!   [`GoalStatus::Failed`] when it cannot proceed; the brain applies a cooldown to a failed goal.
//! * Do not keep borrows of `ctx` between ticks and do not allocate per tick.

pub mod duel;
pub mod grind;
pub mod housing;
pub mod quest;
pub mod rest;
pub mod social;
pub mod trade;
pub mod trade_run;
pub mod wander;

use pb_web::Activity;

pub use duel::Duel;
pub use grind::Grind;
pub use housing::Housing;
pub use quest::Quest;
pub use rest::Rest;
pub use trade::Trade;
pub use trade_run::TradeRun;
pub use wander::Wander;

use super::cmd::{GoalKind, PairOrder};
use super::combat::CombatStatus;
use super::ctx::Ctx;
use super::motion::SWIM_THRESHOLD;

/// Why a goal gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// The goal is a stub; the brain never picks it again.
    NotImplemented,
    /// Nothing to work on (no monster, no quest giver, no partner).
    NoTarget,
    /// The way is blocked.
    Stuck,
    /// It took too long.
    Timeout,
    /// Anything else, with a short reason for the log.
    Other(&'static str),
}

/// What a goal tick came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStatus {
    /// Keep going.
    Running,
    /// Finished normally.
    Done,
    /// Gave up.
    Failed(FailReason),
}

/// The running goal. See the module docs.
#[derive(Debug)]
pub enum Goal {
    /// Walk around.
    Wander(Wander),
    /// Stand still and recover.
    Rest(Rest),
    /// Kill monsters.
    Grind(Grind),
    /// Do quests (stub).
    Quest(Quest),
    /// Duel another bot (stub).
    Duel(Duel),
    /// Trade with another bot (stub).
    Trade(Trade),
    /// Craft, carry and sell trade packs (stub).
    TradeRun(TradeRun),
    /// Build and use a house (stub).
    Housing(Housing),
}

impl Goal {
    /// A fresh goal of `kind`; `order` is the coordinator's pairing for duels and trades.
    pub fn new(kind: GoalKind, order: Option<PairOrder>) -> Goal {
        match kind {
            GoalKind::Wander => Goal::Wander(Wander::default()),
            GoalKind::Rest => Goal::Rest(Rest::default()),
            GoalKind::Grind => Goal::Grind(Grind::default()),
            GoalKind::Quest => Goal::Quest(Quest::default()),
            GoalKind::Duel => Goal::Duel(Duel::new(order)),
            GoalKind::Trade => Goal::Trade(Trade::new(order)),
            GoalKind::TradeRun => Goal::TradeRun(TradeRun::default()),
            GoalKind::Housing => Goal::Housing(Housing::default()),
        }
    }

    /// Which goal this is.
    pub fn kind(&self) -> GoalKind {
        match self {
            Goal::Wander(_) => GoalKind::Wander,
            Goal::Rest(_) => GoalKind::Rest,
            Goal::Grind(_) => GoalKind::Grind,
            Goal::Quest(_) => GoalKind::Quest,
            Goal::Duel(_) => GoalKind::Duel,
            Goal::Trade(_) => GoalKind::Trade,
            Goal::TradeRun(_) => GoalKind::TradeRun,
            Goal::Housing(_) => GoalKind::Housing,
        }
    }

    /// Whether the goal drives the combat controller itself. For every other goal the brain lets
    /// the controller defend the bot and pauses the goal while a fight or a loot job is on.
    pub fn owns_combat(&self) -> bool {
        matches!(self, Goal::Grind(_) | Goal::Quest(_) | Goal::Duel(_))
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        match self {
            Goal::Wander(g) => g.start(ctx),
            Goal::Rest(g) => g.start(ctx),
            Goal::Grind(g) => g.start(ctx),
            Goal::Quest(g) => g.start(ctx),
            Goal::Duel(g) => g.start(ctx),
            Goal::Trade(g) => g.start(ctx),
            Goal::TradeRun(g) => g.start(ctx),
            Goal::Housing(g) => g.start(ctx),
        }
    }

    /// One tick; `combat` is the controller's verdict of this tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>, combat: CombatStatus) -> GoalStatus {
        match self {
            Goal::Wander(g) => g.tick(ctx),
            Goal::Rest(g) => g.tick(ctx),
            Goal::Grind(g) => g.tick(ctx, combat),
            Goal::Quest(g) => g.tick(ctx),
            Goal::Duel(g) => g.tick(ctx),
            Goal::Trade(g) => g.tick(ctx),
            Goal::TradeRun(g) => g.tick(ctx),
            Goal::Housing(g) => g.tick(ctx),
        }
    }

    /// Called when the goal ends, fails or is interrupted.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        match self {
            Goal::Wander(g) => g.stop(ctx),
            Goal::Rest(g) => g.stop(ctx),
            Goal::Grind(g) => g.stop(ctx),
            Goal::Quest(g) => g.stop(ctx),
            Goal::Duel(g) => g.stop(ctx),
            Goal::Trade(g) => g.stop(ctx),
            Goal::TradeRun(g) => g.stop(ctx),
            Goal::Housing(g) => g.stop(ctx),
        }
    }

    /// The activity the web UI shows while this goal runs.
    pub fn activity(&self, moving: bool, fighting: bool) -> Activity {
        match self {
            Goal::Wander(_) => {
                if moving {
                    Activity::Traveling
                } else {
                    Activity::Idle
                }
            }
            Goal::Rest(_) => Activity::Resting,
            Goal::Grind(_) => {
                if fighting {
                    Activity::Fighting
                } else {
                    Activity::Grinding
                }
            }
            Goal::Quest(_) => Activity::Questing,
            Goal::Duel(_) => Activity::Dueling,
            Goal::Trade(_) => Activity::Trading,
            Goal::TradeRun(_) => Activity::TradeRun,
            Goal::Housing(_) => Activity::Housing,
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        match self {
            Goal::Wander(g) => g.describe(out),
            Goal::Rest(_) => out.push_str("resting"),
            Goal::Grind(g) => g.describe(out),
            Goal::Quest(g) => g.describe(out),
            Goal::Duel(g) => g.describe(out),
            Goal::Trade(g) => g.describe(out),
            Goal::Housing(g) => g.describe(out),
            Goal::TradeRun(g) => g.describe(out),
        }
    }
}

// ---- helpers shared by the goals -----------------------------------------------------------

/// Whether a bot may stand at (x, y): on land, and inside the baked terrain when there is one.
/// Without baked terrain every point is accepted (the bots walk on flat ground).
pub fn walkable(world: &aa_world::World, x: f32, y: f32) -> bool {
    match world.ground_z(x, y) {
        Some(g) => g >= SWIM_THRESHOLD + 1.0,
        None => world.baked().terrain.is_none(),
    }
}

/// A random walkable point between `min_r` and `max_r` metres from `center`, or `None` after
/// `tries` attempts.
pub fn random_point(
    world: &aa_world::World,
    rng: &mut fastrand::Rng,
    center: [f32; 3],
    min_r: f32,
    max_r: f32,
    tries: u32,
) -> Option<(f32, f32)> {
    for _ in 0..tries {
        let ang = rng.f32() * std::f32::consts::TAU;
        let r = min_r + rng.f32() * (max_r - min_r).max(0.0);
        let (x, y) = (center[0] + ang.cos() * r, center[1] + ang.sin() * r);
        if walkable(world, x, y) {
            return Some((x, y));
        }
    }
    None
}

/// Whether a monster of `level` is a fair fight for a bot of `own` level.
pub fn level_ok(own: u8, level: u8) -> bool {
    level <= own.saturating_add(3) && level.saturating_add(10) >= own
}

/// A random number of milliseconds in `lo..hi`.
pub fn jitter_ms(rng: &mut fastrand::Rng, lo: u64, hi: u64) -> u64 {
    if hi <= lo {
        lo
    } else {
        lo + rng.u64(0..hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fair_fights() {
        assert!(level_ok(1, 1));
        assert!(level_ok(1, 4));
        assert!(!level_ok(1, 5));
        assert!(level_ok(20, 10));
        assert!(!level_ok(20, 9));
        assert!(level_ok(255, 255));
    }

    #[test]
    fn jitter_stays_in_range() {
        let mut rng = fastrand::Rng::with_seed(1);
        for _ in 0..1000 {
            let v = jitter_ms(&mut rng, 100, 200);
            assert!((100..200).contains(&v));
        }
        assert_eq!(jitter_ms(&mut rng, 5, 5), 5);
        assert_eq!(jitter_ms(&mut rng, 9, 5), 9);
    }

    #[test]
    fn activities_follow_the_goal() {
        let w = Goal::new(GoalKind::Wander, None);
        assert_eq!(w.activity(true, false), Activity::Traveling);
        assert_eq!(w.activity(false, false), Activity::Idle);
        let g = Goal::new(GoalKind::Grind, None);
        assert_eq!(g.activity(false, true), Activity::Fighting);
        assert_eq!(g.activity(true, false), Activity::Grinding);
        assert_eq!(
            Goal::new(GoalKind::Rest, None).activity(false, false),
            Activity::Resting
        );
        assert!(g.owns_combat() && !w.owns_combat());
        for kind in GoalKind::ALL {
            assert_eq!(Goal::new(kind, None).kind(), kind);
        }
    }
}
