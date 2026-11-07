//! A mock bot hub for developing the web UI without any bot runtime.
//!
//! ```text
//! cargo run -p pb-web --example mock -- [--bind 127.0.0.1:8790] [--bots 200] [--token SECRET] [--no-image]
//! ```
//!
//! About 200 fake bots wander around Solzreed (world x 15000..16200, y 14800..15800), change
//! activity at random, pair up for duels and trades and answer the commands of the UI. The map is
//! a generated PNG calibrated to the real world rectangle, with the Solzreed area (west) and a
//! made-up east area outlined so the calibration can be checked by eye. A handful of "players"
//! wander about, mostly in the east, so the World, Economy and Director panels have something to
//! show; the director is a toy that follows the same rules in a few lines.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use pb_web::{
    ActionRecord, Activity, Alliance, BotDetail, BotHub, BotSnapshot, Command, Continent,
    DirectorView, EconomySources, EconomyView, HealthView, HeatCell, HubStats, IssueAction,
    IssueDetail, IssueSample, IssueSummary, ItemRow, LlmStats, LogLine, MapInfo, Marker,
    MarkerKind, PlayerSeen, PlotLine, Policy, PolicyPatch, QuestLine, RegionCell, ServerView,
    SideRow, WebConfig, WorldRect, WorldTotals, WorldView, ZoneRow,
};

const AREA: (f32, f32, f32, f32) = (15000.0, 14800.0, 16200.0, 15800.0);
/// A made-up area on the east continent.
const AREA_EAST: (f32, f32, f32, f32) = (30200.0, 20400.0, 31400.0, 21400.0);
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
const RACES_WEST: [&str; 2] = ["Nuian", "Elf"];
const RACES_EAST: [&str; 2] = ["Hariharan", "Ferre"];
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
    east: bool,
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
    pop: MockPop,
}

/// A made-up player.
struct MockPlayer {
    name: String,
    level: u8,
    east: bool,
    pos: (f32, f32),
    target: (f32, f32),
}

/// The population panels' state.
struct MockPop {
    players: Vec<MockPlayer>,
    policy: Policy,
    log: VecDeque<ActionRecord>,
    owned: Vec<u32>,
    last_action: f32,
    clock: f32,
}

fn area(east: bool) -> (f32, f32, f32, f32) {
    if east {
        AREA_EAST
    } else {
        AREA
    }
}

fn zone_of(east: bool) -> (u16, &'static str) {
    if east {
        (9, "Mahadevi")
    } else {
        (5, "Solzreed")
    }
}

struct MockHub {
    inner: Mutex<Inner>,
    issues: Mutex<Vec<IssueDetail>>,
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

fn random_point(east: bool) -> (f32, f32) {
    let a = area(east);
    (
        fastrand::f32() * (a.2 - a.0) + a.0,
        fastrand::f32() * (a.3 - a.1) + a.1,
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
        Self::new_on(id, fastrand::u8(0..10) < 4)
    }

    fn new_on(id: u32, east: bool) -> Self {
        let (x, y) = random_point(east);
        let activity = weighted_activity();
        let (zone_key, zone_name) = zone_of(east);
        let mut bot = MockBot {
            east,
            snap: BotSnapshot {
                id,
                name: random_name(),
                race: pick(if east { &RACES_EAST } else { &RACES_WEST }).to_string(),
                class: pick(&CLASSES).to_string(),
                level: fastrand::u8(1..=55),
                hp_pct: fastrand::u8(40..=100),
                x,
                y,
                z: 120.0 + fastrand::f32() * 30.0,
                zone: Some(zone_name.into()),
                mother_faction: if east { 149 } else { 148 },
                alliance: if east {
                    Alliance::Haranya
                } else {
                    Alliance::Nuia
                },
                continent: if east {
                    Continent::East
                } else {
                    Continent::West
                },
                zone_key,
                activity,
                goal: String::new(),
                detail: String::new(),
                partner: None,
                gold: i64::from(fastrand::u32(0..250_000)),
                has_house: fastrand::u8(0..10) == 0,
                online_secs: fastrand::u32(0..40_000),
                paused: false,
                ..BotSnapshot::default()
            },
            target: random_point(east),
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
        self.pop.step(dt);
        self.toy_director();
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
                        bot.target = random_point(bot.east);
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
                        bot.target = random_point(bot.east);
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

/// One fake issue for the issue panel.
#[allow(clippy::too_many_arguments)]
fn mock_issue(
    fp: &str,
    kind: &str,
    title: &str,
    suspect: &str,
    severity: &str,
    count: u64,
    bots: u32,
    state: &str,
    reportable: bool,
) -> IssueDetail {
    let now = now_ms();
    let sample = |age_s: u64, bot: &str, actual: &str| {
        IssueSample {
        at: now - age_s * 1000,
        bot: bot.into(),
        expected: "the quest log counts the kill within a few seconds".into(),
        actual: actual.into(),
        context: format!("bot {bot} (level 12 Elf Archery) doing quest (Quest 251: kill boars) at (15348, 15102, 120) in Solzreed"),
        facts: vec![("quest".into(), "251".into()), ("npc".into(), "3475".into())],
        trail: vec![
            "-   900 ms  C2S L1 0x002c CSChangeTargetPacket".into(),
            "-   410 ms  S2C L1 0x0123 SCUnitDeathPacket".into(),
            "-    10 ms  S2C L1 0x0456 SCQuestContextUpdatedPacket".into(),
            "packet head (6 bytes): 0a0b0c0d0e0f".into(),
        ],
        server_log: vec!["14:03:09 [WARN] aaemu_game::systems::quests::engine - no objective matches npc 3475".into()],
        bot_log: vec!["14:03:05 killed Wild Boar".into(), "14:03:08 quest 251: waiting for credit".into()],
    }
    };
    IssueDetail {
        summary: IssueSummary {
            fingerprint: fp.into(),
            kind: kind.into(),
            title: title.into(),
            suspect: suspect.into(),
            severity: severity.into(),
            count,
            bots,
            first_seen: now - 3_600_000,
            last_seen: now - 90_000,
            state: state.into(),
            reportable,
        },
        entry: [
            format!("[2026-11-02 14:03:09 EST] {title} (playerbots)"),
            format!("Observed by playerbots: the quest log did not change within 5 s. Expected: the quest log counts the kill. Seen {count} times by {bots} bots between 2026-11-02 13:03:09 and 2026-11-02 14:01:39."),
            "Context: bot Aria (level 12 Elf Archery) doing quest (Quest 251: kill boars) at (15348, 15102, 120) in Solzreed; quest=251, npc=3475.".to_string(),
            "Likely server area: rust/crates/aaemu-game/src/systems/quests/ (hint from the bot, not verified).".to_string(),
            "Server log at the time: 14:03:09 [WARN] aaemu_game::systems::quests::engine - no objective matches npc 3475 (1 line in the evidence).".to_string(),
            format!("Agent follow-up: reproduce from the packet trail in the evidence; verify against the server code before changing anything. This report comes from an automated client and may be a bot-side misunderstanding (suspect: {suspect})."),
            format!("Evidence: C:/path/to/evidence/{fp} (report.json, trail.txt, server-log.txt, bot-log.txt). Marker {fp}."),
        ]
        .join("\r\n"),
        area: Some("rust/crates/aaemu-game/src/systems/quests/".into()),
        evidence: (state != "open").then(|| format!("C:/path/to/evidence/{fp}")),
        queued_at: (state == "queued").then_some(now - 1_800_000),
        samples: vec![
            sample(3_600, "Aria", "the quest log did not change within 5 s"),
            sample(300, "Bron", "no credit after 5 s; the server logged a warning"),
            sample(90, "Aria", "the quest log did not change within 5 s"),
        ],
    }
}

fn mock_issues() -> Vec<IssueDetail> {
    vec![
        mock_issue(
            "pb:6f3b2e1c",
            "quest_credit_missing",
            "Quest 251: killing NPC 3475 gives no credit",
            "server",
            "medium",
            7,
            3,
            "open",
            true,
        ),
        mock_issue(
            "pb:a41c09d2",
            "kicked",
            "The server kicked a bot (reason 3)",
            "server",
            "high",
            1,
            1,
            "queued",
            true,
        ),
        mock_issue(
            "pb:0c77be10",
            "npc_missing",
            "NPC 3512 is not at its spawn",
            "data",
            "low",
            2,
            1,
            "open",
            false,
        ),
        mock_issue(
            "pb:92d0f3a8",
            "death_without_packet",
            "Bot looks dead but the server never said so",
            "bot",
            "high",
            4,
            2,
            "open",
            false,
        ),
        mock_issue(
            "pb:e5a2177b",
            "server_log_during_action",
            "Server logged an error about a bot: house tax",
            "server",
            "high",
            3,
            2,
            "resolved",
            true,
        ),
    ]
}

impl MockHub {
    fn new(count: usize, with_image: bool) -> Self {
        let bots = (1..=count as u32).map(MockBot::new).collect();
        let mut markers = Vec::new();
        for i in 0..24 {
            let (x, y) = random_point(i % 3 == 0);
            markers.push(Marker {
                kind: MarkerKind::House,
                x,
                y,
                label: format!("{}'s house", random_name()),
            });
            if i < 8 {
                let (x, y) = random_point(false);
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
                pop: MockPop::new(),
            }),
            issues: Mutex::new(mock_issues()),
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
            path: Vec::new(),
            plots: if id.is_multiple_of(3) {
                vec![
                    PlotLine {
                        x: 14_210.0,
                        y: 9_020.5,
                        crop: "Potato".into(),
                        state: "ripe".into(),
                        honest: true,
                        land: "farm".into(),
                    },
                    PlotLine {
                        x: 14_214.5,
                        y: 9_020.5,
                        crop: "Potato".into(),
                        state: "growing, about 6 min left".into(),
                        honest: false,
                        land: "own land".into(),
                    },
                ]
            } else {
                Vec::new()
            },
            gear: None,
            profile: None,
            journal: None,
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
        // One lock for both counts: a second `self.issues.lock()` inside the struct literal below
        // would wait for the first guard (a temporary that lives to the end of the statement) and
        // deadlock `/api/stats` and, through `inner`, the whole mock.
        let (open_issues, queued_issues) = {
            let issues = self.issues.lock();
            let count = |state: &str| issues.iter().filter(|i| i.summary.state == state).count();
            (count("open") as u32, count("queued") as u32)
        };
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
                plans: 1 + inner.tick / 150,
                plan_steps: 2 + inner.tick / 60,
                notes: inner.tick / 300,
            }),
            open_issues,
            queued_issues,
            activity_counts: counts,
            nav: None,
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

    fn zones(&self) -> Vec<pb_web::ZoneEntry> {
        [
            (129, "Solzreed Peninsula"),
            (133, "Gweonid Forest"),
            (144, "Arcum Iris"),
        ]
        .into_iter()
        .map(|(key, name)| pb_web::ZoneEntry {
            key,
            name: name.into(),
        })
        .collect()
    }

    fn issues(&self) -> Vec<IssueSummary> {
        self.issues
            .lock()
            .iter()
            .map(|i| i.summary.clone())
            .collect()
    }

    fn issue(&self, fingerprint: &str) -> Option<IssueDetail> {
        self.issues
            .lock()
            .iter()
            .find(|i| i.summary.fingerprint == fingerprint)
            .cloned()
    }

    fn issue_action(&self, fingerprint: &str, action: IssueAction) -> Result<(), String> {
        let mut issues = self.issues.lock();
        let issue = issues
            .iter_mut()
            .find(|i| i.summary.fingerprint == fingerprint)
            .ok_or_else(|| format!("no issue {fingerprint}"))?;
        let s = &mut issue.summary;
        match action {
            IssueAction::Queue if s.state == "queued" => return Err("already queued".into()),
            IssueAction::Queue => {
                s.state = "queued".into();
                issue.queued_at = Some(now_ms());
            }
            IssueAction::Ignore => s.state = "ignored".into(),
            IssueAction::Resolve => s.state = "resolved".into(),
            IssueAction::Reopen => s.state = "open".into(),
        }
        if action != IssueAction::Queue {
            issue.queued_at = None;
        }
        Ok(())
    }

    fn world(&self) -> WorldView {
        let inner = self.inner.lock();
        inner.world_view()
    }

    fn economy(&self) -> EconomyView {
        mock_economy(&self.inner.lock().pop.policy)
    }

    fn director(&self) -> DirectorView {
        self.inner.lock().director_view()
    }

    fn set_policy(&self, patch: PolicyPatch) -> Result<(), String> {
        let mut inner = self.inner.lock();
        let p = &mut inner.pop.policy;
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = patch.$f { p.$f = v; } )* };
        }
        set!(
            active,
            dry_run,
            max_online,
            min_per_side,
            mirror_pct,
            per_side_cap,
            step,
            hysteresis,
            despawn,
            backoff_rss_mb,
            backoff_packets_in,
            backoff_login_failures,
            backoff_open_issues,
            auto_boost_min,
            auto_boost
        );
        if let Some(b) = patch.boosts {
            p.boosts = b
                .into_iter()
                .map(|w| pb_web::BoostWish {
                    template: w.template,
                    until_ms: if w.hours == 0 {
                        0
                    } else {
                        now_ms() + u64::from(w.hours) * 3_600_000
                    },
                    auto: false,
                })
                .collect();
        }
        if let Some(g) = patch.goal_bias {
            p.goal_bias = g;
        }
        inner.pop.note(
            "policy",
            Alliance::Unknown,
            0,
            true,
            "changed by the operator",
            "",
        );
        Ok(())
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
                b.target = random_point(b.east);
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
            Command::Act(act) => {
                for (i, b) in inner.bots.iter_mut().enumerate() {
                    if idx.is_none() || idx == Some(i) {
                        b.snap.goal = act.goal.clone();
                        b.say_log(format!("act: {}", act.goal));
                    }
                }
            }
            Command::Profile(_) => {
                let i = idx.ok_or("needs a bot")?;
                inner.bots[i].say_log("personality changed");
            }
            Command::Note { text } => {
                let i = idx.ok_or("needs a bot")?;
                inner.bots[i].say_log(format!("note: {text}"));
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
            Command::SpawnWhere { count, filter } => {
                let east = match (&filter.alliance, &filter.continent, &filter.race) {
                    (Some(a), _, _) => a == "haranya",
                    (_, Some(c), _) => c == "east",
                    (_, _, Some(r)) => r == "hariharan" || r == "ferre",
                    _ => fastrand::bool(),
                };
                for _ in 0..count {
                    let id = inner.next_id;
                    inner.next_id += 1;
                    inner.bots.push(MockBot::new_on(id, east));
                }
            }
            Command::DespawnWhere { count, filter } => {
                let mut left = count;
                let mut i = inner.bots.len();
                while i > 0 && left > 0 {
                    i -= 1;
                    let b = &inner.bots[i].snap;
                    let side = |a: &Option<String>, want: &str, have: &str| {
                        a.as_deref().is_none_or(|a| a == want || have.is_empty())
                    };
                    let ok = side(&filter.alliance, b.alliance.as_str(), "")
                        && side(&filter.continent, b.continent.as_str(), "")
                        && filter
                            .race
                            .as_deref()
                            .is_none_or(|r| r.eq_ignore_ascii_case(&b.race))
                        && !matches!(b.activity, Activity::Dueling | Activity::Trading);
                    if ok {
                        inner.unpair(i);
                        inner.bots.remove(i);
                        left -= 1;
                    }
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
            for a in [AREA, AREA_EAST] {
                let inside = x > a.0 - m && x < a.2 + m && y > a.1 - m && y < a.3 + m;
                let core = x > a.0 && x < a.2 && y > a.1 && y < a.3;
                if inside && !core {
                    rgb = [235, 60, 60];
                }
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

// ---------------------------------------------------------------- the population panels

impl MockPop {
    fn new() -> Self {
        let players = (0..9)
            .map(|i| {
                let east = i < 7;
                let pos = random_point(east);
                MockPlayer {
                    name: random_name(),
                    level: fastrand::u8(10..=55),
                    east,
                    pos,
                    target: random_point(east),
                }
            })
            .collect();
        MockPop {
            players,
            policy: Policy {
                active: true,
                dry_run: true,
                ..Policy::default()
            },
            log: VecDeque::new(),
            owned: Vec::new(),
            last_action: 0.0,
            clock: 0.0,
        }
    }

    fn step(&mut self, dt: f32) {
        self.clock += dt;
        for p in &mut self.players {
            let (dx, dy) = (p.target.0 - p.pos.0, p.target.1 - p.pos.1);
            let d = dx.hypot(dy);
            if d < 3.0 {
                p.target = random_point(p.east);
            } else {
                let s = (5.0 * dt).min(d);
                p.pos.0 += dx / d * s;
                p.pos.1 += dy / d * s;
            }
        }
    }

    fn note(
        &mut self,
        kind: &str,
        alliance: Alliance,
        count: u32,
        applied: bool,
        reason: &str,
        result: &str,
    ) {
        if self.log.len() >= 100 {
            self.log.pop_back();
        }
        self.log.push_front(ActionRecord {
            at_ms: now_ms(),
            kind: kind.into(),
            alliance,
            count,
            applied,
            reason: reason.into(),
            result: result.into(),
        });
    }
}

impl Inner {
    fn bots_on(&self, east: bool) -> u32 {
        self.bots.iter().filter(|b| b.east == east).count() as u32
    }

    fn players_on(&self, east: bool) -> u32 {
        self.pop.players.iter().filter(|p| p.east == east).count() as u32
    }

    /// The mirror rule in a few lines: each side aims for as many characters as the other side has
    /// players.
    fn target_for(&self, east: bool) -> u32 {
        let p = &self.pop.policy;
        let mirror = (p.mirror_pct * self.players_on(!east)).div_ceil(100);
        mirror
            .saturating_sub(self.players_on(east))
            .max(p.min_per_side)
            .min(p.per_side_cap)
    }

    fn toy_director(&mut self) {
        let p = self.pop.policy.clone();
        if !p.active || self.pop.clock - self.pop.last_action < 20.0 {
            return;
        }
        for east in [false, true] {
            let (have, want) = (self.bots_on(east), self.target_for(east));
            let side = if east {
                Alliance::Haranya
            } else {
                Alliance::Nuia
            };
            let name = if east { "east" } else { "west" };
            if have < want && (self.bots.len() as u32) < p.max_online {
                let n = (want - have).min(p.step);
                let reason = format!(
                    "{} players on the {} side, {} on the {}: {name} wants {want} bots, has {have}",
                    self.players_on(!east),
                    if east { "west" } else { "east" },
                    self.players_on(east),
                    name
                );
                if p.dry_run {
                    self.pop.note("spawn", side, n, false, &reason, "dry run");
                } else {
                    for _ in 0..n {
                        let id = self.next_id;
                        self.next_id += 1;
                        self.pop.owned.push(id);
                        self.bots.push(MockBot::new_on(id, east));
                    }
                    self.pop
                        .note("spawn", side, n, true, &reason, &format!("{n} bots queued"));
                }
                self.pop.last_action = self.pop.clock;
                return;
            }
        }
    }

    fn world_view(&self) -> WorldView {
        let mut cells: Vec<RegionCell> = Vec::new();
        let mut zones: Vec<ZoneRow> = Vec::new();
        let mut heat: Vec<HeatCell> = Vec::new();
        let mut add = |east: bool, player: bool, x: f32, y: f32| {
            let (key, name) = zone_of(east);
            let (continent, alliance) = if east {
                (Continent::East, Alliance::Haranya)
            } else {
                (Continent::West, Alliance::Nuia)
            };
            let c = match cells
                .iter()
                .position(|c| c.continent == continent && c.alliance == alliance)
            {
                Some(i) => i,
                None => {
                    cells.push(RegionCell {
                        continent,
                        alliance,
                        ..RegionCell::default()
                    });
                    cells.len() - 1
                }
            };
            let z = match zones.iter().position(|z| z.zone_key == key) {
                Some(i) => i,
                None => {
                    zones.push(ZoneRow {
                        zone_key: key,
                        name: name.into(),
                        continent,
                        ..ZoneRow::default()
                    });
                    zones.len() - 1
                }
            };
            let (cx, cy) = (
                (x / 512.0).floor() * 512.0 + 256.0,
                (y / 512.0).floor() * 512.0 + 256.0,
            );
            let h = match heat.iter().position(|h| h.x == cx && h.y == cy) {
                Some(i) => i,
                None => {
                    heat.push(HeatCell {
                        x: cx,
                        y: cy,
                        players: 0,
                        bots: 0,
                    });
                    heat.len() - 1
                }
            };
            if player {
                cells[c].players_seen += 1;
                zones[z].players_seen += 1;
                heat[h].players += 1;
            } else {
                cells[c].bots_online += 1;
                zones[z].bots_online += 1;
                heat[h].bots += 1;
            }
        };
        for b in &self.bots {
            add(b.east, false, b.snap.x, b.snap.y);
        }
        for p in &self.pop.players {
            add(p.east, true, p.pos.0, p.pos.1);
        }
        let players: Vec<PlayerSeen> = self
            .pop
            .players
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (_, zone) = zone_of(p.east);
                PlayerSeen {
                    name: p.name.clone(),
                    level: p.level,
                    alliance: if p.east {
                        Alliance::Haranya
                    } else {
                        Alliance::Nuia
                    },
                    continent: if p.east {
                        Continent::East
                    } else {
                        Continent::West
                    },
                    zone: zone.into(),
                    x: p.pos.0,
                    y: p.pos.1,
                    age_secs: (i as u32 * 7) % 40,
                    seen_by: 1 + i as u32,
                }
            })
            .collect();
        let seen = players.len() as u32;
        let server_players = seen + 3;
        WorldView {
            at_ms: now_ms(),
            ttl_secs: 60,
            cells,
            zones,
            players,
            heat,
            totals: WorldTotals {
                players_seen: seen,
                bots_online: self.bots.len() as u32,
                bots_total: self.bots.len() as u32 + 20,
                players_server: Some(server_players),
                players_unlocated: Some(server_players - seen),
                coverage_pct: Some(seen * 100 / server_players),
                watched_km2: self.bots.len() as f32 * 0.05,
            },
            server: ServerView {
                configured: true,
                reachable: true,
                checked_ms: now_ms(),
                status: vec![
                    ("Server uptime".into(), "03:12:44".into()),
                    (
                        "Players online".into(),
                        format!("{}", server_players + self.bots.len() as u32),
                    ),
                    ("Number of TaskManager Jobs".into(), "0".into()),
                ],
                online: Some(server_players + self.bots.len() as u32),
                online_bots: Some(self.bots.len() as u32),
                auction_lots: Some(214),
                latency_ms: 4,
                error: None,
            },
        }
    }

    fn director_view(&self) -> DirectorView {
        let p = self.pop.policy.clone();
        let sides = [false, true]
            .into_iter()
            .map(|east| SideRow {
                alliance: if east {
                    Alliance::Haranya
                } else {
                    Alliance::Nuia
                },
                players: self.players_on(east),
                bots: self.bots_on(east),
                owned: self
                    .bots
                    .iter()
                    .filter(|b| b.east == east && self.pop.owned.contains(&b.snap.id))
                    .count() as u32,
                target: self.target_for(east),
            })
            .collect();
        let mode = if !p.active {
            "off"
        } else if p.dry_run {
            "dry_run"
        } else {
            "live"
        };
        DirectorView {
            available: true,
            policy: p,
            mode: mode.into(),
            sides,
            health: HealthView {
                rss_mb: 190 + self.bots.len() as u32 / 3,
                packets_in_per_sec: self.bots.len() as f32 * 3.1,
                packets_out_per_sec: self.bots.len() as f32 * 1.4,
                login_failures: 0,
                open_issues: 3,
                connecting: 0,
                holding: None,
            },
            owned: self.pop.owned.len() as u32,
            next_action_secs: (20.0 - (self.pop.clock - self.pop.last_action)).max(0.0) as u32,
            spawned_last_min: 0,
            max_spawn_per_min: 10,
            min_action_gap_secs: 20,
            auto_wishes: vec![3412],
            log: self.pop.log.iter().cloned().collect(),
        }
    }
}

fn mock_economy(policy: &Policy) -> EconomyView {
    // (template, name, lots, unit price, vendor price, trend %, wtb, wts)
    type Row = (u32, &'static str, u32, u32, u32, f32, u32, u32);
    let rows: [Row; 10] = [
        (3412, "Copper", 0, 1500, 300, 22.0, 5, 0),
        (3411, "Copper Ore", 1, 900, 250, 12.0, 3, 1),
        (3712, "Hay Bale", 2, 640, 200, 8.0, 2, 0),
        (7765, "Blue Salt", 4, 1100, 700, 3.0, 1, 1),
        (8001, "Rough Log", 5, 210, 180, 0.0, 0, 0),
        (8002, "Iron Ore", 6, 320, 300, -2.0, 0, 1),
        (8003, "Cloth", 9, 140, 150, -6.0, 0, 2),
        (8004, "Lumber", 14, 200, 250, -10.0, 0, 3),
        (8005, "Flax", 20, 90, 100, 0.0, 0, 0),
        (8006, "Wool", 31, 70, 90, -4.0, 0, 1),
    ];
    let items: Vec<ItemRow> = rows
        .into_iter()
        .map(|(template, name, lots, unit, vendor, trend, wtb, wts)| {
            let ratio = unit as f32 / vendor as f32;
            let supply = 1.0 / (1.0 + lots as f32 / 2.0);
            let scarcity = (0.45 * supply
                + 0.25 * ((ratio - 1.0) / 3.0).clamp(0.0, 1.0)
                + 0.15 * (trend / 50.0).clamp(0.0, 1.0)
                + 0.15 * ((wtb as f32 - wts as f32) / 4.0).clamp(0.0, 1.0))
            .clamp(0.0, 1.0);
            let mut reasons = Vec::new();
            if lots <= 2 {
                reasons.push(format!("only {lots} lots"));
            }
            if ratio >= 1.8 {
                reasons.push(format!("{ratio:.1}x the vendor price"));
            }
            if wtb > wts {
                reasons.push(format!("{wtb} wanted"));
            }
            ItemRow {
                template,
                name: name.into(),
                scarcity,
                lots,
                supply_source: if template < 7000 { "bots" } else { "server" }.into(),
                unit_price: unit,
                vendor_price: vendor,
                price_ratio: ratio,
                trend_pct: trend,
                wtb,
                wts,
                reasons,
                boosted: policy.boosts.iter().any(|b| b.template == template),
                growing: if template.is_multiple_of(5) {
                    12.0
                } else {
                    0.0
                },
            }
        })
        .collect();
    EconomyView {
        at_ms: now_ms(),
        sources: EconomySources {
            bot_notes: 31,
            server_lots: Some(214),
            chat_lines: 17,
            chat_window_secs: 600,
        },
        items,
    }
}
