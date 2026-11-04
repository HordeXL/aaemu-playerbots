//! Walking in legs: the helper the gank and warzone goals use to get somewhere far away.
//!
//! The same scheme as the trade run's walk: a destination within one leg (or a trip the portals or
//! a GM `/move` make cheap) is asked for in one piece, a farther one is walked a stretch at a
//! time, and a walk that fails three times in a row is [`Walk::Stuck`].

use crate::bot::ctx::Ctx;
use crate::bot::motion::{MotionStatus, Pace};
use crate::bot::state::dist2d;

/// The bot counts as arrived this much beyond its stop radius.
const ARRIVE_SLACK_M: f32 = 0.7;
/// Failed walks in a row after which the destination is given up.
const MAX_FAILS: u8 = 3;
/// A leg of a walk never ends closer than this to the bot (a stretch shorter than that is not
/// worth a trip of its own).
const MIN_LEG_M: f32 = 40.0;

/// What a walking tick came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// On the way.
    Going,
    /// Within the stop radius of the destination.
    There,
    /// The walk failed repeatedly.
    Stuck,
}

/// The state of one walk (the count of failed legs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Walker {
    fails: u8,
}

impl Walker {
    /// A walker with no failures.
    pub fn new() -> Walker {
        Walker::default()
    }

    /// Starts over (a new destination).
    pub fn reset(&mut self) {
        self.fails = 0;
    }

    /// One walking tick towards `dest`, stopping `radius` metres short of it.
    pub fn step(&mut self, ctx: &mut Ctx<'_>, dest: [f32; 2], radius: f32) -> Walk {
        let me = ctx.state.me.pos;
        let d = dist2d(me, [dest[0], dest[1], 0.0]);
        // (the controller only defends against close attackers while a goal travels)
        ctx.combat.note_travelling(ctx.now_ms);
        if d <= radius + ARRIVE_SLACK_M {
            if ctx.motion.is_moving() {
                ctx.motion.stop();
            }
            self.fails = 0;
            return Walk::There;
        }
        match ctx.motion.status() {
            MotionStatus::Moving => return Walk::Going,
            MotionStatus::Failed(_) => {
                self.fails += 1;
                if self.fails >= MAX_FAILS {
                    return Walk::Stuck;
                }
            }
            MotionStatus::Idle | MotionStatus::Arrived => {}
        }
        let leg = ctx.motion.leg_m();
        if d <= leg + 20.0
            || ctx.would_teleport(d)
            || crate::bot::travel::has_route(ctx, dest[0], dest[1])
        {
            ctx.travel(dest[0], dest[1], radius, Pace::Run);
        } else {
            let k = (leg.max(MIN_LEG_M) / d).min(1.0);
            ctx.travel(
                me[0] + (dest[0] - me[0]) * k,
                me[1] + (dest[1] - me[1]) * k,
                1.0,
                Pace::Run,
            );
        }
        Walk::Going
    }
}
