//! The movement controller: walks a bot to a destination and tells the server about it.
//!
//! The server does not validate movement, so a bot is its own physics: [`Motion::tick`]
//! integrates the position along straight lines through the waypoints at run or walk speed, puts
//! the bot on the terrain ([`surface_z`]) and sends `CSMoveUnit` while moving plus one stopped
//! update on arrival. A stuck detector (less than [`STUCK_MIN_PROGRESS_M`] of progress in
//! [`STUCK_WINDOW_MS`]) inserts side steps and finally reports [`MoveFailure::Stuck`]; every trip
//! also has a [`TRIP_TIMEOUT_MS`] limit.
//!
//! The server only knows the position of the last `CSMoveUnit`, and the update gap grows to a
//! second when no player is near, so the server can see the bot a run-second (5 m) behind. Before
//! a packet whose range the server checks (loot, talk, sell, craft, cast) the caller asks
//! [`Motion::pin`] for an exact update.
//!
//! Nothing here reads a clock or talks to a socket: the caller passes the bot clock and an
//! outbox ([`MotionEnv`]), which keeps the controller testable.

use aa_proto::cs::{self, MoveUnit, Out};
use aa_proto::game::LEVEL_GAME;
use aa_proto::opcodes::cs as op;
use smallvec::SmallVec;

use super::state::{dist2d, dist3d};

/// Sea level of the 1.2 world in metres.
pub const SEA_LEVEL: f32 = 100.0;
/// Terrain below this height is sea floor: the bot swims instead of walking on it.
pub const SWIM_THRESHOLD: f32 = 99.0;
/// Height of a swimming bot (just under the surface).
pub const SWIM_Z: f32 = 98.5;
/// Minimum progress inside [`STUCK_WINDOW_MS`] for the bot not to count as stuck.
pub const STUCK_MIN_PROGRESS_M: f32 = 0.3;
/// Length of the stuck observation window.
pub const STUCK_WINDOW_MS: u64 = 3000;
/// Failed windows after which a trip is abandoned.
pub const STUCK_MAX_TRIES: u8 = 5;
/// Longest a single trip may take.
pub const TRIP_TIMEOUT_MS: u64 = 90_000;
/// How long a `/move` teleport may take before the bot walks instead.
pub const TELEPORT_WAIT_MS: u64 = 10_000;
/// Longest time step one tick integrates (a stalled task must not teleport the bot).
const MAX_STEP_MS: u64 = 1000;
/// A facing change smaller than this (radians) is not worth a packet.
const FACE_EPSILON: f32 = 0.09;
/// The server's idea of the position may be this far off before [`Motion::pin`] corrects it.
const PIN_EPSILON_M: f32 = 0.15;

/// Where the ground is. [`aa_world::World`] answers from the baked height grid.
pub trait Ground {
    /// Terrain height at (x, y), `None` where unknown.
    fn ground_z(&self, x: f32, y: f32) -> Option<f32>;
}

impl Ground for aa_world::World {
    fn ground_z(&self, x: f32, y: f32) -> Option<f32> {
        aa_world::World::ground_z(self, x, y)
    }
}

impl Ground for aa_world::terrain::Terrain {
    fn ground_z(&self, x: f32, y: f32) -> Option<f32> {
        self.height(x, y)
    }
}

impl<F: Fn(f32, f32) -> Option<f32>> Ground for F {
    fn ground_z(&self, x: f32, y: f32) -> Option<f32> {
        self(x, y)
    }
}

/// The height a bot stands at: the terrain, the water surface over sea floor, or `fallback`
/// where the terrain is unknown. A bot is never put under the sea surface.
pub fn surface_z(ground: Option<f32>, fallback: f32) -> f32 {
    match ground {
        Some(g) if g < SWIM_THRESHOLD => SWIM_Z,
        Some(g) => g,
        None => fallback,
    }
}

/// How fast the bot goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Run speed.
    Run,
    /// Walk speed.
    Walk,
}

/// Why a trip failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveFailure {
    /// No progress after several side steps.
    Stuck,
    /// The trip took longer than [`TRIP_TIMEOUT_MS`].
    Timeout,
}

/// What the controller is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionStatus {
    /// Standing, nothing asked.
    Idle,
    /// On the way.
    Moving,
    /// The last destination was reached.
    Arrived,
    /// The last trip was abandoned.
    Failed(MoveFailure),
}

/// Speeds of the controller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionCfg {
    /// Run speed in m/s.
    pub run_speed: f32,
    /// Walk speed in m/s.
    pub walk_speed: f32,
}

impl Default for MotionCfg {
    fn default() -> Self {
        Self {
            run_speed: 5.4,
            walk_speed: 1.8,
        }
    }
}

/// What one [`Motion::tick`] works with.
pub struct MotionEnv<'a> {
    /// The terrain.
    pub ground: &'a dyn Ground,
    /// The bot clock in milliseconds.
    pub now_ms: u64,
    /// Packets to send.
    pub out: &'a mut Vec<Out>,
    /// Randomness for side steps.
    pub rng: &'a mut fastrand::Rng,
    /// Minimum time between two "moving" updates (the tick time, or a second when nobody is
    /// watching).
    pub send_gap_ms: u64,
    /// The bot is fighting: weapons drawn.
    pub combat: bool,
}

/// Verdict of the stuck detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StuckVerdict {
    /// Making progress, or the window is not over.
    Fine,
    /// No progress: try a side step.
    Sidestep,
    /// Too many failed windows.
    GiveUp,
}

/// Detects a bot that does not get anywhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stuck {
    anchor: [f32; 2],
    since_ms: u64,
    tries: u8,
    flip: bool,
}

impl Stuck {
    /// Starts watching from `pos` at `now_ms`.
    pub fn reset(&mut self, now_ms: u64, pos: [f32; 3]) {
        *self = Stuck {
            anchor: [pos[0], pos[1]],
            since_ms: now_ms,
            tries: 0,
            flip: false,
        };
    }

    /// Looks at the bot's position; call every tick while it should be moving.
    pub fn observe(&mut self, now_ms: u64, pos: [f32; 3]) -> StuckVerdict {
        if now_ms.saturating_sub(self.since_ms) < STUCK_WINDOW_MS {
            return StuckVerdict::Fine;
        }
        let moved = ((self.anchor[0] - pos[0]).powi(2) + (self.anchor[1] - pos[1]).powi(2)).sqrt();
        self.anchor = [pos[0], pos[1]];
        self.since_ms = now_ms;
        if moved >= STUCK_MIN_PROGRESS_M {
            self.tries = 0;
            return StuckVerdict::Fine;
        }
        self.tries += 1;
        if self.tries >= STUCK_MAX_TRIES {
            StuckVerdict::GiveUp
        } else {
            StuckVerdict::Sidestep
        }
    }

    /// The side the next side step goes to (alternates).
    fn next_side(&mut self) -> f32 {
        self.flip = !self.flip;
        if self.flip {
            1.0
        } else {
            -1.0
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingTeleport {
    deadline_ms: u64,
}

/// The movement controller of one bot. See the module docs.
#[derive(Debug)]
pub struct Motion {
    cfg: MotionCfg,
    obj: u32,
    pos: [f32; 3],
    yaw: f32,
    path: SmallVec<[(f32, f32); 3]>,
    radius: f32,
    pace: Pace,
    status: MotionStatus,
    /// The last packet announced motion.
    moving_sent: bool,
    /// A stopped update is owed (after `stop`, a face change or an arrival).
    stop_due: bool,
    force_send: bool,
    last_tick_ms: u64,
    last_send_ms: u64,
    started_ms: u64,
    stuck: Stuck,
    pending_tp: Option<PendingTeleport>,
    /// Where the last `CSMoveUnit` (or the server's own placement) put the bot, as the server
    /// knows it.
    sent_pos: [f32; 3],
}

impl Motion {
    /// A controller that is not in the world yet; [`Motion::enter_world`] places it.
    pub fn new(cfg: MotionCfg) -> Motion {
        Motion {
            cfg,
            obj: 0,
            pos: [0.0; 3],
            yaw: 0.0,
            path: SmallVec::new(),
            radius: 0.0,
            pace: Pace::Run,
            status: MotionStatus::Idle,
            moving_sent: false,
            stop_due: false,
            force_send: false,
            last_tick_ms: 0,
            last_send_ms: 0,
            started_ms: 0,
            stuck: Stuck::default(),
            pending_tp: None,
            sent_pos: [0.0; 3],
        }
    }

    /// Places the bot in the world (after the spawn): its object id, position and facing.
    pub fn enter_world(&mut self, obj: u32, pos: [f32; 3], yaw: f32, now_ms: u64) {
        self.obj = obj;
        self.pos = pos;
        self.sent_pos = pos;
        self.yaw = yaw;
        self.path.clear();
        self.status = MotionStatus::Idle;
        self.moving_sent = false;
        self.stop_due = false;
        self.pending_tp = None;
        self.last_tick_ms = now_ms;
    }

    /// Takes the bot out of the world (disconnect).
    pub fn leave_world(&mut self) {
        self.obj = 0;
        self.path.clear();
        self.status = MotionStatus::Idle;
        self.moving_sent = false;
        self.stop_due = false;
        self.pending_tp = None;
    }

    /// Current position.
    pub fn pos(&self) -> [f32; 3] {
        self.pos
    }

    /// Current facing in radians (0 = +Y, counter-clockwise).
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// What the controller is doing.
    pub fn status(&self) -> MotionStatus {
        self.status
    }

    /// Whether a trip is under way.
    pub fn is_moving(&self) -> bool {
        self.status == MotionStatus::Moving
    }

    /// Whether the controller still owes the server a packet (so the bot is not "idle" yet).
    pub fn busy(&self) -> bool {
        self.is_moving() || self.stop_due
    }

    /// Horizontal distance to the end of the path, `None` when not moving.
    pub fn remaining(&self) -> Option<f32> {
        let last = self.path.last()?;
        Some(dist2d(self.pos, [last.0, last.1, 0.0]))
    }

    /// The destination of the current trip.
    pub fn destination(&self) -> Option<(f32, f32)> {
        self.path.last().copied()
    }

    /// Speed of a pace in m/s.
    pub fn speed(&self, pace: Pace) -> f32 {
        match pace {
            Pace::Run => self.cfg.run_speed,
            Pace::Walk => self.cfg.walk_speed,
        }
    }

    /// Starts a trip to (x, y); the bot stops `radius` metres short of it.
    pub fn go_to(&mut self, x: f32, y: f32, radius: f32, pace: Pace, now_ms: u64) {
        self.go_via(&[(x, y)], radius, pace, now_ms);
    }

    /// Starts a trip through `points` (the last one is the destination).
    pub fn go_via(&mut self, points: &[(f32, f32)], radius: f32, pace: Pace, now_ms: u64) {
        self.path.clear();
        self.path.extend(
            points
                .iter()
                .copied()
                .filter(|p| p.0.is_finite() && p.1.is_finite()),
        );
        self.radius = radius.max(0.0);
        self.pace = pace;
        self.pending_tp = None;
        self.started_ms = now_ms;
        self.stuck.reset(now_ms, self.pos);
        self.force_send = true;
        self.status = if self.path.is_empty() {
            MotionStatus::Idle
        } else {
            MotionStatus::Moving
        };
    }

    /// Like [`Motion::go_to`], but a trip longer than `teleport_over_m` (when positive) asks for a
    /// GM `/move`: the destination `[x, y, z]` is returned and the caller sends the command (all
    /// GM lines are made in one place, `Ctx::gm`) and then calls [`Motion::begin_teleport`]; if
    /// the server does not move the bot within [`TELEPORT_WAIT_MS`] the bot walks. Only call with
    /// `teleport_over_m > 0` when the account may use GM commands.
    pub fn travel_to(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        pace: Pace,
        teleport_over_m: f32,
        env: &mut MotionEnv<'_>,
    ) -> Option<[f32; 3]> {
        self.go_to(x, y, radius, pace, env.now_ms);
        if teleport_over_m > 0.0 && dist2d(self.pos, [x, y, 0.0]) > teleport_over_m {
            let z = env
                .ground
                .ground_z(x, y)
                .map_or(self.pos[2], |g| surface_z(Some(g), self.pos[2]) + 1.0);
            return Some([x, y, z]);
        }
        None
    }

    /// The `/move` asked for by [`Motion::travel_to`] was sent: wait for the server to move the
    /// bot instead of walking.
    pub fn begin_teleport(&mut self, now_ms: u64) {
        self.pending_tp = Some(PendingTeleport {
            deadline_ms: now_ms + TELEPORT_WAIT_MS,
        });
    }

    /// Stops walking; a stopped update follows on the next tick.
    pub fn stop(&mut self) {
        if self.status == MotionStatus::Moving || self.moving_sent {
            self.stop_due = true;
        }
        self.path.clear();
        self.pending_tp = None;
        self.status = MotionStatus::Idle;
    }

    /// Turns towards (x, y); a stopped update with the new facing follows if the bot stands.
    pub fn face(&mut self, x: f32, y: f32) {
        let yaw = cs::yaw_towards((self.pos[0], self.pos[1]), (x, y));
        if (yaw - self.yaw).abs() > FACE_EPSILON {
            self.yaw = yaw;
            if !self.is_moving() {
                self.stop_due = true;
            }
        }
    }

    /// The server put the bot at `pos` (teleport, resurrection, instance load): forget the
    /// trip unless a GM teleport was what moved it.
    pub fn teleported(&mut self, pos: [f32; 3]) {
        self.pos = pos;
        self.sent_pos = pos;
        if self.pending_tp.take().is_some() {
            if let Some(&(x, y)) = self.path.last() {
                if dist2d(pos, [x, y, 0.0]) <= self.radius + 10.0 {
                    self.path.clear();
                    self.status = MotionStatus::Arrived;
                }
            }
            self.stuck.reset(self.last_tick_ms, pos);
        } else {
            self.path.clear();
            self.status = MotionStatus::Idle;
        }
        self.moving_sent = false;
        self.stop_due = false;
    }

    /// A navigation mesh search finished (owned by the motion feature): `result` answers the
    /// request that was made with `ticket`. Delivered by `BotCmd::PathReady`; does nothing yet.
    pub fn on_path_ready(&mut self, _ticket: u32, _result: aa_world::nav::PathResult) {}

    /// Integrates the position by the time since the last tick and sends the packets.
    pub fn tick(&mut self, env: &mut MotionEnv<'_>) -> MotionStatus {
        let now = env.now_ms;
        let raw_dt_ms = now.saturating_sub(self.last_tick_ms);
        let dt_ms = raw_dt_ms.min(MAX_STEP_MS);
        self.last_tick_ms = now;
        if raw_dt_ms > MAX_STEP_MS {
            // The task was descheduled: the missing progress is not the terrain's fault.
            self.stuck.reset(now, self.pos);
        }
        if self.obj == 0 {
            return self.status;
        }
        if let Some(tp) = self.pending_tp {
            if now < tp.deadline_ms {
                return self.status; // waiting for SCTeleportUnit
            }
            self.pending_tp = None;
        }
        if self.path.is_empty() {
            if self.stop_due {
                self.send(env, false, (0.0, 0.0));
            }
            return self.status;
        }
        if now.saturating_sub(self.started_ms) > TRIP_TIMEOUT_MS {
            return self.fail(env, MoveFailure::Timeout);
        }
        match self.stuck.observe(now, self.pos) {
            StuckVerdict::Fine => {}
            StuckVerdict::GiveUp => return self.fail(env, MoveFailure::Stuck),
            StuckVerdict::Sidestep => self.sidestep(env),
        }

        let speed = self.speed(self.pace);
        let mut budget = speed * dt_ms as f32 / 1000.0;
        let mut heading = (0.0f32, 0.0f32);
        while budget > 0.0 {
            let Some(&(tx, ty)) = self.path.first() else {
                break;
            };
            let last = self.path.len() == 1;
            let stop_short = if last { self.radius } else { 0.5 };
            let (dx, dy) = (tx - self.pos[0], ty - self.pos[1]);
            let dist = (dx * dx + dy * dy).sqrt();
            let to_go = dist - stop_short;
            if to_go <= 0.02 {
                if last {
                    self.path.clear();
                    self.status = MotionStatus::Arrived;
                    break;
                }
                self.path.remove(0);
                continue;
            }
            let (ux, uy) = (dx / dist, dy / dist);
            heading = (ux, uy);
            let step = budget.min(to_go);
            self.pos[0] += ux * step;
            self.pos[1] += uy * step;
            budget -= step;
            if step >= to_go - 1e-4 && last {
                self.path.clear();
                self.status = MotionStatus::Arrived;
                break;
            }
            if step >= to_go - 1e-4 {
                self.path.remove(0);
            }
        }
        if heading != (0.0, 0.0) {
            self.yaw = cs::yaw_towards((0.0, 0.0), heading);
        }
        self.pos[2] = surface_z(env.ground.ground_z(self.pos[0], self.pos[1]), self.pos[2]);

        if self.status == MotionStatus::Arrived {
            self.send(env, false, (0.0, 0.0));
        } else if heading != (0.0, 0.0) {
            let due = self.force_send
                || !self.moving_sent
                || now.saturating_sub(self.last_send_ms) >= env.send_gap_ms;
            if due {
                self.send(env, true, (heading.0 * speed, heading.1 * speed));
            }
        }
        self.status
    }

    fn fail(&mut self, env: &mut MotionEnv<'_>, why: MoveFailure) -> MotionStatus {
        self.path.clear();
        self.status = MotionStatus::Failed(why);
        self.stop_due = self.moving_sent;
        if self.stop_due {
            self.send(env, false, (0.0, 0.0));
        }
        self.status
    }

    /// Inserts a waypoint 2..4 m to the side of the way ahead.
    fn sidestep(&mut self, env: &mut MotionEnv<'_>) {
        let Some(&(tx, ty)) = self.path.first() else {
            return;
        };
        let (dx, dy) = (tx - self.pos[0], ty - self.pos[1]);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let side = self.stuck.next_side();
        let off = 2.0 + env.rng.f32() * 2.0;
        let (px, py) = (-dy / len * off * side, dx / len * off * side);
        if self.path.len() < 6 {
            self.path.insert(0, (self.pos[0] + px, self.pos[1] + py));
        }
        self.force_send = true;
    }

    /// Queues one `CSMoveUnit` and updates the bookkeeping.
    fn send(&mut self, env: &mut MotionEnv<'_>, moving: bool, vel: (f32, f32)) {
        let packet = self.packet(env.now_ms, env.combat, moving, vel);
        env.out.push(packet);
    }

    /// Builds one `CSMoveUnit` for the current position and updates the bookkeeping.
    fn packet(&mut self, now_ms: u64, combat: bool, moving: bool, vel: (f32, f32)) -> Out {
        let packet = cs::move_unit(&MoveUnit {
            obj: self.obj,
            time: now_ms as u32,
            pos: (self.pos[0], self.pos[1], self.pos[2]),
            vel: (vel.0, vel.1, 0.0),
            yaw: self.yaw,
            moving,
            stance: if combat {
                cs::STANCE_COMBAT
            } else {
                cs::STANCE_RELAXED
            },
            alertness: if combat {
                cs::ALERT_COMBAT
            } else {
                cs::ALERT_IDLE
            },
        });
        self.moving_sent = moving;
        self.stop_due = false;
        self.force_send = false;
        self.last_send_ms = now_ms;
        self.sent_pos = self.pos;
        packet
    }

    /// An exact `CSMoveUnit` for the position the bot has now, when the server's idea of it is
    /// off: the bot stands but the server last heard it moving, or the last update is more than
    /// [`PIN_EPSILON_M`] away. Send it right before a packet the server checks a distance for
    /// (loot, talk, sell, craft, cast), at the position of the packet in the outbox: the movement
    /// updates of a walking bot are a second apart when nobody is near, which leaves the server
    /// metres behind. A standing bot gets a stopped update, a walking one a moving update.
    pub fn pin(&mut self, now_ms: u64, combat: bool) -> Option<Out> {
        if self.obj == 0 || self.pending_tp.is_some() {
            return None;
        }
        let moving = self.is_moving();
        let off = dist3d(self.sent_pos, self.pos) > PIN_EPSILON_M;
        if !off && self.moving_sent == moving {
            return None;
        }
        let mut vel = (0.0, 0.0);
        if moving {
            if let Some(&(tx, ty)) = self.path.first() {
                let (dx, dy) = (tx - self.pos[0], ty - self.pos[1]);
                let len = (dx * dx + dy * dy).sqrt();
                if len > 1e-3 {
                    let speed = self.speed(self.pace);
                    vel = (dx / len * speed, dy / len * speed);
                    self.yaw = cs::yaw_towards((0.0, 0.0), (dx / len, dy / len));
                }
            }
        }
        Some(self.packet(now_ms, combat, moving && vel != (0.0, 0.0), vel))
    }
}

/// Whether the server checks the distance between the bot and something for this packet: casts
/// (`TOO_FAR`), opening and emptying a loot window (5 m), talking to an NPC or doodad, quest
/// steps, crafting at a bench and selling a trade pack (2.5 m).
pub fn range_checked(p: &Out) -> bool {
    p.level == LEVEL_GAME
        && matches!(
            p.opcode,
            op::CSStartSkillPacket
                | op::CSInteractNPCPacket
                | op::CSStartInteractionPacket
                | op::CSLootOpenBagPacket
                | op::CSLootItemPacket
                | op::CSStartQuestContextPacket
                | op::CSQuestTalkMadePacket
                | op::CSCompleteQuestContextPacket
                | op::CSExecuteCraft
                | op::CSSellBackpackGoodsPacket
        )
}

/// Puts [`Motion::pin`]'s exact update in `out` right before the first [`range_checked`] packet
/// at or after index `from` (the packets the caller queued since it looked), when the server's
/// idea of the position is off.
pub fn pin_before_range_checked(
    motion: &mut Motion,
    out: &mut Vec<Out>,
    from: usize,
    now_ms: u64,
    combat: bool,
) {
    let Some(at) = out
        .get(from..)
        .and_then(|queued| queued.iter().position(range_checked))
    else {
        return;
    };
    if let Some(pin) = motion.pin(now_ms, combat) {
        out.insert(from + at, pin);
    }
}

/// Distance between two bot positions when the height matters (loot and interaction range).
pub fn reach(a: [f32; 3], b: [f32; 3]) -> f32 {
    dist3d(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    use aa_world::terrain::{HeightGridHeader, Terrain};

    const T0: u64 = 1_000;

    /// 64x64 samples of 4 m: a ramp rising 0.1 m per metre in x from 120 m.
    fn ramp() -> Terrain {
        let (w, h) = (64u32, 64u32);
        let header = HeightGridHeader {
            width: w,
            height: h,
            origin_x: 0.0,
            origin_y: 0.0,
            step: 4.0,
            scale: 1.0 / 32.0,
            offset: 0.0,
            flags: 0,
        };
        let mut samples = Vec::new();
        for _row in 0..h {
            for col in 0..w {
                let metres = 120.0 + col as f32 * 4.0 * 0.1;
                samples.push((metres * 32.0) as u16);
            }
        }
        Terrain::from_samples(header, samples).unwrap()
    }

    struct Rig {
        motion: Motion,
        out: Vec<Out>,
        rng: fastrand::Rng,
        now: u64,
    }

    impl Rig {
        fn new(pos: [f32; 3]) -> Rig {
            let mut motion = Motion::new(MotionCfg::default());
            motion.enter_world(0x400, pos, 0.0, T0);
            Rig {
                motion,
                out: Vec::new(),
                rng: fastrand::Rng::with_seed(4),
                now: T0,
            }
        }

        fn tick(&mut self, ground: &dyn Ground, dt: u64, gap: u64) -> MotionStatus {
            self.now += dt;
            let mut env = MotionEnv {
                ground,
                now_ms: self.now,
                out: &mut self.out,
                rng: &mut self.rng,
                send_gap_ms: gap,
                combat: false,
            };
            self.motion.tick(&mut env)
        }
    }

    fn flags(p: &Out) -> u8 {
        assert_eq!(p.opcode, op::CSMoveUnitPacket);
        p.body[8]
    }

    #[test]
    fn walks_a_straight_line_at_run_speed_and_stops_with_one_packet() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        assert_eq!(rig.motion.status(), MotionStatus::Moving);
        let mut ticks = 0;
        while rig.tick(&t, 250, 250) == MotionStatus::Moving {
            ticks += 1;
            assert!(ticks < 200);
        }
        assert_eq!(rig.motion.status(), MotionStatus::Arrived);
        // 50 m at 5.4 m/s = 9.26 s = 37 ticks of 250 ms (the last one is partial).
        assert!((36..=38).contains(&(ticks + 1)), "{ticks} ticks");
        let pos = rig.motion.pos();
        assert!(
            (pos[0] - 60.0).abs() < 0.01 && (pos[1] - 10.0).abs() < 0.01,
            "{pos:?}"
        );
        // z follows the terrain: 120 + 0.1 * 60
        assert!((pos[2] - 126.0).abs() < 0.2, "z = {}", pos[2]);
        // a moving update per tick, then exactly one stopped update
        let moving = rig.out.iter().filter(|p| flags(p) == 0x02).count();
        let stopped = rig.out.iter().filter(|p| flags(p) == 0x04).count();
        assert_eq!(stopped, 1);
        assert!(moving >= 36, "{moving}");
        assert_eq!(flags(rig.out.last().unwrap()), 0x04);
        // no further packets while standing
        let n = rig.out.len();
        rig.tick(&t, 250, 250);
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), n);
    }

    #[test]
    fn faces_the_direction_of_travel() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        rig.tick(&t, 250, 250);
        // heading +X is yaw -pi/2 (0 faces +Y, positive is counter-clockwise)
        assert!((rig.motion.yaw() + std::f32::consts::FRAC_PI_2).abs() < 0.01);
        let first = &rig.out[0];
        assert_eq!(flags(first), 0x02);
        // rotation z byte at offset 3 (bc) + 1 + 4 + 1 + 9 + 6 + 2
        assert_eq!(
            first.body[26] as i8,
            cs::yaw_to_direction(-std::f32::consts::FRAC_PI_2)
        );
        // velocity x is the run speed in mm/s
        let vx = i16::from_le_bytes([first.body[18], first.body[19]]);
        assert_eq!(vx, 5400);
    }

    /// The exact stopped update the bot would send now.
    fn stopped_here(rig: &Rig) -> Out {
        cs::move_unit(&MoveUnit {
            obj: 0x400,
            time: rig.now as u32,
            pos: (
                rig.motion.pos()[0],
                rig.motion.pos()[1],
                rig.motion.pos()[2],
            ),
            vel: (0.0, 0.0, 0.0),
            yaw: rig.motion.yaw(),
            moving: false,
            stance: cs::STANCE_RELAXED,
            alertness: cs::ALERT_IDLE,
        })
    }

    #[test]
    fn pin_stops_the_server_lagging_behind_a_bot_that_halts() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        assert!(
            rig.motion.pin(rig.now, false).is_none(),
            "standing where the server put it"
        );
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        // nobody is near: one update, then a second between them
        for _ in 0..3 {
            rig.tick(&t, 250, 1000);
        }
        assert_eq!(rig.out.len(), 1, "the gap holds the updates back");
        // an action is about to be sent: the bot halts and the server must know where
        rig.motion.stop();
        let pin = rig
            .motion
            .pin(rig.now, false)
            .expect("the server is behind");
        assert_eq!(
            pin,
            stopped_here(&rig),
            "a stopped update at the exact position"
        );
        assert!(rig.motion.pos()[0] > 14.0, "the bot has run on");
        // nothing else is owed: no second stopped update, no second pin
        assert!(rig.motion.pin(rig.now, false).is_none());
        let n = rig.out.len();
        rig.tick(&t, 250, 1000);
        assert_eq!(rig.out.len(), n);
    }

    #[test]
    fn pin_of_a_walking_bot_is_a_moving_update_and_a_standing_bot_needs_none() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        for _ in 0..3 {
            rig.tick(&t, 250, 1000);
        }
        let pin = rig.motion.pin(rig.now, true).expect("the server is behind");
        assert_eq!(flags(&pin), 0x02, "still walking");
        // the walk goes on from the pinned position; the next update waits for the gap
        let n = rig.out.len();
        rig.tick(&t, 250, 1000);
        assert_eq!(rig.out.len(), n);
        // arrival sends the stopped update itself: a pin after it has nothing to add
        rig.motion.go_to(40.0, 10.0, 0.0, Pace::Run, rig.now);
        while rig.tick(&t, 250, 1000) == MotionStatus::Moving {}
        assert_eq!(flags(rig.out.last().unwrap()), 0x04);
        assert!(rig.motion.pin(rig.now, false).is_none());
        // a teleport is the server's own placement
        rig.motion.teleported([500.0, 500.0, 130.0]);
        assert!(rig.motion.pin(rig.now, false).is_none());
    }

    #[test]
    fn the_pin_goes_before_the_first_range_checked_packet() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        for _ in 0..3 {
            rig.tick(&t, 250, 1000);
        }
        rig.motion.stop();
        // a ping, then an unrelated chat, then a loot request and a cast
        let mut out = vec![cs::ping(1), cs::chat(cs::chat_type::SAY, "", "hi")];
        let from = out.len();
        out.push(cs::loot_open_bag(7, true));
        out.push(cs::start_skill_unit(18132, 0x400, 9));
        assert!(range_checked(&out[2]) && range_checked(&out[3]));
        assert!(!range_checked(&out[0]) && !range_checked(&out[1]));
        pin_before_range_checked(&mut rig.motion, &mut out, from, rig.now, false);
        assert_eq!(out.len(), 5);
        assert_eq!(out[2], stopped_here(&rig), "right before the loot request");
        assert_eq!(out[3].opcode, op::CSLootOpenBagPacket);
        // once is enough: the second packet needs no second pin
        pin_before_range_checked(&mut rig.motion, &mut out, from, rig.now, false);
        assert_eq!(out.len(), 5);
        // packets that are not range checked, or queued before `from`, get none
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(60.0, 10.0, 0.0, Pace::Run, rig.now);
        for _ in 0..3 {
            rig.tick(&t, 250, 1000);
        }
        rig.motion.stop();
        let mut out = vec![cs::loot_open_bag(7, true), cs::ping(1)];
        pin_before_range_checked(&mut rig.motion, &mut out, 1, rig.now, false);
        assert_eq!(out.len(), 2);
        assert!(
            rig.motion.pin(rig.now, false).is_some(),
            "nothing was spent"
        );
    }

    #[test]
    fn stops_at_the_requested_radius() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(40.0, 10.0, 3.0, Pace::Run, rig.now);
        while rig.tick(&t, 250, 250) == MotionStatus::Moving {}
        let pos = rig.motion.pos();
        assert!((pos[0] - 37.0).abs() < 0.05, "{pos:?}");
        assert_eq!(rig.motion.status(), MotionStatus::Arrived);
        // already inside the radius: arrives without moving
        rig.motion.go_to(38.0, 10.0, 3.0, Pace::Run, rig.now);
        assert_eq!(rig.tick(&t, 250, 250), MotionStatus::Arrived);
        assert!((rig.motion.pos()[0] - 37.0).abs() < 0.05);
    }

    #[test]
    fn walk_pace_is_slower_and_waypoints_are_followed_in_order() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion
            .go_via(&[(10.0, 20.0), (20.0, 20.0)], 0.0, Pace::Walk, rig.now);
        let mut seconds = 0.0;
        while rig.tick(&t, 250, 250) == MotionStatus::Moving {
            seconds += 0.25;
            assert!(seconds < 60.0);
        }
        // 20 m at 1.8 m/s = 11 s
        assert!((10.5..=12.0).contains(&seconds), "{seconds}");
        let pos = rig.motion.pos();
        assert!((pos[0] - 20.0).abs() < 0.05 && (pos[1] - 20.0).abs() < 0.05);
        // it went via (10, 20): the first moving packet heads +Y (yaw 0)
        assert_eq!(rig.out[0].body[26] as i8, 0);
    }

    #[test]
    fn sea_floor_is_swum_over_and_unknown_ground_keeps_the_height() {
        let sea = |_: f32, _: f32| Some(60.0);
        let mut rig = Rig::new([10.0, 10.0, 98.5]);
        rig.motion.go_to(30.0, 10.0, 0.0, Pace::Run, rig.now);
        while rig.tick(&sea, 250, 250) == MotionStatus::Moving {}
        assert_eq!(rig.motion.pos()[2], SWIM_Z);

        let nothing = |_: f32, _: f32| None;
        let mut rig = Rig::new([10.0, 10.0, 133.0]);
        rig.motion.go_to(30.0, 10.0, 0.0, Pace::Run, rig.now);
        while rig.tick(&nothing, 250, 250) == MotionStatus::Moving {}
        assert_eq!(rig.motion.pos()[2], 133.0);

        assert_eq!(surface_z(Some(98.9), 0.0), SWIM_Z);
        assert_eq!(surface_z(Some(99.0), 0.0), 99.0);
        assert_eq!(surface_z(Some(300.0), 0.0), 300.0);
        assert_eq!(surface_z(None, 7.0), 7.0);
    }

    #[test]
    fn the_send_rate_drops_when_nobody_watches() {
        let t = ramp();
        let count = |gap: u64| {
            let mut rig = Rig::new([10.0, 10.0, 121.0]);
            rig.motion.go_to(400.0, 10.0, 0.0, Pace::Run, rig.now);
            for _ in 0..40 {
                rig.tick(&t, 250, gap); // 10 s
            }
            rig.out.iter().filter(|p| flags(p) == 0x02).count()
        };
        let busy = count(250);
        let quiet = count(1000);
        assert_eq!(busy, 40);
        assert!((10..=11).contains(&quiet), "{quiet}");
    }

    #[test]
    fn stop_and_face_send_a_stopped_update() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(400.0, 10.0, 0.0, Pace::Run, rig.now);
        rig.tick(&t, 250, 250);
        rig.tick(&t, 250, 250);
        rig.out.clear();
        rig.motion.stop();
        assert_eq!(rig.motion.status(), MotionStatus::Idle);
        assert!(rig.motion.busy());
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), 1);
        assert_eq!(flags(&rig.out[0]), 0x04);
        assert!(!rig.motion.busy());
        // facing a point north while standing
        rig.motion
            .face(rig.motion.pos()[0], rig.motion.pos()[1] + 5.0);
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), 2);
        assert_eq!(rig.out[1].body[26] as i8, 0);
        // facing the same way again sends nothing
        rig.motion
            .face(rig.motion.pos()[0], rig.motion.pos()[1] + 9.0);
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), 2);
        // standing still and stopping again sends nothing
        rig.motion.stop();
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), 2);
    }

    #[test]
    fn a_bot_that_cannot_move_side_steps_and_then_gives_up() {
        let t = ramp();
        let cfg = MotionCfg {
            run_speed: 0.05, // effectively frozen
            ..MotionCfg::default()
        };
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion = Motion::new(cfg);
        rig.motion.enter_world(0x400, [10.0, 10.0, 121.0], 0.0, T0);
        rig.motion.go_to(200.0, 10.0, 0.0, Pace::Run, rig.now);
        let mut seconds = 0;
        let mut sides = 0;
        let mut last_path_len = 1;
        loop {
            let st = rig.tick(&t, 1000, 1000);
            seconds += 1;
            let len = rig.motion.path.len();
            if len > last_path_len {
                sides += 1;
            }
            last_path_len = len;
            if st != MotionStatus::Moving {
                assert_eq!(st, MotionStatus::Failed(MoveFailure::Stuck));
                break;
            }
            assert!(seconds < 60);
        }
        assert!((12..=17).contains(&seconds), "gave up after {seconds} s");
        assert!(sides >= 3, "{sides} side steps");
        // the failure ends with a stopped update
        assert_eq!(flags(rig.out.last().unwrap()), 0x04);
    }

    #[test]
    fn a_slow_trip_times_out() {
        let t = ramp();
        let cfg = MotionCfg {
            run_speed: 0.5, // 1.5 m per window: not stuck, but never arrives
            ..MotionCfg::default()
        };
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion = Motion::new(cfg);
        rig.motion.enter_world(0x400, [10.0, 10.0, 121.0], 0.0, T0);
        rig.motion.go_to(10.0, 600.0, 0.0, Pace::Run, rig.now);
        let mut seconds = 0;
        let end = loop {
            let st = rig.tick(&t, 1000, 1000);
            seconds += 1;
            if st != MotionStatus::Moving {
                break st;
            }
            assert!(seconds < 200);
        };
        assert_eq!(end, MotionStatus::Failed(MoveFailure::Timeout));
        assert!((90..=92).contains(&seconds), "{seconds}");
    }

    #[test]
    fn the_stuck_detector_in_isolation() {
        let mut s = Stuck::default();
        s.reset(0, [0.0, 0.0, 0.0]);
        assert_eq!(s.observe(1000, [0.0, 0.0, 0.0]), StuckVerdict::Fine);
        assert_eq!(s.observe(3000, [0.1, 0.0, 0.0]), StuckVerdict::Sidestep);
        // progress clears the count
        assert_eq!(s.observe(6000, [5.0, 0.0, 0.0]), StuckVerdict::Fine);
        for i in 0..4 {
            assert_eq!(
                s.observe(9000 + 3000 * i, [5.0, 0.0, 0.0]),
                StuckVerdict::Sidestep,
                "try {i}"
            );
        }
        assert_eq!(s.observe(21_000, [5.0, 0.0, 0.0]), StuckVerdict::GiveUp);
        // the side alternates
        assert_ne!(s.next_side(), s.next_side());
    }

    #[test]
    fn a_long_stall_does_not_teleport_the_bot() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(400.0, 10.0, 0.0, Pace::Run, rig.now);
        rig.tick(&t, 60_000, 250); // the task was descheduled for a minute
        assert!(
            (rig.motion.pos()[0] - 15.4).abs() < 0.01,
            "{:?}",
            rig.motion.pos()
        );
    }

    #[test]
    fn gm_teleport_for_long_trips_and_the_walking_fallback() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        let now = rig.now;
        {
            let mut env = MotionEnv {
                ground: &t,
                now_ms: now,
                out: &mut rig.out,
                rng: &mut rig.rng,
                send_gap_ms: 250,
                combat: false,
            };
            // short trips walk
            let jump = rig
                .motion
                .travel_to(30.0, 10.0, 0.0, Pace::Run, 100.0, &mut env);
            assert_eq!(jump, None);
        }
        assert!(rig.out.is_empty());
        assert_eq!(rig.motion.status(), MotionStatus::Moving);
        {
            let mut env = MotionEnv {
                ground: &t,
                now_ms: now,
                out: &mut rig.out,
                rng: &mut rig.rng,
                send_gap_ms: 250,
                combat: false,
            };
            let jump = rig
                .motion
                .travel_to(200.0, 10.0, 2.0, Pace::Run, 100.0, &mut env);
            // the motion controller sends no chat itself: it names the destination and the
            // caller (Ctx::gm) makes the safe GM line
            let [jx, jy, _] = jump.expect("a long trip asks for a teleport");
            assert_eq!((jx, jy), (200.0, 10.0));
        }
        assert!(rig.out.is_empty());
        rig.motion.begin_teleport(now);
        // while the teleport is pending the bot does not walk
        rig.tick(&t, 250, 250);
        assert_eq!(rig.motion.pos()[0], 10.0);
        // the server answers: arrived
        rig.motion.teleported([199.0, 10.0, 140.0]);
        assert_eq!(rig.motion.status(), MotionStatus::Arrived);
        assert_eq!(rig.motion.pos(), [199.0, 10.0, 140.0]);

        // an unanswered /move falls back to walking after the wait
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        let now = rig.now;
        {
            let mut env = MotionEnv {
                ground: &t,
                now_ms: now,
                out: &mut rig.out,
                rng: &mut rig.rng,
                send_gap_ms: 250,
                combat: false,
            };
            let jump = rig
                .motion
                .travel_to(200.0, 10.0, 0.0, Pace::Run, 100.0, &mut env);
            assert!(jump.is_some());
        }
        rig.motion.begin_teleport(now);
        for _ in 0..5 {
            rig.tick(&t, 2500, 250);
        }
        assert!(rig.motion.pos()[0] > 10.0);
    }

    #[test]
    fn a_server_teleport_cancels_the_trip() {
        let t = ramp();
        let mut rig = Rig::new([10.0, 10.0, 121.0]);
        rig.motion.go_to(100.0, 10.0, 0.0, Pace::Run, rig.now);
        rig.tick(&t, 250, 250);
        rig.motion.teleported([500.0, 500.0, 130.0]);
        assert_eq!(rig.motion.status(), MotionStatus::Idle);
        assert_eq!(rig.motion.pos(), [500.0, 500.0, 130.0]);
        let n = rig.out.len();
        rig.tick(&t, 250, 250);
        assert_eq!(rig.out.len(), n);
    }
}
