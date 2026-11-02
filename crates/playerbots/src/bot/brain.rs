//! The brain: chooses the next goal by utility scoring and runs it.
//!
//! Each goal gets a score: its configured weight, scaled by the bot's temperament (a quester likes
//! quests, a homebody likes housing and resting) and by the situation (rest when hurt, no grinding
//! when hurt or nothing to hit). The next goal is drawn at random in proportion to the scores, so
//! the bots do not all do the same thing at the same time. Goals that report
//! [`FailReason::NotImplemented`] are switched off for good, goals that fail otherwise rest for a
//! while ([`FAIL_COOLDOWN_MS`]).
//!
//! The brain also calls the combat controller on every tick, so a bot defends itself whatever it is
//! doing, and runs the social hook ([`social::tick`]).

use super::cmd::{BotCmd, GoalChoice, GoalKind, PairKind, PairOrder};
use super::combat::Combat;
use super::ctx::Ctx;
use super::goals::{social, FailReason, Goal, GoalStatus};
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

/// A failed goal is not picked again for this long.
pub const FAIL_COOLDOWN_MS: u64 = 60_000;
/// Hit points (percent) below which a hurt bot rests instead of carrying on.
const HURT_PCT: u8 = 30;
/// Pause between two goals.
const GAP_MS: (u64, u64) = (800, 4_000);
/// A forced goal that keeps failing is dropped after this many failures in a row.
const MAX_FORCED_FAILS: u8 = 3;

/// What the scoring looks at.
#[derive(Debug, Clone, Copy)]
pub struct ScoreInput {
    /// Hit points in percent.
    pub hp_pct: u8,
    /// Mana in percent.
    pub mp_pct: u8,
    /// A fair monster is in view or has a known spawn close by.
    pub monsters_near: bool,
    /// The goal that ended last.
    pub last: Option<GoalKind>,
    /// Goals that must not be picked (stubs, cooling down), as a bit per [`GoalKind::index`].
    pub blocked: u16,
}

/// The configured weight of every goal scaled by the temperament, in [`GoalKind::index`] order.
pub fn base_weights(b: &BehaviorCfg, t: &Temperament) -> [f32; GoalKind::COUNT] {
    let mut w = [0.0; GoalKind::COUNT];
    w[GoalKind::Quest.index()] = b.quest * (0.4 + 1.2 * t.quester);
    w[GoalKind::Grind.index()] = b.grind * (0.7 + 0.6 * (1.0 - t.quester));
    w[GoalKind::Duel.index()] = b.duel * (0.2 + 1.6 * t.duelist);
    w[GoalKind::Trade.index()] = b.trade * (0.2 + 1.6 * t.trader);
    w[GoalKind::TradeRun.index()] = b.trade_run * (0.2 + 1.6 * t.merchant);
    w[GoalKind::Housing.index()] = b.housing * (0.4 + 1.2 * t.homebody);
    w[GoalKind::Rest.index()] = b.rest * (0.5 + t.homebody);
    w[GoalKind::Wander.index()] = b.wander * (0.6 + 0.8 * t.homebody);
    w
}

/// The score of every goal. A blocked goal or a weight of 0 scores 0.
pub fn scores(base: &[f32; GoalKind::COUNT], i: &ScoreInput) -> [f32; GoalKind::COUNT] {
    let hp = f32::from(i.hp_pct) / 100.0;
    let mp = f32::from(i.mp_pct) / 100.0;
    let mut s = *base;
    // The more hurt (or drained), the more the bot wants to rest; a healthy bot hardly rests.
    let need = (1.0 - hp).max((0.7 - mp).max(0.0));
    s[GoalKind::Rest.index()] *= if need < 0.05 { 0.1 } else { 1.0 + 6.0 * need };
    if hp < 0.5 {
        for g in [GoalKind::Grind, GoalKind::Quest, GoalKind::TradeRun] {
            s[g.index()] *= 0.1;
        }
    }
    if !i.monsters_near {
        s[GoalKind::Grind.index()] *= 0.5;
    }
    if let Some(last) = i.last {
        s[last.index()] *= 0.4;
    }
    for g in GoalKind::ALL {
        if i.blocked & (1 << g.index()) != 0 || !s[g.index()].is_finite() || s[g.index()] < 0.0 {
            s[g.index()] = 0.0;
        }
    }
    s
}

/// Draws a goal at random in proportion to `scores`; `None` when all are 0.
pub fn choose(scores: &[f32; GoalKind::COUNT], rng: &mut fastrand::Rng) -> Option<GoalKind> {
    let total: f32 = scores.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let mut roll = rng.f32() * total;
    let mut last_nonzero = None;
    for g in GoalKind::ALL {
        let s = scores[g.index()];
        if s <= 0.0 {
            continue;
        }
        last_nonzero = Some(g);
        if roll < s {
            return Some(g);
        }
        roll -= s;
    }
    last_nonzero
}

/// Chooses and runs goals for one bot. See the module docs.
#[derive(Debug)]
pub struct Brain {
    base: [f32; GoalKind::COUNT],
    goal: Option<Goal>,
    choice: GoalChoice,
    last: Option<GoalKind>,
    /// Goals that reported `NotImplemented`.
    disabled: u16,
    /// Bot clock until which a failed goal is not picked.
    cooldown_until: [u64; GoalKind::COUNT],
    next_think_ms: u64,
    /// A goal to start at the next tick (operator command, pairing, recovery after death).
    queued: Option<(GoalKind, Option<PairOrder>)>,
    forced_fails: u8,
}

impl Brain {
    /// A brain with the goal weights of `behavior` scaled by the bot's `temper`.
    pub fn new(behavior: &BehaviorCfg, temper: &Temperament) -> Brain {
        Brain {
            base: base_weights(behavior, temper),
            goal: None,
            choice: GoalChoice::Auto,
            last: None,
            disabled: 0,
            cooldown_until: [0; GoalKind::COUNT],
            next_think_ms: 0,
            queued: None,
            forced_fails: 0,
        }
    }

    /// The running goal.
    pub fn goal(&self) -> Option<&Goal> {
        self.goal.as_ref()
    }

    /// The name of the running goal (`"-"` when none).
    pub fn goal_name(&self) -> &'static str {
        self.goal.as_ref().map_or("-", |g| g.kind().name())
    }

    /// What the operator chose.
    pub fn choice(&self) -> GoalChoice {
        self.choice
    }

    /// Whether goal `kind` is a stub that was found out.
    pub fn is_disabled(&self, kind: GoalKind) -> bool {
        self.disabled & (1 << kind.index()) != 0
    }

    /// Applies an operator or coordinator command. `Pause`, `Resume` and `Say` belong to the bot
    /// task and are ignored here.
    pub fn command(&mut self, cmd: &BotCmd) {
        match cmd {
            BotCmd::Goal(choice) => {
                self.choice = *choice;
                self.forced_fails = 0;
                match choice {
                    GoalChoice::Forced(kind) => self.queued = Some((*kind, None)),
                    GoalChoice::Auto => {}
                }
            }
            BotCmd::Pair(order) => {
                let kind = match order.kind {
                    PairKind::Duel => GoalKind::Duel,
                    PairKind::Trade => GoalKind::Trade,
                };
                self.queued = Some((kind, Some(order.clone())));
            }
            BotCmd::Pause | BotCmd::Resume | BotCmd::Say(_) => {}
            BotCmd::DuelWith(_) | BotCmd::TradeWith(_) => {}
        }
    }

    /// Queues `kind` to start at the next tick, replacing the running goal.
    pub fn queue(&mut self, kind: GoalKind, order: Option<PairOrder>) {
        self.queued = Some((kind, order));
    }

    /// One brain tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) {
        social::tick(ctx);
        if ctx.state.me.dead {
            // Dead bots drop their goal; the bot task asks for the resurrection.
            self.end_goal(ctx, None);
            return;
        }
        if !ctx.state.me.spawned {
            return;
        }
        let combat = Combat::tick(ctx);
        let now = ctx.now_ms;

        if let Some((kind, order)) = self.queued.take() {
            self.start_goal(ctx, kind, order);
        }
        if self.goal.is_none() && now >= self.next_think_ms {
            self.pick(ctx);
        } else if self.hurt_and_idle(ctx) {
            self.start_goal(ctx, GoalKind::Rest, None);
        }
        let Some(goal) = self.goal.as_mut() else {
            return;
        };
        if ctx.combat.busy() && !goal.owns_combat() {
            // The controller is defending the bot; the goal waits.
            return;
        }
        match goal.tick(ctx, combat) {
            GoalStatus::Running => {}
            GoalStatus::Done => {
                self.forced_fails = 0;
                self.end_goal(ctx, None);
            }
            GoalStatus::Failed(why) => self.end_goal(ctx, Some(why)),
        }
    }

    /// A badly hurt bot that is not fighting and not resting yet.
    fn hurt_and_idle(&self, ctx: &Ctx<'_>) -> bool {
        ctx.state.me.hp_pct() < HURT_PCT
            && !ctx.combat.busy()
            && !matches!(self.choice, GoalChoice::Forced(_))
            && self
                .goal
                .as_ref()
                .is_some_and(|g| g.kind() != GoalKind::Rest)
    }

    fn start_goal(&mut self, ctx: &mut Ctx<'_>, kind: GoalKind, order: Option<PairOrder>) {
        if self.goal.is_some() {
            self.end_goal(ctx, None);
        }
        let mut goal = Goal::new(kind, order);
        goal.start(ctx);
        ctx.log.say(format!("goal: {}", kind.name()));
        self.goal = Some(goal);
    }

    fn end_goal(&mut self, ctx: &mut Ctx<'_>, why: Option<FailReason>) {
        let Some(mut goal) = self.goal.take() else {
            return;
        };
        let kind = goal.kind();
        goal.stop(ctx);
        self.last = Some(kind);
        self.next_think_ms = ctx.now_ms + super::goals::jitter_ms(ctx.rng, GAP_MS.0, GAP_MS.1);
        let Some(why) = why else { return };
        match why {
            FailReason::NotImplemented => {
                self.disabled |= 1 << kind.index();
                ctx.log.debug(format!(
                    "goal {} is not implemented yet; switched off",
                    kind.name()
                ));
                if self.choice == GoalChoice::Forced(kind) {
                    ctx.log
                        .say(format!("goal {} is not implemented yet", kind.name()));
                    self.choice = GoalChoice::Auto;
                }
            }
            other => {
                ctx.log
                    .debug(format!("goal {} failed: {other:?}", kind.name()));
                self.cooldown_until[kind.index()] = ctx.now_ms + FAIL_COOLDOWN_MS;
                if self.choice == GoalChoice::Forced(kind) {
                    self.forced_fails += 1;
                    if self.forced_fails >= MAX_FORCED_FAILS {
                        ctx.log
                            .say(format!("goal {} keeps failing; back to auto", kind.name()));
                        self.choice = GoalChoice::Auto;
                    }
                }
            }
        }
    }

    fn pick(&mut self, ctx: &mut Ctx<'_>) {
        let now = ctx.now_ms;
        if let GoalChoice::Forced(kind) = self.choice {
            if !self.is_disabled(kind) && now >= self.cooldown_until[kind.index()] {
                self.start_goal(ctx, kind, None);
                return;
            }
            // The forced goal is cooling down: fill the time with something harmless.
            let filler = if ctx.state.me.hp_pct() < 90 {
                GoalKind::Rest
            } else {
                GoalKind::Wander
            };
            self.start_goal(ctx, filler, None);
            return;
        }
        let mut blocked = self.disabled;
        for g in GoalKind::ALL {
            if now < self.cooldown_until[g.index()] {
                blocked |= 1 << g.index();
            }
        }
        let monsters_near = monsters_near(ctx);
        let input = ScoreInput {
            hp_pct: ctx.state.me.hp_pct(),
            mp_pct: ctx.state.me.mp_pct(),
            monsters_near,
            last: self.last,
            blocked,
        };
        let s = scores(&self.base, &input);
        let kind = choose(&s, ctx.rng).unwrap_or(GoalKind::Wander);
        self.start_goal(ctx, kind, None);
    }
}

/// Whether there is something to hit: a fair monster in view, else a known spawn within 100 m.
fn monsters_near(ctx: &Ctx<'_>) -> bool {
    let me = ctx.state.me.pos;
    let (faction, level) = (ctx.state.me.faction, ctx.state.me.level);
    let in_view = ctx.state.units.values().any(|u| {
        u.dist2d(me) <= 70.0
            && super::combat::attackable(ctx.world, faction, u)
                .is_some_and(|t| t.grade <= 1 && super::goals::level_ok(level, t.level))
    });
    if in_view {
        return true;
    }
    let mut found = false;
    ctx.world.npc_spawns_within(
        aa_world::Vec3::new(me[0], me[1], me[2]),
        100.0,
        |template, _| {
            if found {
                return;
            }
            if let Some(t) = ctx.world.npc(template) {
                found = t.grade <= 1
                    && super::goals::level_ok(level, t.level)
                    && ctx.world.relation(faction, t.faction_id) == aa_world::Relation::Hostile;
            }
        },
    );
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(hp: u8) -> ScoreInput {
        ScoreInput {
            hp_pct: hp,
            mp_pct: 100,
            monsters_near: true,
            last: None,
            blocked: 0,
        }
    }

    fn base() -> [f32; GoalKind::COUNT] {
        base_weights(&BehaviorCfg::default(), &Temperament::NEUTRAL)
    }

    #[test]
    fn config_weights_reach_the_scores() {
        let b = BehaviorCfg {
            quest: 0.0,
            grind: 5.0,
            ..BehaviorCfg::default()
        };
        let w = base_weights(&b, &Temperament::NEUTRAL);
        assert_eq!(w[GoalKind::Quest.index()], 0.0);
        assert!(w[GoalKind::Grind.index()] > w[GoalKind::Wander.index()] * 4.0);
    }

    #[test]
    fn temperament_shifts_the_weights() {
        let b = BehaviorCfg::default();
        let duelist = Temperament {
            duelist: 1.0,
            ..Temperament::NEUTRAL
        };
        let peaceful = Temperament {
            duelist: 0.05,
            ..Temperament::NEUTRAL
        };
        let a = base_weights(&b, &duelist)[GoalKind::Duel.index()];
        let p = base_weights(&b, &peaceful)[GoalKind::Duel.index()];
        assert!(a > p * 4.0, "{a} vs {p}");
    }

    #[test]
    fn a_hurt_bot_rests_and_a_healthy_one_does_not() {
        let b = base();
        let hurt = scores(&b, &input(20));
        let well = scores(&b, &input(100));
        let top = |s: &[f32; GoalKind::COUNT]| {
            GoalKind::ALL
                .into_iter()
                .max_by(|a, b| s[a.index()].total_cmp(&s[b.index()]))
                .unwrap()
        };
        assert_eq!(top(&hurt), GoalKind::Rest);
        assert_ne!(top(&well), GoalKind::Rest);
        assert!(hurt[GoalKind::Grind.index()] < well[GoalKind::Grind.index()] * 0.2);
    }

    #[test]
    fn no_monsters_makes_grinding_less_likely() {
        let b = base();
        let mut i = input(100);
        let with = scores(&b, &i)[GoalKind::Grind.index()];
        i.monsters_near = false;
        let without = scores(&b, &i)[GoalKind::Grind.index()];
        assert!(without < with);
        assert!(without > 0.0);
    }

    #[test]
    fn blocked_goals_and_zero_weights_never_win() {
        let b = BehaviorCfg {
            wander: 0.0,
            ..BehaviorCfg::default()
        };
        let base = base_weights(&b, &Temperament::NEUTRAL);
        let mut i = input(100);
        i.blocked = (1 << GoalKind::Grind.index()) | (1 << GoalKind::Quest.index());
        let s = scores(&base, &i);
        assert_eq!(s[GoalKind::Grind.index()], 0.0);
        assert_eq!(s[GoalKind::Quest.index()], 0.0);
        assert_eq!(s[GoalKind::Wander.index()], 0.0);
        let mut rng = fastrand::Rng::with_seed(7);
        for _ in 0..2000 {
            let g = choose(&s, &mut rng).unwrap();
            assert!(![GoalKind::Grind, GoalKind::Quest, GoalKind::Wander].contains(&g));
        }
        assert_eq!(choose(&[0.0; GoalKind::COUNT], &mut rng), None);
    }

    #[test]
    fn the_same_goal_twice_in_a_row_is_less_likely() {
        let b = base();
        let mut i = input(100);
        let fresh = scores(&b, &i);
        i.last = Some(GoalKind::Wander);
        let after = scores(&b, &i);
        assert!(
            (after[GoalKind::Wander.index()] - fresh[GoalKind::Wander.index()] * 0.4).abs() < 1e-5
        );
        assert_eq!(
            after[GoalKind::Grind.index()],
            fresh[GoalKind::Grind.index()]
        );
    }

    #[test]
    fn the_draw_follows_the_scores() {
        let mut s = [0.0; GoalKind::COUNT];
        s[GoalKind::Grind.index()] = 3.0;
        s[GoalKind::Wander.index()] = 1.0;
        let mut rng = fastrand::Rng::with_seed(42);
        let (mut grind, mut wander) = (0u32, 0u32);
        for _ in 0..10_000 {
            match choose(&s, &mut rng).unwrap() {
                GoalKind::Grind => grind += 1,
                GoalKind::Wander => wander += 1,
                other => panic!("{other:?} has no score"),
            }
        }
        let ratio = f64::from(grind) / f64::from(wander);
        assert!((2.6..3.4).contains(&ratio), "ratio {ratio}");
    }

    #[test]
    fn commands_queue_goals_and_pairings() {
        let mut brain = Brain::new(&BehaviorCfg::default(), &Temperament::NEUTRAL);
        assert_eq!(brain.choice(), GoalChoice::Auto);
        brain.command(&BotCmd::Goal(GoalChoice::Forced(GoalKind::Rest)));
        assert_eq!(brain.choice(), GoalChoice::Forced(GoalKind::Rest));
        assert!(matches!(brain.queued, Some((GoalKind::Rest, None))));
        brain.command(&BotCmd::Goal(GoalChoice::Auto));
        assert_eq!(brain.choice(), GoalChoice::Auto);
        let order = PairOrder {
            kind: PairKind::Trade,
            role: crate::bot::cmd::PairRole::Initiator,
            partner_bot: 2,
            partner_char: 20,
            partner_name: "Other".into(),
            meet_at: (1.0, 2.0),
        };
        brain.command(&BotCmd::Pair(order.clone()));
        match &brain.queued {
            Some((GoalKind::Trade, Some(o))) => assert_eq!(o, &order),
            other => panic!("{other:?}"),
        }
        assert_eq!(brain.goal_name(), "-");
        assert!(!brain.is_disabled(GoalKind::Quest));
    }
}
