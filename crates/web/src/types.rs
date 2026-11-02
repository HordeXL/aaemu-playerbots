//! Types shared between the web UI and whatever implements [`BotHub`].
//!
//! The web crate knows nothing about how bots work: the bot runtime publishes plain snapshots
//! through a [`BotHub`] and receives [`Command`]s back. Everything here is `serde`-friendly and is
//! exactly what the JSON API sends or accepts.

use serde::{Deserialize, Serialize};

/// What a bot is doing right now. Serialized as `snake_case` (`"trade_run"`).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    #[default]
    Offline,
    Connecting,
    Idle,
    Traveling,
    Questing,
    Grinding,
    Fighting,
    Dueling,
    Trading,
    TradeRun,
    Housing,
    Resting,
    Dead,
}

impl Activity {
    /// Every activity, in the order the UI lists them.
    pub const ALL: [Activity; 13] = [
        Activity::Offline,
        Activity::Connecting,
        Activity::Idle,
        Activity::Traveling,
        Activity::Questing,
        Activity::Grinding,
        Activity::Fighting,
        Activity::Dueling,
        Activity::Trading,
        Activity::TradeRun,
        Activity::Housing,
        Activity::Resting,
        Activity::Dead,
    ];

    /// The `snake_case` name used in the JSON API.
    pub fn as_str(self) -> &'static str {
        match self {
            Activity::Offline => "offline",
            Activity::Connecting => "connecting",
            Activity::Idle => "idle",
            Activity::Traveling => "traveling",
            Activity::Questing => "questing",
            Activity::Grinding => "grinding",
            Activity::Fighting => "fighting",
            Activity::Dueling => "dueling",
            Activity::Trading => "trading",
            Activity::TradeRun => "trade_run",
            Activity::Housing => "housing",
            Activity::Resting => "resting",
            Activity::Dead => "dead",
        }
    }
}

/// One bot as the map and the bot list show it. Published by the bot runtime about once a second.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct BotSnapshot {
    pub id: u32,
    pub name: String,
    pub race: String,
    pub class: String,
    pub level: u8,
    /// 0..=100.
    pub hp_pct: u8,
    /// World position in metres. `x`/`y` are the map plane, `z` is height.
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub zone: Option<String>,
    pub activity: Activity,
    /// Short goal name ("quest", "grind", …).
    pub goal: String,
    /// One line of detail, e.g. `Quest 330 A Friendly Reminder: talk to Maude`.
    pub detail: String,
    /// The other bot of a duel or a trade.
    pub partner: Option<u32>,
    /// Money carried. Shown as-is (with thousands separators); the unit is the hub's choice.
    pub gold: i64,
    pub has_house: bool,
    pub online_secs: u32,
    pub paused: bool,
}

/// One line of a bot's event log.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LogLine {
    /// Unix time in milliseconds.
    pub t: u64,
    pub text: String,
}

/// One quest of a bot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuestLine {
    pub id: u32,
    pub name: String,
    /// Free text, e.g. `active`, `ready`, `done`, `quarantined`.
    pub status: String,
}

/// Everything the detail panel shows about one bot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BotDetail {
    pub snapshot: BotSnapshot,
    /// Event log; the UI sorts it newest first.
    pub log: Vec<LogLine>,
    pub quests: Vec<QuestLine>,
    /// Extra key/value rows (persona, tick rate, …), shown in order.
    pub extra: Vec<(String, String)>,
    /// The last chat lines the bot heard or said, oldest first (`t` is unix milliseconds).
    #[serde(default)]
    pub chat: Vec<LogLine>,
}

/// Token-budget numbers of the optional LLM.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LlmStats {
    pub requests: u64,
    pub failures: u64,
    pub tokens_today: u64,
    /// Tokens left today; `u64::MAX` (or anything above 10^15) means "no budget".
    pub budget_left: u64,
    /// Requests refused by a local limit or the endpoint (they cost nothing).
    #[serde(default)]
    pub rate_limited: u64,
    /// Chat replies the model wrote and the bots sent.
    #[serde(default)]
    pub chat_replies: u64,
    /// Goals the model chose and the bots started.
    #[serde(default)]
    pub director_picks: u64,
}

/// Process-wide numbers for the header of the UI.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct HubStats {
    pub bots_total: u32,
    pub bots_online: u32,
    pub rss_bytes: u64,
    pub bytes_per_bot: u64,
    pub packets_in_per_sec: f32,
    pub packets_out_per_sec: f32,
    pub uptime_secs: u64,
    pub llm: Option<LlmStats>,
    /// Serialized as `[["idle", 12], ["questing", 40], …]`.
    pub activity_counts: Vec<(Activity, u32)>,
}

/// What a map marker stands for. Serialized as `snake_case`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    House,
    DuelFlag,
    QuestGiver,
    TradeOutlet,
}

/// A static point of interest drawn on the map.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Marker {
    pub kind: MarkerKind,
    pub x: f32,
    pub y: f32,
    pub label: String,
}

/// The world rectangle covered by the map image.
///
/// Pixel `(0, 0)` is `(min_x, max_y)`: `u = (x - min_x) / (max_x - min_x)` and
/// `v = 1 - (y - min_y) / (max_y - min_y)`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldRect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

/// Calibration of the map image.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MapInfo {
    /// `Some("/map.png")` when [`BotHub::map_image`] has an image; `None` draws a plain grid. The
    /// web layer appends a `?v=<hash>` cache-busting query to `/map.png` in `/api/map`.
    pub image_url: Option<String>,
    /// Pixel size of the image.
    pub width: u32,
    pub height: u32,
    pub world: WorldRect,
}

/// Something the operator asks a bot (or all bots) to do. JSON: `{"type":"goal","goal":"quest"}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Pause one bot, or every bot when no bot is given.
    Pause,
    Resume,
    /// Force a goal (see [`Command::GOALS`]); `auto` hands the choice back to the bot.
    Goal {
        goal: String,
    },
    /// Make the bot challenge the bot `other` to a duel.
    DuelWith {
        other: u32,
    },
    /// Make the bot trade with the bot `other`.
    TradeWith {
        other: u32,
    },
    /// Make the bot say a line in local chat.
    Say {
        text: String,
    },
    /// Make the bot whisper a line to the character `to`.
    Whisper {
        to: String,
        text: String,
    },
    /// Global: log in `count` more bots.
    Spawn {
        count: u32,
    },
    /// Global: log out `count` bots.
    Despawn {
        count: u32,
    },
}

impl Command {
    /// Values accepted by [`Command::Goal`].
    pub const GOALS: [&'static str; 9] = [
        "quest",
        "grind",
        "duel",
        "trade",
        "trade_run",
        "housing",
        "rest",
        "wander",
        "auto",
    ];
    /// Largest `count` of a spawn or despawn command.
    pub const MAX_COUNT: u32 = 1000;
    /// Longest `Say` / `Whisper` text, in characters.
    pub const MAX_SAY_CHARS: usize = 200;
    /// Longest whisper target, in characters.
    pub const MAX_NAME_CHARS: usize = 32;

    /// Cheap sanity checks the web layer applies before the hub sees a command.
    ///
    /// `bot` is the target from the URL (`None` for `POST /api/command`).
    pub fn validate(&self, bot: Option<u32>) -> Result<(), String> {
        match self {
            Command::Pause | Command::Resume => Ok(()),
            Command::Goal { goal } => {
                if Command::GOALS.contains(&goal.as_str()) {
                    Ok(())
                } else {
                    Err(format!(
                        "unknown goal {goal:?}; expected one of {}",
                        Command::GOALS.join(", ")
                    ))
                }
            }
            Command::DuelWith { other } | Command::TradeWith { other } => match bot {
                None => Err("this command needs a bot: POST /api/bots/<id>/command".into()),
                Some(b) if b == *other => Err("a bot cannot pair with itself".into()),
                Some(_) => Ok(()),
            },
            Command::Say { text } | Command::Whisper { text, .. } => {
                if bot.is_none() {
                    return Err("this command needs a bot: POST /api/bots/<id>/command".into());
                }
                if let Command::Whisper { to, .. } = self {
                    let to = to.trim();
                    if to.is_empty() || to.chars().count() > Command::MAX_NAME_CHARS {
                        return Err(format!(
                            "the whisper target must be a character name of 1..={} characters",
                            Command::MAX_NAME_CHARS
                        ));
                    }
                    if !to
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        return Err("the whisper target is not a plain character name".into());
                    }
                }
                let text = text.trim();
                if text.is_empty() {
                    Err("text is empty".into())
                } else if text.chars().count() > Command::MAX_SAY_CHARS {
                    Err(format!(
                        "text is longer than {} characters",
                        Command::MAX_SAY_CHARS
                    ))
                } else if text.starts_with('/') {
                    Err("text starting with '/' would run as a chat command; not allowed".into())
                } else if text.chars().any(char::is_control) {
                    Err("text contains control characters".into())
                } else {
                    Ok(())
                }
            }
            Command::Spawn { count } | Command::Despawn { count } => {
                if bot.is_some() {
                    Err("spawn/despawn are global: POST /api/command".into())
                } else if *count == 0 || *count > Command::MAX_COUNT {
                    Err(format!("count must be 1..={}", Command::MAX_COUNT))
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// What the web UI needs from the bot runtime. Implemented by the `playerbots` binary.
///
/// All methods are called from async handlers, so they must return quickly and never block on
/// network I/O: read published snapshots, enqueue commands.
pub trait BotHub: Send + Sync + 'static {
    /// One snapshot per bot (also offline ones), in any order.
    fn snapshots(&self) -> Vec<BotSnapshot>;
    /// Detail of one bot, `None` if the id is unknown.
    fn detail(&self, id: u32) -> Option<BotDetail>;
    fn stats(&self) -> HubStats;
    fn markers(&self) -> Vec<Marker>;
    fn map_info(&self) -> MapInfo;
    /// PNG bytes of the world map, served at `/map.png`. Called at most every few minutes by the
    /// web layer, but the hub should still cache the encoded bytes.
    fn map_image(&self) -> Option<Vec<u8>>;
    /// Run a command. `bot` is `None` for global commands (spawn/despawn, pause all, …). The error
    /// text is shown to the operator.
    fn command(&self, bot: Option<u32>, cmd: Command) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_names_match_serde() {
        let mut seen = std::collections::HashSet::new();
        for a in Activity::ALL {
            let json = serde_json::to_string(&a).unwrap();
            assert_eq!(json, format!("\"{}\"", a.as_str()));
            assert!(seen.insert(a), "{a:?} listed twice");
        }
        assert_eq!(seen.len(), 13);
    }

    #[test]
    fn command_json_shapes() {
        let cases = [
            (r#"{"type":"pause"}"#, Command::Pause),
            (r#"{"type":"resume"}"#, Command::Resume),
            (
                r#"{"type":"goal","goal":"trade_run"}"#,
                Command::Goal {
                    goal: "trade_run".into(),
                },
            ),
            (
                r#"{"type":"duel_with","other":7}"#,
                Command::DuelWith { other: 7 },
            ),
            (
                r#"{"type":"trade_with","other":9}"#,
                Command::TradeWith { other: 9 },
            ),
            (
                r#"{"type":"say","text":"hello"}"#,
                Command::Say {
                    text: "hello".into(),
                },
            ),
            (
                r#"{"type":"whisper","to":"Bob","text":"hello"}"#,
                Command::Whisper {
                    to: "Bob".into(),
                    text: "hello".into(),
                },
            ),
            (r#"{"type":"spawn","count":5}"#, Command::Spawn { count: 5 }),
            (
                r#"{"type":"despawn","count":2}"#,
                Command::Despawn { count: 2 },
            ),
        ];
        for (json, want) in cases {
            let got: Command = serde_json::from_str(json).unwrap();
            assert_eq!(got, want, "{json}");
            // and back, so the mock UI and the tests agree on the wire format
            let again: Command =
                serde_json::from_str(&serde_json::to_string(&got).unwrap()).unwrap();
            assert_eq!(again, want);
        }
        assert!(serde_json::from_str::<Command>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Command>(r#"{"type":"spawn"}"#).is_err());
    }

    #[test]
    fn validation_rules() {
        let g = |s: &str| Command::Goal { goal: s.into() };
        assert!(g("auto").validate(Some(1)).is_ok());
        assert!(g("auto").validate(None).is_ok());
        assert!(g("fly").validate(Some(1)).is_err());

        assert!(Command::DuelWith { other: 2 }.validate(Some(1)).is_ok());
        assert!(Command::DuelWith { other: 1 }.validate(Some(1)).is_err());
        assert!(Command::TradeWith { other: 2 }.validate(None).is_err());

        let say = |s: &str| Command::Say { text: s.into() };
        assert!(say("hi there").validate(Some(1)).is_ok());
        assert!(say("   ").validate(Some(1)).is_err());
        assert!(say("/gold add self 1").validate(Some(1)).is_err());
        assert!(say("a\nb").validate(Some(1)).is_err());
        assert!(say(&"x".repeat(201)).validate(Some(1)).is_err());
        assert!(say("hi").validate(None).is_err());

        let whisper = |to: &str, text: &str| Command::Whisper {
            to: to.into(),
            text: text.into(),
        };
        assert!(whisper("Bob", "hi there").validate(Some(1)).is_ok());
        assert!(whisper("Bob", "hi there").validate(None).is_err());
        assert!(whisper("", "hi").validate(Some(1)).is_err());
        assert!(whisper("Bo b", "hi").validate(Some(1)).is_err());
        assert!(whisper("/Bob", "hi").validate(Some(1)).is_err());
        assert!(whisper(&"B".repeat(33), "hi").validate(Some(1)).is_err());
        assert!(whisper("Bob", "/gold add self 1")
            .validate(Some(1))
            .is_err());
        assert!(whisper("Bob", "  ").validate(Some(1)).is_err());
        assert!(g("wander").validate(Some(1)).is_ok());

        assert!(Command::Spawn { count: 10 }.validate(None).is_ok());
        assert!(Command::Spawn { count: 0 }.validate(None).is_err());
        assert!(Command::Despawn { count: 1001 }.validate(None).is_err());
        assert!(Command::Spawn { count: 1 }.validate(Some(3)).is_err());
    }

    #[test]
    fn stats_json_uses_pairs_for_counts() {
        let stats = HubStats {
            activity_counts: vec![(Activity::Idle, 3), (Activity::TradeRun, 1)],
            ..HubStats::default()
        };
        let v = serde_json::to_value(&stats).unwrap();
        assert_eq!(v["activity_counts"][0][0], "idle");
        assert_eq!(v["activity_counts"][1][0], "trade_run");
        assert_eq!(v["activity_counts"][1][1], 1);
        assert!(v["llm"].is_null());
    }

    #[test]
    fn older_json_without_the_chat_and_llm_extras_still_parses() {
        let stats: LlmStats =
            serde_json::from_str(r#"{"requests":1,"failures":0,"tokens_today":5,"budget_left":9}"#)
                .unwrap();
        assert_eq!(
            (stats.rate_limited, stats.chat_replies, stats.director_picks),
            (0, 0, 0)
        );
        let detail: BotDetail = serde_json::from_str(
            r#"{"snapshot":{"id":1,"name":"A","race":"","class":"","level":1,"hp_pct":1,"x":0,"y":0,"z":0,"zone":null,"activity":"idle","goal":"","detail":"","partner":null,"gold":0,"has_house":false,"online_secs":0,"paused":false},"log":[],"quests":[],"extra":[]}"#,
        )
        .unwrap();
        assert!(detail.chat.is_empty());
    }
}
