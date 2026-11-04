//! Fights against characters: when to give one up, whether the odds are fair, and the run away.
//!
//! The combat controller ([`Combat`](crate::bot::combat::Combat)) fights a character like any
//! target, with the rotation's PvP rows. What it needs on top, outside of a duel, is here:
//!
//! * [`judge`] ends a fight that cannot be won or must not go on: the bot is below
//!   `retreat_hp_pct`, or the server accepts cast after cast and the target loses nothing (the
//!   silent refusal of a protected zone, see [`rules`](super::rules)). A fight also ends by time
//!   ([`FIGHT_TIMEOUT_MS`]) and by distance ([`LOST_DISTANCE`]).
//! * [`odds`] weighs the two sides of a planned fight for the gank goal.
//! * [`flee`] runs away (the destination is a recall point of the bot's own alliance within
//!   reach, else straight away from the attacker).

use crate::bot::ctx::Ctx;
use crate::bot::motion::Pace;
use crate::bot::state::{dist2d, UnitKind};

use super::Flee;

/// A fight against a character that does not end otherwise ends after this long.
pub const FIGHT_TIMEOUT_MS: u64 = 90_000;
/// A character that runs farther than this from the bot is let go.
pub const LOST_DISTANCE: f32 = 70.0;
/// Casts the server accepted at a target that lost nothing, after which it counts as protected.
pub const PROTECTED_CASTS: u8 = 5;
/// ... over at least this long.
pub const PROTECTED_MIN_MS: u64 = 5_000;
/// A character the bot gave up on is left alone this long.
pub const SHUN_MS: u64 = 180_000;
/// A bot runs at most this long.
pub const FLEE_MS: u64 = 25_000;
/// A bot runs this far from an attacker when it knows no better place.
const FLEE_AWAY_M: f32 = 70.0;
/// A recall point of the bot's own alliance is a place to run to when it is no farther than this.
const FLEE_HOME_MAX_M: f32 = 2_500.0;
/// The run ends when the attacker is farther than this from the bot.
const FLEE_SAFE_M: f32 = 55.0;

/// Why a fight against a character is given up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Break {
    /// The bot is too hurt to go on.
    Retreat,
    /// Casts are accepted and the target loses nothing: the server protects it.
    Protected,
}

/// What [`judge`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// The bot's hit points in percent.
    pub hp_pct: u8,
    /// `[pvp] retreat_hp_pct` (0 = never retreat).
    pub retreat_pct: u8,
    /// Casts at the target the server accepted.
    pub accepted: u8,
    /// The target lost hit points during the fight.
    pub dealt: bool,
    /// Milliseconds since the first accepted cast.
    pub since_first_ms: u64,
}

/// Whether the fight must end, and why. The protected verdict comes first: a bot that cannot hurt
/// its target has nothing to retreat from.
pub fn judge(f: &Facts) -> Option<Break> {
    if !f.dealt && f.accepted >= PROTECTED_CASTS && f.since_first_ms >= PROTECTED_MIN_MS {
        return Some(Break::Protected);
    }
    if f.retreat_pct > 0 && f.hp_pct < f.retreat_pct {
        return Some(Break::Retreat);
    }
    None
}

/// The two sides of a planned fight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Odds {
    /// The bot (with the friends at its side) is the stronger or the match is even.
    Fair,
    /// The other side is stronger, but a squad could take it.
    NeedsHelp,
    /// The other side is far too strong.
    Hopeless,
}

/// A character's weight in a fight: its level and hit points (the wire has no maximum, so the
/// current value stands for it).
fn power(level: u8, hp: i32) -> f32 {
    (f32::from(level) + 5.0) * hp.max(100) as f32
}

/// A fight is fair when the other side is not stronger than this many times the bot's.
pub const FAIR_RATIO: f32 = 1.25;
/// A squad can take a side up to this many times as strong as the bot alone.
pub const SQUAD_RATIO: f32 = 3.0;

/// Weighs the bot's side (itself and the `friends` at its side, `(level, hp)` each) against the
/// target's (the target and the `foes` that stand by it).
pub fn odds(me: (u8, i32), friends: &[(u8, i32)], target: (u8, i32), foes: &[(u8, i32)]) -> Odds {
    let mine: f32 = power(me.0, me.1) + friends.iter().map(|&(l, h)| power(l, h)).sum::<f32>();
    let theirs: f32 =
        power(target.0, target.1) + foes.iter().map(|&(l, h)| power(l, h)).sum::<f32>();
    let ratio = theirs / mine.max(1.0);
    if ratio <= FAIR_RATIO {
        Odds::Fair
    } else if ratio <= SQUAD_RATIO {
        Odds::NeedsHelp
    } else {
        Odds::Hopeless
    }
}

/// Where to run from a threat at `from`: a recall point of the bot's own alliance (a home zone,
/// where the server protects it) when one is within [`FLEE_HOME_MAX_M`] and not behind the
/// threat, else a point [`FLEE_AWAY_M`] metres straight away from it.
pub fn flee_destination(ctx: &mut Ctx<'_>, from: [f32; 3]) -> Option<[f32; 2]> {
    let me = ctx.state.me.pos;
    let world = ctx.world;
    let mine = world.mother_faction(ctx.state.me.faction);
    let away = (me[1] - from[1]).atan2(me[0] - from[0]);
    if mine != 0 && world.baked().zones.is_some() {
        let mut best: Option<(f32, [f32; 2])> = None;
        for r in world.recall_points() {
            if world.zone_info(r.zone_key).map(|z| z.faction) != Some(mine) {
                continue;
            }
            let d = dist2d(me, [r.pos.x, r.pos.y, 0.0]);
            if d > FLEE_HOME_MAX_M || d < 25.0 {
                continue;
            }
            // not through the attacker: the point must lie on this side of the bot
            let bearing = (r.pos.y - me[1]).atan2(r.pos.x - me[0]);
            let off = (bearing - away + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            if off.abs() > 1.9 {
                continue;
            }
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, [r.pos.x, r.pos.y]));
            }
        }
        if let Some((_, p)) = best {
            return Some(p);
        }
    }
    // straight away, with some fan-out when the first direction is water or a cliff
    for turn in [0.0f32, 0.5, -0.5, 1.0, -1.0, 1.5, -1.5] {
        let a = away + turn;
        let (x, y) = (me[0] + a.cos() * FLEE_AWAY_M, me[1] + a.sin() * FLEE_AWAY_M);
        if crate::bot::goals::walkable(world, x, y) {
            return Some([x, y]);
        }
    }
    None
}

/// Starts running from the unit `from_obj` at `from_pos`. `false` when there is nowhere to go.
pub fn start_flee(ctx: &mut Ctx<'_>, from_obj: u32, from_pos: [f32; 3]) -> bool {
    let Some(dest) = flee_destination(ctx, from_pos) else {
        return false;
    };
    let now = ctx.now_ms;
    ctx.state.pvp_mut().flee = Some(Flee {
        dest,
        until_ms: now + FLEE_MS,
        from: from_obj,
    });
    ctx.combat.set_fleeing(true);
    ctx.motion.chase_to(dest[0], dest[1], 4.0, Pace::Run, now);
    ctx.log.say(format!(
        "pvp: running from the fight to {:.0}, {:.0}",
        dest[0], dest[1]
    ));
    true
}

/// One tick of the run; `false` once it is over (arrived, safe, out of time, dead).
pub fn flee_step(ctx: &mut Ctx<'_>) -> bool {
    let now = ctx.now_ms;
    let Some(flee) = ctx.state.pvp.as_ref().and_then(|p| p.flee) else {
        ctx.combat.set_fleeing(false);
        return false;
    };
    let me = ctx.state.me.pos;
    let threat = ctx
        .state
        .units
        .get(&flee.from)
        .filter(|u| u.kind == UnitKind::Character && !u.dead())
        .map(|u| u.dist2d(me));
    let arrived = dist2d(me, [flee.dest[0], flee.dest[1], 0.0]) <= 6.0;
    // the attacker gave up or fell behind, or the bot got where it was going
    let safe = threat.is_none_or(|d| d > FLEE_SAFE_M) && now + FLEE_MS / 2 > flee.until_ms;
    if now >= flee.until_ms || arrived || safe || ctx.state.me.dead {
        if let Some(p) = ctx.state.pvp.as_mut() {
            p.flee = None;
        }
        ctx.combat.set_fleeing(false);
        if ctx.motion.is_moving() {
            ctx.motion.stop();
        }
        return false;
    }
    if !ctx.motion.is_moving() {
        ctx.motion
            .chase_to(flee.dest[0], flee.dest[1], 4.0, Pace::Run, now);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            hp_pct: 80,
            retreat_pct: 35,
            accepted: 0,
            dealt: false,
            since_first_ms: 0,
        }
    }

    #[test]
    fn a_healthy_bot_in_a_fresh_fight_goes_on() {
        assert_eq!(judge(&facts()), None);
        // damage dealt: never protected
        let f = Facts {
            accepted: 20,
            dealt: true,
            since_first_ms: 60_000,
            ..facts()
        };
        assert_eq!(judge(&f), None);
    }

    #[test]
    fn a_bot_below_the_retreat_line_retreats() {
        let f = Facts {
            hp_pct: 34,
            ..facts()
        };
        assert_eq!(judge(&f), Some(Break::Retreat));
        let f = Facts {
            hp_pct: 35,
            ..facts()
        };
        assert_eq!(judge(&f), None, "the line itself is still fine");
        // 0 switches the retreat off
        let f = Facts {
            hp_pct: 1,
            retreat_pct: 0,
            ..facts()
        };
        assert_eq!(judge(&f), None);
    }

    #[test]
    fn casts_that_hurt_nobody_mean_a_protected_target() {
        let f = Facts {
            accepted: PROTECTED_CASTS,
            since_first_ms: PROTECTED_MIN_MS,
            ..facts()
        };
        assert_eq!(judge(&f), Some(Break::Protected));
        // too few casts, or too soon: the target may just be dodging
        assert_eq!(
            judge(&Facts {
                accepted: PROTECTED_CASTS - 1,
                ..f
            }),
            None
        );
        assert_eq!(
            judge(&Facts {
                since_first_ms: PROTECTED_MIN_MS - 1,
                ..f
            }),
            None
        );
        // a protected target beats the retreat: running would not help
        assert_eq!(judge(&Facts { hp_pct: 5, ..f }), Some(Break::Protected));
    }

    #[test]
    fn odds_compare_levels_hit_points_and_numbers() {
        // even
        assert_eq!(odds((30, 3000), &[], (30, 3000), &[]), Odds::Fair);
        // a slightly stronger target is still taken
        assert_eq!(odds((30, 3000), &[], (31, 3100), &[]), Odds::Fair);
        // much stronger: needs help, and a friend at the side helps
        assert_eq!(odds((30, 3000), &[], (45, 6000), &[]), Odds::NeedsHelp);
        assert_eq!(
            odds((30, 3000), &[(30, 3000), (30, 3000)], (45, 6000), &[]),
            Odds::Fair
        );
        // a carrier with two bodyguards: three to one, a squad of three evens it
        assert_eq!(
            odds((30, 3000), &[], (30, 3000), &[(30, 3000), (30, 3000)]),
            Odds::NeedsHelp
        );
        assert_eq!(
            odds(
                (30, 3000),
                &[],
                (30, 3000),
                &[(30, 3000), (30, 3000), (30, 3000)]
            ),
            Odds::Hopeless
        );
        assert_eq!(
            odds(
                (30, 3000),
                &[(30, 3000)],
                (30, 3000),
                &[(30, 3000), (30, 3000)]
            ),
            Odds::NeedsHelp
        );
        // two of ours against two of theirs
        assert_eq!(
            odds((30, 3000), &[(30, 3000)], (30, 3000), &[(30, 3000)]),
            Odds::Fair
        );
        // a hopeless gap
        assert_eq!(odds((10, 500), &[], (50, 9000), &[]), Odds::Hopeless);
        // no hit points known at all do not divide by zero
        assert_eq!(odds((0, 0), &[], (0, 0), &[]), Odds::Fair);
    }
}
