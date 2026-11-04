//! The population side of the API: what the World, Economy and Director panels show, and what the
//! operator can set. Plain `serde` types; the bot runtime fills them, this crate only serves them.
//!
//! Vocabulary: a *side* is an alliance (Nuia in the west, Haranya in the east); a *continent* is
//! where a position is on the map (west, east, Auroria, sea). A *player* is a character that is not
//! one of our bots.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The two sides players choose, plus what the data does not place. `snake_case` in JSON.
#[derive(
    Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Alliance {
    /// Not known (before the character spawned, or a faction the data does not list).
    #[default]
    Unknown,
    /// The Nuia alliance (Nuian, Elf), at home on the west continent.
    Nuia,
    /// The Haranya alliance (Hariharan, Ferre), at home on the east continent.
    Haranya,
    /// Another faction (pirates, monsters, a duel flag).
    Other,
}

impl Alliance {
    /// The two sides in the order the UI lists them.
    pub const SIDES: [Alliance; 2] = [Alliance::Nuia, Alliance::Haranya];

    /// The `snake_case` name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Alliance::Unknown => "unknown",
            Alliance::Nuia => "nuia",
            Alliance::Haranya => "haranya",
            Alliance::Other => "other",
        }
    }

    /// The side `name` stands for (`nuia`, `haranya`; case-insensitive).
    pub fn parse(name: &str) -> Option<Alliance> {
        match name.trim().to_ascii_lowercase().as_str() {
            "nuia" => Some(Alliance::Nuia),
            "haranya" => Some(Alliance::Haranya),
            _ => None,
        }
    }

    /// The other side; `Unknown` for anything that is not a side.
    pub fn opposite(self) -> Alliance {
        match self {
            Alliance::Nuia => Alliance::Haranya,
            Alliance::Haranya => Alliance::Nuia,
            _ => Alliance::Unknown,
        }
    }

    /// 0 for Nuia, 1 for Haranya, `None` for the rest (index into per-side arrays).
    pub fn side(self) -> Option<usize> {
        match self {
            Alliance::Nuia => Some(0),
            Alliance::Haranya => Some(1),
            _ => None,
        }
    }

    /// The side at `index` (see [`Alliance::side`]).
    pub fn from_side(index: usize) -> Alliance {
        match index {
            0 => Alliance::Nuia,
            1 => Alliance::Haranya,
            _ => Alliance::Unknown,
        }
    }
}

/// Where on the map a position is. `snake_case` in JSON.
#[derive(
    Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Continent {
    /// Not known (offline, or outside the zone grid).
    #[default]
    Unknown,
    /// The west continent (Nuia's).
    West,
    /// The east continent (Haranya's).
    East,
    /// Auroria.
    Auroria,
    /// The seas.
    Sea,
}

impl Continent {
    /// Every continent, in the order the UI lists them.
    pub const ALL: [Continent; 5] = [
        Continent::West,
        Continent::East,
        Continent::Auroria,
        Continent::Sea,
        Continent::Unknown,
    ];

    /// The `snake_case` name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Continent::Unknown => "unknown",
            Continent::West => "west",
            Continent::East => "east",
            Continent::Auroria => "auroria",
            Continent::Sea => "sea",
        }
    }

    /// The continent `name` stands for (`west`, `east`, `auroria`, `sea`; case-insensitive).
    pub fn parse(name: &str) -> Option<Continent> {
        match name.trim().to_ascii_lowercase().as_str() {
            "west" => Some(Continent::West),
            "east" => Some(Continent::East),
            "auroria" => Some(Continent::Auroria),
            "sea" => Some(Continent::Sea),
            _ => None,
        }
    }
}

/// Which bots a filtered spawn or despawn means. Every field narrows the choice; an empty filter
/// means every bot. JSON: `{"alliance":"nuia"}`, `{"race":"elf"}`, `{"continent":"east"}`.
///
/// For a **spawn** the filter applies to the roster: a bot's race decides its side and its home
/// continent (Nuian and Elf are at home in the west, Hariharan and Ferre in the east). For a
/// **despawn** it applies to the bots that run: `alliance` and `continent` are where the bot is
/// now, `race` is its race.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnFilter {
    /// `nuia` or `haranya`.
    pub alliance: Option<String>,
    /// `nuian`, `elf`, `hariharan` or `ferre`.
    pub race: Option<String>,
    /// `west`, `east`, `auroria` or `sea`.
    pub continent: Option<String>,
}

impl SpawnFilter {
    /// The races a filter may name.
    pub const RACES: [&'static str; 4] = ["nuian", "elf", "hariharan", "ferre"];

    /// Checks the words of the filter.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(a) = &self.alliance {
            Alliance::parse(a).ok_or_else(|| {
                format!("unknown alliance {a:?}; expected \"nuia\" or \"haranya\"")
            })?;
        }
        if let Some(r) = &self.race {
            let r = r.trim().to_ascii_lowercase();
            if !Self::RACES.contains(&r.as_str()) {
                return Err(format!(
                    "unknown race {r:?}; expected one of {}",
                    Self::RACES.join(", ")
                ));
            }
        }
        if let Some(c) = &self.continent {
            Continent::parse(c).ok_or_else(|| {
                format!("unknown continent {c:?}; expected west, east, auroria or sea")
            })?;
        }
        Ok(())
    }

    /// Whether the filter names nothing.
    pub fn is_empty(&self) -> bool {
        self.alliance.is_none() && self.race.is_none() && self.continent.is_none()
    }
}

// ------------------------------------------------------------------------------------ the world

/// Players and bots of one continent on one side.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RegionCell {
    pub continent: Continent,
    pub alliance: Alliance,
    /// Players that a bot saw in the last [`WorldView::ttl_secs`] seconds.
    pub players_seen: u32,
    /// Bots that are in the world.
    pub bots_online: u32,
}

/// Players and bots of one zone.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ZoneRow {
    pub zone_key: u16,
    pub name: String,
    pub continent: Continent,
    pub players_seen: u32,
    pub bots_online: u32,
}

/// One player a bot has in view.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PlayerSeen {
    pub name: String,
    pub level: u8,
    pub alliance: Alliance,
    pub continent: Continent,
    pub zone: String,
    pub x: f32,
    pub y: f32,
    /// Seconds since a bot last saw the player.
    pub age_secs: u32,
    /// The bot that saw the player last.
    pub seen_by: u32,
}

/// A map square (512 m) with players and bots in it; the map draws these as heat.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct HeatCell {
    /// Centre of the square, in world metres.
    pub x: f32,
    pub y: f32,
    pub players: u32,
    pub bots: u32,
}

/// What the game server's own web API says (all fields optional: the API is optional).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ServerView {
    /// `[population] server_api` is set.
    pub configured: bool,
    /// The last poll got an answer.
    pub reachable: bool,
    /// Why the last poll failed (a short text, never a key or an address).
    pub error: Option<String>,
    /// Unix milliseconds of the last successful poll.
    pub checked_ms: u64,
    /// The `/status` page as `(label, value)` rows (uptime, players online, job counts, ...).
    pub status: Vec<(String, String)>,
    /// Characters the server lists as online (needs the API key).
    pub online: Option<u32>,
    /// Of those, our bots.
    pub online_bots: Option<u32>,
    /// Lots on the auction house (needs the API key).
    pub auction_lots: Option<u32>,
    /// Round trip of the last poll, milliseconds.
    pub latency_ms: u32,
}

/// The totals above the table.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct WorldTotals {
    /// Players that a bot has in view now.
    pub players_seen: u32,
    pub bots_online: u32,
    pub bots_total: u32,
    /// Players online according to the server (online characters minus our bots); `None` without
    /// the server API.
    pub players_server: Option<u32>,
    /// Players the server counts that no bot sees (`players_server - players_seen`, never below
    /// 0); `None` without the server API.
    pub players_unlocated: Option<u32>,
    /// Share of the server's players that a bot sees, percent; `None` without the server API.
    pub coverage_pct: Option<u32>,
    /// Area the online bots watch (a bot sees about 128 m around it), square kilometres.
    pub watched_km2: f32,
}

/// `GET /api/world`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct WorldView {
    /// Unix milliseconds.
    pub at_ms: u64,
    /// A player is counted this long after a bot last saw it.
    pub ttl_secs: u32,
    /// Continent by side. Rows with nobody in them are left out.
    pub cells: Vec<RegionCell>,
    /// The busiest zones first.
    pub zones: Vec<ZoneRow>,
    /// The players in view, most recent first.
    pub players: Vec<PlayerSeen>,
    /// Map squares with someone in them.
    pub heat: Vec<HeatCell>,
    pub totals: WorldTotals,
    pub server: ServerView,
}

// ---------------------------------------------------------------------------------- the economy

/// One item in the economy table.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ItemRow {
    pub template: u32,
    pub name: String,
    /// 0 (plenty) to 1 (scarce).
    pub scarcity: f32,
    /// Lots on the auction house: the server's count when the API is on, else what the bots saw
    /// (they look at five at most).
    pub lots: u32,
    /// `server` or `bots`: where `lots` comes from.
    pub supply_source: String,
    /// Market price per item in copper (the median of the cheapest lots); 0 when unknown.
    pub unit_price: u32,
    /// What a vendor asks for one item in copper; 0 when the item is not sold.
    pub vendor_price: u32,
    /// `unit_price / vendor_price`; 0 when either is unknown.
    pub price_ratio: f32,
    /// Price change over the last half hour, percent; 0 without two samples.
    pub trend_pct: f32,
    /// Buy and sell lines in chat in the last ten minutes (a bot's line counts a quarter).
    pub wtb: u32,
    pub wts: u32,
    /// Why the item scores as it does.
    pub reasons: Vec<String>,
    /// The director was told to grow this item.
    pub boosted: bool,
}

/// Where the economy numbers come from.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct EconomySources {
    /// Price notes of the bots that are still fresh.
    pub bot_notes: u32,
    /// Lots the server API listed at the last poll; `None` without it.
    pub server_lots: Option<u32>,
    /// Chat lines about buying and selling heard in the window.
    pub chat_lines: u32,
    /// The window of the chat counts, seconds.
    pub chat_window_secs: u32,
}

/// `GET /api/economy`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct EconomyView {
    pub at_ms: u64,
    /// The most scarce items first.
    pub items: Vec<ItemRow>,
    pub sources: EconomySources,
}

// ---------------------------------------------------------------------------------- the director

/// An item the operator (or the director) wants more of. The farming goal reads these.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct BoostWish {
    pub template: u32,
    /// Unix milliseconds the wish ends; 0 = until it is removed.
    pub until_ms: u64,
    /// The director added it by itself (`auto_boost`).
    pub auto: bool,
}

/// The rules of the director. The operator edits them live; they start from `[population]`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Policy {
    /// The director acts (the kill switch). Off: it does nothing at all.
    pub active: bool,
    /// It only says what it would do.
    pub dry_run: bool,
    /// Bots online, all together, that the director will not exceed when it starts more.
    pub max_online: u32,
    /// Each side keeps at least this many bots.
    pub min_per_side: u32,
    /// A side aims for this many characters per 100 players on the other side (players plus bots).
    pub mirror_pct: u32,
    /// No side gets more bots than this from the director.
    pub per_side_cap: u32,
    /// The most bots one action starts or stops.
    pub step: u32,
    /// Bots above the target that a side keeps before the director stops some.
    pub hysteresis: u32,
    /// The director may stop bots it started.
    pub despawn: bool,
    /// Stop starting bots above this much process memory (megabytes); 0 = ignore.
    pub backoff_rss_mb: u32,
    /// ... above this many packets per second from the server; 0 = ignore.
    pub backoff_packets_in: u32,
    /// ... after this many failed or short sessions in the last five minutes; 0 = ignore.
    pub backoff_login_failures: u32,
    /// ... with this many open server issues; 0 = ignore.
    pub backoff_open_issues: u32,
    /// Make the items that score this scarce a wish by itself (1.0 = never); with `auto_boost`.
    pub auto_boost_min: f32,
    /// At most this many automatic wishes (0 = none).
    pub auto_boost: u32,
    /// The wishes the operator made.
    pub boosts: Vec<BoostWish>,
    /// Multipliers on the goal scores of every bot, by goal name (0.1 to 3). A goal that is not
    /// listed keeps 1.
    pub goal_bias: BTreeMap<String, f32>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            active: false,
            dry_run: true,
            max_online: 50,
            min_per_side: 0,
            mirror_pct: 100,
            per_side_cap: 20,
            step: 3,
            hysteresis: 2,
            despawn: true,
            backoff_rss_mb: 3072,
            backoff_packets_in: 30_000,
            backoff_login_failures: 8,
            backoff_open_issues: 50,
            auto_boost_min: 0.7,
            auto_boost: 0,
            boosts: Vec::new(),
            goal_bias: BTreeMap::new(),
        }
    }
}

impl Policy {
    /// Most wishes the operator can make.
    pub const MAX_BOOSTS: usize = 16;
    /// The range of a goal bias.
    pub const BIAS_RANGE: (f32, f32) = (0.1, 3.0);
    /// Largest number any bot-count field takes.
    pub const MAX_BOTS: u32 = 5000;
}

/// A change to the policy: only the fields that are present change. JSON: `{"dry_run":false}`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyPatch {
    pub active: Option<bool>,
    pub dry_run: Option<bool>,
    pub max_online: Option<u32>,
    pub min_per_side: Option<u32>,
    pub mirror_pct: Option<u32>,
    pub per_side_cap: Option<u32>,
    pub step: Option<u32>,
    pub hysteresis: Option<u32>,
    pub despawn: Option<bool>,
    pub backoff_rss_mb: Option<u32>,
    pub backoff_packets_in: Option<u32>,
    pub backoff_login_failures: Option<u32>,
    pub backoff_open_issues: Option<u32>,
    pub auto_boost_min: Option<f32>,
    pub auto_boost: Option<u32>,
    /// Replaces the wishes. `until_ms` is ignored here; use `boost_hours`.
    pub boosts: Option<Vec<BoostRequest>>,
    /// Replaces the goal biases.
    pub goal_bias: Option<BTreeMap<String, f32>>,
}

/// A wish as the operator writes it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BoostRequest {
    pub template: u32,
    /// Hours the wish lasts (0 = until removed, at most 720).
    pub hours: u32,
}

impl PolicyPatch {
    /// Checks every number against its range; `goals` are the names a bias may use.
    pub fn validate(&self, goals: &[&str]) -> Result<(), String> {
        let count = |name: &str, v: Option<u32>, max: u32| match v {
            Some(v) if v > max => Err(format!("{name} must be at most {max}")),
            _ => Ok(()),
        };
        count("max_online", self.max_online, Policy::MAX_BOTS)?;
        count("min_per_side", self.min_per_side, Policy::MAX_BOTS)?;
        count("per_side_cap", self.per_side_cap, Policy::MAX_BOTS)?;
        count("mirror_pct", self.mirror_pct, 1000)?;
        count("step", self.step, 1000)?;
        count("hysteresis", self.hysteresis, 1000)?;
        count("backoff_rss_mb", self.backoff_rss_mb, 1_000_000)?;
        count("backoff_packets_in", self.backoff_packets_in, 100_000_000)?;
        count(
            "backoff_login_failures",
            self.backoff_login_failures,
            100_000,
        )?;
        count("backoff_open_issues", self.backoff_open_issues, 100_000)?;
        count("auto_boost", self.auto_boost, Policy::MAX_BOOSTS as u32)?;
        if self.step == Some(0) {
            return Err("step must be at least 1".into());
        }
        if let Some(m) = self.auto_boost_min {
            if !m.is_finite() || !(0.0..=1.0).contains(&m) {
                return Err("auto_boost_min must be a number from 0 to 1".into());
            }
        }
        if let Some(b) = &self.boosts {
            if b.len() > Policy::MAX_BOOSTS {
                return Err(format!(
                    "at most {} wishes can be active",
                    Policy::MAX_BOOSTS
                ));
            }
            for w in b {
                if w.template == 0 {
                    return Err("a wish needs an item template".into());
                }
                if w.hours > 720 {
                    return Err("a wish lasts at most 720 hours".into());
                }
            }
        }
        if let Some(map) = &self.goal_bias {
            let (lo, hi) = Policy::BIAS_RANGE;
            for (goal, v) in map {
                if !goals.contains(&goal.as_str()) || goal == "auto" {
                    return Err(format!("unknown goal {goal:?} in goal_bias"));
                }
                if !v.is_finite() || *v < lo || *v > hi {
                    return Err(format!("the bias of {goal} must be between {lo} and {hi}"));
                }
            }
        }
        Ok(())
    }
}

/// What the director wants for one side right now.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SideRow {
    pub alliance: Alliance,
    /// Players seen on this side (by the faction they play).
    pub players: u32,
    /// Bots of this side that are in the world or logging in.
    pub bots: u32,
    /// Of those, the ones the director started.
    pub owned: u32,
    /// How many bots the side should have.
    pub target: u32,
}

/// What the director looks at to back off.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct HealthView {
    pub rss_mb: u32,
    pub packets_in_per_sec: f32,
    pub packets_out_per_sec: f32,
    /// Failed or short sessions in the last five minutes.
    pub login_failures: u32,
    pub open_issues: u32,
    /// Bots logging in right now.
    pub connecting: u32,
    /// Why starting bots is on hold, if it is.
    pub holding: Option<String>,
}

/// One line of the director's log.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ActionRecord {
    pub at_ms: u64,
    /// `spawn`, `despawn`, `hold`, `policy`, `wish`, `result` (what the supervisor did with the
    /// order of the line before).
    pub kind: String,
    pub alliance: Alliance,
    pub count: u32,
    /// The action was carried out (false: a dry run, or it was refused).
    pub applied: bool,
    /// Why, in words.
    pub reason: String,
    /// What came of it (how many bots, what the supervisor said).
    pub result: String,
}

/// `GET /api/director`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DirectorView {
    /// `[population] enabled`: when false the policy cannot be switched on.
    pub available: bool,
    pub policy: Policy,
    /// `off`, `dry_run`, `live`, `holding`.
    pub mode: String,
    pub sides: Vec<SideRow>,
    pub health: HealthView,
    /// Bots the director started that are still running.
    pub owned: u32,
    /// Seconds until the director may act again (0 = now).
    pub next_action_secs: u32,
    /// Bots started in the last minute / the configured limit per minute.
    pub spawned_last_min: u32,
    pub max_spawn_per_min: u32,
    pub min_action_gap_secs: u32,
    /// Items the director put on the wish list by itself.
    pub auto_wishes: Vec<u32>,
    /// Newest first.
    pub log: Vec<ActionRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOALS: [&str; 4] = ["quest", "grind", "farm", "auto"];

    #[test]
    fn names_round_trip() {
        for a in [Alliance::Nuia, Alliance::Haranya] {
            assert_eq!(Alliance::parse(a.as_str()), Some(a));
            assert_eq!(a.opposite().opposite(), a);
            assert_eq!(Alliance::from_side(a.side().unwrap()), a);
        }
        assert_eq!(Alliance::parse(" NUIA "), Some(Alliance::Nuia));
        assert_eq!(Alliance::parse("pirate"), None);
        assert_eq!(Alliance::Other.side(), None);
        for c in Continent::ALL {
            if c != Continent::Unknown {
                assert_eq!(Continent::parse(c.as_str()), Some(c));
            }
        }
        assert_eq!(
            serde_json::to_string(&Continent::Auroria).unwrap(),
            "\"auroria\""
        );
        assert_eq!(
            serde_json::from_str::<Alliance>("\"haranya\"").unwrap(),
            Alliance::Haranya
        );
    }

    #[test]
    fn filters_are_checked_and_unknown_keys_refused() {
        let ok = |s: &str| serde_json::from_str::<SpawnFilter>(s).unwrap().validate();
        assert!(ok("{}").is_ok());
        assert!(ok(r#"{"alliance":"nuia","race":"Elf","continent":"east"}"#).is_ok());
        assert!(ok(r#"{"alliance":"orcs"}"#).is_err());
        assert!(ok(r#"{"race":"dwarf"}"#).is_err());
        assert!(ok(r#"{"continent":"moon"}"#).is_err());
        assert!(serde_json::from_str::<SpawnFilter>(r#"{"alliances":"nuia"}"#).is_err());
        assert!(SpawnFilter::default().is_empty());
    }

    #[test]
    fn a_policy_patch_is_checked_field_by_field() {
        let patch = |s: &str| serde_json::from_str::<PolicyPatch>(s).unwrap();
        assert!(patch(r#"{"dry_run":false,"max_online":80}"#)
            .validate(&GOALS)
            .is_ok());
        assert!(patch(r#"{"max_online":5001}"#).validate(&GOALS).is_err());
        assert!(patch(r#"{"step":0}"#).validate(&GOALS).is_err());
        assert!(patch(r#"{"mirror_pct":1001}"#).validate(&GOALS).is_err());
        assert!(patch(r#"{"auto_boost_min":1.5}"#).validate(&GOALS).is_err());
        assert!(patch(r#"{"goal_bias":{"farm":1.5}}"#)
            .validate(&GOALS)
            .is_ok());
        assert!(patch(r#"{"goal_bias":{"farm":9}}"#)
            .validate(&GOALS)
            .is_err());
        assert!(patch(r#"{"goal_bias":{"fly":1.0}}"#)
            .validate(&GOALS)
            .is_err());
        assert!(patch(r#"{"goal_bias":{"auto":1.0}}"#)
            .validate(&GOALS)
            .is_err());
        assert!(patch(r#"{"boosts":[{"template":3412,"hours":2}]}"#)
            .validate(&GOALS)
            .is_ok());
        assert!(patch(r#"{"boosts":[{"template":0}]}"#)
            .validate(&GOALS)
            .is_err());
        assert!(patch(r#"{"boosts":[{"template":1,"hours":9999}]}"#)
            .validate(&GOALS)
            .is_err());
        let many = format!(
            r#"{{"boosts":[{}]}}"#,
            vec![r#"{"template":1}"#; Policy::MAX_BOOSTS + 1].join(",")
        );
        assert!(patch(&many).validate(&GOALS).is_err());
        assert!(serde_json::from_str::<PolicyPatch>(r#"{"nope":1}"#).is_err());
        assert!(serde_json::from_str::<PolicyPatch>(r#"{"max_online":-1}"#).is_err());
        // an empty patch changes nothing and is fine
        assert_eq!(patch("{}"), PolicyPatch::default());
        assert!(PolicyPatch::default().validate(&GOALS).is_ok());
    }

    #[test]
    fn the_default_policy_is_inert() {
        let p = Policy::default();
        assert!(!p.active && p.dry_run);
        let back: Policy = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
}
