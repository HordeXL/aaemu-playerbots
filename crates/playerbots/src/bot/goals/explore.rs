//! Explore: see new places: zones the bot has not been to, landmarks, the view from a hill, the
//! coast.
//!
//! The bot picks a destination ([`plan::choose`]) from the world's landmarks
//! ([`aa_world::landmarks`]) and its journal (`seen`: the zones it stood in, the places it saw,
//! the places that did not work out). A zone it has never been to counts most, then a hill or a
//! stretch of coast it has not seen, then recall points and zone middles. A destination must be
//! out of zones at war (`ctx.in_war_zone`), on the bot's part of the navigation mesh or on a
//! portal route, and safe for its level: no monsters `[explore] level_gap` levels above it around
//! the place, nor the quest goal's deadly ones on the way (`quest::progress`).
//!
//! It travels there with `ctx.travel` (in legs; a far trip may go by portal), then looks around
//! for a minute or two: short strolls with pauses, a jump for joy at a new zone or on a hill. The
//! place goes into the journal on arrival (the zone is recorded by the bot task whatever the bot
//! does); the end of the trip, done or not, sets the time of the last trip. A destination it could
//! not reach is avoided for [`AVOID_SECS`].
//!
//! [`score`]: the goal's whole eligibility and weight. 0 while `[explore]` is off, in an instance,
//! hurt, soon after log-in, within the gap after the last trip (`trip_gap_mins`, shorter for an
//! explorer) or when nothing new is within `max_trip_m`. Else it grows with
//! `persona.profile.explorer`, the number of new places and the time since the last trip.
//!
//! Arguments ([`GoalArgs`]): `dest` heads for that point, `zone` for a landmark of that zone (the
//! checks still apply; a zone at war or out of reach fails the goal).

pub mod plan;

use std::fmt::Write;

use pb_web::Activity;

use crate::bot::cmd::GoalArgs;
use crate::bot::ctx::Ctx;
use crate::bot::manner;
use crate::bot::motion::{MotionStatus, MoveFailure, Pace};
use crate::bot::state::dist2d;
use crate::config::BehaviorCfg;
use crate::hub::unix_ms;
use crate::roster::Temperament;
use aa_world::landmarks::LandmarkKind;

use super::{jitter_ms, random_point, walkable, FailReason, GoalStatus};
use plan::{Env, Pick, Query, Why, WorldChecks};

/// No trips this soon after the bot entered the world (ms of the bot clock).
const SETTLE_MS: u64 = 60_000;
/// No trips below this health.
const MIN_HP_PCT: u8 = 70;
/// A trip is given up below this health.
const LEAVE_HP_PCT: u8 = 35;
/// How close counts as there (metres).
const ARRIVE_M: f32 = 6.0;
/// The travel order is re-issued at most this often while walking (ms).
const GO_GAP_MS: u64 = 2_500;
/// A trip the controller cut off after its time limit is continued this many times...
const MAX_LEGS: u8 = 16;
/// ...as long as every leg gets this much closer (metres).
const LEG_MIN_GAIN_M: f32 = 30.0;
/// The time a trip may take: this much plus the distance at [`SLOW_MPS`] (ms)...
const TRIP_BASE_MS: u64 = 120_000;
/// ...metres per second, well below a run, for detours, fights and portals...
const SLOW_MPS: f32 = 2.0;
/// ...and never more than this (ms).
const TRIP_MAX_MS: u64 = 40 * 60_000;
/// Time spent looking around at the place (ms); a hill, the coast and a new zone get half again.
const LOOK_MS: (u64, u64) = (40_000, 100_000);
/// The strolls around the place (metres from it).
const STROLL_M: (f32, f32) = (6.0, 30.0);
/// Pauses between two strolls (ms).
const PAUSE_MS: (u64, u64) = (3_000, 12_000);
/// Strolls that fail in a row before the bot just stands and looks.
const MAX_STROLL_FAILS: u8 = 3;
/// A destination that could not be reached is left alone this long (seconds).
pub const AVOID_SECS: u64 = 6 * 3600;
/// New places counted at most by the score.
const NOVELTY_CAP: u32 = 6;

/// Exploring. See the module docs.
#[derive(Debug, Default)]
pub struct Explore {
    /// What the goal was started with (`None`: the bot's own choice).
    args: Option<Box<GoalArgs>>,
    /// The trip, once planned (boxed: the goal enum stays small).
    run: Option<Box<Trip>>,
    /// Why the start found no trip, returned by the first tick.
    failed: Option<FailReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Going,
    Looking,
}

#[derive(Debug)]
struct Trip {
    pick: Pick,
    label: String,
    phase: Phase,
    deadline_ms: u64,
    walking: bool,
    go_ms: u64,
    legs: u8,
    leg_from: f32,
    look_end_ms: u64,
    pause_until_ms: u64,
    strolling: bool,
    stroll_fails: u8,
}

impl Explore {
    /// A fresh goal (the brain starts it with [`Goal::with_args`](super::Goal::with_args)).
    pub fn new(args: Option<Box<GoalArgs>>) -> Explore {
        Explore {
            args,
            ..Explore::default()
        }
    }

    /// The arguments it was started with.
    pub fn args(&self) -> Option<&GoalArgs> {
        self.args.as_deref()
    }

    /// Called once when the brain picks the goal: plans the trip.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        self.run = None;
        self.failed = None;
        if !ctx.cfg.explore.enabled {
            self.failed = Some(FailReason::Other("[explore] is switched off"));
            return;
        }
        if ctx.state.place.in_instance() {
            self.failed = Some(FailReason::Other("in an instance"));
            return;
        }
        match self.plan(ctx) {
            Ok(pick) => {
                let label = label(ctx, &pick);
                let d = pick.dist;
                ctx.log.say(format!(
                    "explore: heading for {label} ({:.1} km, {})",
                    d / 1000.0,
                    pick.why.name()
                ));
                let budget = TRIP_BASE_MS + (d / SLOW_MPS * 1000.0) as u64;
                self.run = Some(Box::new(Trip {
                    deadline_ms: ctx.now_ms + budget.min(TRIP_MAX_MS),
                    leg_from: d,
                    pick,
                    label,
                    phase: Phase::Going,
                    walking: false,
                    go_ms: 0,
                    legs: 0,
                    look_end_ms: 0,
                    pause_until_ms: 0,
                    strolling: false,
                    stroll_fails: 0,
                }));
            }
            Err(why) => {
                ctx.log
                    .debug(format!("{}: explore: {why}", ctx.persona.name));
                self.failed = Some(FailReason::Other(why));
            }
        }
    }

    /// The destination: the point or zone the bot was sent to, else its own choice.
    fn plan(&self, ctx: &mut Ctx<'_>) -> Result<Pick, &'static str> {
        let me = ctx.pos();
        if let Some([x, y]) = self.args().and_then(|a| a.dest) {
            if ctx.in_war_zone(x, y) {
                return Err("the place it was sent to is at war");
            }
            let z = ctx.world.ground_z(x, y).unwrap_or(me[2]);
            return Ok(Pick {
                kind: None,
                zone_key: ctx.world.zone_key_at(x, y),
                pos: [x, y, z],
                why: Why::Asked,
                dist: dist2d(me, [x, y, 0.0]),
                name: "".into(),
            });
        }
        let zone = self.args().and_then(|a| a.zone);
        let (id, now_unix) = (ctx.persona.id, unix_ms() / 1000);
        let seen = ctx.hub.journal.read(id, |j| j.seen.clone());
        let q = Query {
            me,
            max_trip_m: ctx.cfg.explore.max_trip_m,
            now_unix,
            seen: &seen,
            zone,
        };
        let mut rng = fastrand::Rng::with_seed(ctx.rng.u64(..));
        let world = ctx.world;
        let mut env = CtxEnv {
            checks: WorldChecks {
                world,
                faction: ctx.state.me.faction,
                level: ctx.state.me.level,
                level_gap: ctx.cfg.explore.level_gap,
            },
            ctx,
        };
        plan::choose(world.landmarks(), &q, &mut env, &mut rng).map_err(|e| e.text())
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        if let Some(f) = self.failed.take() {
            return GoalStatus::Failed(f);
        }
        let Some(run) = self.run.as_deref_mut() else {
            return GoalStatus::Failed(FailReason::NoTarget);
        };
        let now = ctx.now_ms;
        if ctx.state.me.hp_pct() < LEAVE_HP_PCT {
            return GoalStatus::Failed(FailReason::Other("too hurt to go on exploring"));
        }
        match run.phase {
            Phase::Going => {
                let [x, y, _] = run.pick.pos;
                if now >= run.deadline_ms {
                    avoid(ctx, x, y);
                    ctx.log
                        .say(format!("explore: gave up on {} (too long)", run.label));
                    return GoalStatus::Failed(FailReason::Timeout);
                }
                if ctx.in_war_zone(x, y) {
                    return GoalStatus::Failed(FailReason::Other("the place is at war now"));
                }
                match walk(run, ctx, x, y) {
                    Walk::Moving => GoalStatus::Running,
                    Walk::Failed => {
                        avoid(ctx, x, y);
                        ctx.log
                            .say(format!("explore: could not get to {}", run.label));
                        GoalStatus::Failed(FailReason::Stuck)
                    }
                    Walk::Arrived => {
                        arrive(run, ctx);
                        GoalStatus::Running
                    }
                }
            }
            Phase::Looking => look(run, ctx),
        }
    }

    /// Called when the goal ends, fails or is interrupted: stops walking and notes the trip.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        if let Some(run) = self.run.take() {
            if run.walking || run.strolling {
                ctx.motion.stop();
            }
            let now_unix = unix_ms() / 1000;
            ctx.hub
                .journal
                .update(ctx.persona.id, |j| j.seen.trip_done(now_unix));
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
        } else if moving && self.run.as_ref().is_none_or(|r| r.phase == Phase::Going) {
            Activity::Traveling
        } else {
            Activity::Idle
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        match self.run.as_deref() {
            Some(r) if r.phase == Phase::Going => {
                let _ = write!(out, "explore: heading for {}", r.label);
            }
            Some(r) => {
                let _ = write!(out, "explore: looking around {}", r.label);
            }
            None => out.push_str("explore: choosing where to go"),
        }
    }
}

/// The checks of [`plan::choose`] with the bot's context: the war zones, the portal routes and
/// the world's danger and mesh checks.
struct CtxEnv<'c, 'a, 'w> {
    checks: WorldChecks<'w>,
    ctx: &'c mut Ctx<'a>,
}

impl Env for CtxEnv<'_, '_, '_> {
    fn at_war(&mut self, x: f32, y: f32) -> bool {
        self.ctx.in_war_zone(x, y)
    }

    fn unreachable(&mut self, from: [f32; 2], to: [f32; 2]) -> bool {
        self.checks.cut_off(from, to) && !crate::bot::travel::has_route(self.ctx, to[0], to[1])
    }

    fn dangerous(&mut self, from: [f32; 2], to: [f32; 2]) -> bool {
        self.checks.dangerous(from, to)
    }
}

/// "the high ground of Two Crowns", "Dewstone", "1234, 567" and the like.
fn label(ctx: &Ctx<'_>, pick: &Pick) -> String {
    let zone = pick.zone_key.map(|k| match ctx.world.zone_name(k) {
        Some(n) => n.to_string(),
        None => format!("zone {k}"),
    });
    let zone = zone.as_deref().unwrap_or("the wilds");
    let mut s = match pick.kind {
        Some(LandmarkKind::Recall) if !pick.name.is_empty() => format!("{} in {zone}", pick.name),
        Some(LandmarkKind::Recall) => format!("a recall point in {zone}"),
        Some(LandmarkKind::Hill) => format!("the high ground of {zone}"),
        Some(LandmarkKind::Coast) => format!("the coast of {zone}"),
        Some(LandmarkKind::Centre) => format!("the heart of {zone}"),
        None => format!("{:.0}, {:.0} in {zone}", pick.pos[0], pick.pos[1]),
    };
    if pick.why == Why::NewZone {
        s.push_str(" (new)");
    }
    s
}

/// Leaves the destination alone for [`AVOID_SECS`].
fn avoid(ctx: &Ctx<'_>, x: f32, y: f32) {
    let now = unix_ms() / 1000;
    ctx.hub.journal.update(ctx.persona.id, |j| {
        j.seen.avoid(x, y, now + AVOID_SECS, now)
    });
}

/// The bot got there: the place goes into the journal and it starts looking around.
fn arrive(run: &mut Trip, ctx: &mut Ctx<'_>) {
    let now = ctx.now_ms;
    let [x, y, _] = run.pick.pos;
    ctx.hub
        .journal
        .update(ctx.persona.id, |j| j.seen.add_place(x, y));
    let special = run.pick.why == Why::NewZone
        || matches!(
            run.pick.kind,
            Some(LandmarkKind::Hill | LandmarkKind::Coast)
        );
    let mut look = jitter_ms(ctx.rng, LOOK_MS.0, LOOK_MS.1);
    if special {
        look += look / 2;
        // (a jump for joy, when the bot jumps at all)
        if let Some(l) = ctx.motion.life_mut() {
            l.sched.cheer();
        }
    }
    run.phase = Phase::Looking;
    run.walking = false;
    run.look_end_ms = now + look;
    run.pause_until_ms = now + jitter_ms(ctx.rng, 1_000, 4_000);
    ctx.log.say(format!("explore: reached {}", run.label));
}

/// Looking around the place: short strolls with pauses until the time is up.
fn look(run: &mut Trip, ctx: &mut Ctx<'_>) -> GoalStatus {
    let now = ctx.now_ms;
    if run.strolling {
        match ctx.motion.status() {
            MotionStatus::Moving => return GoalStatus::Running,
            MotionStatus::Failed(_) => {
                run.strolling = false;
                run.stroll_fails = run.stroll_fails.saturating_add(1);
                run.pause_until_ms = now + 1_000;
            }
            MotionStatus::Arrived | MotionStatus::Idle => {
                run.strolling = false;
                run.stroll_fails = 0;
                run.pause_until_ms = now + jitter_ms(ctx.rng, PAUSE_MS.0, PAUSE_MS.1);
            }
        }
    }
    if now >= run.look_end_ms {
        return GoalStatus::Done;
    }
    if now < run.pause_until_ms || run.stroll_fails >= MAX_STROLL_FAILS {
        manner::wander_pause(ctx);
        return GoalStatus::Running;
    }
    let centre = run.pick.pos;
    match random_point(ctx.world, ctx.rng, centre, STROLL_M.0, STROLL_M.1, 6)
        .filter(|&(x, y)| !ctx.in_war_zone(x, y))
    {
        Some((x, y)) => {
            ctx.motion.go_to(x, y, 1.0, Pace::Walk, now);
            run.strolling = true;
        }
        None => {
            run.stroll_fails = run.stroll_fails.saturating_add(1);
            run.pause_until_ms = now + 1_000;
        }
    }
    GoalStatus::Running
}

enum Walk {
    Moving,
    Arrived,
    Failed,
}

/// Travels to (x, y). A trip the controller cut off after its time limit is continued while
/// every leg gets closer; a far trip may go by portal (see [`Ctx::travel`]).
fn walk(run: &mut Trip, ctx: &mut Ctx<'_>, x: f32, y: f32) -> Walk {
    let now = ctx.now_ms;
    let left = dist2d(ctx.pos(), [x, y, 0.0]);
    if left <= ARRIVE_M + 0.6 {
        if ctx.motion.is_moving() {
            ctx.motion.stop();
        }
        return Walk::Arrived;
    }
    if run.walking {
        match ctx.motion.status() {
            MotionStatus::Moving => {
                ctx.combat.note_travelling(now);
                return Walk::Moving;
            }
            MotionStatus::Failed(MoveFailure::Timeout)
                if run.legs < MAX_LEGS && run.leg_from - left >= LEG_MIN_GAIN_M =>
            {
                run.legs += 1;
                run.leg_from = left;
                run.go_ms = 0;
            }
            MotionStatus::Failed(_) => {
                run.walking = false;
                return Walk::Failed;
            }
            _ => {}
        }
    }
    if run.walking && now.saturating_sub(run.go_ms) < GO_GAP_MS {
        return Walk::Moving;
    }
    ctx.travel(x, y, ARRIVE_M, Pace::Run);
    ctx.combat.note_travelling(now);
    run.walking = true;
    run.go_ms = now;
    Walk::Moving
}

/// The goal's weight before the situation is looked at: 0, [`score`] weighs it from the bot's
/// profile (`brain::base_weights` calls this).
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The weight of a trip for a bot whose `explorer` trait is this (0..=1): a little for a bot that
/// does not care, about half the quest weight for a born explorer.
pub fn weight(explorer: f32) -> f32 {
    let e = explorer.clamp(0.0, 1.0);
    0.05 + 0.45 * e * e
}

/// The gap between two trips (seconds) for a bot whose `explorer` trait is this: the configured
/// `trip_gap_mins` for an average bot, less than half of it for a born explorer, half again for
/// one that does not care.
pub fn trip_gap_secs(trip_gap_mins: u32, explorer: f32) -> u64 {
    let e = explorer.clamp(0.0, 1.0);
    (f64::from(trip_gap_mins) * 60.0 * f64::from(1.5 - e)) as u64
}

/// The score of the goal for this tick, given its `base` weight (unused, see [`base_weight`]). 0
/// keeps the bot from picking it. See the module docs.
pub fn score(ctx: &Ctx<'_>, _base: f32) -> f32 {
    let cfg = &ctx.cfg.explore;
    let me = &ctx.state.me;
    if !cfg.enabled
        || ctx.state.place.in_instance()
        || ctx.now_ms < SETTLE_MS
        || me.dead
        || me.hp_pct() < MIN_HP_PCT
        || !walkable(ctx.world, me.pos[0], me.pos[1])
    {
        return 0.0;
    }
    let lm = ctx.world.landmarks();
    if lm.all().is_empty() {
        return 0.0;
    }
    let explorer = ctx.persona.profile.explorer;
    let gap = trip_gap_secs(cfg.trip_gap_mins, explorer).max(1);
    let now_unix = unix_ms() / 1000;
    let (since, novel) = ctx.hub.journal.read(ctx.persona.id, |j| {
        let since = now_unix.saturating_sub(j.seen.last_trip_unix);
        if since < gap {
            return (since, 0);
        }
        let q = Query {
            me: me.pos,
            max_trip_m: cfg.max_trip_m,
            now_unix,
            seen: &j.seen,
            zone: None,
        };
        (since, plan::novelty(lm, &q, NOVELTY_CAP))
    });
    if since < gap || novel == 0 {
        return 0.0;
    }
    let overdue = ((since - gap) as f32 / gap as f32).min(2.0);
    weight(explorer) * (1.0 + 0.1 * novel as f32) * (1.0 + 0.2 * overdue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_goal_shows_as_idle_or_traveling_and_keeps_its_arguments() {
        let args = GoalArgs {
            zone: Some(3),
            ..GoalArgs::default()
        };
        let g = Explore::new(Some(Box::new(args.clone())));
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        assert_eq!(g.args(), Some(&args));
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("explore"), "{text}");
    }

    #[test]
    fn explorers_go_more_often_and_want_it_more() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
        assert!(weight(1.0) > 2.5 * weight(0.5));
        assert!(weight(0.0) > 0.0 && weight(1.0) <= 0.5);
        assert_eq!(trip_gap_secs(40, 0.5), 2400);
        assert!(trip_gap_secs(40, 1.0) < trip_gap_secs(40, 0.0) / 2);
        assert!(trip_gap_secs(40, 7.0) > 0, "clamped");
    }

    #[test]
    fn the_goal_stays_small() {
        // (the trip is boxed; the goal enum must stay within its budget)
        assert!(std::mem::size_of::<Explore>() <= 48);
    }
}
