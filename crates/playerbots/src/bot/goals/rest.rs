//! Rest: stand still until hit points and mana have recovered.
//!
//! The server regenerates a standing character; the goal ends when both are nearly full (after at
//! least ten seconds) or after at most 90 seconds.

use crate::bot::ctx::Ctx;

use super::{jitter_ms, GoalStatus};

/// Shortest rest in milliseconds.
const MIN_MS: u64 = 10_000;
/// Longest rest in milliseconds.
const MAX_MS: (u64, u64) = (45_000, 90_000);
/// Recovered enough: hit points and mana in percent.
const HP_OK: u8 = 95;
const MP_OK: u8 = 80;

/// Standing still. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct Rest {
    min_end_ms: u64,
    end_ms: u64,
}

impl Rest {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        ctx.motion.stop();
        self.min_end_ms = ctx.now_ms + MIN_MS;
        self.end_ms = ctx.now_ms + jitter_ms(ctx.rng, MAX_MS.0, MAX_MS.1);
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let now = ctx.now_ms;
        let me = &ctx.state.me;
        let recovered = me.hp_pct() >= HP_OK && me.mp_pct() >= MP_OK;
        if now >= self.end_ms || (now >= self.min_end_ms && recovered) {
            GoalStatus::Done
        } else {
            GoalStatus::Running
        }
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, _ctx: &mut Ctx<'_>) {}
}
