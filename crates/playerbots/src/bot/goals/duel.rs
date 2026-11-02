//! Duels between bots.
//!
//! Two pieces:
//!
//! * [`DuelWatch`] lives in the combat controller of every bot. It collects the server's duel
//!   packets (a challenge, the countdown, the start, the end, the refusal errors), answers
//!   challenges (accept the partner the goal expects or, when the bot is idle and duels are
//!   switched on, anyone; decline otherwise) and keeps the controller engaged with the opponent
//!   while a duel runs, whatever goal is active.
//! * [`Duel`] is the goal. It asks the coordinator for a partner (or takes the order the
//!   coordinator, or a web `DuelWith`, queued), walks to the meeting point, lets the
//!   `Initiator` send `CSChallengeDuel` (the `Responder` accepts through the watch after a short
//!   random delay), follows the fight until `SCDuelEnded`, records the result in the bot log and
//!   rests to full afterwards. While it runs the partner is shown in the snapshot and a duel
//!   flag marker is on the map.
//!
//! Server rules the goal works around (`systems/social/duel.rs`): a challenge expires after 30 s
//! (the challenger gets error 507 `TargetRejectedDuel`), a bot cannot be in two duels (504 bad
//! target, 505/506 already in a duel, 513 self), the duel starts 3 s after the acceptance and ends
//! when a side is down to 1 hit point, after five minutes, or when a side leaves.

use std::fmt::Write;

use aa_proto::cs;
use aa_proto::sc::ServerEvent;
use aa_proto::sc_activity::{duel_det, ActivityEvent};
use aa_proto::sc_world::WorldEvent;
use pb_web::Activity;

use crate::bot::cmd::{BotCmd, GoalKind, PairKind, PairOrder, PairRole};
use crate::bot::ctx::Ctx;
use crate::bot::motion::{MotionStatus, Pace};
use crate::bot::state::{dist2d, UnitKind};

use super::{FailReason, GoalStatus};

/// `ErrorMessageType` ids of duel refusals (`systems/social/util.rs`).
pub const ERR_FIRST: i16 = 504;
/// Last duel error id (513, `BadDuelSelf`).
pub const ERR_LAST: i16 = 513;
/// `TargetRejectedDuel`: the challenged bot declined or let the challenge expire.
pub const ERR_REJECTED: i16 = 507;
/// A challenge older than this is dropped (the server expires it after 30 s).
const CHALLENGE_TTL_MS: u64 = 28_000;
/// Delay before an accept, so the answer does not come in the same instant.
const ACCEPT_DELAY_MS: (u64, u64) = (500, 2_000);
/// A bot with fewer hit points (percent) than this declines unexpected challenges and does not
/// look for a duel.
const MIN_HP_PCT: u8 = 60;
/// Waiting for the coordinator to find a partner.
const PAIR_WAIT_MS: u64 = 60_000;
/// A pairing request that has not been answered is repeated after this long.
const REREQUEST_MS: u64 = 20_000;
/// Meeting point reached, partner within this many metres: the challenge goes out.
const CHALLENGE_DIST: f32 = 12.0;
/// The initiator stops this far from its partner.
const MEET_RADIUS: f32 = 6.0;
/// The responder stands still this far from the meeting point.
const WAIT_RADIUS: f32 = 3.0;
/// Time the partner gets to receive its order before the challenge goes out.
const SETTLE_MS: u64 = 1_500;
/// Longest walk to the meeting point.
const MEET_TIMEOUT_MS: u64 = 90_000;
/// How long the challenged bot has (the server expires the challenge after 30 s).
const CHALLENGE_TIMEOUT_MS: u64 = 35_000;
/// Countdown (3 s on the server) plus slack.
const COUNTDOWN_TIMEOUT_MS: u64 = 10_000;
/// The server ends a duel after five minutes.
const FIGHT_TIMEOUT_MS: u64 = 330_000;
/// The opponent may be out of sight this long during a fight.
const OPPONENT_GONE_MS: u64 = 10_000;
/// Rest after the duel: at least this long, at most [`RECOVER_MAX_MS`].
const RECOVER_MIN_MS: u64 = 5_000;
const RECOVER_MAX_MS: u64 = 60_000;
/// Recovered: hit points and mana in percent.
const HP_OK: u8 = 95;
const MP_OK: u8 = 80;

// ---- the watch ---------------------------------------------------------------------------------

/// How a duel ended for this bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The bot won.
    Won,
    /// The bot lost (or fled beyond the flag).
    Lost,
    /// Nobody won (time out, a side left).
    Draw,
}

impl Outcome {
    /// Word for the log.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Won => "won",
            Outcome::Lost => "lost",
            Outcome::Draw => "draw",
        }
    }
}

/// The end of a duel as the server reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuelEnd {
    /// Result for this bot.
    pub outcome: Outcome,
    /// `duel_det` value of the packet.
    pub det: u8,
    /// Bot clock when it arrived.
    pub at_ms: u64,
}

/// Where a duel stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DuelPhase {
    /// No duel.
    #[default]
    Idle,
    /// Accepted; the 3 s countdown runs.
    Countdown {
        /// Bot clock at the start of the countdown.
        since_ms: u64,
    },
    /// The duel is on.
    Active {
        /// Object id of the opponent.
        opponent: u32,
        /// Bot clock at the start.
        since_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    challenger: u32,
    at_ms: u64,
    due_ms: u64,
}

/// What to do with a pending challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Not yet.
    Wait,
    /// Send the acceptance for this challenger.
    Accept(u32),
    /// Send the refusal for this challenger.
    Decline(u32),
    /// The challenge is stale; forget it.
    Drop,
}

/// Collects duel packets for one bot. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct DuelWatch {
    pending: Option<Pending>,
    expect: Option<(u32, u64)>,
    phase: DuelPhase,
    ended: Option<DuelEnd>,
    error: Option<(i16, u64)>,
    attended: bool,
    started: u16,
    finished: u16,
}

impl DuelWatch {
    /// Feeds one server event.
    pub fn on_event(&mut self, ev: &ServerEvent, me_obj: u32, now_ms: u64) {
        match ev {
            ServerEvent::Activity(a) => self.on_activity(a, me_obj, now_ms),
            ServerEvent::World(WorldEvent::ErrorMsg { err, .. })
                if (ERR_FIRST..=ERR_LAST).contains(err) =>
            {
                self.error = Some((*err, now_ms));
            }
            _ => {}
        }
    }

    fn on_activity(&mut self, a: &ActivityEvent, me_obj: u32, now_ms: u64) {
        match a {
            ActivityEvent::DuelChallenged { challenger_char_id } => {
                self.pending = Some(Pending {
                    challenger: *challenger_char_id,
                    at_ms: now_ms,
                    due_ms: 0,
                });
            }
            ActivityEvent::DuelStartCountdown => {
                self.phase = DuelPhase::Countdown { since_ms: now_ms };
            }
            ActivityEvent::DuelStarted {
                challenger_obj,
                challenged_obj,
            } if me_obj != 0 && (*challenger_obj == me_obj || *challenged_obj == me_obj) => {
                let opponent = if *challenger_obj == me_obj {
                    *challenged_obj
                } else {
                    *challenger_obj
                };
                self.phase = DuelPhase::Active {
                    opponent,
                    since_ms: now_ms,
                };
                self.pending = None;
                self.started = self.started.saturating_add(1);
            }
            ActivityEvent::DuelEnded {
                winner_obj,
                loser_obj,
                det,
                ..
            } if me_obj != 0 => {
                let mine = *winner_obj == me_obj || *loser_obj == me_obj;
                if !mine && *det != duel_det::DRAW {
                    return;
                }
                let outcome = if *det == duel_det::DRAW {
                    Outcome::Draw
                } else if *winner_obj == me_obj {
                    Outcome::Won
                } else {
                    Outcome::Lost
                };
                self.ended = Some(DuelEnd {
                    outcome,
                    det: *det,
                    at_ms: now_ms,
                });
                self.phase = DuelPhase::Idle;
                self.pending = None;
                self.finished = self.finished.saturating_add(1);
            }
            _ => {}
        }
    }

    /// The goal expects a challenge from character `partner_char` (0 = from anyone) until
    /// `until_ms`.
    pub fn expect(&mut self, partner_char: u32, until_ms: u64) {
        self.expect = Some((partner_char, until_ms));
    }

    /// The goal no longer expects anything.
    pub fn clear_expect(&mut self) {
        self.expect = None;
    }

    /// Marks whether a duel goal is running (it reads the end of the duel itself).
    pub fn set_attended(&mut self, on: bool) {
        self.attended = on;
    }

    /// Whether a duel goal is running.
    pub fn attended(&self) -> bool {
        self.attended
    }

    /// Where the duel stands.
    pub fn phase(&self) -> DuelPhase {
        self.phase
    }

    /// Whether a duel is being fought or about to start.
    pub fn in_duel(&self) -> bool {
        self.phase != DuelPhase::Idle
    }

    /// Object id of the opponent while the duel is on.
    pub fn opponent(&self) -> Option<u32> {
        match self.phase {
            DuelPhase::Active { opponent, .. } => Some(opponent),
            _ => None,
        }
    }

    /// Duels started and ended since the bot connected.
    pub fn counts(&self) -> (u16, u16) {
        (self.started, self.finished)
    }

    /// Takes the end of the last duel (once).
    pub fn take_end(&mut self) -> Option<DuelEnd> {
        self.ended.take()
    }

    /// Looks at the end of the last duel without taking it.
    pub fn peek_end(&self) -> Option<DuelEnd> {
        self.ended
    }

    /// Takes the last duel refusal `(error id, bot clock)` (once).
    pub fn take_error(&mut self) -> Option<(i16, u64)> {
        self.error.take()
    }

    /// Whether a challenge waits for an answer.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Forgets a stale error, end and challenge (the goal starts clean).
    pub fn clear_notes(&mut self) {
        self.error = None;
        self.ended = None;
    }

    /// Decides what to do with the pending challenge. `idle` says the bot may take an
    /// unexpected one (not fighting, healthy, duels switched on).
    pub fn answer(&mut self, now_ms: u64, idle: bool, rng: &mut fastrand::Rng) -> Answer {
        let Some(mut p) = self.pending else {
            return Answer::Wait;
        };
        if self.in_duel() || now_ms.saturating_sub(p.at_ms) > CHALLENGE_TTL_MS {
            self.pending = None;
            return Answer::Drop;
        }
        let expected = self
            .expect
            .is_some_and(|(c, until)| now_ms < until && (c == 0 || c == p.challenger));
        if !expected && !idle {
            self.pending = None;
            return Answer::Decline(p.challenger);
        }
        if p.due_ms == 0 {
            p.due_ms = now_ms + super::jitter_ms(rng, ACCEPT_DELAY_MS.0, ACCEPT_DELAY_MS.1);
            self.pending = Some(p);
        }
        if now_ms < p.due_ms {
            return Answer::Wait;
        }
        self.pending = None;
        Answer::Accept(p.challenger)
    }
}

// ---- the goal ----------------------------------------------------------------------------------

/// A phase step: `Ok` keeps the goal running, `Err` ends it with that status.
type Step = Result<(), GoalStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the coordinator to name a partner.
    Pairing,
    /// Walking to the partner / the meeting point.
    Meeting,
    /// The challenge is out.
    Challenging,
    /// Accepted; the countdown runs.
    Countdown,
    /// The duel is on.
    Fighting,
    /// Resting after the duel.
    Recover,
}

/// What the partner looks like from here.
#[derive(Debug, Clone, Copy)]
struct PartnerView {
    obj: u32,
    pos: [f32; 3],
    char_id: u32,
    dead: bool,
}

/// Duel another bot. See the module docs.
#[derive(Debug, Clone)]
pub struct Duel {
    order: Option<PairOrder>,
    phase: Phase,
    started_ms: u64,
    phase_ms: u64,
    last_seen_ms: u64,
    walk_to: Option<(f32, f32)>,
    notified: bool,
    flag_key: u32,
    result: Option<(Outcome, u64)>,
    partner_char: u32,
    next_request_ms: u64,
}

impl Duel {
    /// A goal for the pairing `order` (`None` when picked by the brain on its own).
    pub fn new(order: Option<PairOrder>) -> Duel {
        Duel {
            order,
            phase: Phase::Pairing,
            started_ms: 0,
            phase_ms: 0,
            last_seen_ms: 0,
            walk_to: None,
            notified: false,
            flag_key: 0,
            result: None,
            partner_char: 0,
            next_request_ms: 0,
        }
    }

    /// The coordinator's pairing, if any.
    pub fn order(&self) -> Option<&PairOrder> {
        self.order.as_ref()
    }

    fn set_phase(&mut self, phase: Phase, now_ms: u64) {
        self.phase = phase;
        self.phase_ms = now_ms;
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        let now = ctx.now_ms;
        self.started_ms = now;
        self.phase_ms = now;
        ctx.combat.duel.clear_notes();
        ctx.combat.duel.set_attended(true);
        match self.order.clone() {
            None => {
                self.phase = Phase::Pairing;
                if ctx.state.me.hp_pct() >= MIN_HP_PCT {
                    self.request(ctx);
                    self.next_request_ms = now + REREQUEST_MS;
                }
            }
            Some(o) => {
                self.partner_char = o.partner_char;
                self.phase = Phase::Meeting;
                self.set_partner(ctx, Some(o.partner_bot));
                if o.role == PairRole::Responder {
                    ctx.combat
                        .duel
                        .expect(o.partner_char, now + MEET_TIMEOUT_MS);
                }
                ctx.log.say(format!(
                    "duel with {} ({})",
                    o.partner_name,
                    if o.role == PairRole::Initiator {
                        "challenger"
                    } else {
                        "waiting"
                    }
                ));
            }
        }
    }

    fn request(&self, ctx: &mut Ctx<'_>) {
        let p = ctx.pos();
        if !ctx
            .coord
            .request_partner(PairKind::Duel, ctx.persona.id, (p[0], p[1]))
        {
            ctx.log.debug("duel: the coordinator is busy");
        }
    }

    fn set_partner(&self, ctx: &Ctx<'_>, partner: Option<u32>) {
        if let Some(slot) = ctx.hub.slot(ctx.persona.id) {
            slot.with(|g| g.snap.partner = partner);
        }
    }

    /// The partner as the bot sees it: a character unit with the partner's id or name.
    fn partner_view(&self, ctx: &Ctx<'_>) -> Option<PartnerView> {
        let o = self.order.as_ref()?;
        let want_id = if self.partner_char != 0 {
            self.partner_char
        } else {
            o.partner_char
        };
        ctx.state
            .units
            .iter()
            .find(|(_, u)| {
                u.kind == UnitKind::Character
                    && ((want_id != 0 && u.id == want_id)
                        || (want_id == 0
                            && u.name
                                .as_deref()
                                .is_some_and(|n| n.eq_ignore_ascii_case(&o.partner_name))))
            })
            .map(|(&obj, u)| PartnerView {
                obj,
                pos: u.pos,
                char_id: u.id,
                dead: u.dead(),
            })
    }

    /// The partner's published state: position, goal is "duel" with us, offline.
    fn partner_slot(&self, ctx: &Ctx<'_>) -> Option<(f32, f32, bool, bool)> {
        let o = self.order.as_ref()?;
        let slot = ctx.hub.slot(o.partner_bot)?;
        let me = ctx.persona.id;
        Some(slot.with(|g| {
            let s = &g.snap;
            (
                s.x,
                s.y,
                s.activity == Activity::Offline,
                s.goal == GoalKind::Duel.name() && s.partner == Some(me),
            )
        }))
    }

    /// One tick; the combat controller has already run.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let step = match self.phase {
            Phase::Pairing => self.pairing(ctx),
            Phase::Meeting => self.meeting(ctx),
            Phase::Challenging => self.challenging(ctx),
            Phase::Countdown => self.countdown(ctx),
            Phase::Fighting => self.fighting(ctx),
            Phase::Recover => self.recover(ctx),
        };
        match step {
            Ok(()) => GoalStatus::Running,
            Err(end) => end,
        }
    }

    fn pairing(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let waited = ctx.now_ms.saturating_sub(self.phase_ms);
        if ctx.state.me.hp_pct() < MIN_HP_PCT {
            return Err(GoalStatus::Failed(FailReason::Other("too hurt to duel")));
        }
        if waited > PAIR_WAIT_MS {
            return Err(GoalStatus::Failed(FailReason::NoTarget));
        }
        // A pairing replaces this goal; meanwhile an unexpected challenge may still arrive
        // (the combat controller answers it). Ask again now and then in case the first request
        // was lost.
        if ctx.now_ms >= self.next_request_ms {
            self.request(ctx);
            self.next_request_ms = ctx.now_ms + REREQUEST_MS;
        }
        Ok(())
    }

    fn meeting(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let now = ctx.now_ms;
        let Some(order) = self.order.clone() else {
            return Err(GoalStatus::Failed(FailReason::NoTarget));
        };
        if now.saturating_sub(self.phase_ms) > MEET_TIMEOUT_MS {
            return Err(GoalStatus::Failed(FailReason::Timeout));
        }
        if ctx.combat.duel.in_duel() {
            self.on_duel_started(ctx);
            return Ok(());
        }
        if let Some((_, _, offline, _)) = self.partner_slot(ctx) {
            if offline {
                return Err(GoalStatus::Failed(FailReason::Other(
                    "partner went offline",
                )));
            }
        }
        let view = self.partner_view(ctx);
        if let Some(v) = view {
            if self.partner_char == 0 {
                self.partner_char = v.char_id;
                if order.role == PairRole::Responder {
                    ctx.combat.duel.expect(v.char_id, now + MEET_TIMEOUT_MS);
                }
            }
            self.last_seen_ms = now;
        }
        let me = ctx.pos();
        let near = view.is_some_and(|v| dist2d(me, v.pos) <= CHALLENGE_DIST && !v.dead);

        if order.role == PairRole::Initiator {
            self.help_partner(ctx, &order);
            if near && now.saturating_sub(self.started_ms) >= SETTLE_MS {
                if let Some(v) = view {
                    ctx.motion.stop();
                    ctx.motion.face(v.pos[0], v.pos[1]);
                    ctx.send(cs::challenge_duel(v.char_id));
                    self.partner_char = v.char_id;
                    ctx.combat
                        .duel
                        .expect(v.char_id, now + CHALLENGE_TIMEOUT_MS);
                    ctx.log
                        .say(format!("challenging {} to a duel", order.partner_name));
                    self.set_phase(Phase::Challenging, now);
                    return Ok(());
                }
            }
            // Walk to the partner: where it stands now, else where it was published.
            let goal = view.map(|v| (v.pos[0], v.pos[1])).or_else(|| {
                self.partner_slot(ctx)
                    .map(|(x, y, _, _)| (x, y))
                    .or(Some(order.meet_at))
            });
            if let Some((x, y)) = goal {
                self.walk(ctx, x, y, MEET_RADIUS);
            }
        } else {
            // The responder waits at the meeting point, turned towards the challenger.
            if let Some(v) = view.filter(|v| dist2d(me, v.pos) <= CHALLENGE_DIST) {
                if ctx.motion.is_moving() {
                    ctx.motion.stop();
                }
                ctx.motion.face(v.pos[0], v.pos[1]);
            } else {
                self.walk(ctx, order.meet_at.0, order.meet_at.1, WAIT_RADIUS);
            }
        }
        if let MotionStatus::Failed(_) = ctx.motion.status() {
            return Err(GoalStatus::Failed(FailReason::Stuck));
        }
        Ok(())
    }

    /// A duel by web command only reaches the initiator: tell the other bot to wait for the
    /// challenge (the coordinator tells both, which shows as the partner's goal being "duel").
    fn help_partner(&mut self, ctx: &Ctx<'_>, order: &PairOrder) {
        if self.notified || ctx.now_ms.saturating_sub(self.started_ms) < SETTLE_MS {
            return;
        }
        self.notified = true;
        let Some((_, _, _, ready)) = self.partner_slot(ctx) else {
            return;
        };
        if ready {
            return;
        }
        let me = ctx.pos();
        let (my_char, my_name) = (ctx.state.me.char_id, ctx.state.me.name.to_string());
        let reply = PairOrder {
            kind: PairKind::Duel,
            role: PairRole::Responder,
            partner_bot: ctx.persona.id,
            partner_char: my_char,
            partner_name: my_name.into_boxed_str(),
            meet_at: (me[0], me[1]),
        };
        if let Some(slot) = ctx.hub.slot(order.partner_bot) {
            if let Err(e) = slot.send(BotCmd::Pair(reply)) {
                ctx.log
                    .debug(format!("duel: could not call the partner: {e}"));
            }
        }
    }

    fn walk(&mut self, ctx: &mut Ctx<'_>, x: f32, y: f32, radius: f32) {
        let moved = self
            .walk_to
            .is_none_or(|(wx, wy)| dist2d([wx, wy, 0.0], [x, y, 0.0]) > 3.0);
        if moved || (!ctx.motion.is_moving() && dist2d(ctx.pos(), [x, y, 0.0]) > radius + 1.0) {
            ctx.travel(x, y, radius, Pace::Run);
            self.walk_to = Some((x, y));
        }
    }

    fn challenging(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let now = ctx.now_ms;
        if ctx.combat.duel.in_duel() {
            self.on_duel_started(ctx);
            return Ok(());
        }
        if let Some((err, _)) = ctx.combat.duel.take_error() {
            let reason = if err == ERR_REJECTED {
                "the challenge was declined"
            } else {
                "the server refused the challenge"
            };
            ctx.log.say(format!("duel: {reason} (error {err})"));
            return Err(GoalStatus::Failed(FailReason::Other("duel refused")));
        }
        if let Some((_, _, offline, _)) = self.partner_slot(ctx) {
            if offline {
                return Err(GoalStatus::Failed(FailReason::Other(
                    "partner went offline",
                )));
            }
        }
        if now.saturating_sub(self.phase_ms) > CHALLENGE_TIMEOUT_MS {
            return Err(GoalStatus::Failed(FailReason::Timeout));
        }
        Ok(())
    }

    fn countdown(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let now = ctx.now_ms;
        match ctx.combat.duel.phase() {
            DuelPhase::Active { .. } => {
                self.on_duel_started(ctx);
                Ok(())
            }
            DuelPhase::Countdown { .. } => {
                if now.saturating_sub(self.phase_ms) > COUNTDOWN_TIMEOUT_MS {
                    return Err(GoalStatus::Failed(FailReason::Timeout));
                }
                Ok(())
            }
            DuelPhase::Idle => {
                // Called off before it began.
                if let Some(end) = ctx.combat.duel.take_end() {
                    self.finish(ctx, end);
                    return Ok(());
                }
                Err(GoalStatus::Failed(FailReason::Other(
                    "the duel was called off",
                )))
            }
        }
    }

    /// Moves to the right phase when the watch reports a countdown or a start.
    fn on_duel_started(&mut self, ctx: &mut Ctx<'_>) {
        let now = ctx.now_ms;
        match ctx.combat.duel.phase() {
            DuelPhase::Countdown { .. } => {
                if self.phase != Phase::Countdown {
                    self.set_phase(Phase::Countdown, now);
                    ctx.motion.stop();
                }
            }
            DuelPhase::Active { opponent, .. } => {
                if self.phase != Phase::Fighting {
                    self.set_phase(Phase::Fighting, now);
                    self.last_seen_ms = now;
                    ctx.motion.stop();
                    ctx.combat.engage(opponent);
                    self.put_flag(ctx, opponent);
                    let name = self.partner_name(ctx);
                    ctx.log.say(format!("duel against {name} started"));
                }
            }
            DuelPhase::Idle => {}
        }
    }

    fn partner_name(&self, ctx: &Ctx<'_>) -> String {
        self.order
            .as_ref()
            .map(|o| o.partner_name.to_string())
            .filter(|n| !n.is_empty())
            .or_else(|| {
                ctx.combat
                    .duel
                    .opponent()
                    .and_then(|o| ctx.state.units.get(&o))
                    .and_then(|u| u.name.as_ref().map(|n| n.to_string()))
            })
            .unwrap_or_else(|| "someone".into())
    }

    fn put_flag(&mut self, ctx: &Ctx<'_>, opponent: u32) {
        let me = ctx.pos();
        let (x, y) = match ctx.state.units.get(&opponent) {
            Some(u) => ((me[0] + u.pos[0]) / 2.0, (me[1] + u.pos[1]) / 2.0),
            None => (me[0], me[1]),
        };
        let mine = ctx.state.me.char_id;
        self.flag_key = if self.partner_char == 0 {
            mine
        } else {
            mine.min(self.partner_char)
        };
        let label = format!("duel: {} vs {}", ctx.persona.name, self.partner_name(ctx));
        ctx.hub.set_duel_flag(self.flag_key, x, y, label);
    }

    fn fighting(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let now = ctx.now_ms;
        if let Some(end) = ctx.combat.duel.take_end() {
            self.finish(ctx, end);
            return Ok(());
        }
        if !ctx.combat.duel.in_duel() {
            return Err(GoalStatus::Failed(FailReason::Other("the duel vanished")));
        }
        if now.saturating_sub(self.phase_ms) > FIGHT_TIMEOUT_MS {
            return Err(GoalStatus::Failed(FailReason::Timeout));
        }
        if let Some(opp) = ctx.combat.duel.opponent() {
            if ctx.state.units.contains_key(&opp) {
                self.last_seen_ms = now;
            } else if now.saturating_sub(self.last_seen_ms) > OPPONENT_GONE_MS {
                return Err(GoalStatus::Failed(FailReason::Other("lost the opponent")));
            }
        }
        Ok(())
    }

    fn finish(&mut self, ctx: &mut Ctx<'_>, end: DuelEnd) {
        let now = ctx.now_ms;
        let name = self.partner_name(ctx);
        let secs = now.saturating_sub(self.phase_ms) / 1000;
        ctx.log.say(format!(
            "duel against {name}: {} (after {secs} s)",
            end.outcome.word()
        ));
        self.result = Some((end.outcome, now));
        if ctx.combat.target() == self.opponent_obj(ctx) {
            ctx.combat.disengage();
        }
        ctx.combat.duel.clear_expect();
        if self.flag_key != 0 {
            ctx.hub.clear_duel_flag(self.flag_key);
        }
        ctx.motion.stop();
        self.set_phase(Phase::Recover, now);
    }

    fn opponent_obj(&self, ctx: &Ctx<'_>) -> Option<u32> {
        self.partner_view(ctx).map(|v| v.obj)
    }

    fn recover(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let waited = ctx.now_ms.saturating_sub(self.phase_ms);
        let me = &ctx.state.me;
        let recovered = me.hp_pct() >= HP_OK && me.mp_pct() >= MP_OK;
        if waited >= RECOVER_MAX_MS || (waited >= RECOVER_MIN_MS && recovered) {
            return Err(GoalStatus::Done);
        }
        Ok(())
    }

    /// Called when the goal ends.
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        if self.phase == Phase::Pairing {
            ctx.coord.cancel(ctx.persona.id);
        }
        if self.flag_key != 0 {
            ctx.hub.clear_duel_flag(self.flag_key);
        }
        self.set_partner(ctx, None);
        ctx.combat.duel.clear_expect();
        ctx.combat.duel.set_attended(false);
        if !ctx.combat.duel.in_duel() {
            ctx.motion.stop();
        }
    }

    /// Appends what the bot is doing.
    pub fn describe(&self, out: &mut String) {
        let who = self
            .order
            .as_ref()
            .map_or("a partner", |o| &*o.partner_name);
        let _ = match (self.phase, self.result) {
            (Phase::Pairing, _) => write!(out, "looking for someone to duel"),
            (Phase::Meeting, _) => write!(out, "meeting {who} for a duel"),
            (Phase::Challenging, _) => write!(out, "challenged {who}"),
            (Phase::Countdown, _) => write!(out, "duel with {who} starts"),
            (Phase::Fighting, _) => write!(out, "dueling {who}"),
            (Phase::Recover, Some((o, _))) => write!(out, "{} against {who}, recovering", o.word()),
            (Phase::Recover, None) => write!(out, "recovering from a duel"),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::sc_activity::duel_det;

    fn act(a: ActivityEvent) -> ServerEvent {
        ServerEvent::Activity(a)
    }

    const ME: u32 = 0x100;
    const FOE: u32 = 0x200;

    #[test]
    fn a_duel_runs_from_challenge_to_end() {
        let mut w = DuelWatch::default();
        let mut rng = fastrand::Rng::with_seed(3);
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            1000,
        );
        assert!(w.has_pending());
        // not expected, not idle: declined at once
        assert_eq!(w.answer(1000, false, &mut rng), Answer::Decline(9));
        assert!(!w.has_pending());
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            2000,
        );
        // idle: accepted after a short delay
        assert_eq!(w.answer(2000, true, &mut rng), Answer::Wait);
        let due = match w.answer(2000 + 2500, true, &mut rng) {
            Answer::Accept(c) => c,
            other => panic!("{other:?}"),
        };
        assert_eq!(due, 9);
        w.on_event(&act(ActivityEvent::DuelStartCountdown), ME, 5000);
        assert!(w.in_duel() && w.opponent().is_none());
        w.on_event(
            &act(ActivityEvent::DuelStarted {
                challenger_obj: FOE,
                challenged_obj: ME,
            }),
            ME,
            8000,
        );
        assert_eq!(w.opponent(), Some(FOE));
        assert_eq!(w.counts(), (1, 0));
        w.on_event(
            &act(ActivityEvent::DuelEnded {
                winner_id: 1,
                loser_id: 2,
                winner_obj: ME,
                loser_obj: FOE,
                det: duel_det::WIN,
            }),
            ME,
            60_000,
        );
        assert!(!w.in_duel());
        let end = w.take_end().unwrap();
        assert_eq!((end.outcome, end.det), (Outcome::Won, duel_det::WIN));
        assert!(w.take_end().is_none());
        assert_eq!(w.counts(), (1, 1));
    }

    #[test]
    fn outcomes_follow_the_winner_slot_and_the_draw_flag() {
        let ended = |winner_obj, loser_obj, det| {
            let mut w = DuelWatch::default();
            w.on_event(
                &act(ActivityEvent::DuelEnded {
                    winner_id: 1,
                    loser_id: 2,
                    winner_obj,
                    loser_obj,
                    det,
                }),
                ME,
                1,
            );
            w.take_end().map(|e| e.outcome)
        };
        assert_eq!(ended(ME, FOE, duel_det::WIN), Some(Outcome::Won));
        assert_eq!(ended(FOE, ME, duel_det::WIN), Some(Outcome::Lost));
        assert_eq!(ended(FOE, ME, duel_det::SURRENDER), Some(Outcome::Lost));
        assert_eq!(ended(ME, FOE, duel_det::DRAW), Some(Outcome::Draw));
        assert_eq!(ended(FOE, ME, duel_det::DRAW), Some(Outcome::Draw));
        // somebody else's duel
        assert_eq!(ended(0x300, FOE, duel_det::WIN), None);
    }

    #[test]
    fn an_expected_challenger_is_accepted_even_when_not_idle() {
        let mut w = DuelWatch::default();
        let mut rng = fastrand::Rng::with_seed(1);
        w.expect(9, 100_000);
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            1000,
        );
        assert_eq!(
            w.answer(1000, false, &mut rng),
            Answer::Wait,
            "short delay first"
        );
        assert_eq!(w.answer(4000, false, &mut rng), Answer::Accept(9));
        // another challenger while expecting someone specific and busy: declined
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 8,
            }),
            ME,
            5000,
        );
        assert_eq!(w.answer(5000, false, &mut rng), Answer::Decline(8));
        // expecting anyone (0) takes anyone
        w.expect(0, 100_000);
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 8,
            }),
            ME,
            6000,
        );
        assert_eq!(w.answer(6000, false, &mut rng), Answer::Wait);
        assert_eq!(w.answer(9000, false, &mut rng), Answer::Accept(8));
        // an expectation that ran out counts for nothing
        w.expect(9, 7000);
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            8000,
        );
        assert_eq!(w.answer(8000, false, &mut rng), Answer::Decline(9));
    }

    #[test]
    fn stale_challenges_and_challenges_during_a_duel_are_dropped() {
        let mut w = DuelWatch::default();
        let mut rng = fastrand::Rng::with_seed(1);
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            1000,
        );
        assert_eq!(
            w.answer(1000 + CHALLENGE_TTL_MS + 1, true, &mut rng),
            Answer::Drop
        );
        w.on_event(
            &act(ActivityEvent::DuelChallenged {
                challenger_char_id: 9,
            }),
            ME,
            1000,
        );
        w.on_event(&act(ActivityEvent::DuelStartCountdown), ME, 1100);
        assert_eq!(w.answer(1200, true, &mut rng), Answer::Drop);
        assert_eq!(
            w.answer(1300, true, &mut rng),
            Answer::Wait,
            "nothing pending"
        );
    }

    #[test]
    fn duel_errors_are_remembered_once_and_other_errors_ignored() {
        let mut w = DuelWatch::default();
        let err = |e: i16| {
            ServerEvent::World(WorldEvent::ErrorMsg {
                err: e,
                kind: 0,
                notify: false,
            })
        };
        w.on_event(&err(12), ME, 10);
        assert!(w.take_error().is_none());
        w.on_event(&err(507), ME, 20);
        assert_eq!(w.take_error(), Some((507, 20)));
        assert!(w.take_error().is_none());
        w.on_event(&err(513), ME, 30);
        assert_eq!(w.take_error(), Some((513, 30)));
    }

    #[test]
    fn a_duel_start_of_somebody_else_is_not_ours() {
        let mut w = DuelWatch::default();
        w.on_event(
            &act(ActivityEvent::DuelStarted {
                challenger_obj: 0x300,
                challenged_obj: 0x400,
            }),
            ME,
            1,
        );
        assert!(!w.in_duel());
        // and before the bot is spawned nothing counts
        w.on_event(
            &act(ActivityEvent::DuelStarted {
                challenger_obj: 0,
                challenged_obj: 0,
            }),
            0,
            1,
        );
        assert!(!w.in_duel());
    }

    #[test]
    fn the_goal_stays_small_and_describes_itself() {
        assert!(
            std::mem::size_of::<Duel>() <= 128,
            "{}",
            std::mem::size_of::<Duel>()
        );
        let mut d = Duel::new(None);
        let mut s = String::new();
        d.describe(&mut s);
        assert_eq!(s, "looking for someone to duel");
        d.order = Some(PairOrder {
            kind: PairKind::Duel,
            role: PairRole::Initiator,
            partner_bot: 2,
            partner_char: 20,
            partner_name: "Foe".into(),
            meet_at: (0.0, 0.0),
        });
        d.phase = Phase::Recover;
        d.result = Some((Outcome::Lost, 5));
        s.clear();
        d.describe(&mut s);
        assert_eq!(s, "lost against Foe, recovering");
        assert_eq!(d.order().map(|o| o.partner_bot), Some(2));
    }
}
