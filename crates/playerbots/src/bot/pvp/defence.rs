//! Self-defence against characters: who counts as an attacker.
//!
//! The combat controller ([`Combat`](crate::bot::combat::Combat)) turns on monsters that
//! target the bot or hurt it ([`pick_threat`](crate::bot::combat)); this module is the same for
//! characters, used when `[pvp] enabled` and `defend` are on. A character is an attacker when
//!
//! * it targets the bot within the window (10 s; 4 s and 15 m while a goal travels) and the rules
//!   allow a defence against it ([`Purpose::Defend`]: hostile alliance, not protected by its home
//!   zone, not in `never_names`), or
//! * the bot lost hit points a moment ago, no monster explains it, and such a character stands
//!   within [`NEAR_M`].
//!
//! A character that merely targets a friendly bot (to invite it to a duel or a trade) is no
//! attacker: the rules refuse the friendly and the neutral. A bot in a duel leaves the duel's
//! opponent to the duel code, and a bot in a town peace aura casts nothing.

use crate::bot::ctx::Ctx;
use crate::bot::state::UnitKind;

use super::rules::{in_peace_area, Purpose, Rules, Target};

/// A character that targeted the bot this long ago still counts as an attacker.
const WINDOW_MS: u64 = 10_000;
/// While a goal travels, only this recent targeting counts ...
const TRAVEL_WINDOW_MS: u64 = 4_000;
/// ... and only from this close.
const TRAVEL_M: f32 = 15.0;
/// Hit point loss this recent makes the bot look for an unnamed attacker ...
const DAMAGE_WINDOW_MS: u64 = 3_000;
/// ... among the hostile characters this close.
const NEAR_M: f32 = 10.0;

/// The character to defend against, if any: the nearest attacker the rules allow.
pub fn pick(ctx: &Ctx<'_>, travelling: bool) -> Option<u32> {
    let cfg = &ctx.cfg.pvp;
    if !cfg.enabled || !cfg.defend || ctx.state.me.dead || ctx.combat.duel.in_duel() {
        return None;
    }
    if in_peace_area(&ctx.state.buffs) {
        return None;
    }
    let now = ctx.now_ms;
    let me = ctx.state.me.pos;
    let rules = Rules::new(ctx.world, ctx.cfg, ctx.state.me.faction);
    let (window, max_dist) = if travelling {
        (TRAVEL_WINDOW_MS, TRAVEL_M)
    } else {
        (WINDOW_MS, f32::INFINITY)
    };
    let hurt = ctx.state.damaged_within(DAMAGE_WINDOW_MS);
    // the opponent of a duel that just ended: they hit the bot during the duel, which the
    // defence must not read as an attack (the player would stay in combat for ever)
    let duel_recent = ctx.combat.duel.recent_opponent(now);
    let allowed = |obj: u32| -> Option<f32> {
        let u = ctx.state.units.get(&obj)?;
        if u.kind != UnitKind::Character || u.dead() {
            return None;
        }
        if duel_recent == Some(obj) {
            return None;
        }
        if ctx.state.pvp.as_ref().is_some_and(|p| p.shunned(obj, now)) {
            return None;
        }
        let t = Target::of(u, ctx.hub.is_bot_char(u.id), ctx.state.pack_of(obj));
        rules.check(&t, Purpose::Defend).ok()?;
        Some(u.dist2d(me))
    };
    let mut best: Option<(u32, f32)> = None;
    let consider = |best: &mut Option<(u32, f32)>, obj: u32, d: f32| {
        if best.is_none_or(|b| d < b.1) {
            *best = Some((obj, d));
        }
    };
    for &(obj, at) in &ctx.state.threats {
        if now.saturating_sub(at) > window {
            continue;
        }
        if let Some(d) = allowed(obj) {
            if d <= max_dist {
                consider(&mut best, obj, d);
            }
        }
    }
    if best.is_none() && hurt {
        for (&obj, u) in &ctx.state.units {
            if u.kind != UnitKind::Character {
                continue;
            }
            if let Some(d) = allowed(obj) {
                if d <= NEAR_M {
                    consider(&mut best, obj, d);
                }
            }
        }
    }
    best.map(|b| b.0)
}
