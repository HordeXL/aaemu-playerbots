//! Wander: stroll around the place the bot stands in, with pauses.
//!
//! The bot picks a walkable point 10 to 60 metres from where it started the goal, walks there
//! (mostly running, sometimes walking), waits a few seconds and repeats for one to three minutes.
//! It is the fallback every bot can do anywhere, and what makes the map look alive.

use std::fmt::Write;

use crate::bot::ctx::Ctx;
use crate::bot::manner;
use crate::bot::motion::{MotionStatus, Pace};

use super::{jitter_ms, random_point, FailReason, GoalStatus};

/// Shortest and longest leg in metres.
const LEG_MIN_M: f32 = 10.0;
const LEG_MAX_M: f32 = 60.0;
/// Total time of the goal in milliseconds.
const TOTAL_MS: (u64, u64) = (60_000, 180_000);
/// Pause between two legs in milliseconds.
const PAUSE_MS: (u64, u64) = (2_000, 10_000);
/// Chance that a leg is walked instead of run.
const WALK_CHANCE: f32 = 0.3;
/// Failed legs in a row after which the goal gives up.
const MAX_FAILS: u8 = 3;

/// Strolling around. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct Wander {
    home: [f32; 3],
    end_ms: u64,
    pause_until_ms: u64,
    walking: bool,
    fails: u8,
    dest: (f32, f32),
}

impl Wander {
    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        self.home = ctx.pos();
        self.end_ms = ctx.now_ms + jitter_ms(ctx.rng, TOTAL_MS.0, TOTAL_MS.1);
        self.pause_until_ms = ctx.now_ms + jitter_ms(ctx.rng, 500, 3_000);
        self.walking = false;
        self.fails = 0;
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let now = ctx.now_ms;
        if self.walking {
            match ctx.motion.status() {
                MotionStatus::Moving => return GoalStatus::Running,
                MotionStatus::Failed(_) => {
                    self.walking = false;
                    self.fails += 1;
                    self.pause_until_ms = now + 1_000;
                }
                MotionStatus::Arrived | MotionStatus::Idle => {
                    self.walking = false;
                    self.fails = 0;
                    self.pause_until_ms = now + jitter_ms(ctx.rng, PAUSE_MS.0, PAUSE_MS.1);
                }
            }
        }
        if self.fails >= MAX_FAILS {
            return GoalStatus::Failed(FailReason::Stuck);
        }
        if now >= self.end_ms {
            return GoalStatus::Done;
        }
        if now < self.pause_until_ms {
            // (in front of a player the bot looks around while it pauses)
            manner::wander_pause(ctx);
            return GoalStatus::Running;
        }
        match random_point(ctx.world, ctx.rng, self.home, LEG_MIN_M, LEG_MAX_M, 8) {
            Some((x, y)) => {
                let pace = if ctx.rng.f32() < WALK_CHANCE {
                    Pace::Walk
                } else {
                    Pace::Run
                };
                self.dest = (x, y);
                ctx.motion.go_to(x, y, 1.0, pace, now);
                self.walking = true;
            }
            None => {
                // Boxed in by water or the edge of the map: count it as a failed leg.
                self.fails += 1;
                self.pause_until_ms = now + 1_000;
            }
        }
        GoalStatus::Running
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        if self.walking {
            ctx.motion.stop();
            self.walking = false;
        }
    }

    /// Appends what the bot is doing.
    pub fn describe(&self, out: &mut String) {
        if self.walking {
            let _ = write!(out, "strolling to {:.0}, {:.0}", self.dest.0, self.dest.1);
        } else {
            out.push_str("looking around");
        }
    }
}
