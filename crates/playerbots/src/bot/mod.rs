//! One bot: a single tokio task that logs in, lives in the world and survives disconnects.
//!
//! # Architecture
//!
//! [`run_bot`] is the task. It owns everything about the bot ([`Bot`]): the state, the movement
//! and combat controllers, the brain and the game connection. There is no reader task: the task
//! `select!`s over the connection, its tick timer, a one-second housekeeping timer and the command
//! channel, so a bot costs one task and no locks on the hot path.
//!
//! ```text
//!   run_bot ─► session (login, lobby) ─► world loop ─┬─ packet  ─► BotState::apply, Combat::on_event
//!        ▲                                           ├─ tick    ─► Brain::tick(Ctx) ─► Motion::tick ─► send
//!        └── reconnect with backoff ◄────────────────┼─ 1 s     ─► ping, publish to the hub
//!                                                    └─ command ─► Brain / pause / chat
//! ```
//!
//! The tick period is `bots.tick_ms` while the bot moves, fights or is threatened and
//! `bots.idle_tick_ms` otherwise. A [`Ctx`] is built around the task's own fields for each tick and
//! handed to the [`Brain`](brain::Brain); goals never see the connection, only the outbox
//! (`ctx.out`), which the task flushes after the tick.

pub mod brain;
pub mod cmd;
pub mod combat;
pub mod coordinator;
pub mod ctx;
pub mod goals;
pub mod motion;
pub mod session;
pub mod state;

use std::fmt::Write as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use aa_proto::cs::{self, Out};
use aa_proto::game::{ConnStats, GameConn, Packet};
use aa_proto::sc::ServerEvent;
use aa_proto::sc_world::{DoodadInfo, WorldEvent};
use aa_world::{DoodadFlags, World};
use pb_web::Activity;
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::archetype::{Archetype, Archetypes};
use crate::config::{race_name, Config};
use crate::hub::{Hub, Logger};
use crate::roster::{RosterEntry, SharedRoster};

use brain::Brain;
use cmd::{BotCmd, GoalKind, PairKind, PairOrder, PairRole};
use combat::Combat;
use coordinator::CoordHandle;
use ctx::{Ctx, Persona};
use motion::{Motion, MotionCfg, MotionEnv};
use session::{SessionError, Wish};
use state::BotState;

/// Concurrent logins (login server plus lobby), so a restart does not stampede the servers.
pub const LOGIN_PERMITS: usize = 4;
/// A session that lasted this long resets the reconnect backoff.
const HEALTHY_SESSION: Duration = Duration::from_secs(60);
/// Name rejections in a row after which the bot waits like after any other failure.
const MAX_NAME_RETRIES: u32 = 6;
/// How long a bot waits for the server's logout grant at shutdown.
const LEAVE_WAIT: Duration = Duration::from_secs(2);
/// Interval of the temple resurrection request while dead.
const RESURRECT_RETRY_MS: u64 = 5_000;
/// A real player within this many metres makes the bot send movement at the full tick rate.
const WATCH_RADIUS: f32 = 150.0;
/// The bot's detail panel is refreshed this often.
const EXTRA_EVERY_SECS: u32 = 5;
/// Lifetime of a bot between connects: the longest the connect step may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything the bot tasks share.
pub struct Shared {
    /// The configuration.
    pub cfg: Arc<Config>,
    /// The static world data.
    pub world: Arc<World>,
    /// The hub (web UI state, counters, markers).
    pub hub: Arc<Hub>,
    /// The persistent roster.
    pub roster: Arc<SharedRoster>,
    /// The archetypes.
    pub archetypes: Arc<Archetypes>,
    /// The pairing service.
    pub coord: CoordHandle,
    /// Limits concurrent logins.
    pub login_gate: tokio::sync::Semaphore,
    /// The bot account password.
    pub password: String,
}

/// Why a session ended.
enum SessionEnd {
    /// The process is shutting down (or this bot was despawned).
    Shutdown,
    /// Try again at once (the character name was replaced).
    Retry,
    /// The connection ended or could not be made.
    Ended {
        why: String,
        permanent: bool,
        online: Duration,
    },
}

/// Why the world loop returned.
enum WorldEnd {
    Shutdown,
    Lost(String),
}

/// Runs bot `id` until `cancel` fires: connect, play, reconnect with backoff, log out.
pub async fn run_bot(id: u32, shared: Arc<Shared>, cancel: CancellationToken) {
    let Some(entry) = shared.roster.entry(id) else {
        tracing::warn!(bot = id, "no roster entry; not starting");
        return;
    };
    let Some(archetype) = shared.archetypes.get(&entry.archetype).cloned() else {
        tracing::warn!(bot = id, archetype = %entry.archetype, "unknown archetype; not starting");
        return;
    };
    let span = tracing::info_span!("bot", id, name = %entry.char_name);
    let mut bot = Bot::new(entry, archetype, shared);
    bot.run(cancel).instrument(span).await;
}

/// The task state of one bot. See the module docs.
struct Bot {
    shared: Arc<Shared>,
    slot: Arc<crate::hub::BotSlot>,
    log: Logger,
    entry: RosterEntry,
    persona: Persona,
    state: BotState,
    motion: Motion,
    combat: Combat,
    brain: Brain,
    rng: fastrand::Rng,
    out: Vec<Out>,
    cmd_rx: mpsc::Receiver<BotCmd>,
    clock: Instant,
    paused: bool,
    real_near: bool,
    online_since: Option<Instant>,
    next_rez_ms: u64,
    last_stats: ConnStats,
    zone_key: Option<u16>,
    detail: String,
    secs: u32,
    sessions: u32,
}

impl Bot {
    fn new(entry: RosterEntry, archetype: Arc<Archetype>, shared: Arc<Shared>) -> Bot {
        let cfg = &shared.cfg;
        let temper = entry.temperament.with_spread(cfg.behavior.persona_spread);
        let slot = shared.hub.register(initial_snapshot(&entry, &archetype));
        let cmd_rx = slot.attach();
        let persona = Persona {
            id: entry.id,
            name: entry.char_name.clone().into_boxed_str(),
            archetype,
            temper,
        };
        let brain = Brain::new(&cfg.behavior, &persona.temper);
        let motion = Motion::new(MotionCfg {
            run_speed: cfg.bots.move_speed,
            walk_speed: cfg.bots.walk_speed,
        });
        Bot {
            log: Logger::new(slot.clone()),
            slot,
            entry,
            persona,
            state: BotState::new(),
            motion,
            combat: Combat::new(),
            brain,
            rng: fastrand::Rng::new(),
            out: Vec::with_capacity(8),
            cmd_rx,
            clock: Instant::now(),
            paused: false,
            real_near: false,
            online_since: None,
            next_rez_ms: 0,
            last_stats: ConnStats::default(),
            zone_key: None,
            detail: String::new(),
            secs: 0,
            sessions: 0,
            shared,
        }
    }

    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64 + 1
    }

    // ---- the reconnect loop --------------------------------------------------------------

    async fn run(&mut self, cancel: CancellationToken) {
        let bots = &self.shared.cfg.bots;
        let (min, max) = (
            Duration::from_secs(bots.reconnect_min_secs),
            Duration::from_secs(bots.reconnect_max_secs),
        );
        let mut backoff = min;
        let mut name_retries = 0u32;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            self.set_connecting();
            let end = self.session(&cancel).await;
            self.go_offline();
            match end {
                SessionEnd::Shutdown => break,
                SessionEnd::Retry if name_retries < MAX_NAME_RETRIES => {
                    name_retries += 1;
                    continue;
                }
                SessionEnd::Retry => {}
                SessionEnd::Ended {
                    why,
                    permanent,
                    online,
                } => {
                    name_retries = 0;
                    self.log.say(format!("offline: {why}"));
                    if online >= HEALTHY_SESSION {
                        backoff = min;
                    }
                    if permanent {
                        backoff = max;
                    }
                }
            }
            let wait = jittered(backoff, &mut self.rng);
            tracing::debug!(secs = wait.as_secs(), "reconnecting");
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(wait) => {}
            }
            backoff = (backoff * 2).min(max);
        }
        self.go_offline();
    }

    fn set_connecting(&mut self) {
        self.slot.with(|g| {
            g.snap.activity = Activity::Connecting;
            g.snap.detail.clear();
            g.snap.detail.push_str("logging in");
        });
    }

    fn go_offline(&mut self) {
        self.online_since = None;
        self.motion.leave_world();
        self.slot.set_offline();
    }

    // ---- one session ---------------------------------------------------------------------

    async fn session(&mut self, cancel: &CancellationToken) -> SessionEnd {
        let shared = self.shared.clone();
        let cfg = &shared.cfg;
        let permit = tokio::select! {
            _ = cancel.cancelled() => return SessionEnd::Shutdown,
            p = shared.login_gate.acquire() => p.ok(),
        };
        self.sessions += 1;
        let ticket = tokio::select! {
            _ = cancel.cancelled() => return SessionEnd::Shutdown,
            r = session::login_server(
                &cfg.server.login,
                &self.entry.account,
                &shared.password,
                cfg.server.world_id,
            ) => match r {
                Ok(t) => t,
                Err(e) => return failed(e, Duration::ZERO),
            },
        };
        let connect = GameConn::connect(ticket.cookie.game_addr);
        let mut conn = tokio::select! {
            _ = cancel.cancelled() => return SessionEnd::Shutdown,
            r = tokio::time::timeout(CONNECT_TIMEOUT, connect) => match r {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => return failed(e.into(), Duration::ZERO),
                Err(_) => return failed(SessionError::Timeout("the game server"), Duration::ZERO),
            },
        };
        self.state.reset();
        self.combat.reset();
        self.last_stats = ConnStats::default();
        let name = self.entry.char_name.clone();
        let wish = Wish {
            name: &name,
            char_id: self.entry.char_id,
            race: self.entry.race,
            gender: self.entry.gender,
            ability: self.persona.archetype.ability,
        };
        let entered = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = conn.close().await;
                return SessionEnd::Shutdown;
            }
            r = session::enter_game(
                &mut conn,
                &mut self.state,
                ticket.account_id,
                ticket.cookie.cookie,
                &wish,
                self.clock,
            ) => r,
        };
        drop(permit);
        let entered = match entered {
            Ok(e) => e,
            Err(e) if e.is_name_rejected() => {
                let _ = conn.close().await;
                return self.rename();
            }
            Err(e) => {
                let _ = conn.close().await;
                return failed(e, Duration::ZERO);
            }
        };
        self.record_character(&entered);
        self.log.say(format!(
            "in the world at {:.0}, {:.0} ({})",
            self.state.me.pos[0], self.state.me.pos[1], ticket.world_name
        ));
        let started = Instant::now();
        let end = self.world_loop(&mut conn, cancel).await;
        self.count_packets(&conn);
        let online = started.elapsed();
        match end {
            WorldEnd::Shutdown => {
                session::leave(&mut conn, &mut self.state, LEAVE_WAIT).await;
                self.count_packets(&conn);
                SessionEnd::Shutdown
            }
            WorldEnd::Lost(why) => {
                let _ = conn.close().await;
                SessionEnd::Ended {
                    why,
                    permanent: false,
                    online,
                }
            }
        }
        // `ticket.login` drops here, after the game connection.
    }

    /// The server rejected the name: take a new one and try again.
    fn rename(&mut self) -> SessionEnd {
        let id = self.entry.id;
        let new = self
            .shared
            .roster
            .update(|r| (r.regenerate_name(id, &mut self.rng), true));
        match new {
            Some(name) => {
                self.log.say(format!(
                    "the server rejected {}; trying {name}",
                    self.entry.char_name
                ));
                self.persona.name = name.clone().into_boxed_str();
                self.entry.char_name = name;
                self.entry.char_id = None;
                SessionEnd::Retry
            }
            None => SessionEnd::Ended {
                why: "the roster entry is gone".into(),
                permanent: true,
                online: Duration::ZERO,
            },
        }
    }

    /// Remembers the character the lobby ended up with.
    fn record_character(&mut self, entered: &session::Entered) {
        let id = self.entry.id;
        self.shared.hub.mark_bot_char(entered.char_id);
        self.slot.char_id.store(entered.char_id, Ordering::Relaxed);
        self.slot.obj_id.store(self.state.me.obj, Ordering::Relaxed);
        let changed = self.shared.roster.update(|r| {
            let mut changed = r.set_char_id(id, entered.char_id);
            if !entered.name.is_empty() && !entered.name.eq_ignore_ascii_case(&self.entry.char_name)
            {
                changed |= r.adopt_name(id, &entered.name);
            }
            (changed, changed)
        });
        if changed {
            if let Some(e) = self.shared.roster.entry(id) {
                self.entry = e;
                self.persona.name = self.entry.char_name.clone().into_boxed_str();
            }
        }
        self.entry.char_id = Some(entered.char_id);
        if entered.created {
            self.log.say(format!("created character {}", entered.name));
        }
    }

    // ---- the world loop ------------------------------------------------------------------

    async fn world_loop(&mut self, conn: &mut GameConn, cancel: &CancellationToken) -> WorldEnd {
        let now = self.now_ms();
        self.motion
            .enter_world(self.state.me.obj, self.state.me.pos, self.state.me.yaw, now);
        self.online_since = Some(Instant::now());
        self.paused = false;
        self.secs = 0;
        self.publish(conn);
        let mut housekeeping = tokio::time::interval(Duration::from_secs(1));
        housekeeping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let tick = tokio::time::sleep(Duration::from_millis(self.shared.cfg.bots.tick_ms));
        tokio::pin!(tick);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return WorldEnd::Shutdown,
                _ = &mut tick => {
                    if let Some(end) = self.on_tick() {
                        return end;
                    }
                    if let Err(e) = session::flush_out(conn, &mut self.out).await {
                        return WorldEnd::Lost(e.to_string());
                    }
                    tick.as_mut().reset(Instant::now() + self.tick_period());
                }
                _ = housekeeping.tick() => {
                    self.housekeeping(conn);
                    if let Err(e) = session::flush_out(conn, &mut self.out).await {
                        return WorldEnd::Lost(e.to_string());
                    }
                }
                packet = conn.recv() => match packet {
                    Ok(p) => {
                        self.on_packet(&p);
                        if let Err(e) = session::flush_out(conn, &mut self.out).await {
                            return WorldEnd::Lost(e.to_string());
                        }
                    }
                    Err(e) => return WorldEnd::Lost(e.to_string()),
                },
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(c) => self.on_command(c),
                    // The slot was replaced: this task is stale.
                    None => return WorldEnd::Shutdown,
                },
            }
        }
    }

    fn tick_period(&self) -> Duration {
        let b = &self.shared.cfg.bots;
        let active = self.motion.busy()
            || self.combat.busy()
            || !self.state.threats.is_empty()
            || !self.state.me.spawned;
        Duration::from_millis(if active { b.tick_ms } else { b.idle_tick_ms })
    }

    fn on_packet(&mut self, p: &Packet) {
        let Some(mut ev) = session::decode(p) else {
            return;
        };
        if let ServerEvent::World(WorldEvent::DoodadsCreated(list)) = &mut ev {
            let world = &self.shared.world;
            list.retain(|d| keep_doodad(world, d));
            if list.is_empty() {
                return;
            }
        }
        let now = self.now_ms();
        self.state.now_ms = now;
        self.state.apply(&ev);
        self.combat
            .on_event(&ev, self.state.me.obj, now, &mut self.out);
    }

    fn on_command(&mut self, cmd: BotCmd) {
        match &cmd {
            BotCmd::Pause => {
                self.paused = true;
                self.motion.stop();
                self.combat.disengage();
            }
            BotCmd::Resume => self.paused = false,
            BotCmd::Say(text) => {
                let text = text.trim();
                if !text.is_empty() && !text.starts_with('/') {
                    self.out.push(cs::chat(cs::chat_type::SAY, "", text));
                }
            }
            BotCmd::DuelWith(other) => self.pair_with(*other, PairKind::Duel),
            BotCmd::TradeWith(other) => self.pair_with(*other, PairKind::Trade),
            BotCmd::Goal(_) | BotCmd::Pair(_) => self.brain.command(&cmd),
        }
    }

    /// The operator asked this bot to duel or trade with bot `other`.
    fn pair_with(&mut self, other: u32, kind: PairKind) {
        let (partner_char, partner_name) = coordinator::Dispatch::who(&*self.shared.hub, other);
        let pos = self.state.me.pos;
        let order = PairOrder {
            kind,
            role: PairRole::Initiator,
            partner_bot: other,
            partner_char,
            partner_name: partner_name.into_boxed_str(),
            meet_at: (pos[0], pos[1]),
        };
        let goal = match kind {
            PairKind::Duel => GoalKind::Duel,
            PairKind::Trade => GoalKind::Trade,
        };
        self.brain.queue(goal, Some(order));
    }

    /// One brain tick: reactions, resurrection, goals, movement. Returns the reason to leave the
    /// loop when the connection should end.
    fn on_tick(&mut self) -> Option<WorldEnd> {
        let now_ms = self.now_ms();
        self.state.now_ms = now_ms;
        if let Some(end) = self.react() {
            return Some(end);
        }
        self.state.me.pos = self.motion.pos();
        self.state.me.yaw = self.motion.yaw();

        if self.state.me.dead && now_ms >= self.state.me.respawn_at_ms && now_ms >= self.next_rez_ms
        {
            self.out.push(cs::resurrect(false));
            self.next_rez_ms = now_ms + RESURRECT_RETRY_MS;
        }
        if !self.paused || self.state.me.dead {
            let mut ctx = Ctx {
                world: &self.shared.world,
                cfg: &self.shared.cfg,
                persona: &self.persona,
                state: &mut self.state,
                motion: &mut self.motion,
                combat: &mut self.combat,
                out: &mut self.out,
                coord: &self.shared.coord,
                hub: &self.shared.hub,
                log: &self.log,
                rng: &mut self.rng,
                now: Instant::now(),
                now_ms,
                real_player_near: self.real_near,
            };
            self.brain.tick(&mut ctx);
        }
        let send_gap_ms = if self.real_near {
            self.shared.cfg.bots.tick_ms
        } else {
            1000
        };
        let mut env = MotionEnv {
            ground: &*self.shared.world,
            now_ms,
            out: &mut self.out,
            rng: &mut self.rng,
            send_gap_ms,
            combat: self.combat.target().is_some(),
        };
        self.motion.tick(&mut env);
        self.state.me.pos = self.motion.pos();
        None
    }

    /// Answers what the state noticed: teleports, instance loads, death, resurrection, kicks.
    fn react(&mut self) -> Option<WorldEnd> {
        let r = std::mem::take(&mut self.state.reactions);
        if let Some(p) = r.teleport {
            self.motion.teleported(p);
            self.out.push(cs::teleport_ended(p[0], p[1], p[2]));
        }
        if r.load_instance {
            self.motion.teleported(self.state.me.pos);
            self.out.push(session::instance_loaded());
        }
        if r.died {
            self.motion.stop();
            self.combat.reset();
            self.log.say("died");
        }
        if let Some(p) = r.resurrected {
            self.motion.teleported(p);
            self.next_rez_ms = 0;
            self.brain.queue(GoalKind::Rest, None);
            self.log.say("resurrected");
        }
        if let Some((reason, msg)) = r.kicked {
            return Some(WorldEnd::Lost(format!("kicked (reason {reason}): {msg}")));
        }
        if r.leave_granted {
            return Some(WorldEnd::Lost("the server logged the character out".into()));
        }
        None
    }

    // ---- housekeeping and publishing -----------------------------------------------------

    fn housekeeping(&mut self, conn: &GameConn) {
        self.secs += 1;
        // The first housekeeping tick fires as the loop starts: ping then and every
        // `PING_EVERY` after (the server drops connections that have not pinged in its sweep).
        if (self.secs - 1).is_multiple_of(session::PING_EVERY.as_secs() as u32) {
            let ms = self.clock.elapsed().as_millis() as i64;
            self.out.push(cs::ping(ms));
        }
        let hub = &self.shared.hub;
        self.real_near = self
            .state
            .any_character_within(WATCH_RADIUS, |id| hub.is_bot_char(id));
        self.publish(conn);
    }

    fn count_packets(&mut self, conn: &GameConn) {
        let s = conn.stats();
        let l = self.last_stats;
        self.shared.hub.count(
            s.packets_in.saturating_sub(l.packets_in),
            s.bytes_in.saturating_sub(l.bytes_in),
            s.packets_out.saturating_sub(l.packets_out),
            s.bytes_out.saturating_sub(l.bytes_out),
        );
        self.last_stats = s;
    }

    fn activity(&self) -> Activity {
        if self.state.me.dead {
            return Activity::Dead;
        }
        if !self.state.me.spawned {
            return Activity::Connecting;
        }
        if self.paused {
            return Activity::Idle;
        }
        match self.brain.goal() {
            Some(g) => g.activity(self.motion.is_moving(), self.combat.target().is_some()),
            None => {
                if self.motion.is_moving() {
                    Activity::Traveling
                } else {
                    Activity::Idle
                }
            }
        }
    }

    /// Writes the bot's snapshot into its hub slot and adds the packet counters.
    fn publish(&mut self, conn: &GameConn) {
        self.count_packets(conn);
        let activity = self.activity();
        self.detail.clear();
        self.describe();
        let me = &self.state.me;
        let world = &self.shared.world;
        let zone_key = world.zone_key_at(me.pos[0], me.pos[1]);
        let zone_changed = zone_key != self.zone_key;
        self.zone_key = zone_key;
        let online_secs = self
            .online_since
            .map_or(0, |t| t.elapsed().as_secs() as u32);
        let with_extra = self.secs.is_multiple_of(EXTRA_EVERY_SECS);
        let goal = self.brain.goal_name();
        let paused = self.paused;
        let has_house = !self.state.houses.is_empty();
        let (level, hp, pos, money) = (me.level, me.hp_pct(), me.pos, me.money);
        self.slot.with(|g| {
            let s = &mut g.snap;
            s.level = level;
            s.hp_pct = hp;
            s.x = pos[0];
            s.y = pos[1];
            s.z = pos[2];
            s.activity = activity;
            s.gold = money;
            s.has_house = has_house;
            s.online_secs = online_secs;
            s.paused = paused;
            if s.goal != goal {
                s.goal.clear();
                s.goal.push_str(goal);
            }
            if s.detail != self.detail {
                s.detail.clear();
                s.detail.push_str(&self.detail);
            }
            if zone_changed {
                s.zone = zone_key.and_then(|k| world.zone_name(k)).map(str::to_owned);
            }
            if with_extra {
                g.extra.clear();
                g.extra.push(("account".into(), self.entry.account.clone()));
                g.extra.push((
                    "character".into(),
                    format!(
                        "{} (id {})",
                        self.entry.char_name,
                        self.entry.char_id.unwrap_or(0)
                    ),
                ));
                g.extra
                    .push(("persona".into(), self.persona.temper.dominant().into()));
                g.extra
                    .push(("kills".into(), self.combat.kills().to_string()));
                g.extra.push(("sessions".into(), self.sessions.to_string()));
            }
        });
        self.slot.obj_id.store(self.state.me.obj, Ordering::Relaxed);
    }

    /// Fills `self.detail` with the one-line description of the current activity.
    fn describe(&mut self) {
        let me = &self.state.me;
        if me.dead {
            let left = me.respawn_at_ms.saturating_sub(self.now_ms()) / 1000;
            let _ = write!(self.detail, "dead, can respawn in {left}s");
        } else if self.paused {
            self.detail.push_str("paused");
        } else if let Some(obj) = self.combat.target() {
            let name = self
                .state
                .units
                .get(&obj)
                .and_then(|u| self.shared.world.npc(u.id))
                .map_or("a monster", |t| &*t.name);
            let _ = write!(self.detail, "fighting {name}");
        } else if let Some(g) = self.brain.goal() {
            g.describe(&mut self.detail);
        }
    }
}

/// Builds the offline snapshot a bot has before its first login.
pub fn initial_snapshot(entry: &RosterEntry, archetype: &Archetype) -> pb_web::BotSnapshot {
    pb_web::BotSnapshot {
        id: entry.id,
        name: entry.char_name.clone(),
        race: race_name(entry.race).to_string(),
        class: archetype.name.clone(),
        level: 1,
        hp_pct: 100,
        activity: Activity::Offline,
        ..pb_web::BotSnapshot::default()
    }
}

fn failed(e: SessionError, online: Duration) -> SessionEnd {
    SessionEnd::Ended {
        permanent: e.is_permanent(),
        why: e.to_string(),
        online,
    }
}

/// `d` plus up to a quarter of it, so a restart does not reconnect everyone at the same second.
fn jittered(d: Duration, rng: &mut fastrand::Rng) -> Duration {
    d + d.mul_f32(rng.f32() * 0.25)
}

/// Whether a doodad is worth remembering: it can be used, looted, crafted at, or a quest wants it.
fn keep_doodad(world: &World, d: &DoodadInfo) -> bool {
    d.has_loot
        || d.quest_glow != 0
        || world
            .doodad(d.template)
            .is_some_and(|t| t.flags != DoodadFlags::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes of the per-bot structures, printed with `--nocapture` for the memory estimate in the
    /// README; the asserts keep them from growing unnoticed.
    #[test]
    fn per_bot_structures_stay_small() {
        use std::mem::size_of;
        let sizes = [
            ("Bot", size_of::<Bot>()),
            ("BotState", size_of::<BotState>()),
            ("Motion", size_of::<Motion>()),
            ("Combat", size_of::<Combat>()),
            ("Brain", size_of::<Brain>()),
            ("Goal", size_of::<goals::Goal>()),
            ("Persona", size_of::<Persona>()),
            ("Unit", size_of::<state::Unit>()),
            ("Doodad", size_of::<state::Doodad>()),
            ("Item", size_of::<state::Item>()),
        ];
        for (name, n) in sizes {
            println!("size_of {name}: {n} bytes");
        }
        assert!(size_of::<Bot>() < 3072, "Bot is {} bytes", size_of::<Bot>());
        assert!(size_of::<goals::Goal>() <= 160);
        assert!(size_of::<state::Unit>() <= 64);
    }

    #[test]
    fn jitter_adds_at_most_a_quarter() {
        let mut rng = fastrand::Rng::with_seed(3);
        for _ in 0..200 {
            let d = jittered(Duration::from_secs(8), &mut rng);
            assert!(d >= Duration::from_secs(8) && d <= Duration::from_secs(10));
        }
    }

    #[test]
    fn the_first_snapshot_is_offline_with_class_and_race() {
        let archetypes = Archetypes::builtin().unwrap();
        let arch = archetypes.get("Darkrunner").unwrap();
        let entry = RosterEntry {
            id: 4,
            account: "pb4".into(),
            char_name: "Tester".into(),
            race: 4,
            gender: 2,
            archetype: "Darkrunner".into(),
            temperament: crate::roster::Temperament::NEUTRAL,
            char_id: None,
        };
        let s = initial_snapshot(&entry, arch);
        assert_eq!(
            (s.id, s.name.as_str(), s.race.as_str()),
            (4, "Tester", "Elf")
        );
        assert_eq!(s.class, "Darkrunner");
        assert_eq!(s.activity, Activity::Offline);
    }

    #[test]
    fn failures_report_the_reason_and_permanence() {
        match failed(
            SessionError::Login(aa_proto::login::LoginError::Denied(3)),
            Duration::ZERO,
        ) {
            SessionEnd::Ended { why, permanent, .. } => {
                assert!(permanent);
                assert!(why.contains("denied"), "{why}");
            }
            _ => panic!("expected Ended"),
        }
        assert!(matches!(
            failed(SessionError::Timeout("x"), Duration::ZERO),
            SessionEnd::Ended {
                permanent: false,
                ..
            }
        ));
    }
}
