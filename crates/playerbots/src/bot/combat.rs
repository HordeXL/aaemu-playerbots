//! The basic combat controller: engage a target, get into range, face it, cast the archetype's
//! starter skill or the auto attack, and loot what dies.
//!
//! The API is **stable on purpose** so the rotation engine can replace the skill choice later
//! without touching the goals:
//!
//! * [`Combat::engage`] / [`Combat::disengage`] set or clear the target,
//! * [`Combat::tick`] runs one step and returns a [`CombatStatus`],
//! * [`Combat::on_event`] feeds it the server's answers (skill failures, loot windows),
//! * [`Combat::busy`] says whether a fight or a loot job is under way.
//!
//! Everything it decides is in small pure pieces ([`pick_skill`], [`Backoffs`], [`LootJob`],
//! [`arrival_radius`]) with their own tests; [`Combat::tick`] only gathers the facts from the
//! [`Ctx`] and acts on the verdict.

use aa_proto::cs;
use aa_proto::sc::ServerEvent;
use aa_proto::sc_world::WorldEvent;
use aa_world::{NpcTemplate, Relation, World};
use smallvec::SmallVec;

use super::ctx::Ctx;
use super::motion::{MotionStatus, Pace};
use super::state::{dist2d, Unit, UnitKind};

/// Distance within which a corpse can be looted.
pub const LOOT_OPEN_RANGE: f32 = 5.0;
/// How close a bot walks to a corpse before it opens the loot.
const LOOT_WALK_RADIUS: f32 = 2.5;
/// How long a kill waits for the server's "has loot" notice.
const LOOT_AWAIT_MS: u64 = 3000;
/// How long an opened loot window may take.
const LOOT_OPEN_TIMEOUT_MS: u64 = 4000;
/// Global cooldown between two skills.
pub const GCD_MS: u64 = 1000;
/// Time between two auto attacks.
pub const AUTO_ATTACK_GAP_MS: u64 = 1200;
/// A fight against one target ends after this long.
const FIGHT_TIMEOUT_MS: u64 = 120_000;
/// A target further away than this is lost.
const LOST_DISTANCE: f32 = 80.0;
/// Pauses after a refused skill: first, second and later failures.
const BACKOFF_MS: [u64; 3] = [2_000, 8_000, 30_000];
/// Pause after an error message that probably belongs to the last cast.
const SHORT_BACKOFF_MS: u64 = 1_500;
/// An error message this soon after a cast is blamed on it.
const ERROR_BLAME_MS: u64 = 600;
/// A unit that targeted the bot this long ago still counts as a threat.
const THREAT_WINDOW_MS: u64 = 10_000;
/// Hit point loss this recent makes the bot look for an attacker.
const DAMAGE_WINDOW_MS: u64 = 3_000;
/// How far the bot looks for an attacker it cannot name.
const REACT_RANGE: f32 = 12.0;
/// Slack on a skill's range.
const RANGE_SLACK: f32 = 0.25;

/// What one combat step came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatStatus {
    /// No target, nothing to do.
    Idle,
    /// Walking into range of the target.
    Approaching,
    /// In range and attacking.
    Fighting,
    /// The target just died (reported once).
    TargetDead {
        /// Object id of the dead unit.
        obj: u32,
    },
    /// The target left the view, ran too far or the fight took too long.
    Lost,
}

/// A skill the bot could cast right now, as far as the facts go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillCand {
    /// Skill id.
    pub id: u32,
    /// The basic attack (spaced by [`AUTO_ATTACK_GAP_MS`] instead of the global cooldown).
    pub is_auto: bool,
    /// Learned, off cooldown, affordable and not backed off.
    pub ready: bool,
    /// The target is within the skill's range.
    pub in_range: bool,
}

/// Picks the skill to cast: the first ready candidate in range whose timer allows it.
/// `cands` is in order of preference (starter skill, then the auto attack).
pub fn pick_skill(
    cands: &[SkillCand],
    now_ms: u64,
    casting_until_ms: u64,
    gcd_until_ms: u64,
    next_auto_ms: u64,
) -> Option<u32> {
    if now_ms < casting_until_ms {
        return None;
    }
    cands
        .iter()
        .filter(|c| c.ready && c.in_range)
        .find(|c| {
            if c.is_auto {
                now_ms >= next_auto_ms
            } else {
                now_ms >= gcd_until_ms
            }
        })
        .map(|c| c.id)
}

/// How close the bot walks to a target it wants to fight from `preferred` metres.
pub fn arrival_radius(preferred: f32) -> f32 {
    (preferred - 1.0).max(1.0)
}

/// Whether the bot has to move to fight a target `dist` metres away.
pub fn must_approach(dist: f32, preferred: f32) -> bool {
    dist > preferred + 1.0
}

/// Skills the server refused, with growing pauses.
#[derive(Debug, Default, Clone)]
pub struct Backoffs {
    /// `(skill, blocked until ms, failures in a row)`.
    list: SmallVec<[(u32, u64, u8); 4]>,
}

impl Backoffs {
    /// The server refused `skill`: block it for longer each time in a row.
    pub fn fail(&mut self, skill: u32, now_ms: u64) {
        self.block(skill, now_ms, true);
    }

    /// An error that may belong to `skill`: a short pause, no escalation.
    pub fn fail_short(&mut self, skill: u32, now_ms: u64) {
        self.block(skill, now_ms, false);
    }

    fn block(&mut self, skill: u32, now_ms: u64, escalate: bool) {
        if let Some(e) = self.list.iter_mut().find(|e| e.0 == skill) {
            if escalate {
                e.2 = (e.2 + 1).min(BACKOFF_MS.len() as u8);
            }
            let ms = if escalate {
                BACKOFF_MS[usize::from(e.2.saturating_sub(1)).min(BACKOFF_MS.len() - 1)]
            } else {
                SHORT_BACKOFF_MS
            };
            e.1 = e.1.max(now_ms + ms);
            return;
        }
        if self.list.len() >= 4 {
            self.list.remove(0);
        }
        let ms = if escalate {
            BACKOFF_MS[0]
        } else {
            SHORT_BACKOFF_MS
        };
        self.list.push((skill, now_ms + ms, u8::from(escalate)));
    }

    /// The skill worked: forget its failures.
    pub fn succeed(&mut self, skill: u32) {
        self.list.retain(|e| e.0 != skill);
    }

    /// Whether `skill` is paused at `now_ms`.
    pub fn blocked(&self, skill: u32, now_ms: u64) -> bool {
        self.list.iter().any(|e| e.0 == skill && now_ms < e.1)
    }
}

/// Where a loot job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LootStage {
    /// Waiting for the server to say the corpse has loot.
    Await,
    /// Walking to the corpse.
    Walk,
    /// The loot window was asked for.
    Opened,
}

/// What the loot job sees of its corpse.
#[derive(Debug, Clone, Copy)]
pub struct LootView {
    /// The corpse is still in view.
    pub exists: bool,
    /// The server says it has loot.
    pub has_loot: bool,
    /// Distance to the corpse.
    pub dist: f32,
    /// The bot clock.
    pub now_ms: u64,
}

/// What the loot job wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LootAction {
    /// Nothing yet.
    Wait,
    /// Walk to the corpse.
    WalkTo,
    /// Send the open request.
    Open,
    /// The job is over (looted, gone or given up).
    Done,
}

/// Looting one corpse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootJob {
    /// Object id of the corpse.
    pub owner: u32,
    /// Where the job is.
    pub stage: LootStage,
    /// Bot clock at the start of the stage.
    pub since_ms: u64,
}

impl LootJob {
    /// A job for a fresh kill.
    pub fn new(owner: u32, now_ms: u64) -> LootJob {
        LootJob {
            owner,
            stage: LootStage::Await,
            since_ms: now_ms,
        }
    }

    /// One step of the job.
    pub fn step(&mut self, v: &LootView) -> LootAction {
        if !v.exists {
            return LootAction::Done;
        }
        if self.stage == LootStage::Await {
            if v.has_loot {
                self.stage = LootStage::Walk;
                self.since_ms = v.now_ms;
            } else if v.now_ms.saturating_sub(self.since_ms) > LOOT_AWAIT_MS {
                return LootAction::Done;
            } else {
                return LootAction::Wait;
            }
        }
        match self.stage {
            LootStage::Walk => {
                if v.dist > LOOT_OPEN_RANGE - 1.0 {
                    LootAction::WalkTo
                } else {
                    self.stage = LootStage::Opened;
                    self.since_ms = v.now_ms;
                    LootAction::Open
                }
            }
            LootStage::Opened => {
                if !v.has_loot || v.now_ms.saturating_sub(self.since_ms) > LOOT_OPEN_TIMEOUT_MS {
                    LootAction::Done
                } else {
                    LootAction::Wait
                }
            }
            LootStage::Await => LootAction::Wait,
        }
    }
}

/// The combat controller of one bot. See the module docs.
#[derive(Debug)]
pub struct Combat {
    target: Option<u32>,
    announced: bool,
    last_goto: Option<(f32, f32)>,
    engaged_at_ms: Option<u64>,
    status: CombatStatus,
    casting_until_ms: u64,
    gcd_until_ms: u64,
    next_auto_ms: u64,
    last_cast: Option<(u32, u64)>,
    backoffs: Backoffs,
    loot: Option<LootJob>,
    kills: u32,
    casts: u32,
}

impl Default for Combat {
    fn default() -> Self {
        Combat::new()
    }
}

impl Combat {
    /// An idle controller.
    pub fn new() -> Combat {
        Combat {
            target: None,
            announced: false,
            last_goto: None,
            engaged_at_ms: None,
            status: CombatStatus::Idle,
            casting_until_ms: 0,
            gcd_until_ms: 0,
            next_auto_ms: 0,
            last_cast: None,
            backoffs: Backoffs::default(),
            loot: None,
            kills: 0,
            casts: 0,
        }
    }

    /// Fights the unit with object id `target_obj` (a no-op if it is the target already).
    pub fn engage(&mut self, target_obj: u32) {
        if self.target == Some(target_obj) {
            return;
        }
        self.target = Some(target_obj);
        self.announced = false;
        self.last_goto = None;
        self.engaged_at_ms = None;
    }

    /// Stops fighting (a running loot job continues).
    pub fn disengage(&mut self) {
        self.target = None;
        self.announced = false;
        self.last_goto = None;
        self.engaged_at_ms = None;
    }

    /// Forgets everything (death, reconnect).
    pub fn reset(&mut self) {
        let (kills, casts) = (self.kills, self.casts);
        *self = Combat::new();
        self.kills = kills;
        self.casts = casts;
    }

    /// The current target.
    pub fn target(&self) -> Option<u32> {
        self.target
    }

    /// Whether a fight or a loot job is under way.
    pub fn busy(&self) -> bool {
        self.target.is_some() || self.loot.is_some()
    }

    /// The result of the last [`Combat::tick`].
    pub fn status(&self) -> CombatStatus {
        self.status
    }

    /// Kills so far (this process).
    pub fn kills(&self) -> u32 {
        self.kills
    }

    /// Skills cast so far (this process).
    pub fn casts(&self) -> u32 {
        self.casts
    }

    /// The loot job, if any.
    pub fn loot_job(&self) -> Option<&LootJob> {
        self.loot.as_ref()
    }

    /// Feeds the controller the server's answers: skill failures and the loot window.
    pub fn on_event(&mut self, ev: &ServerEvent, me_obj: u32, now_ms: u64, out: &mut Vec<cs::Out>) {
        let ServerEvent::World(w) = ev else { return };
        match w {
            WorldEvent::SkillStarted {
                caster_obj,
                skill,
                result,
                ..
            } if *caster_obj == me_obj && me_obj != 0 => {
                if *result != 0 {
                    self.backoffs.fail(*skill, now_ms);
                } else {
                    self.backoffs.succeed(*skill);
                }
            }
            WorldEvent::ErrorMsg { .. } => {
                if let Some((skill, at)) = self.last_cast {
                    if now_ms.saturating_sub(at) <= ERROR_BLAME_MS {
                        self.backoffs.fail_short(skill, now_ms);
                    }
                }
            }
            WorldEvent::CastingStopped { .. } => self.casting_until_ms = 0,
            WorldEvent::LootBag {
                owner_obj,
                owner_type,
                items,
                ..
            } if self.loot.is_some_and(|j| j.owner == *owner_obj) => {
                for it in items {
                    out.push(cs::loot_item(it.index, *owner_type, *owner_obj));
                }
                out.push(cs::loot_close_bag(0, *owner_type, *owner_obj));
            }
            _ => {}
        }
    }

    /// One combat step. See the module docs.
    pub fn tick(ctx: &mut Ctx<'_>) -> CombatStatus {
        let status = if ctx.state.me.dead || ctx.state.me.obj == 0 {
            ctx.combat.reset();
            CombatStatus::Idle
        } else {
            Self::step(ctx)
        };
        ctx.combat.status = status;
        status
    }

    fn step(ctx: &mut Ctx<'_>) -> CombatStatus {
        let now = ctx.now_ms;
        if ctx.combat.target.is_none() {
            if let Some(obj) = pick_threat(ctx) {
                ctx.log.debug(format!("defending against unit {obj}"));
                ctx.combat.engage(obj);
            }
        }
        let Some(obj) = ctx.combat.target else {
            Self::loot_step(ctx);
            return CombatStatus::Idle;
        };
        let engaged_at = *ctx.combat.engaged_at_ms.get_or_insert(now);

        let Some((upos, dead)) = ctx.state.units.get(&obj).map(|u| (u.pos, u.dead())) else {
            ctx.combat.disengage();
            return CombatStatus::Lost;
        };
        if dead {
            let ours = ctx.state.kills.iter().any(|k| k.0 == obj);
            ctx.combat.disengage();
            ctx.combat.kills += 1;
            if ours {
                ctx.combat.loot = Some(LootJob::new(obj, now));
            }
            return CombatStatus::TargetDead { obj };
        }

        let me = ctx.state.me.pos;
        let dist = dist2d(me, upos);
        if dist > LOST_DISTANCE || now.saturating_sub(engaged_at) > FIGHT_TIMEOUT_MS {
            ctx.combat.disengage();
            return CombatStatus::Lost;
        }
        let preferred = ctx.persona.archetype.preferred_range;

        if must_approach(dist, preferred) {
            if ctx.combat.last_goto.is_some()
                && matches!(ctx.motion.status(), MotionStatus::Failed(_))
            {
                // The way to the target is blocked: give up on it.
                ctx.combat.disengage();
                ctx.motion.stop();
                return CombatStatus::Lost;
            }
            let moved_target = ctx
                .combat
                .last_goto
                .is_none_or(|g| dist2d([g.0, g.1, 0.0], [upos[0], upos[1], 0.0]) > 2.0);
            if moved_target || !ctx.motion.is_moving() {
                ctx.motion
                    .go_to(upos[0], upos[1], arrival_radius(preferred), Pace::Run, now);
                ctx.combat.last_goto = Some((upos[0], upos[1]));
            }
            return CombatStatus::Approaching;
        }

        if ctx.motion.is_moving() {
            ctx.motion.stop();
            ctx.combat.last_goto = None;
        }
        ctx.motion.face(upos[0], upos[1]);
        if !ctx.combat.announced {
            ctx.out.push(cs::change_target(obj));
            ctx.combat.announced = true;
        }
        Self::cast(ctx, obj, dist, preferred);
        CombatStatus::Fighting
    }

    /// Casts the starter skill or the auto attack if the timers and the facts allow it.
    fn cast(ctx: &mut Ctx<'_>, target: u32, dist: f32, preferred: f32) {
        let now = ctx.now_ms;
        let arch = &ctx.persona.archetype;
        let auto = arch.auto_attack_skill();
        let cands = [
            Self::candidate(ctx, arch.starter_skill, false, dist, preferred),
            Self::candidate(ctx, auto, true, dist, preferred),
        ];
        let c = &mut *ctx.combat;
        let Some(skill) = pick_skill(
            &cands,
            now,
            c.casting_until_ms,
            c.gcd_until_ms,
            c.next_auto_ms,
        ) else {
            return;
        };
        let is_auto = skill == auto;
        let (cooldown, cast_time) = ctx.world.skill(skill).map_or((0, 0), |s| {
            (u64::from(s.cooldown_time), u64::from(s.casting_time))
        });
        c.casting_until_ms = now + cast_time + 100;
        if is_auto {
            c.next_auto_ms = now + AUTO_ATTACK_GAP_MS.max(cooldown);
        } else {
            c.gcd_until_ms = now + GCD_MS;
        }
        c.last_cast = Some((skill, now));
        c.casts += 1;
        ctx.state.set_cooldown(skill, now + cooldown);
        let me_obj = ctx.state.me.obj;
        ctx.out.push(cs::start_skill_unit(skill, me_obj, target));
    }

    fn candidate(ctx: &Ctx<'_>, skill: u32, is_auto: bool, dist: f32, preferred: f32) -> SkillCand {
        let now = ctx.now_ms;
        let info = ctx.world.skill(skill);
        let known =
            is_auto || ctx.state.skills.is_empty() || ctx.state.skill_level(skill).is_some();
        let mana_ok = info.is_none_or(|i| i64::from(i.mana_cost) <= i64::from(ctx.state.me.mp));
        let max_range = info
            .map(|i| f32::from(i.max_range))
            .filter(|r| *r > 0.0)
            .unwrap_or(preferred + 1.0)
            .min(preferred + 1.5);
        let min_range = info.map_or(0.0, |i| f32::from(i.min_range));
        SkillCand {
            id: skill,
            is_auto,
            ready: known
                && mana_ok
                && !ctx.combat.backoffs.blocked(skill, now)
                && ctx.state.cooldown_ready(skill, now),
            in_range: dist <= max_range + RANGE_SLACK && dist >= min_range,
        }
    }

    /// Runs the loot job; true while it is still going.
    fn loot_step(ctx: &mut Ctx<'_>) -> bool {
        let Some(mut job) = ctx.combat.loot else {
            return false;
        };
        let now = ctx.now_ms;
        let view = match ctx.state.units.get(&job.owner) {
            Some(u) => (
                LootView {
                    exists: true,
                    has_loot: u.has_loot(),
                    dist: dist2d(ctx.state.me.pos, u.pos),
                    now_ms: now,
                },
                u.pos,
            ),
            None => (
                LootView {
                    exists: false,
                    has_loot: false,
                    dist: 0.0,
                    now_ms: now,
                },
                [0.0; 3],
            ),
        };
        let action = job.step(&view.0);
        match action {
            LootAction::Wait => {}
            LootAction::WalkTo => {
                if !ctx.motion.is_moving() {
                    ctx.motion
                        .go_to(view.1[0], view.1[1], LOOT_WALK_RADIUS, Pace::Run, now);
                }
            }
            LootAction::Open => {
                ctx.motion.stop();
                ctx.out.push(cs::loot_open_bag(job.owner, true));
            }
            LootAction::Done => {
                ctx.combat.loot = None;
                return false;
            }
        }
        ctx.combat.loot = Some(job);
        true
    }
}

/// Whether a unit is a monster the bot may attack: an NPC the bot's faction is hostile to that no
/// player owns. Returns its template.
pub fn attackable<'w>(world: &'w World, own_faction: u32, unit: &Unit) -> Option<&'w NpcTemplate> {
    if unit.kind != UnitKind::Npc || unit.dead() || unit.owned() {
        return None;
    }
    let template = world.npc(unit.id)?;
    let faction = if unit.faction != 0 {
        unit.faction
    } else {
        template.faction_id
    };
    (world.relation(own_faction, faction) == Relation::Hostile).then_some(template)
}

/// The unit to defend against: one that targets the bot, else (when the bot is losing hit
/// points) the nearest monster close by.
fn pick_threat(ctx: &Ctx<'_>) -> Option<u32> {
    let me = ctx.state.me.pos;
    let now = ctx.now_ms;
    let mut best: Option<(u32, f32)> = None;
    for &(obj, at) in &ctx.state.threats {
        if now.saturating_sub(at) > THREAT_WINDOW_MS {
            continue;
        }
        let Some(u) = ctx.state.units.get(&obj) else {
            continue;
        };
        if u.dead() || u.kind != UnitKind::Npc {
            continue;
        }
        let d = u.dist2d(me);
        if best.is_none_or(|b| d < b.1) {
            best = Some((obj, d));
        }
    }
    if let Some((obj, _)) = best {
        return Some(obj);
    }
    if !ctx.state.damaged_within(DAMAGE_WINDOW_MS) {
        return None;
    }
    let mut near: Option<(u32, f32)> = None;
    for (&obj, u) in &ctx.state.units {
        let d = u.dist2d(me);
        if d > REACT_RANGE || attackable(ctx.world, ctx.state.me.faction, u).is_none() {
            continue;
        }
        if near.is_none_or(|b| d < b.1) {
            near = Some((obj, d));
        }
    }
    near.map(|n| n.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::opcodes::cs as op;
    use aa_proto::sc_world::LootItem;

    fn cand(id: u32, is_auto: bool, ready: bool, in_range: bool) -> SkillCand {
        SkillCand {
            id,
            is_auto,
            ready,
            in_range,
        }
    }

    #[test]
    fn the_starter_skill_goes_first_then_the_auto_attack() {
        let both = [cand(18132, false, true, true), cand(2, true, true, true)];
        assert_eq!(pick_skill(&both, 5000, 0, 0, 0), Some(18132));
        // global cooldown running: the auto attack fills in
        assert_eq!(pick_skill(&both, 5000, 0, 5500, 0), Some(2));
        // both timers running
        assert_eq!(pick_skill(&both, 5000, 0, 5500, 6000), None);
        // still casting
        assert_eq!(pick_skill(&both, 5000, 5100, 0, 0), None);
        // casting just ended
        assert_eq!(pick_skill(&both, 5100, 5100, 0, 0), Some(18132));
    }

    #[test]
    fn unready_or_out_of_range_skills_are_skipped() {
        let c = [cand(1, false, false, true), cand(2, true, true, false)];
        assert_eq!(pick_skill(&c, 0, 0, 0, 0), None);
        let c = [cand(1, false, false, true), cand(2, true, true, true)];
        assert_eq!(pick_skill(&c, 0, 0, 0, 0), Some(2));
        assert_eq!(pick_skill(&[], 0, 0, 0, 0), None);
    }

    #[test]
    fn refused_skills_back_off_longer_each_time() {
        let mut b = Backoffs::default();
        assert!(!b.blocked(7, 0));
        b.fail(7, 1000);
        assert!(b.blocked(7, 2999) && !b.blocked(7, 3000));
        b.fail(7, 3000);
        assert!(b.blocked(7, 10_999) && !b.blocked(7, 11_000));
        b.fail(7, 11_000);
        b.fail(7, 41_000);
        assert!(
            b.blocked(7, 70_000) && !b.blocked(7, 71_000),
            "capped at the last step"
        );
        b.succeed(7);
        assert!(!b.blocked(7, 0));
        // a short pause does not escalate
        b.fail_short(8, 0);
        assert!(b.blocked(8, 1499) && !b.blocked(8, 1500));
        b.fail_short(8, 2000);
        assert!(!b.blocked(8, 3500));
        // bounded
        for s in 0..20 {
            b.fail(100 + s, 0);
        }
        assert!(b.list.len() <= 4);
    }

    #[test]
    fn range_helpers() {
        assert!(!must_approach(4.0, 3.0));
        assert!(must_approach(4.1, 3.0));
        assert!(!must_approach(19.0, 18.0));
        assert!(must_approach(19.5, 18.0));
        assert_eq!(arrival_radius(3.0), 2.0);
        assert_eq!(arrival_radius(18.0), 17.0);
        assert_eq!(arrival_radius(1.5), 1.0);
    }

    fn view(exists: bool, has_loot: bool, dist: f32, now_ms: u64) -> LootView {
        LootView {
            exists,
            has_loot,
            dist,
            now_ms,
        }
    }

    #[test]
    fn a_loot_job_waits_walks_opens_and_finishes() {
        let mut j = LootJob::new(9, 1000);
        assert_eq!(j.step(&view(true, false, 2.0, 1500)), LootAction::Wait);
        // the notice arrives while the corpse is 12 m away
        assert_eq!(j.step(&view(true, true, 12.0, 2000)), LootAction::WalkTo);
        assert_eq!(j.stage, LootStage::Walk);
        assert_eq!(j.step(&view(true, true, 6.0, 2500)), LootAction::WalkTo);
        assert_eq!(j.step(&view(true, true, 3.5, 3000)), LootAction::Open);
        assert_eq!(j.stage, LootStage::Opened);
        assert_eq!(j.step(&view(true, true, 2.5, 3500)), LootAction::Wait);
        // everything taken: the server clears the flag
        assert_eq!(j.step(&view(true, false, 2.5, 3600)), LootAction::Done);
    }

    #[test]
    fn a_loot_job_gives_up_when_nothing_happens() {
        let mut j = LootJob::new(9, 0);
        assert_eq!(
            j.step(&view(true, false, 1.0, 3001)),
            LootAction::Done,
            "no notice"
        );
        let mut j = LootJob::new(9, 0);
        assert_eq!(
            j.step(&view(false, true, 1.0, 10)),
            LootAction::Done,
            "corpse gone"
        );
        let mut j = LootJob::new(9, 0);
        j.step(&view(true, true, 1.0, 100));
        assert_eq!(j.stage, LootStage::Opened);
        assert_eq!(
            j.step(&view(true, true, 1.0, 4101)),
            LootAction::Done,
            "window timeout"
        );
        // looting right next to the corpse opens at once
        let mut j = LootJob::new(9, 0);
        assert_eq!(j.step(&view(true, true, 1.0, 0)), LootAction::Open);
    }

    #[test]
    fn failed_skill_starts_pause_that_skill_only_for_the_bot() {
        let mut c = Combat::new();
        let mut out = Vec::new();
        let started = |caster: u32, result: u8| {
            ServerEvent::World(WorldEvent::SkillStarted {
                caster_obj: caster,
                skill: 18132,
                tl: 1,
                target_obj: 5,
                result,
            })
        };
        c.on_event(&started(0x500, 3), 0x400, 1000, &mut out);
        assert!(!c.backoffs.blocked(18132, 1000), "someone else's skill");
        c.on_event(&started(0x400, 3), 0x400, 1000, &mut out);
        assert!(c.backoffs.blocked(18132, 1500));
        c.on_event(&started(0x400, 0), 0x400, 1600, &mut out);
        assert!(!c.backoffs.blocked(18132, 1600));
        assert!(out.is_empty());
    }

    #[test]
    fn an_error_right_after_a_cast_is_blamed_on_the_cast() {
        let mut c = Combat::new();
        let mut out = Vec::new();
        let err = ServerEvent::World(WorldEvent::ErrorMsg {
            err: 1,
            kind: 0,
            notify: false,
        });
        c.on_event(&err, 0x400, 1000, &mut out);
        c.last_cast = Some((18132, 5000));
        c.on_event(&err, 0x400, 5700, &mut out); // too late to be the cast's
        assert!(!c.backoffs.blocked(18132, 5700));
        c.on_event(&err, 0x400, 5500, &mut out);
        assert!(c.backoffs.blocked(18132, 5600));
    }

    #[test]
    fn the_loot_window_is_emptied_and_closed() {
        let mut c = Combat::new();
        let mut out = Vec::new();
        let bag = |owner: u32| {
            ServerEvent::World(WorldEvent::LootBag {
                owner_obj: owner,
                owner_type: 1,
                items: [
                    LootItem {
                        index: 3,
                        template: 500,
                        count: 1,
                    },
                    LootItem {
                        index: 4,
                        template: 501,
                        count: 2,
                    },
                ]
                .into_iter()
                .collect(),
                loot_all: true,
            })
        };
        // no loot job: ignore
        c.on_event(&bag(9), 0x400, 0, &mut out);
        assert!(out.is_empty());
        c.loot = Some(LootJob::new(9, 0));
        c.on_event(&bag(8), 0x400, 0, &mut out);
        assert!(out.is_empty(), "another corpse");
        c.on_event(&bag(9), 0x400, 0, &mut out);
        let ops: Vec<u16> = out.iter().map(|o| o.opcode).collect();
        assert_eq!(
            ops,
            [
                op::CSLootItemPacket,
                op::CSLootItemPacket,
                op::CSLootCloseBagPacket
            ]
        );
        // first item: index 3, owner type 1, owner 9
        assert_eq!(out[0].body[..4], [3, 0, 1, 0]);
    }

    #[test]
    fn engage_and_disengage_and_reset() {
        let mut c = Combat::new();
        assert!(!c.busy());
        c.engage(5);
        assert!(c.busy() && c.target() == Some(5));
        c.announced = true;
        c.engage(5);
        assert!(c.announced, "re-engaging the same target keeps its state");
        c.engage(6);
        assert!(!c.announced);
        c.disengage();
        assert!(!c.busy());
        c.loot = Some(LootJob::new(1, 0));
        assert!(c.busy(), "a loot job keeps the controller busy");
        c.kills = 3;
        c.reset();
        assert!(!c.busy());
        assert_eq!(c.kills(), 3);
    }
}
