//! Life-like movement: jumps, idle mannerisms and emotes (`[behavior] lifelike`, `jump_rate`,
//! `emotes`).
//!
//! A bot that only walks and stands still looks like a bot to the player who watches it. This
//! module is what a person adds: the odd jump, a look around, a wave. Everything here is for the
//! eyes of a **real player** (a character that is not one of our bots), so nothing happens unless
//! one is near ([`Gate::audience`], the same signal that raises the movement update rate), and
//! nothing happens while the bot is busy ([`Gate::busy`]): fighting, looting, casting, trading,
//! in a boat or an instance, dead, loading.
//!
//! The parts, each a plain value with no clock and no socket (the caller passes the bot clock and
//! a random source), so the rates and limits are tested without a world:
//!
//! | Part | What |
//! | --- | --- |
//! | [`Traits`] | the bot's character, from its seed: how playful (some never jump), how fidgety, a speed offset of a few percent |
//! | [`Arc`], [`Jump`] | the physics of a jump and the packet schedule along it |
//! | [`JumpSched`] | when to jump: on a straight run, at a step, after a stuck side-step, in place when idle, to cheer |
//! | [`Fidget`] | what a bot that stands does: turns in place, a small shuffle |
//! | [`Dawdle`] | a short pause at some corners of a route |
//! | [`Emotes`], [`Budget`] | which emote when, with per-bot, per-player and process-wide limits |
//!
//! The movement controller ([`super::motion::Motion`]) owns the per-bot [`Life`] and applies the
//! movement parts inside its tick; [`tick`] and the goal hooks ([`rest_begin`], [`rest_tick`],
//! [`rest_end`], [`wander_pause`]) drive the emotes and the mannerisms.
//!
//! The wire shape of a jump is [`aa_proto::cs::MoveUnit`] with [`aa_proto::cs::JumpPhase`]; the
//! physics are the client's (`game_jump_height` 1.3 m, gravity 25 m/s², so 8.06 m/s up, 0.32 s to
//! the apex and 0.65 s in the air).

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use aa_proto::cs::{self, emotion, JumpPhase, Out};
use aa_proto::sc_emote::Expressed;
use smallvec::SmallVec;

use super::ctx::Ctx;
use super::motion::Motion;
use super::state::{BotState, UnitKind};
use crate::hub::Hub;

// ---- the bot's character ----------------------------------------------------------------------

/// Share of bots that never jump (they are the serious ones).
const NEVER_JUMP: f32 = 0.12;
/// Spread of the speed offset: a bot runs 3 % faster or slower than the configured speed at most.
const SPEED_SPREAD: f32 = 0.03;

/// A stable number in `0..1` for `(seed, salt)`.
fn unit_hash(seed: u32, salt: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9E37_79B1) ^ salt.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0xFF_FFFF) as f32 / 0x100_0000 as f32
}

/// The character of one bot, fixed for its life (it comes from the bot id, so a bot is the same
/// person after a reconnect).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Traits {
    /// Scales how often the bot jumps (0 = never; the mean over the bots that do is 1).
    pub jump_mult: f32,
    /// Scales how much the bot fidgets when it stands (0.5 to 1.5).
    pub fidget: f32,
    /// The bot's own speed, as a factor of the configured one (0.97 to 1.03).
    pub speed_k: f32,
    /// Chance that the bot stops for a moment at a corner (0 to 0.3).
    pub dawdle: f32,
    /// Scales how often the bot makes an ambient emote (0.5 to 1; it only slows the bot down).
    pub chatty: f32,
}

impl Traits {
    /// The traits of bot `seed`; `jump_rate` is the configured multiplier (`[behavior]
    /// jump_rate`).
    pub fn new(seed: u32, jump_rate: f32) -> Traits {
        let playful = unit_hash(seed, 1);
        let jump_mult = if playful < NEVER_JUMP || jump_rate.is_nan() || jump_rate <= 0.0 {
            0.0
        } else {
            (0.35 + 1.3 * (playful - NEVER_JUMP) / (1.0 - NEVER_JUMP)) * jump_rate
        };
        Traits {
            jump_mult,
            fidget: 0.5 + unit_hash(seed, 2),
            speed_k: 1.0 + (unit_hash(seed, 3) - 0.5) * 2.0 * SPEED_SPREAD,
            dawdle: unit_hash(seed, 4) * 0.3,
            chatty: 0.5 + 0.5 * unit_hash(seed, 5),
        }
    }
}

/// What the bot loop knows about the moment, set before the movement tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gate {
    /// A real player is near (within the watch radius): somebody sees the bot.
    pub audience: bool,
    /// The bot is busy and must not show off: fighting, looting, casting, trading, in a vehicle
    /// or an instance, dead, loading, on a portal journey.
    pub busy: bool,
    /// The bot stands in a town (a peace zone or a crowd).
    pub town: bool,
}

impl Gate {
    /// Whether the bot may do something for the eyes of a player now.
    pub fn show(&self) -> bool {
        self.audience && !self.busy
    }
}

// ---- jumps ------------------------------------------------------------------------------------

/// Gravity of the client's jump (m/s²).
pub const GRAVITY: f32 = 25.0;
/// Height of a full jump (m): `game_jump_height`.
pub const FULL_HEIGHT: f32 = 1.3;
/// Height of a hop over a step (m).
pub const HOP_HEIGHT: f32 = 0.55;
/// Where along the air time (0 to 1) the packets of a jump go: the take-off, the apex, the way
/// down, the touchdown and the ordinary ground update that settles the jump
/// ([`SETTLE_MS`] later).
const SCHEDULE: [f32; 5] = [0.0, 0.5, 0.8, 1.0, 1.0];
/// How long after the touchdown the ordinary ground update goes (ms).
const SETTLE_MS: u64 = 90;
/// No jump within this long of the last (ms).
const JUMP_COOLDOWN_MS: u64 = 10_000;
/// Seconds between two jumps on the run, before the bot's multiplier.
const RUN_JUMP_S: (f32, f32) = (30.0, 60.0);
/// Seconds a standing bot waits before it jumps in place, before the multiplier.
const IDLE_JUMP_S: (f32, f32) = (90.0, 180.0);
/// A run jump needs this much straight way ahead (m).
const STRAIGHT_MIN_M: f32 = 6.0;
/// A step this high (m) within [`LOOKAHEAD_M`] ahead makes a hop.
const STEP_MIN_M: f32 = 0.3;
/// A step higher than this is a wall or a cliff, not a step.
const STEP_MAX_M: f32 = 1.2;
/// How far ahead (m) the ground is looked at for a step.
pub const LOOKAHEAD_M: f32 = 1.5;
/// Chance that a hop is taken at a step.
const HOP_CHANCE: f32 = 0.5;
/// No hop within this long of the last (ms).
const HOP_COOLDOWN_MS: u64 = 15_000;
/// Share of run jumps that are two in a row.
const DOUBLE_SHARE: f32 = 0.10;
/// The pause between the two jumps of a double (ms).
pub const DOUBLE_GAP_MS: u64 = 120;

/// The physics of one jump: a parabola.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    /// Take-off speed (m/s).
    pub v0: f32,
}

impl Arc {
    /// The arc that peaks `height` metres above the take-off.
    pub fn for_height(height: f32) -> Arc {
        Arc {
            v0: (2.0 * GRAVITY * height).sqrt(),
        }
    }

    /// Time in the air over level ground (s).
    pub fn air_s(&self) -> f32 {
        2.0 * self.v0 / GRAVITY
    }

    /// Height above the take-off after `t` seconds (never below it).
    pub fn lift(&self, t: f32) -> f32 {
        (self.v0 * t - 0.5 * GRAVITY * t * t).max(0.0)
    }

    /// Vertical speed after `t` seconds (m/s).
    pub fn vz(&self, t: f32) -> f32 {
        self.v0 - GRAVITY * t
    }
}

/// What a jump is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpKind {
    /// A full jump.
    Full,
    /// A low hop (a step on the way).
    Hop,
    /// Two full jumps in a row.
    Double,
}

/// One packet's worth of a jump in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AirStep {
    /// The take-off: height 0, speed up.
    TakeOff {
        /// Vertical speed (m/s).
        vz: f32,
    },
    /// On the way: height above the ground and vertical speed.
    Air {
        /// Height above the ground (m).
        lift: f32,
        /// Vertical speed (m/s).
        vz: f32,
    },
    /// Touchdown: on the ground, the jump flag still set (the real client sends this once).
    Land,
    /// The ordinary ground update after the touchdown; the jump is over.
    Settle,
}

impl AirStep {
    /// The height above the ground (m).
    pub fn lift(&self) -> f32 {
        match self {
            AirStep::Air { lift, .. } => *lift,
            _ => 0.0,
        }
    }

    /// The vertical speed (m/s).
    pub fn vz(&self) -> f32 {
        match self {
            AirStep::TakeOff { vz } | AirStep::Air { vz, .. } => *vz,
            AirStep::Land | AirStep::Settle => 0.0,
        }
    }

    /// The wire phase.
    pub fn phase(&self) -> JumpPhase {
        match self {
            AirStep::TakeOff { .. } => JumpPhase::TakeOff,
            AirStep::Air { .. } => JumpPhase::Air,
            AirStep::Land => JumpPhase::Land,
            AirStep::Settle => JumpPhase::None,
        }
    }
}

/// A jump in progress: its arc, when it began and which packet is next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jump {
    start_ms: u64,
    arc: Arc,
    /// Index into [`SCHEDULE`] of the next packet.
    next: u8,
    /// A second jump follows the landing.
    again: bool,
}

impl Jump {
    /// A jump of `kind` that takes off at `now_ms`.
    pub fn start(now_ms: u64, kind: JumpKind) -> Jump {
        let height = if kind == JumpKind::Hop {
            HOP_HEIGHT
        } else {
            FULL_HEIGHT
        };
        Jump {
            start_ms: now_ms,
            arc: Arc::for_height(height),
            next: 0,
            again: kind == JumpKind::Double,
        }
    }

    /// Time in the air (ms).
    pub fn air_ms(&self) -> u64 {
        (self.arc.air_s() * 1000.0).round() as u64
    }

    /// When the next packet is due (bot clock, ms).
    pub fn due_at(&self) -> u64 {
        let i = usize::from(self.next).min(SCHEDULE.len() - 1);
        let settle = if i + 1 == SCHEDULE.len() {
            SETTLE_MS
        } else {
            0
        };
        self.start_ms + (SCHEDULE[i] * self.air_ms() as f32).round() as u64 + settle
    }

    /// The packet to send now, if one is due. A late tick skips to the newest due packet (the
    /// landing wins over a packet on the way). `None` once the jump is settled.
    pub fn step(&mut self, now_ms: u64) -> Option<AirStep> {
        if usize::from(self.next) >= SCHEDULE.len() || now_ms < self.due_at() {
            return None;
        }
        while usize::from(self.next) + 1 < SCHEDULE.len() {
            let ahead = Jump {
                next: self.next + 1,
                ..*self
            };
            if now_ms >= ahead.due_at() {
                self.next += 1;
            } else {
                break;
            }
        }
        let i = usize::from(self.next);
        self.next += 1;
        let t = (now_ms.saturating_sub(self.start_ms)) as f32 / 1000.0;
        Some(if i == 0 {
            AirStep::TakeOff { vz: self.arc.v0 }
        } else if i + 1 == SCHEDULE.len() {
            AirStep::Settle
        } else if i + 2 == SCHEDULE.len() {
            AirStep::Land
        } else {
            let t = t.min(self.arc.air_s());
            AirStep::Air {
                lift: self.arc.lift(t),
                vz: self.arc.vz(t),
            }
        })
    }

    /// The jump is settled (all its packets went out).
    pub fn done(&self) -> bool {
        usize::from(self.next) >= SCHEDULE.len()
    }

    /// Whether a second jump follows the landing.
    pub fn again(&self) -> bool {
        self.again
    }
}

/// What the movement controller sees of the world when it considers a jump.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// The bot walks a trip.
    pub moving: bool,
    /// ... at running speed.
    pub running: bool,
    /// Straight way ahead before the next corner or the end of the trip (m).
    pub straight_m: f32,
    /// The ground ahead rises like a step.
    pub step_up: bool,
}

/// Whether the ground `ahead` (a height, `None` unknown) is a step up from `here`.
pub fn is_step(here: Option<f32>, ahead: Option<f32>) -> bool {
    match (here, ahead) {
        (Some(h), Some(a)) => {
            let d = a - h;
            // (sea floor and unknown ground are no steps)
            h >= super::motion::SWIM_THRESHOLD && (STEP_MIN_M..=STEP_MAX_M).contains(&d)
        }
        _ => false,
    }
}

fn spread(rng: &mut fastrand::Rng, (lo, hi): (f32, f32)) -> f32 {
    lo + rng.f32() * (hi - lo)
}

/// When a bot jumps. See the module docs for the triggers.
#[derive(Debug, Clone, Copy, Default)]
pub struct JumpSched {
    next_run_ms: u64,
    /// 0 = not armed (the bot is not standing).
    next_idle_ms: u64,
    next_hop_ms: u64,
    cooldown_until_ms: u64,
    /// The stuck detector side-stepped: hop at the next chance.
    want_hop: bool,
    /// Cheer with a jump at the next chance.
    want_cheer: bool,
}

impl JumpSched {
    /// A schedule that starts at `now_ms`, with the first run jump at a random time.
    pub fn new(now_ms: u64, mult: f32, rng: &mut fastrand::Rng) -> JumpSched {
        JumpSched {
            next_run_ms: now_ms + Self::wait_ms(RUN_JUMP_S, mult, rng) / 2,
            ..JumpSched::default()
        }
    }

    fn wait_ms(range: (f32, f32), mult: f32, rng: &mut fastrand::Rng) -> u64 {
        (spread(rng, range) * 1000.0 / mult.max(0.05)) as u64
    }

    /// The stuck detector side-stepped.
    pub fn stuck(&mut self) {
        self.want_hop = true;
    }

    /// Something to cheer about (a level, a quest): a jump at the next chance.
    pub fn cheer(&mut self) {
        self.want_cheer = true;
    }

    /// Decides whether to jump now. `mult` is the bot's [`Traits::jump_mult`]; 0 never jumps.
    pub fn decide(
        &mut self,
        now_ms: u64,
        look: &Look,
        mult: f32,
        rng: &mut fastrand::Rng,
    ) -> Option<JumpKind> {
        if mult <= 0.0 {
            return None;
        }
        if !look.moving {
            // (a stuck side-step belongs to a walking bot)
            self.want_hop = false;
        }
        if now_ms < self.cooldown_until_ms {
            return None;
        }
        if self.want_cheer {
            self.want_cheer = false;
            return Some(self.took(now_ms, JumpKind::Full));
        }
        if !look.moving {
            if self.next_idle_ms == 0 {
                self.next_idle_ms = now_ms + Self::wait_ms(IDLE_JUMP_S, mult, rng);
            }
            if now_ms >= self.next_idle_ms {
                self.next_idle_ms = 0;
                return Some(self.took(now_ms, JumpKind::Full));
            }
            return None;
        }
        self.next_idle_ms = 0;
        if !look.running {
            return None;
        }
        if self.want_hop {
            self.want_hop = false;
            return Some(self.took(now_ms, JumpKind::Hop));
        }
        if look.step_up && now_ms >= self.next_hop_ms && rng.f32() < HOP_CHANCE {
            self.next_hop_ms = now_ms + HOP_COOLDOWN_MS;
            return Some(self.took(now_ms, JumpKind::Hop));
        }
        if now_ms >= self.next_run_ms && look.straight_m >= STRAIGHT_MIN_M {
            self.next_run_ms = now_ms + Self::wait_ms(RUN_JUMP_S, mult, rng);
            let kind = if rng.f32() < DOUBLE_SHARE {
                JumpKind::Double
            } else {
                JumpKind::Full
            };
            return Some(self.took(now_ms, kind));
        }
        None
    }

    fn took(&mut self, now_ms: u64, kind: JumpKind) -> JumpKind {
        self.cooldown_until_ms = now_ms + JUMP_COOLDOWN_MS;
        kind
    }
}

// ---- mannerisms while standing ----------------------------------------------------------------

/// Turns in a row after a trip.
const TURNS: (u8, u8) = (1, 3);
/// Seconds between two turns.
const TURN_GAP_S: (f32, f32) = (2.0, 6.0);
/// A turn is this many degrees either way.
const TURN_DEG: (f32, f32) = (30.0, 120.0);
/// Chance (before the bot's fidget trait) that a standing bot also shuffles a step or two.
const SHUFFLE_CHANCE: f32 = 0.4;
/// Length of a shuffle (m).
const SHUFFLE_M: (f32, f32) = (0.5, 1.5);

/// What a standing bot does next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fid {
    /// Nothing now.
    None,
    /// Turn by this many radians (positive is counter-clockwise).
    Turn(f32),
    /// Walk this many metres at walk pace in a direction this far (radians) from the facing.
    Shuffle {
        /// Distance (m).
        len: f32,
        /// Direction relative to the facing (radians).
        rel: f32,
    },
}

/// The idle mannerisms of a bot that stands: after a trip, one to three turns in place two to
/// six seconds apart, and sometimes a small shuffle. Any new command of the controller starts it
/// over ([`Fidget::reset`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Fidget {
    armed: bool,
    turns_left: u8,
    next_ms: u64,
    shuffled: bool,
}

impl Fidget {
    /// The bot was told to move, stop or turn: start over.
    pub fn reset(&mut self) {
        *self = Fidget::default();
    }

    /// A fidget that has nothing left to do until the next [`Fidget::reset`].
    pub fn finished() -> Fidget {
        Fidget {
            armed: true,
            turns_left: 0,
            next_ms: 0,
            shuffled: true,
        }
    }

    /// Whether anything is left to do before the next [`Fidget::reset`].
    pub fn pending(&self) -> bool {
        !self.armed || self.turns_left > 0 || !self.shuffled
    }

    /// What to do at `now_ms`; call once per tick while the bot stands and may show off.
    pub fn step(&mut self, now_ms: u64, fidget: f32, rng: &mut fastrand::Rng) -> Fid {
        if !self.armed {
            self.armed = true;
            self.turns_left = rng.u8(TURNS.0..=TURNS.1);
            self.next_ms = now_ms + (spread(rng, TURN_GAP_S) * 1000.0) as u64;
            return Fid::None;
        }
        if now_ms < self.next_ms {
            return Fid::None;
        }
        if self.turns_left > 0 {
            self.turns_left -= 1;
            self.next_ms = now_ms + (spread(rng, TURN_GAP_S) * 1000.0) as u64;
            let deg = spread(rng, TURN_DEG);
            let sign = if rng.bool() { 1.0 } else { -1.0 };
            return Fid::Turn((deg * sign).to_radians());
        }
        if !self.shuffled {
            self.shuffled = true;
            if rng.f32() < SHUFFLE_CHANCE * fidget {
                return Fid::Shuffle {
                    len: spread(rng, SHUFFLE_M),
                    rel: spread(rng, (-1.5, 1.5)),
                };
            }
        }
        Fid::None
    }
}

// ---- pauses at corners ------------------------------------------------------------------------

/// A turn of this many radians within [`SAMPLE_MS`] is a corner.
const CORNER_RAD: f32 = 0.9;
/// The heading is compared with the one this long ago (ms).
const SAMPLE_MS: u64 = 400;
/// After a corner, no new roll for this long (ms).
const CORNER_GAP_MS: u64 = 8_000;
/// How long a pause at a corner lasts (ms).
const PAUSE_MS: (u64, u64) = (300, 1000);

/// Short pauses at some corners of a route.
#[derive(Debug, Clone, Copy, Default)]
pub struct Dawdle {
    sample: (f32, f32),
    sample_ms: u64,
    quiet_until_ms: u64,
}

impl Dawdle {
    /// Looks at the heading of this tick; `Some(ms)` is a pause to take now. `dawdle` is the
    /// bot's [`Traits::dawdle`], the chance a corner stops it.
    pub fn on_heading(
        &mut self,
        now_ms: u64,
        heading: (f32, f32),
        dawdle: f32,
        rng: &mut fastrand::Rng,
    ) -> Option<u64> {
        if heading == (0.0, 0.0) {
            return None;
        }
        if self.sample == (0.0, 0.0) || now_ms.saturating_sub(self.sample_ms) > 4 * SAMPLE_MS {
            self.sample = heading;
            self.sample_ms = now_ms;
            return None;
        }
        if now_ms.saturating_sub(self.sample_ms) < SAMPLE_MS {
            return None;
        }
        let dot = (self.sample.0 * heading.0 + self.sample.1 * heading.1).clamp(-1.0, 1.0);
        self.sample = heading;
        self.sample_ms = now_ms;
        if dot.acos() < CORNER_RAD || now_ms < self.quiet_until_ms {
            return None;
        }
        self.quiet_until_ms = now_ms + CORNER_GAP_MS;
        (rng.f32() < dawdle).then(|| rng.u64(PAUSE_MS.0..=PAUSE_MS.1))
    }

    /// Forget the heading (a new trip).
    pub fn reset(&mut self) {
        self.sample = (0.0, 0.0);
    }
}

// ---- emotes -----------------------------------------------------------------------------------

/// Which allowance an emote draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Ambient: a stretch in the square, a sit while resting.
    Ambient,
    /// For a player: a greeting, an answer, a cheer.
    Social,
}

/// A refilling allowance.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    cap: f64,
    per_s: f64,
    last_ms: u64,
}

impl Bucket {
    fn new(cap: f64, per_s: f64) -> Bucket {
        Bucket {
            tokens: cap,
            cap,
            per_s,
            last_ms: 0,
        }
    }

    fn take(&mut self, now_ms: u64) -> bool {
        let dt = now_ms.saturating_sub(self.last_ms) as f64 / 1000.0;
        self.last_ms = now_ms.max(self.last_ms);
        self.tokens = (self.tokens + dt * self.per_s).min(self.cap);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The allowance of emotes for the whole process: the server logs every emote at WARN, so a few
/// hundred bots must not make a line each. Ambient emotes get 5 a minute, those for a player 15.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    ambient: Bucket,
    social: Bucket,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            ambient: Bucket::new(3.0, 1.0 / 12.0),
            social: Bucket::new(6.0, 1.0 / 4.0),
        }
    }
}

impl Budget {
    /// Spends one emote of `class` at `now_ms`; `false` when the allowance is used up.
    pub fn take(&mut self, class: Class, now_ms: u64) -> bool {
        match class {
            Class::Ambient => self.ambient.take(now_ms),
            Class::Social => self.social.take(now_ms),
        }
    }
}

/// Milliseconds on a clock all bots share.
fn process_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The process-wide [`Budget`]: spends one emote of `class`.
pub fn global_take(class: Class) -> bool {
    static BUDGET: OnceLock<Mutex<Budget>> = OnceLock::new();
    let b = BUDGET.get_or_init(|| Mutex::new(Budget::default()));
    match b.lock() {
        Ok(mut b) => b.take(class, process_ms()),
        Err(_) => false,
    }
}

/// A bot makes no emote within this long of its last (ms).
const EMOTE_GAP_MS: u64 = 8_000;
/// Between two ambient emotes of one bot (s), before its chatty trait.
const AMBIENT_S: (f32, f32) = (45.0, 120.0);
/// A player is greeted at most once in this long (ms).
const GREET_AGAIN_MS: u64 = 10 * 60_000;
/// A player gets an answer to an emote at most once in this long (ms).
const ANSWER_AGAIN_MS: u64 = 30_000;
/// A player this close (m) is greeted.
pub const GREET_M: f32 = 6.0;
/// An emote played this close (m) to the bot is answered.
pub const ANSWER_M: f32 = 10.0;
/// Chance that a passing player is greeted at all.
const GREET_CHANCE: f32 = 0.6;
/// Chance that an emote at the bot is answered.
const ANSWER_DIRECT: f32 = 0.9;
/// Chance that an emote nearby, but not at the bot, is answered.
const ANSWER_NEARBY: f32 = 0.3;
/// The wait before an emote that reacts to something (ms).
const REACT_MS: (u64, u64) = (600, 2_500);
/// The ambient emotes of a town and how often each comes (weights).
const AMBIENT: [(u32, u32); 5] = [
    (emotion::STRETCH, 3),
    (emotion::YAWN, 3),
    (emotion::SIGH, 2),
    (emotion::LAUGH, 2),
    (emotion::CLAP, 1),
];
/// Seconds into a rest after which the bot sits down.
const SIT_AFTER_S: (f32, f32) = (8.0, 25.0);
/// No celebration for quests finished within this long after the bot logged in (ms): the quest
/// log arrives then.
const LOGIN_QUIET_MS: u64 = 20_000;

/// The emote an emote is answered with.
pub fn answer_to(emotion_id: u32) -> u32 {
    match emotion_id {
        emotion::SALUTE | emotion::THANKS | emotion::BOW => emotion::SALUTE,
        emotion::CLAP => emotion::CLAP,
        emotion::LAUGH => emotion::LAUGH,
        emotion::HAPPY | emotion::CELEBRATE | emotion::HEART => emotion::HAPPY,
        _ => emotion::GREET,
    }
}

/// An emote that waits for its time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pending {
    due_ms: u64,
    emotion: u32,
    /// Object id the emote is played at (0 = nobody).
    target: u32,
    /// The player it is for: the emote is dropped when they are gone (0 = ambient).
    char_id: u32,
    class: Class,
}

/// A real player near the bot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    /// Character id.
    pub char_id: u32,
    /// Object id.
    pub obj: u32,
    /// Position.
    pub pos: [f32; 2],
    /// Distance (m).
    pub dist: f32,
}

/// What an emote tick looks at, gathered by the bot loop.
#[derive(Debug, Clone, Copy)]
pub struct TickIn {
    /// The bot clock (ms).
    pub now_ms: u64,
    /// Milliseconds since the bot logged in.
    pub online_ms: u64,
    /// The bot's level.
    pub level: u8,
    /// Quests the bot has finished.
    pub quests_done: u16,
    /// The nearest real player within [`ANSWER_M`], if any.
    pub near: Option<Near>,
    /// The bot walks a trip.
    pub moving: bool,
    /// The moment.
    pub gate: Gate,
}

/// What the emote logic asks the caller to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Play an emote (object id of the target, 0 = none).
    Emote {
        /// `express_texts` id.
        emotion: u32,
        /// Object id of the target.
        target: u32,
    },
    /// Turn to face a point.
    Face([f32; 2]),
    /// Jump for joy.
    Cheer,
}

/// The emote state of one bot.
#[derive(Debug, Clone, Default)]
pub struct Emotes {
    last_ms: u64,
    next_ambient_ms: u64,
    greeted: SmallVec<[(u32, u64); 6]>,
    answered: SmallVec<[(u32, u64); 6]>,
    pending: Option<Pending>,
    level: u8,
    quests_done: u16,
    /// The bot sits (the sit emote is a pose until the bot moves again).
    pub sitting: bool,
    /// Bot clock (ms) at which a resting bot sits down; 0 = not scheduled.
    sit_at_ms: u64,
}

fn recent(list: &[(u32, u64)], char_id: u32, now_ms: u64) -> bool {
    list.iter()
        .any(|&(id, until)| id == char_id && now_ms < until)
}

fn remember(list: &mut SmallVec<[(u32, u64); 6]>, cap: usize, char_id: u32, until_ms: u64) {
    if let Some(e) = list.iter_mut().find(|e| e.0 == char_id) {
        e.1 = until_ms;
        return;
    }
    if list.len() >= cap {
        list.remove(0);
    }
    list.push((char_id, until_ms));
}

impl Emotes {
    /// Whether the bot has played an emote lately.
    fn rested(&self, now_ms: u64) -> bool {
        self.last_ms == 0 || now_ms.saturating_sub(self.last_ms) >= EMOTE_GAP_MS
    }

    fn queue(&mut self, p: Pending) {
        // (one thing at a time; a greeting is not pushed out by an ambient emote)
        if self.pending.is_none_or(|q| q.class == Class::Ambient) {
            self.pending = Some(p);
        }
    }

    /// A player played emote `e` at distance `dist`; `directed` when it was played at the bot.
    /// Queues the answer.
    pub fn on_expressed(
        &mut self,
        now_ms: u64,
        e: &Expressed,
        char_id: u32,
        dist: f32,
        directed: bool,
        rng: &mut fastrand::Rng,
    ) {
        if dist > ANSWER_M || recent(&self.answered, char_id, now_ms) {
            return;
        }
        let chance = if directed {
            ANSWER_DIRECT
        } else {
            ANSWER_NEARBY
        };
        if rng.f32() >= chance {
            return;
        }
        remember(&mut self.answered, 6, char_id, now_ms + ANSWER_AGAIN_MS);
        self.queue(Pending {
            due_ms: now_ms + rng.u64(REACT_MS.0..=REACT_MS.1),
            emotion: answer_to(e.emotion),
            target: e.obj,
            char_id,
            class: Class::Social,
        });
    }

    /// Schedules the sit of a rest that begins now.
    pub fn rest_begin(&mut self, now_ms: u64, rng: &mut fastrand::Rng) {
        self.sit_at_ms = now_ms + (spread(rng, SIT_AFTER_S) * 1000.0) as u64;
    }

    /// The rest is over: `true` when the bot has to stand up.
    pub fn rest_end(&mut self) -> bool {
        self.sit_at_ms = 0;
        std::mem::take(&mut self.sitting)
    }

    /// Whether a resting bot sits down now.
    pub fn sit_due(&self, now_ms: u64) -> bool {
        self.sit_at_ms != 0 && !self.sitting && now_ms >= self.sit_at_ms
    }

    /// One tick: what to do now. `budget` spends the process-wide allowance.
    pub fn tick(
        &mut self,
        i: &TickIn,
        traits: &Traits,
        rng: &mut fastrand::Rng,
        mut budget: impl FnMut(Class) -> bool,
    ) -> SmallVec<[Act; 2]> {
        let mut acts = SmallVec::new();
        let now = i.now_ms;
        if self.next_ambient_ms == 0 {
            self.next_ambient_ms = now + (spread(rng, AMBIENT_S) * 1000.0 / traits.chatty) as u64;
        }
        // cheer for a level or a quest
        let cheered = (self.level != 0 && i.level > self.level)
            || (i.online_ms > LOGIN_QUIET_MS && i.quests_done > self.quests_done);
        self.level = i.level;
        self.quests_done = i.quests_done;
        if cheered && i.gate.show() {
            acts.push(Act::Cheer);
            self.queue(Pending {
                due_ms: now + 400,
                emotion: emotion::CELEBRATE,
                target: 0,
                char_id: 0,
                class: Class::Social,
            });
        }
        // a player who walks by is greeted
        if let Some(n) = i.near.filter(|n| n.dist <= GREET_M) {
            if i.gate.show() && !i.moving && !self.sitting && !recent(&self.greeted, n.char_id, now)
            {
                // (the roll is spent either way: a bot that did not greet does not try again
                // on the next tick)
                remember(&mut self.greeted, 6, n.char_id, now + GREET_AGAIN_MS);
                if rng.f32() < GREET_CHANCE {
                    self.queue(Pending {
                        due_ms: now + rng.u64(REACT_MS.0..=REACT_MS.1),
                        emotion: emotion::GREET,
                        target: 0,
                        char_id: n.char_id,
                        class: Class::Social,
                    });
                }
            }
        }
        // an ambient emote now and then in a town
        if i.gate.show()
            && i.gate.town
            && !i.moving
            && !self.sitting
            && self.pending.is_none()
            && now >= self.next_ambient_ms
        {
            self.next_ambient_ms = now + (spread(rng, AMBIENT_S) * 1000.0 / traits.chatty) as u64;
            let total: u32 = AMBIENT.iter().map(|a| a.1).sum();
            let mut roll = rng.u32(0..total);
            let pick = AMBIENT
                .iter()
                .find(|a| {
                    if roll < a.1 {
                        true
                    } else {
                        roll -= a.1;
                        false
                    }
                })
                .map_or(emotion::STRETCH, |a| a.0);
            self.queue(Pending {
                due_ms: now,
                emotion: pick,
                target: 0,
                char_id: 0,
                class: Class::Ambient,
            });
        }
        // the emote whose time has come
        if let Some(p) = self.pending {
            if now >= p.due_ms {
                self.pending = None;
                let still_there = p.char_id == 0 || i.near.is_some_and(|n| n.char_id == p.char_id);
                if still_there && i.gate.show() && self.rested(now) && budget(p.class) {
                    self.last_ms = now;
                    if let Some(n) = i.near.filter(|_| p.char_id != 0 && !i.moving) {
                        acts.push(Act::Face(n.pos));
                    }
                    acts.push(Act::Emote {
                        emotion: p.emotion,
                        target: p.target,
                    });
                }
            }
        }
        acts
    }

    /// The sit emote of a resting bot, if it is time and the allowance and the moment permit.
    pub fn sit(
        &mut self,
        now_ms: u64,
        gate: Gate,
        mut budget: impl FnMut(Class) -> bool,
    ) -> Option<Act> {
        if !self.sit_due(now_ms) {
            return None;
        }
        if !gate.show() || !self.rested(now_ms) || !budget(Class::Ambient) {
            // (no one to see it, or no allowance: try again in a while)
            self.sit_at_ms = now_ms + 15_000;
            return None;
        }
        self.sitting = true;
        self.last_ms = now_ms;
        Some(Act::Emote {
            emotion: emotion::SIT,
            target: 0,
        })
    }
}

// ---- the per-bot state ------------------------------------------------------------------------

/// Everything life-like one bot carries. Owned by its [`Motion`] (one box per bot).
#[derive(Debug)]
pub struct Life {
    /// The bot's character.
    pub traits: Traits,
    /// The moment, set by the bot loop each tick.
    pub gate: Gate,
    /// The jump in progress.
    pub jump: Option<Jump>,
    /// When to jump.
    pub sched: JumpSched,
    /// The second jump of a double is due at this time (ms), 0 = none.
    pub rejump_ms: u64,
    /// Idle mannerisms.
    pub fidget: Fidget,
    /// Pauses at corners.
    pub dawdle: Dawdle,
    /// The bot stands still for a pause at a corner until this time (ms), 0 = no pause.
    pub pause_until_ms: u64,
    /// Emotes.
    pub emotes: Emotes,
    /// Emotes are allowed (`[behavior] emotes`).
    pub emotes_on: bool,
    /// The pace chosen for the trip toward this goal, so that a trip that is started again
    /// keeps it: `(goal, until_ms, walk)`.
    pub pace_memo: Option<([f32; 2], u64, bool)>,
    /// Randomness for the decisions the controller makes outside its tick.
    pub rng: fastrand::Rng,
    /// The current trip is a walk for its own sake (not a chase, a loot walk or a shuffle).
    pub cosmetic: bool,
}

impl Life {
    /// The life of bot `seed` that starts at `now_ms`.
    pub fn new(
        seed: u32,
        jump_rate: f32,
        emotes_on: bool,
        now_ms: u64,
        rng: &mut fastrand::Rng,
    ) -> Life {
        let traits = Traits::new(seed, jump_rate);
        Life {
            sched: JumpSched::new(now_ms, traits.jump_mult, rng),
            traits,
            gate: Gate::default(),
            jump: None,
            rejump_ms: 0,
            fidget: Fidget::default(),
            dawdle: Dawdle::default(),
            pause_until_ms: 0,
            emotes: Emotes::default(),
            emotes_on,
            pace_memo: None,
            rng: fastrand::Rng::with_seed(u64::from(seed) ^ 0x5EED_11FE),
            cosmetic: false,
        }
    }

    /// The jump in progress ended or was cancelled.
    pub fn end_jump(&mut self) {
        self.jump = None;
        self.rejump_ms = 0;
    }

    /// Whether a packet of the jump in progress is due.
    pub fn jump_due(&self, now_ms: u64) -> bool {
        self.jump.is_some_and(|j| now_ms >= j.due_at())
    }

    /// The next time the jump needs the bot loop (bot clock, ms).
    pub fn wake_at(&self) -> Option<u64> {
        let rejump = (self.rejump_ms != 0).then_some(self.rejump_ms);
        let jump = self.jump.map(|j| j.due_at());
        match (jump, rejump) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

// ---- the bot loop's side ----------------------------------------------------------------------

/// The peace-zone aura of the Nui statues in towns.
const PEACE_AURA: u32 = 2149;
/// This many units within 25 m make a crowd.
const CROWD: usize = 8;

/// Whether the bot stands in a town: in the peace aura or among a crowd.
pub fn in_town(st: &BotState) -> bool {
    st.buffs.iter().any(|b| b.1 == PEACE_AURA)
        || st
            .units
            .values()
            .filter(|u| u.dist2d(st.me.pos) <= 25.0)
            .take(CROWD)
            .count()
            >= CROWD
}

/// The nearest real player within `radius` metres; `is_bot` says which character ids are ours.
pub fn nearest_player(st: &BotState, is_bot: &dyn Fn(u32) -> bool, radius: f32) -> Option<Near> {
    let mut best: Option<Near> = None;
    for (&obj, u) in &st.units {
        if u.kind != UnitKind::Character || u.dead() {
            continue;
        }
        let dist = u.dist2d(st.me.pos);
        if dist > radius || best.is_some_and(|b| b.dist <= dist) || is_bot(u.id) {
            continue;
        }
        best = Some(Near {
            char_id: u.id,
            obj,
            pos: [u.pos[0], u.pos[1]],
            dist,
        });
    }
    best
}

/// What the bot loop knows when it calls [`tick`].
#[derive(Debug, Clone, Copy)]
pub struct Moment {
    /// A real player is within the watch radius.
    pub audience: bool,
    /// The bot is busy (see [`Gate::busy`]).
    pub busy: bool,
    /// Milliseconds since the bot entered the world.
    pub online_ms: u64,
}

/// The per-tick hook of the bot loop: tells the movement controller the moment ([`Gate`]) and
/// runs the emotes.
pub fn tick(
    motion: &mut Motion,
    st: &BotState,
    hub: &Hub,
    m: Moment,
    out: &mut Vec<Out>,
    rng: &mut fastrand::Rng,
) {
    tick_with(
        motion,
        st,
        &|id| hub.is_bot_char(id),
        m,
        out,
        rng,
        &mut global_take,
    );
}

/// [`tick`] with the test for our own bots and the emote allowance as functions.
pub fn tick_with(
    motion: &mut Motion,
    st: &BotState,
    is_bot: &dyn Fn(u32) -> bool,
    m: Moment,
    out: &mut Vec<Out>,
    rng: &mut fastrand::Rng,
    budget: &mut dyn FnMut(Class) -> bool,
) {
    let now_ms = st.now_ms;
    let near = nearest_player(st, is_bot, ANSWER_M);
    let obj = motion.obj();
    let moving = motion.is_moving();
    let Some(life) = motion.life_mut() else {
        return;
    };
    let town = m.audience && !m.busy && in_town(st);
    life.gate = Gate {
        audience: m.audience,
        busy: m.busy,
        town,
    };
    if !life.emotes_on && life.traits.jump_mult <= 0.0 {
        return;
    }
    let i = TickIn {
        now_ms,
        online_ms: m.online_ms,
        level: st.me.level,
        quests_done: st.quests.completed.len().min(usize::from(u16::MAX)) as u16,
        near,
        moving,
        gate: life.gate,
    };
    let emotes_on = life.emotes_on;
    let traits = life.traits;
    let acts = life
        .emotes
        .tick(&i, &traits, rng, |c| emotes_on && budget(c));
    for act in acts {
        match act {
            Act::Emote { emotion, target } if obj != 0 => {
                out.push(cs::express_emotion(obj, target, emotion));
            }
            Act::Emote { .. } => {}
            Act::Face([x, y]) => motion.face(x, y),
            Act::Cheer => {
                if let Some(l) = motion.life_mut() {
                    l.sched.cheer();
                }
            }
        }
    }
}

/// An emote of somebody in view reached the bot: queues an answer when a real player played it
/// close by.
pub fn on_expressed(
    motion: &mut Motion,
    st: &BotState,
    hub: &Hub,
    e: &Expressed,
    rng: &mut fastrand::Rng,
) {
    on_expressed_with(motion, st, &|id| hub.is_bot_char(id), e, rng);
}

/// [`on_expressed`] with the test for our own bots as a function.
pub fn on_expressed_with(
    motion: &mut Motion,
    st: &BotState,
    is_bot: &dyn Fn(u32) -> bool,
    e: &Expressed,
    rng: &mut fastrand::Rng,
) {
    let my_obj = motion.obj();
    if e.obj == my_obj || my_obj == 0 {
        return;
    }
    let Some(u) = st.units.get(&e.obj) else {
        return;
    };
    if u.kind != UnitKind::Character || is_bot(u.id) {
        return;
    }
    let dist = u.dist2d(st.me.pos);
    let directed = e.target == my_obj;
    if let Some(life) = motion.life_mut() {
        if life.emotes_on && life.gate.show() {
            life.emotes
                .on_expressed(st.now_ms, e, u.id, dist, directed, rng);
        }
    }
}

// ---- goal hooks -------------------------------------------------------------------------------

/// A rest begins.
pub fn rest_begin(ctx: &mut Ctx<'_>) {
    let now = ctx.now_ms;
    if let Some(l) = ctx.motion.life_mut() {
        l.emotes.rest_begin(now, ctx.rng);
    }
}

/// A rest goes on: the bot looks around, and sits down after a while.
pub fn rest_tick(ctx: &mut Ctx<'_>) {
    let now = ctx.now_ms;
    let obj = ctx.motion.obj();
    let Some(l) = ctx.motion.life_mut() else {
        return;
    };
    let gate = l.gate;
    let emotes_on = l.emotes_on;
    if emotes_on && obj != 0 {
        if let Some(Act::Emote { emotion, target }) = l.emotes.sit(now, gate, global_take) {
            ctx.out.push(cs::express_emotion(obj, target, emotion));
        }
    }
    if !ctx.motion.life().is_some_and(|l| l.emotes.sitting) {
        ctx.motion
            .fidget_tick(now, ctx.rng, ctx.state.place.ground(ctx.world));
    }
}

/// A rest ends: a bot that sits stands up (the movement update that follows is what shows it).
pub fn rest_end(ctx: &mut Ctx<'_>) {
    if ctx.motion.life_mut().is_some_and(|l| l.emotes.rest_end()) {
        ctx.motion.announce();
    }
}

/// A wandering bot pauses between two legs: it looks around.
pub fn wander_pause(ctx: &mut Ctx<'_>) {
    let now = ctx.now_ms;
    ctx.motion
        .fidget_tick(now, ctx.rng, ctx.state.place.ground(ctx.world));
}

// ---- debugging --------------------------------------------------------------------------------

/// Logs the movement and emote packets other units send, at `trace` level under the target
/// `pb_move` (`RUST_LOG=pb_move=trace`): what a real client puts on the wire when it walks,
/// jumps or sits, as the server relays it. The caller checks `tracing::enabled!` first.
pub fn trace_packet(level: u8, opcode: u16, body: &[u8]) {
    use aa_proto::opcodes::sc;
    if level != aa_proto::game::LEVEL_GAME {
        return;
    }
    if opcode == sc::SCOneUnitMovementPacket {
        let _ = trace_move(body);
    } else if aa_proto::sc_emote::is_expressed(level, opcode) {
        if let Ok(e) = aa_proto::sc_emote::read_expressed(body) {
            tracing::trace!(
                target: "pb_move",
                "emote obj={:06x} target={:06x} id={}",
                e.obj,
                e.target,
                e.emotion
            );
        }
    }
}

fn trace_move(body: &[u8]) -> Result<(), aa_proto::MarshalError> {
    let mut s = aa_proto::PacketStream::from_bytes(body);
    let obj = s.read_bc()?;
    let kind = s.read_u8()?;
    if kind != 1 {
        return Ok(());
    }
    let time = s.read_u32()?;
    let flags = s.read_u8()?;
    if flags & 0x10 != 0 {
        s.read_u32()?;
        s.read_u8()?;
    }
    let (x, y, z) = s.read_position()?;
    let vel = [s.read_i16()?, s.read_i16()?, s.read_i16()?];
    let rot = [s.read_i8()?, s.read_i8()?, s.read_i8()?];
    let delta = [s.read_i8()?, s.read_i8()?, s.read_i8()?];
    let stance = s.read_u8()?;
    let alert = s.read_u8()?;
    let actor = s.read_u8()?;
    let fall = if actor & 0x80 != 0 { s.read_u16()? } else { 0 };
    tracing::trace!(
        target: "pb_move",
        "move obj={obj:06x} t={time} fl={flags:02x} pos=({x:.2},{y:.2},{z:.2}) vel={vel:?} rot={rot:?} delta={delta:?} stance={stance} alert={alert} actor={actor:02x} fall={fall}"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
