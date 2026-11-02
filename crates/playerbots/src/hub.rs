//! The hub: what the bots publish for the operator web UI and how commands reach them.
//!
//! Every bot owns one [`BotSlot`] (a snapshot, a bounded event log, a quest summary and its
//! command channel). The bot task writes its slot about once a second; the web UI only ever
//! reads. [`Hub`] implements [`pb_web::BotHub`] on top of the slots plus process-wide numbers:
//! packet and byte counters (atomics the bot tasks add to), the resident memory of the process,
//! the optional LLM budget, map markers and the world map.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};
use pb_web::{
    Activity, BotDetail, BotHub, BotSnapshot, Command, HubStats, LlmStats, LogLine, MapInfo,
    Marker, MarkerKind, QuestLine, WorldRect,
};
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::mpsc;

use crate::bot::cmd::{BotCmd, GoalChoice};

/// Entries of a bot's event log.
pub const LOG_LINES: usize = 32;
/// Capacity of a bot's command channel.
pub const CMD_QUEUE: usize = 8;
/// How often the process memory is read at most.
const RSS_REFRESH: Duration = Duration::from_secs(2);
/// Window of the packet-rate figures.
const RATE_WINDOW: Duration = Duration::from_secs(1);
/// The world rectangle assumed when no map was baked (`main_world`).
const DEFAULT_WORLD: WorldRect = WorldRect {
    min_x: 0.0,
    min_y: 0.0,
    max_x: 35840.0,
    max_y: 38912.0,
};

/// Unix time in milliseconds.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Everything about one bot that the web UI may read.
#[derive(Debug, Default)]
pub struct SlotInner {
    /// The snapshot the map and the bot list show.
    pub snap: BotSnapshot,
    /// Event log, oldest first, at most [`LOG_LINES`] lines.
    pub log: VecDeque<LogLine>,
    /// Quest summary.
    pub quests: Vec<QuestLine>,
    /// Extra rows of the detail panel.
    pub extra: Vec<(String, String)>,
}

/// The public face of one bot.
#[derive(Debug)]
pub struct BotSlot {
    /// Bot id.
    pub id: u32,
    inner: Mutex<SlotInner>,
    tx: RwLock<mpsc::Sender<BotCmd>>,
    /// Character id once known (0 before).
    pub char_id: AtomicU32,
    /// Object id in the world while spawned (0 otherwise).
    pub obj_id: AtomicU32,
}

impl BotSlot {
    /// A slot with the given first snapshot and a command channel nobody listens to yet.
    pub fn new(snap: BotSnapshot) -> Arc<BotSlot> {
        let (tx, _rx) = mpsc::channel(CMD_QUEUE);
        Arc::new(BotSlot {
            id: snap.id,
            inner: Mutex::new(SlotInner {
                snap,
                ..SlotInner::default()
            }),
            tx: RwLock::new(tx),
            char_id: AtomicU32::new(0),
            obj_id: AtomicU32::new(0),
        })
    }

    /// Opens a fresh command channel for a (re)started bot task and returns its receiver.
    pub fn attach(&self) -> mpsc::Receiver<BotCmd> {
        let (tx, rx) = mpsc::channel(CMD_QUEUE);
        *self.tx.write() = tx;
        rx
    }

    /// Queues a command for the bot.
    pub fn send(&self, cmd: BotCmd) -> Result<(), String> {
        use mpsc::error::TrySendError;
        self.tx.read().try_send(cmd).map_err(|e| match e {
            TrySendError::Full(_) => format!("bot {} is busy (command queue full)", self.id),
            TrySendError::Closed(_) => format!("bot {} is not running", self.id),
        })
    }

    /// Runs `f` on the slot data.
    pub fn with<R>(&self, f: impl FnOnce(&mut SlotInner) -> R) -> R {
        f(&mut self.inner.lock())
    }

    /// Appends a line to the event log (dropping the oldest at [`LOG_LINES`]).
    pub fn push_log(&self, text: String) {
        let mut g = self.inner.lock();
        if g.log.len() >= LOG_LINES {
            g.log.pop_front();
        }
        g.log.push_back(LogLine { t: unix_ms(), text });
    }

    /// A copy of the snapshot.
    pub fn snapshot(&self) -> BotSnapshot {
        self.inner.lock().snap.clone()
    }

    /// Marks the bot offline (keeps name, class and the rest).
    pub fn set_offline(&self) {
        let mut g = self.inner.lock();
        g.snap.activity = Activity::Offline;
        g.snap.detail.clear();
        g.snap.partner = None;
        g.snap.online_secs = 0;
        self.obj_id.store(0, Ordering::Relaxed);
    }
}

/// A bot's handle for writing log lines (to its slot ring and to `tracing`).
#[derive(Debug, Clone)]
pub struct Logger {
    slot: Arc<BotSlot>,
}

impl Logger {
    /// A logger for `slot`.
    pub fn new(slot: Arc<BotSlot>) -> Logger {
        Logger { slot }
    }

    /// A notable event: shown in the web UI and logged at info level.
    pub fn say(&self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!("{text}");
        self.slot.push_log(text);
    }

    /// A detail: logged at debug level only.
    pub fn debug(&self, text: impl AsRef<str>) {
        tracing::debug!("{}", text.as_ref());
    }
}

/// Packet and byte counters of all bots.
#[derive(Debug, Default)]
pub struct Counters {
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
}

/// A request to the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupCmd {
    /// Start `n` more bots.
    Spawn(u32),
    /// Stop `n` bots.
    Despawn(u32),
}

#[derive(Debug)]
struct Rates {
    at: Instant,
    packets_in: u64,
    packets_out: u64,
    in_per_sec: f32,
    out_per_sec: f32,
}

impl Rates {
    /// Recomputes the per-second rates from the counters if a window has passed.
    fn update(&mut self, now: Instant, packets_in: u64, packets_out: u64) {
        let dt = now.saturating_duration_since(self.at);
        if dt < RATE_WINDOW {
            return;
        }
        let secs = dt.as_secs_f32();
        self.in_per_sec = packets_in.saturating_sub(self.packets_in) as f32 / secs;
        self.out_per_sec = packets_out.saturating_sub(self.packets_out) as f32 / secs;
        self.packets_in = packets_in;
        self.packets_out = packets_out;
        self.at = now;
    }
}

struct Rss {
    sys: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    refreshed: Instant,
    bytes: u64,
    baseline: u64,
}

impl Rss {
    fn new() -> Rss {
        let mut r = Rss {
            sys: sysinfo::System::new(),
            pid: sysinfo::get_current_pid().ok(),
            refreshed: Instant::now() - RSS_REFRESH,
            bytes: 0,
            baseline: 0,
        };
        r.refresh(true);
        r.baseline = r.bytes;
        r
    }

    fn refresh(&mut self, force: bool) {
        if !force && self.refreshed.elapsed() < RSS_REFRESH {
            return;
        }
        self.refreshed = Instant::now();
        if let Some(pid) = self.pid {
            self.sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::nothing().with_memory(),
            );
            if let Some(p) = self.sys.process(pid) {
                self.bytes = p.memory();
            }
        }
    }
}

#[derive(Default)]
struct Markers {
    /// Static trade outlets, computed once.
    outlets: Vec<Marker>,
    /// Houses by bot id.
    houses: FxHashMap<u32, Marker>,
    /// Duel flags by key.
    flags: FxHashMap<u32, Marker>,
}

struct MapAsset {
    info: MapInfo,
    png: Option<Arc<Vec<u8>>>,
}

/// What [`Hub::new`] needs besides the world.
pub struct HubParams<'a> {
    /// The static world (trade outlets, map calibration).
    pub world: &'a aa_world::World,
    /// The baked data directory (the map image lives there).
    pub baked_dir: Option<&'a Path>,
    /// Where `Spawn` / `Despawn` commands go.
    pub supervisor: mpsc::Sender<SupCmd>,
    /// The LLM add-on, if on.
    pub llm: Option<Arc<pb_llm::Llm>>,
}

/// The hub. See the module docs.
pub struct Hub {
    slots: RwLock<FxHashMap<u32, Arc<BotSlot>>>,
    /// Character ids of all our bots, to tell them from real players.
    bot_chars: RwLock<FxHashSet<u32>>,
    /// Packet counters; bot tasks add to them.
    pub counters: Counters,
    rates: Mutex<Rates>,
    rss: Mutex<Rss>,
    started: Instant,
    markers: Mutex<Markers>,
    map: MapAsset,
    supervisor: mpsc::Sender<SupCmd>,
    llm: Option<Arc<pb_llm::Llm>>,
}

impl Hub {
    /// Builds the hub: computes the trade outlet markers and loads the map image once.
    pub fn new(p: HubParams<'_>) -> Hub {
        let outlets = trade_outlets(p.world);
        let map = load_map(p.world, p.baked_dir);
        tracing::info!(
            outlets = outlets.len(),
            map_image = map.png.is_some(),
            "hub ready"
        );
        Hub {
            slots: RwLock::new(FxHashMap::default()),
            bot_chars: RwLock::new(FxHashSet::default()),
            counters: Counters::default(),
            rates: Mutex::new(Rates {
                at: Instant::now(),
                packets_in: 0,
                packets_out: 0,
                in_per_sec: 0.0,
                out_per_sec: 0.0,
            }),
            rss: Mutex::new(Rss::new()),
            started: Instant::now(),
            markers: Mutex::new(Markers {
                outlets,
                ..Markers::default()
            }),
            map,
            supervisor: p.supervisor,
            llm: p.llm,
        }
    }

    /// Adds a slot for a bot (or returns the existing one).
    pub fn register(&self, snap: BotSnapshot) -> Arc<BotSlot> {
        let mut slots = self.slots.write();
        slots
            .entry(snap.id)
            .or_insert_with(|| BotSlot::new(snap))
            .clone()
    }

    /// Remembers that character `char_id` belongs to one of our bots.
    pub fn mark_bot_char(&self, char_id: u32) {
        if char_id != 0 && !self.bot_chars.read().contains(&char_id) {
            self.bot_chars.write().insert(char_id);
        }
    }

    /// Whether character `char_id` is one of our bots.
    pub fn is_bot_char(&self, char_id: u32) -> bool {
        self.bot_chars.read().contains(&char_id)
    }

    /// The slot of bot `id`.
    pub fn slot(&self, id: u32) -> Option<Arc<BotSlot>> {
        self.slots.read().get(&id).cloned()
    }

    /// All slots.
    pub fn slots(&self) -> Vec<Arc<BotSlot>> {
        self.slots.read().values().cloned().collect()
    }

    /// Adds packet numbers from a bot task.
    pub fn count(&self, packets_in: u64, bytes_in: u64, packets_out: u64, bytes_out: u64) {
        let c = &self.counters;
        c.packets_in.fetch_add(packets_in, Ordering::Relaxed);
        c.bytes_in.fetch_add(bytes_in, Ordering::Relaxed);
        c.packets_out.fetch_add(packets_out, Ordering::Relaxed);
        c.bytes_out.fetch_add(bytes_out, Ordering::Relaxed);
    }

    /// Call about once a second: refreshes the packet rates and (every 2 s) the memory figure.
    pub fn tick(&self) {
        self.rates.lock().update(
            Instant::now(),
            self.counters.packets_in.load(Ordering::Relaxed),
            self.counters.packets_out.load(Ordering::Relaxed),
        );
        self.rss.lock().refresh(false);
    }

    /// Records (or replaces) the house marker of a bot.
    pub fn set_house(&self, bot: u32, x: f32, y: f32, label: String) {
        self.markers.lock().houses.insert(
            bot,
            Marker {
                kind: MarkerKind::House,
                x,
                y,
                label,
            },
        );
    }

    /// Removes the house marker of a bot.
    pub fn clear_house(&self, bot: u32) {
        self.markers.lock().houses.remove(&bot);
    }

    /// Records (or replaces) a duel flag marker; `key` identifies the duel.
    pub fn set_duel_flag(&self, key: u32, x: f32, y: f32, label: String) {
        self.markers.lock().flags.insert(
            key,
            Marker {
                kind: MarkerKind::DuelFlag,
                x,
                y,
                label,
            },
        );
    }

    /// Removes a duel flag marker.
    pub fn clear_duel_flag(&self, key: u32) {
        self.markers.lock().flags.remove(&key);
    }

    /// Seconds since the hub was created.
    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    fn per_bot_bytes(rss: &Rss, online: u32) -> u64 {
        if online == 0 {
            0
        } else {
            rss.bytes.saturating_sub(rss.baseline) / u64::from(online)
        }
    }
}

/// Specialty NPC spawns: where a trade pack can be sold.
fn trade_outlets(world: &aa_world::World) -> Vec<Marker> {
    let mut out = Vec::new();
    for t in world.npc_templates().iter().filter(|t| t.is_specialty()) {
        for p in world.npc_spawns(t.id) {
            out.push(Marker {
                kind: MarkerKind::TradeOutlet,
                x: p.x,
                y: p.y,
                label: t.name.to_string(),
            });
        }
    }
    out
}

fn load_map(world: &aa_world::World, baked_dir: Option<&Path>) -> MapAsset {
    let Some(cal) = world.baked().map.as_ref() else {
        return MapAsset {
            info: MapInfo {
                image_url: None,
                width: 896,
                height: 973,
                world: DEFAULT_WORLD,
            },
            png: None,
        };
    };
    let png = baked_dir.and_then(|dir| {
        let path = dir.join(&cal.image);
        match std::fs::read(&path) {
            Ok(bytes) => Some(Arc::new(bytes)),
            Err(e) => {
                tracing::warn!("world map image {}: {e}", path.display());
                None
            }
        }
    });
    MapAsset {
        info: MapInfo {
            image_url: png.as_ref().map(|_| "/map.png".to_string()),
            width: cal.width,
            height: cal.height,
            world: WorldRect {
                min_x: cal.world.min_x,
                min_y: cal.world.min_y,
                max_x: cal.world.max_x,
                max_y: cal.world.max_y,
            },
        },
        png,
    }
}

impl BotHub for Hub {
    fn snapshots(&self) -> Vec<BotSnapshot> {
        self.slots.read().values().map(|s| s.snapshot()).collect()
    }

    fn detail(&self, id: u32) -> Option<BotDetail> {
        let slot = self.slot(id)?;
        Some(slot.with(|g| BotDetail {
            snapshot: g.snap.clone(),
            log: g.log.iter().cloned().collect(),
            quests: g.quests.clone(),
            extra: g.extra.clone(),
        }))
    }

    fn stats(&self) -> HubStats {
        let slots = self.slots();
        let mut counts = [0u32; Activity::ALL.len()];
        let mut online = 0u32;
        for s in &slots {
            let a = s.with(|g| g.snap.activity);
            if let Some(i) = Activity::ALL.iter().position(|x| *x == a) {
                counts[i] += 1;
            }
            if !matches!(a, Activity::Offline | Activity::Connecting) {
                online += 1;
            }
        }
        let rates = self.rates.lock();
        let rss = self.rss.lock();
        HubStats {
            bots_total: slots.len() as u32,
            bots_online: online,
            rss_bytes: rss.bytes,
            bytes_per_bot: Hub::per_bot_bytes(&rss, online),
            packets_in_per_sec: rates.in_per_sec,
            packets_out_per_sec: rates.out_per_sec,
            uptime_secs: self.uptime_secs(),
            llm: self.llm.as_ref().map(|l| {
                let u = l.usage();
                LlmStats {
                    requests: u.requests,
                    failures: u.failures,
                    tokens_today: u.tokens_today,
                    budget_left: u.budget_left,
                }
            }),
            activity_counts: Activity::ALL
                .iter()
                .zip(counts)
                .filter(|(_, n)| *n > 0)
                .map(|(a, n)| (*a, n))
                .collect(),
        }
    }

    fn markers(&self) -> Vec<Marker> {
        let m = self.markers.lock();
        let mut out = Vec::with_capacity(m.outlets.len() + m.houses.len() + m.flags.len());
        out.extend(m.outlets.iter().cloned());
        out.extend(m.houses.values().cloned());
        out.extend(m.flags.values().cloned());
        out
    }

    fn map_info(&self) -> MapInfo {
        self.map.info.clone()
    }

    fn map_image(&self) -> Option<Vec<u8>> {
        self.map.png.as_ref().map(|p| p.as_ref().clone())
    }

    fn command(&self, bot: Option<u32>, cmd: Command) -> Result<(), String> {
        cmd.validate(bot)?;
        match bot {
            Some(id) => {
                let slot = self.slot(id).ok_or_else(|| format!("no bot {id}"))?;
                slot.send(to_bot_cmd(cmd)?)
            }
            None => match cmd {
                Command::Spawn { count } => self
                    .supervisor
                    .try_send(SupCmd::Spawn(count))
                    .map_err(|_| "the supervisor is busy, try again".to_string()),
                Command::Despawn { count } => self
                    .supervisor
                    .try_send(SupCmd::Despawn(count))
                    .map_err(|_| "the supervisor is busy, try again".to_string()),
                other => {
                    // Pause, Resume and Goal reach every bot. A full queue only skips that bot.
                    let bot_cmd = to_bot_cmd(other)?;
                    let mut busy = 0usize;
                    for s in self.slots() {
                        if s.send(bot_cmd.clone()).is_err() {
                            busy += 1;
                        }
                    }
                    if busy > 0 {
                        tracing::debug!(busy, "global command skipped some bots");
                    }
                    Ok(())
                }
            },
        }
    }
}

fn to_bot_cmd(cmd: Command) -> Result<BotCmd, String> {
    Ok(match cmd {
        Command::Pause => BotCmd::Pause,
        Command::Resume => BotCmd::Resume,
        Command::Goal { goal } => {
            BotCmd::Goal(GoalChoice::parse(&goal).ok_or_else(|| format!("unknown goal {goal:?}"))?)
        }
        Command::DuelWith { other } => BotCmd::DuelWith(other),
        Command::TradeWith { other } => BotCmd::TradeWith(other),
        Command::Say { text } => BotCmd::Say(text.trim().to_string()),
        Command::Spawn { .. } | Command::Despawn { .. } => {
            return Err("spawn and despawn are global commands".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: u32) -> BotSnapshot {
        BotSnapshot {
            id,
            name: format!("Bot{id}"),
            ..BotSnapshot::default()
        }
    }

    #[test]
    fn a_slot_keeps_a_bounded_log_and_delivers_commands() {
        let slot = BotSlot::new(snap(1));
        for i in 0..100 {
            slot.push_log(format!("line {i}"));
        }
        slot.with(|g| {
            assert_eq!(g.log.len(), LOG_LINES);
            assert_eq!(g.log.front().unwrap().text, "line 68");
            assert_eq!(g.log.back().unwrap().text, "line 99");
        });

        // nobody listens yet: the command is dropped with an error
        let mut rx = slot.attach();
        slot.send(BotCmd::Pause).unwrap();
        assert_eq!(rx.try_recv().unwrap(), BotCmd::Pause);
        for _ in 0..CMD_QUEUE {
            slot.send(BotCmd::Resume).unwrap();
        }
        let err = slot.send(BotCmd::Resume).unwrap_err();
        assert!(err.contains("busy"), "{err}");
        // a restart gets a new channel; the old one is closed
        drop(rx);
        let err = slot.send(BotCmd::Pause).unwrap_err();
        assert!(err.contains("not running"), "{err}");
        let mut rx = slot.attach();
        slot.send(BotCmd::Pause).unwrap();
        assert_eq!(rx.try_recv().unwrap(), BotCmd::Pause);
    }

    #[test]
    fn offline_keeps_the_identity() {
        let slot = BotSlot::new(BotSnapshot {
            activity: Activity::Grinding,
            detail: "x".into(),
            partner: Some(4),
            online_secs: 9,
            ..snap(2)
        });
        slot.obj_id.store(5, Ordering::Relaxed);
        slot.set_offline();
        let s = slot.snapshot();
        assert_eq!(s.name, "Bot2");
        assert_eq!(s.activity, Activity::Offline);
        assert!(s.detail.is_empty() && s.partner.is_none() && s.online_secs == 0);
        assert_eq!(slot.obj_id.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn web_commands_map_to_bot_commands() {
        assert_eq!(to_bot_cmd(Command::Pause), Ok(BotCmd::Pause));
        assert_eq!(
            to_bot_cmd(Command::Goal {
                goal: "auto".into()
            }),
            Ok(BotCmd::Goal(GoalChoice::Auto))
        );
        assert_eq!(
            to_bot_cmd(Command::Goal {
                goal: "trade_run".into()
            }),
            Ok(BotCmd::Goal(GoalChoice::Forced(
                crate::bot::cmd::GoalKind::TradeRun
            )))
        );
        assert_eq!(
            to_bot_cmd(Command::Say {
                text: "  hi  ".into()
            }),
            Ok(BotCmd::Say("hi".into()))
        );
        assert!(to_bot_cmd(Command::Goal { goal: "fly".into() }).is_err());
        assert!(to_bot_cmd(Command::Spawn { count: 1 }).is_err());
    }

    #[test]
    fn rates_come_from_counter_deltas_once_a_window_has_passed() {
        let t0 = Instant::now();
        let mut r = Rates {
            at: t0,
            packets_in: 100,
            packets_out: 10,
            in_per_sec: 0.0,
            out_per_sec: 0.0,
        };
        // less than a window: nothing changes
        r.update(t0 + Duration::from_millis(400), 900, 90);
        assert_eq!(r.in_per_sec, 0.0);
        // two seconds later: (500 - 100) / 2 and (50 - 10) / 2
        r.update(t0 + Duration::from_secs(2), 500, 50);
        assert!((r.in_per_sec - 200.0).abs() < 0.01, "{}", r.in_per_sec);
        assert!((r.out_per_sec - 20.0).abs() < 0.01, "{}", r.out_per_sec);
        // counters never run backwards into a huge rate
        r.update(t0 + Duration::from_secs(4), 10, 5);
        assert_eq!((r.in_per_sec, r.out_per_sec), (0.0, 0.0));
    }
}
