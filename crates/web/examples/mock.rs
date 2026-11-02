//! A mock bot hub for developing the web UI without any bot runtime.
//!
//! ```text
//! cargo run -p pb-web --example mock -- [--bind 127.0.0.1:8790] [--bots 200] [--token SECRET] [--no-image]
//! ```
//!
//! About 200 fake bots wander around Solzreed (world x 15000..16200, y 14800..15800), change
//! activity at random, pair up for duels and trades and answer the commands of the UI. The map is
//! a generated PNG calibrated to the real world rectangle, with the Solzreed area outlined so the
//! calibration can be checked by eye.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use pb_web::{
    Activity, BotDetail, BotHub, BotSnapshot, Command, HubStats, LlmStats, LogLine, MapInfo,
    Marker, MarkerKind, QuestLine, WebConfig, WorldRect,
};

const AREA: (f32, f32, f32, f32) = (15000.0, 14800.0, 16200.0, 15800.0);
const WORLD: WorldRect = WorldRect {
    min_x: -14731.0,
    min_y: 46.0,
    max_x: 49920.0,
    max_y: 38911.0,
};
const CLASSES: [&str; 8] = [
    "Battlerage",
    "Witchcraft",
    "Archery",
    "Occultism",
    "Vitalism",
    "Sorcery",
    "Defense",
    "Songcraft",
];
const RACES: [&str; 4] = ["Nuian", "Elf", "Hariharan", "Dwarf"];
const ACTIVITY_WEIGHTS: [(Activity, u32); 11] = [
    (Activity::Idle, 14),
    (Activity::Traveling, 16),
    (Activity::Questing, 26),
    (Activity::Grinding, 14),
    (Activity::Fighting, 5),
    (Activity::Dueling, 6),
    (Activity::Trading, 5),
    (Activity::TradeRun, 6),
    (Activity::Housing, 3),
    (Activity::Resting, 4),
    (Activity::Dead, 1),
];

struct MockBot {
    snap: BotSnapshot,
    target: (f32, f32),
    next_change: f32,
    log: VecDeque<LogLine>,
    quest: u32,
}

struct Inner {
    bots: Vec<MockBot>,
    next_id: u32,
    started: Instant,
    tick: u64,
}

struct MockHub {
    inner: Mutex<Inner>,
    markers: Vec<Marker>,
    png: Option<Vec<u8>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn pick<T>(items: &[T]) -> &T {
    &items[fastrand::usize(..items.len())]
}

fn random_name() -> String {
    const A: [&str; 12] = [
        "Ar", "Bel", "Cor", "Dal", "El", "Fen", "Gor", "Hal", "Ira", "Jor", "Kel", "Lun",
    ];
    const B: [&str; 10] = [
        "an", "ethil", "orin", "adar", "iel", "wyn", "thas", "mir", "dis", "ra",
    ];
    format!("{}{}{}", pick(&A), pick(&B), fastrand::u16(10..99))
}

fn random_point() -> (f32, f32) {
    (
        fastrand::f32() * (AREA.2 - AREA.0) + AREA.0,
        fastrand::f32() * (AREA.3 - AREA.1) + AREA.1,
    )
}

fn weighted_activity() -> Activity {
    let total: u32 = ACTIVITY_WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut roll = fastrand::u32(0..total);
    for (a, w) in ACTIVITY_WEIGHTS {
        if roll < w {
            return a;
        }
        roll -= w;
    }
    Activity::Idle
}

impl MockBot {
    fn new(id: u32) -> Self {
        let (x, y) = random_point();
        let activity = weighted_activity();
        let mut bot = MockBot {
            snap: BotSnapshot {
                id,
                name: random_name(),
                race: pick(&RACES).to_string(),
                class: pick(&CLASSES).to_string(),
                level: fastrand::u8(1..=55),
                hp_pct: fastrand::u8(40..=100),
                x,
                y,
                z: 120.0 + fastrand::f32() * 30.0,
                zone: Some("Solzreed".into()),
                activity,
                goal: String::new(),
                detail: String::new(),
                partner: None,
                gold: i64::from(fastrand::u32(0..250_000)),
                has_house: fastrand::u8(0..10) == 0,
                online_secs: fastrand::u32(0..40_000),
                paused: false,
            },
            target: random_point(),
            next_change: 3.0 + fastrand::f32() * 25.0,
            log: VecDeque::new(),
            quest: 330 + fastrand::u32(0..40),
        };
        bot.say_log("logged in");
        bot.describe();
        bot
    }

    fn say_log(&mut self, text: impl Into<String>) {
        if self.log.len() >= 32 {
            self.log.pop_front();
        }
        self.log.push_back(LogLine {
            t: now_ms(),
            text: text.into(),
        });
    }

    /// Fill `goal` and `detail` for the current activity.
    fn describe(&mut self) {
        let (goal, detail) = match self.snap.activity {
            Activity::Questing => (
                "quest",
                format!("Quest {} A Friendly Reminder: talk to Maude", self.quest),
            ),
            Activity::Grinding => ("grind", "Killing Wild Boar x4".to_string()),
            Activity::Fighting => ("grind", "In combat with Wild Boar".to_string()),
            Activity::Dueling => ("duel", "Dueling".to_string()),
            Activity::Trading => ("trade", "Trade window open".to_string()),
            Activity::TradeRun => (
                "trade_run",
                "Carrying a Solzreed Specialty pack".to_string(),
            ),
            Activity::Housing => ("housing", "Looking for a free plot".to_string()),
            Activity::Resting => ("rest", "Recovering HP".to_string()),
            Activity::Traveling => ("quest", "Walking to the quest giver".to_string()),
            Activity::Dead => ("quest", "Waiting to respawn".to_string()),
            _ => ("auto", "Choosing a goal".to_string()),
        };
        self.snap.goal = goal.into();
        self.snap.detail = detail;
        self.say_log(format!(
            "now {:?}: {}",
            self.snap.activity, self.snap.detail
        ));
    }
}

impl Inner {
    fn index_of(&self, id: u32) -> Option<usize> {
        self.bots.iter().position(|b| b.snap.id == id)
    }

    /// Drop the pairing of bot `i` and its partner.
    fn unpair(&mut self, i: usize) {
        if let Some(p) = self.bots[i].snap.partner.take() {
            if let Some(j) = self.index_of(p) {
                if self.bots[j].snap.partner == Some(self.bots[i].snap.id) {
                    self.bots[j].snap.partner = None;
                    self.bots[j].next_change = 0.0;
                }
            }
        }
    }

    fn pair(&mut self, i: usize, j: usize, activity: Activity) {
        self.unpair(i);
        self.unpair(j);
        let (a, b) = (self.bots[i].snap.id, self.bots[j].snap.id);
        let meet = (
            (self.bots[i].snap.x + self.bots[j].snap.x) / 2.0,
            (self.bots[i].snap.y + self.bots[j].snap.y) / 2.0,
        );
        for (k, other, off) in [(i, b, -1.5f32), (j, a, 1.5f32)] {
            let bot = &mut self.bots[k];
            bot.snap.partner = Some(other);
            bot.snap.activity = activity;
            bot.target = (meet.0 + off, meet.1);
            bot.next_change = 10.0 + fastrand::f32() * 20.0;
            bot.describe();
        }
    }

    fn step(&mut self, dt: f32) {
        self.tick += 1;
        for i in 0..self.bots.len() {
            let paused = self.bots[i].snap.paused;
            if !paused {
                self.bots[i].next_change -= dt;
            }
            if self.bots[i].next_change <= 0.0 && !paused {
                self.unpair(i);
                let activity = weighted_activity();
                let partner_wanted = matches!(activity, Activity::Dueling | Activity::Trading);
                let other = if partner_wanted {
                    (0..self.bots.len()).find(|&j| {
                        j != i
                            && self.bots[j].snap.partner.is_none()
                            && !self.bots[j].snap.paused
                            && fastrand::u8(0..8) == 0
                    })
                } else {
                    None
                };
                match other {
                    Some(j) => self.pair(i, j, activity),
                    None => {
                        let bot = &mut self.bots[i];
                        bot.snap.activity = if partner_wanted {
                            Activity::Idle
                        } else {
                            activity
                        };
                        bot.snap.partner = None;
                        bot.target = random_point();
                        bot.next_change = 4.0 + fastrand::f32() * 25.0;
                        bot.snap.hp_pct = match bot.snap.activity {
                            Activity::Dead => 0,
                            Activity::Resting => bot.snap.hp_pct.max(30),
                            _ => bot.snap.hp_pct.max(60),
                        };
                        bot.describe();
                    }
                }
            }
            let bot = &mut self.bots[i];
            bot.snap.online_secs = bot.snap.online_secs.saturating_add(dt as u32);
            let walks = !bot.snap.paused
                && matches!(
                    bot.snap.activity,
                    Activity::Traveling
                        | Activity::Questing
                        | Activity::Grinding
                        | Activity::TradeRun
                        | Activity::Housing
                        | Activity::Dueling
                        | Activity::Trading
                );
            if walks {
                let speed = if bot.snap.activity == Activity::Traveling {
                    6.5
                } else {
                    4.0
                };
                let (dx, dy) = (bot.target.0 - bot.snap.x, bot.target.1 - bot.snap.y);
                let dist = dx.hypot(dy);
                if dist < 2.0 {
                    if !matches!(bot.snap.activity, Activity::Dueling | Activity::Trading) {
                        bot.target = random_point();
                    }
                } else {
                    let step = (speed * dt).min(dist);
                    bot.snap.x += dx / dist * step;
                    bot.snap.y += dy / dist * step;
                }
            }
            if bot.snap.activity == Activity::Resting && bot.snap.hp_pct < 100 && self.tick & 3 == 0
            {
                bot.snap.hp_pct += 1;
            }
        }
    }
}

impl MockHub {
    fn new(count: usize, with_image: bool) -> Self {
        let bots = (1..=count as u32).map(MockBot::new).collect();
        let mut markers = Vec::new();
        for i in 0..24 {
            let (x, y) = random_point();
            markers.push(Marker {
                kind: MarkerKind::House,
                x,
                y,
                label: format!("{}'s house", random_name()),
            });
            if i < 8 {
                let (x, y) = random_point();
                markers.push(Marker {
                    kind: MarkerKind::QuestGiver,
                    x,
                    y,
                    label: format!("Quest {}", 330 + i),
                });
            }
        }
        for (i, (x, y)) in [(15350.0, 15100.0), (15700.0, 15450.0), (15100.0, 15600.0)]
            .into_iter()
            .enumerate()
        {
            markers.push(Marker {
                kind: MarkerKind::DuelFlag,
                x,
                y,
                label: format!("Duel flag {}", i + 1),
            });
        }
        for (i, (x, y)) in [(15500.0, 15000.0), (15900.0, 15300.0)]
            .into_iter()
            .enumerate()
        {
            markers.push(Marker {
                kind: MarkerKind::TradeOutlet,
                x,
                y,
                label: format!("Trade outlet {}", i + 1),
            });
        }
        MockHub {
            inner: Mutex::new(Inner {
                bots,
                next_id: count as u32 + 1,
                started: Instant::now(),
                tick: 0,
            }),
            markers,
            png: with_image.then(|| make_map_png(1024, 616)),
        }
    }
}

impl BotHub for MockHub {
    fn snapshots(&self) -> Vec<BotSnapshot> {
        self.inner
            .lock()
            .bots
            .iter()
            .map(|b| b.snap.clone())
            .collect()
    }

    fn detail(&self, id: u32) -> Option<BotDetail> {
        let inner = self.inner.lock();
        let bot = inner.bots.iter().find(|b| b.snap.id == id)?;
        Some(BotDetail {
            snapshot: bot.snap.clone(),
            log: bot.log.iter().cloned().collect(),
            quests: (0..3)
                .map(|k| QuestLine {
                    id: bot.quest + k,
                    name: [
                        "A Friendly Reminder",
                        "Wild Boar Trouble",
                        "Mushrooms for Maude",
                    ][k as usize]
                        .into(),
                    status: ["active", "ready", "done"][k as usize].into(),
                })
                .collect(),
            extra: vec![
                ("Persona".into(), "quester 0.7, duelist 0.3".into()),
                ("Brain tick".into(), "250 ms".into()),
                ("Entities in view".into(), format!("{}", 5 + id % 17)),
                ("Notes".into(), "<b>not html</b> & \"quotes\"".into()),
            ],
            chat: bot
                .log
                .iter()
                .filter(|l| l.text.starts_with("says:") || l.text.starts_with("whispers"))
                .cloned()
                .collect(),
        })
    }

    fn stats(&self) -> HubStats {
        let inner = self.inner.lock();
        let total = inner.bots.len() as u32;
        let mut counts = Vec::new();
        for a in Activity::ALL {
            let n = inner.bots.iter().filter(|b| b.snap.activity == a).count() as u32;
            if n > 0 {
                counts.push((a, n));
            }
        }
        let rss = 38_000_000 + u64::from(total) * 190_000 + (inner.tick % 50) * 10_000;
        HubStats {
            bots_total: total,
            bots_online: total,
            rss_bytes: rss,
            bytes_per_bot: if total > 0 { rss / u64::from(total) } else { 0 },
            packets_in_per_sec: f32::from(total as u16) * 3.1 + fastrand::f32() * 30.0,
            packets_out_per_sec: f32::from(total as u16) * 1.4 + fastrand::f32() * 10.0,
            bytes_in_per_sec: f32::from(total as u16) * 410.0 + fastrand::f32() * 4_000.0,
            bytes_out_per_sec: f32::from(total as u16) * 95.0 + fastrand::f32() * 900.0,
            uptime_secs: inner.started.elapsed().as_secs() + 7_300,
            llm: Some(LlmStats {
                requests: 12 + inner.tick / 20,
                failures: 1,
                tokens_today: 4_100 + inner.tick * 15,
                budget_left: 500_000u64.saturating_sub(4_100 + inner.tick * 15),
                rate_limited: inner.tick / 90,
                chat_replies: 3 + inner.tick / 40,
                director_picks: 2 + inner.tick / 120,
            }),
            activity_counts: counts,
        }
    }

    fn markers(&self) -> Vec<Marker> {
        self.markers.clone()
    }

    fn map_info(&self) -> MapInfo {
        MapInfo {
            image_url: self.png.as_ref().map(|_| "/map.png".to_string()),
            width: 1024,
            height: 616,
            world: WORLD,
        }
    }

    fn map_image(&self) -> Option<Vec<u8>> {
        self.png.clone()
    }

    fn command(&self, bot: Option<u32>, cmd: Command) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let idx = match bot {
            Some(id) => Some(inner.index_of(id).ok_or_else(|| format!("no bot {id}"))?),
            None => None,
        };
        match cmd {
            Command::Pause | Command::Resume => {
                let paused = matches!(cmd, Command::Pause);
                for (i, b) in inner.bots.iter_mut().enumerate() {
                    if idx.is_none() || idx == Some(i) {
                        b.snap.paused = paused;
                        b.say_log(if paused {
                            "paused by operator"
                        } else {
                            "resumed by operator"
                        });
                    }
                }
            }
            Command::Goal { goal } => {
                let i = idx.ok_or("goal needs a bot in this mock")?;
                let activity = match goal.as_str() {
                    "quest" => Activity::Questing,
                    "grind" => Activity::Grinding,
                    "duel" => Activity::Dueling,
                    "trade" => Activity::Trading,
                    "trade_run" => Activity::TradeRun,
                    "housing" => Activity::Housing,
                    "rest" => Activity::Resting,
                    _ => Activity::Idle,
                };
                inner.unpair(i);
                let b = &mut inner.bots[i];
                b.snap.activity = activity;
                b.snap.goal = goal.clone();
                b.target = random_point();
                b.next_change = 20.0;
                b.describe();
                b.say_log(format!("operator set goal {goal}"));
            }
            Command::DuelWith { other } | Command::TradeWith { other } => {
                let i = idx.ok_or("needs a bot")?;
                let j = inner
                    .index_of(other)
                    .ok_or_else(|| format!("no bot {other}"))?;
                let activity = if matches!(cmd, Command::DuelWith { .. }) {
                    Activity::Dueling
                } else {
                    Activity::Trading
                };
                inner.pair(i, j, activity);
            }
            Command::Say { text } => {
                let i = idx.ok_or("needs a bot")?;
                inner.bots[i].say_log(format!("says: {text}"));
            }
            Command::Whisper { to, text } => {
                let i = idx.ok_or("needs a bot")?;
                inner.bots[i].say_log(format!("whispers to {to}: {text}"));
            }
            Command::Spawn { count } => {
                for _ in 0..count {
                    let id = inner.next_id;
                    inner.next_id += 1;
                    inner.bots.push(MockBot::new(id));
                }
            }
            Command::Despawn { count } => {
                for _ in 0..count {
                    if inner.bots.is_empty() {
                        break;
                    }
                    let i = inner.bots.len() - 1;
                    inner.unpair(i);
                    inner.bots.pop();
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- a tiny PNG writer

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &v in data {
        a = (a + u32::from(v)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut crc_in = kind.to_vec();
    crc_in.extend_from_slice(body);
    out.extend_from_slice(&crc_in);
    out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
}

/// An uncompressed (stored deflate) RGB PNG: bigger than needed, but needs no dependency.
fn png_rgb(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(pixels.len() + h as usize);
    for row in pixels.chunks(w as usize * 3) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bit, truecolor
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Ocean, a few islands (one under Solzreed), a 10 000 m grid and the Solzreed area outlined.
fn make_map_png(w: u32, h: u32) -> Vec<u8> {
    let span_x = WORLD.max_x - WORLD.min_x;
    let span_y = WORLD.max_y - WORLD.min_y;
    let mut px = Vec::with_capacity((w * h * 3) as usize);
    let center = ((AREA.0 + AREA.2) / 2.0, (AREA.1 + AREA.3) / 2.0);
    for py in 0..h {
        for pxl in 0..w {
            let x = WORLD.min_x + (pxl as f32 + 0.5) / w as f32 * span_x;
            let y = WORLD.max_y - (py as f32 + 0.5) / h as f32 * span_y;
            let bump = 3.0
                * (-((x - center.0).powi(2) + (y - center.1).powi(2)) / (2.0 * 3500.0f32.powi(2)))
                    .exp();
            let land =
                (x / 7000.0).sin() + (y / 5200.0 + 1.0).sin() + ((x + y) / 9000.0).sin() + bump;
            let mut rgb = if land > 0.9 {
                [88, 142, 84]
            } else if land > 0.6 {
                [214, 205, 150]
            } else {
                let depth = (land + 3.0).clamp(0.0, 3.0) / 3.0;
                [
                    (24.0 + 30.0 * depth) as u8,
                    (62.0 + 40.0 * depth) as u8,
                    (118.0 + 40.0 * depth) as u8,
                ]
            };
            // grid every 10 000 m
            let gx = (x.rem_euclid(10_000.0)) < span_x / w as f32;
            let gy = (y.rem_euclid(10_000.0)) < span_y / h as f32;
            if gx || gy {
                rgb = [rgb[0] / 2 + 20, rgb[1] / 2 + 20, rgb[2] / 2 + 30];
            }
            // Solzreed outline, widened to stay visible at this resolution
            let m = 450.0;
            let inside = x > AREA.0 - m && x < AREA.2 + m && y > AREA.1 - m && y < AREA.3 + m;
            let core = x > AREA.0 && x < AREA.2 && y > AREA.1 && y < AREA.3;
            if inside && !core {
                rgb = [235, 60, 60];
            }
            px.extend_from_slice(&rgb);
        }
    }
    png_rgb(w, h, &px)
}

// ---------------------------------------------------------------- main

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bind: SocketAddr = arg_value(&args, "--bind")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or_else(|| WebConfig::default().bind);
    let count: usize = arg_value(&args, "--bots")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(200);
    let cfg = WebConfig {
        bind,
        token: arg_value(&args, "--token"),
    };

    let hub = Arc::new(MockHub::new(count, !args.iter().any(|a| a == "--no-image")));
    let ticker = hub.clone();
    std::thread::spawn(move || {
        let dt = 0.5f32;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            ticker.inner.lock().step(dt);
        }
    });

    tracing::info!("mock hub with {count} bots; open http://{bind}/");
    pb_web::serve(hub, cfg, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
