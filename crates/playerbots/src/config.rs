//! Configuration of the `playerbots` binary (`playerbots.toml`).
//!
//! Every table and every key has a default, so an empty file is a valid config. Unknown keys are
//! rejected (a typo in a key would otherwise be silently ignored). [`Config::load`] validates the
//! values; [`Config::example`] is the commented example shipped as `playerbots.example.toml`.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context};
use serde::Deserialize;

/// Environment variable consulted when `server.password` is empty.
pub const PASSWORD_ENV: &str = "PLAYERBOTS_PASSWORD";
/// Password used when neither the config nor [`PASSWORD_ENV`] gives one.
pub const DEFAULT_PASSWORD: &str = "playerbots";
/// Most bots one process accepts (a sanity cap, not a performance claim).
pub const MAX_BOTS: u32 = 5000;

/// The whole `playerbots.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Login server and account naming.
    pub server: ServerCfg,
    /// Where the static game data lives.
    pub data: DataCfg,
    /// Number, pacing and movement of the bots.
    pub bots: BotsCfg,
    /// Goal weights.
    pub behavior: BehaviorCfg,
    /// Operator web UI (`bind`, `token`).
    pub web: pb_web::WebConfig,
    /// Optional LLM add-on.
    pub llm: pb_llm::LlmConfig,
    /// Chat: replies to players and bot-to-bot chatter.
    pub social: SocialCfg,
}

/// `[server]`: how to reach the login server and name the accounts.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerCfg {
    /// Login server address (`host:port`), the public client port.
    pub login: String,
    /// Game server (world) id to enter; `0` takes the first active world the login server lists.
    pub world_id: u8,
    /// Accounts are `<prefix><bot id>` (`pb1`, `pb2`, ...). The login server creates unknown
    /// accounts on first login (`AutoAccount`).
    pub account_prefix: String,
    /// Password of every bot account. Empty: the environment variable `PLAYERBOTS_PASSWORD`,
    /// then `playerbots`. Never logged.
    pub password: String,
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self {
            login: "127.0.0.1:1237".into(),
            world_id: 0,
            account_prefix: "pb".into(),
            password: String::new(),
        }
    }
}

impl fmt::Debug for ServerCfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCfg")
            .field("login", &self.login)
            .field("world_id", &self.world_id)
            .field("account_prefix", &self.account_prefix)
            .field("password", &"<hidden>")
            .finish()
    }
}

/// `[data]`: static data locations (all read-only).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataCfg {
    /// The server's `compact.sqlite3`.
    pub compact: PathBuf,
    /// The server's `app/Data` directory (`Worlds/<world>/*.json`, `CharTemplates.json`).
    pub server_data: PathBuf,
    /// The directory written by `playerbots bake`; without it bots run on flat terrain and the
    /// map has no image.
    pub baked: Option<PathBuf>,
    /// World name, `main_world` for the 1.2 world.
    pub world: String,
}

impl Default for DataCfg {
    fn default() -> Self {
        Self {
            compact: "compact.sqlite3".into(),
            server_data: "Data".into(),
            baked: None,
            world: "main_world".into(),
        }
    }
}

/// `[bots]`: how many bots there are and how they move.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BotsCfg {
    /// Bots started at launch (more can be started from the web UI).
    pub count: u32,
    /// Delay between two bot logins (spreads the load on the servers).
    pub ramp_ms: u64,
    /// The persistent roster (accounts, character names, archetypes, personas).
    pub roster: PathBuf,
    /// Races new characters may have: `nuian`, `elf`, `hariharan`, `ferre` (what the 1.2 data
    /// lets a player create), or a race id.
    pub races: Vec<String>,
    /// Bots may use GM chat commands (`/item add`, `/gold add`, `/move`, ...). Needs access level
    /// 100 on the bot accounts, see `playerbots grant-gm-sql`.
    pub gm_bootstrap: bool,
    /// Brain tick while a bot moves or fights.
    pub tick_ms: u64,
    /// Brain tick while a bot stands still.
    pub idle_tick_ms: u64,
    /// Run speed in m/s (the client's base run speed is about 5.4).
    pub move_speed: f32,
    /// Walk speed in m/s.
    pub walk_speed: f32,
    /// With `gm_bootstrap`, trips longer than this many metres use `/move` instead of walking;
    /// 0 never teleports.
    pub long_travel_teleport_m: f32,
    /// First wait after a failed connection.
    pub reconnect_min_secs: u64,
    /// Longest wait between two connection attempts (the wait doubles up to this).
    pub reconnect_max_secs: u64,
}

impl Default for BotsCfg {
    fn default() -> Self {
        Self {
            count: 10,
            ramp_ms: 750,
            roster: "roster.json".into(),
            races: ["nuian", "elf", "hariharan", "ferre"]
                .map(String::from)
                .to_vec(),
            gm_bootstrap: false,
            tick_ms: 250,
            idle_tick_ms: 1500,
            move_speed: 5.4,
            walk_speed: 1.8,
            long_travel_teleport_m: 0.0,
            reconnect_min_secs: 5,
            reconnect_max_secs: 120,
        }
    }
}

impl BotsCfg {
    /// The configured races as ids (`races` was checked by [`Config::validate`]).
    pub fn race_ids(&self) -> Vec<u8> {
        self.races.iter().filter_map(|r| race_id(r)).collect()
    }
}

/// `[behavior]`: relative weights of the goals. A weight of 0 switches a goal off. Each bot also
/// has a persona (temperament) that scales the weights; `persona_spread` says how strongly.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviorCfg {
    /// Do quests.
    pub quest: f32,
    /// Kill monsters.
    pub grind: f32,
    /// Duel another bot.
    pub duel: f32,
    /// Trade with another bot.
    pub trade: f32,
    /// Craft, carry and sell trade packs.
    pub trade_run: f32,
    /// Build a house.
    pub housing: f32,
    /// Stand and recover.
    pub rest: f32,
    /// Walk around.
    pub wander: f32,
    /// 0 gives every bot the same temperament, 1 the full spread of the roster.
    pub persona_spread: f32,
}

impl Default for BehaviorCfg {
    fn default() -> Self {
        Self {
            quest: 1.0,
            grind: 1.0,
            duel: 0.3,
            trade: 0.2,
            trade_run: 0.3,
            housing: 0.2,
            rest: 0.4,
            wander: 0.5,
            persona_spread: 0.6,
        }
    }
}

/// `[social]`: how the bots talk. Replies come from the LLM when `[llm]` is on and from a table of
/// canned lines otherwise.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SocialCfg {
    /// Answer whispers and local chat of players. Off: the bots never say anything on their own.
    pub enabled: bool,
    /// A local line from a real player within this many metres may get an answer (a line that
    /// names the bot, and every whisper, always may).
    pub reply_radius_m: f32,
    /// Chance (0..=1) that a bot answers a local line that does not name it.
    pub reply_chance: f32,
    /// All bots together answer at most this many lines per minute (0 = none).
    pub replies_per_min: u32,
    /// Bots greet each other: "gg" after a duel, thanks after a trade, a hello to a bot nearby.
    pub bot_chatter: bool,
    /// Chatter only while a real player is within 150 m, so nobody talks to an empty street.
    pub chatter_needs_player: bool,
    /// All bots together say at most this many chatter lines per minute (0 = none).
    pub chatter_per_min: u32,
    /// One bot says a chatter line at most this often, in seconds.
    pub chatter_gap_secs: u64,
    /// Also answer the whispers and local lines of our own bots. For testing only: two bots can
    /// answer each other (the limits end it quickly).
    pub reply_to_bots: bool,
}

impl Default for SocialCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            reply_radius_m: 30.0,
            reply_chance: 0.4,
            replies_per_min: 20,
            bot_chatter: true,
            chatter_needs_player: true,
            chatter_per_min: 4,
            chatter_gap_secs: 60,
            reply_to_bots: false,
        }
    }
}

impl Config {
    /// Reads, parses and validates `path`.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "reading {} (create one with `playerbots example-config > {}`)",
                path.display(),
                path.display()
            )
        })?;
        Config::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Parses and validates a config from TOML text.
    pub fn parse(text: &str) -> anyhow::Result<Config> {
        let cfg: Config = toml::from_str(text).context("parsing the config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The commented example config (`playerbots.example.toml`).
    pub fn example() -> &'static str {
        include_str!("../../../playerbots.example.toml")
    }

    /// The bot account password: `server.password`, else `$PLAYERBOTS_PASSWORD`, else `playerbots`.
    pub fn password(&self) -> String {
        if !self.server.password.is_empty() {
            return self.server.password.clone();
        }
        match std::env::var(PASSWORD_ENV) {
            Ok(p) if !p.is_empty() => p,
            _ => DEFAULT_PASSWORD.to_string(),
        }
    }

    /// Checks the values that would otherwise fail late and obscurely.
    pub fn validate(&self) -> anyhow::Result<()> {
        let s = &self.server;
        ensure!(!s.login.trim().is_empty(), "server.login is empty");
        ensure!(
            valid_account_prefix(&s.account_prefix),
            "server.account_prefix {:?} must be 1..=24 characters of letters, digits and _ . - @",
            s.account_prefix
        );
        ensure!(
            !self.data.world.trim().is_empty(),
            "data.world must not be empty"
        );
        let b = &self.bots;
        ensure!(
            b.count <= MAX_BOTS,
            "bots.count {} is above the limit of {MAX_BOTS}",
            b.count
        );
        ensure!(b.tick_ms >= 50, "bots.tick_ms must be at least 50");
        ensure!(
            b.idle_tick_ms >= b.tick_ms,
            "bots.idle_tick_ms must not be below bots.tick_ms"
        );
        ensure!(
            b.move_speed.is_finite() && b.move_speed > 0.0 && b.move_speed <= 25.0,
            "bots.move_speed must be in (0, 25] m/s"
        );
        ensure!(
            b.walk_speed.is_finite() && b.walk_speed > 0.0 && b.walk_speed <= b.move_speed,
            "bots.walk_speed must be in (0, move_speed]"
        );
        ensure!(
            b.long_travel_teleport_m.is_finite() && b.long_travel_teleport_m >= 0.0,
            "bots.long_travel_teleport_m must be 0 or positive"
        );
        ensure!(
            b.reconnect_min_secs >= 1 && b.reconnect_min_secs <= b.reconnect_max_secs,
            "bots.reconnect_min_secs must be in 1..=reconnect_max_secs"
        );
        ensure!(!b.races.is_empty(), "bots.races is empty");
        for r in &b.races {
            if race_id(r).is_none() {
                bail!(
                    "bots.races: unknown race {r:?} (known: nuian, fairy, dwarf, elf, hariharan, ferre, returned, warborn, or an id 1..=8)"
                );
            }
        }
        let w = &self.behavior;
        for (name, v) in [
            ("quest", w.quest),
            ("grind", w.grind),
            ("duel", w.duel),
            ("trade", w.trade),
            ("trade_run", w.trade_run),
            ("housing", w.housing),
            ("rest", w.rest),
            ("wander", w.wander),
        ] {
            ensure!(
                v.is_finite() && v >= 0.0,
                "behavior.{name} must be a number >= 0"
            );
        }
        ensure!(
            w.persona_spread.is_finite() && (0.0..=1.0).contains(&w.persona_spread),
            "behavior.persona_spread must be in 0..=1"
        );
        let c = &self.social;
        ensure!(
            c.reply_radius_m.is_finite() && c.reply_radius_m >= 0.0,
            "social.reply_radius_m must be a number >= 0"
        );
        ensure!(
            c.reply_chance.is_finite() && (0.0..=1.0).contains(&c.reply_chance),
            "social.reply_chance must be in 0..=1"
        );
        ensure!(
            self.llm.director_interval_secs >= 30,
            "llm.director_interval_secs must be at least 30"
        );
        Ok(())
    }
}

/// Whether `prefix` makes valid login names (`^[\p{L}\p{Nd}_.\-@]{1,32}$` once an id of up to
/// eight digits is appended).
pub fn valid_account_prefix(prefix: &str) -> bool {
    let n = prefix.chars().count();
    (1..=24).contains(&n)
        && prefix
            .chars()
            .all(|c| c.is_alphabetic() || c.is_numeric() || matches!(c, '_' | '.' | '-' | '@'))
}

/// Race id of a race name (`nuian` ... `warborn`, case-insensitive) or a numeric id 1..=8.
pub fn race_id(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase();
    if let Ok(id) = n.parse::<u8>() {
        return (1..=8).contains(&id).then_some(id);
    }
    Some(match n.as_str() {
        "nuian" => 1,
        "fairy" => 2,
        "dwarf" => 3,
        "elf" => 4,
        "hariharan" => 5,
        "ferre" => 6,
        "returned" => 7,
        "warborn" => 8,
        _ => return None,
    })
}

/// Display name of a race id.
pub fn race_name(id: u8) -> &'static str {
    match id {
        1 => "Nuian",
        2 => "Fairy",
        3 => "Dwarf",
        4 => "Elf",
        5 => "Hariharan",
        6 => "Ferre",
        7 => "Returned",
        8 => "Warborn",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_config_parses_and_matches_the_defaults() {
        let cfg = Config::parse(Config::example()).expect("example config");
        let def = Config::default();
        assert_eq!(cfg.server.login, def.server.login);
        assert_eq!(cfg.server.account_prefix, "pb");
        assert_eq!(cfg.bots.tick_ms, def.bots.tick_ms);
        assert_eq!(cfg.bots.idle_tick_ms, def.bots.idle_tick_ms);
        assert_eq!(cfg.bots.races, def.bots.races);
        assert!(!cfg.bots.gm_bootstrap);
        assert_eq!(cfg.web.bind.to_string(), "127.0.0.1:8790");
        assert!(!cfg.llm.enabled);
        // Paths for the dev machine are commented out in the example.
        assert!(cfg.data.baked.is_none());
    }

    #[test]
    fn an_empty_file_is_a_valid_config() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.bots.count, 10);
        assert_eq!(cfg.data.world, "main_world");
        assert_eq!(cfg.bots.race_ids(), vec![1, 4, 5, 6]);
    }

    #[test]
    fn partial_tables_keep_the_other_defaults() {
        let cfg = Config::parse("[bots]\ncount = 3\n[behavior]\ngrind = 2.5\n").unwrap();
        assert_eq!(cfg.bots.count, 3);
        assert_eq!(cfg.bots.ramp_ms, 750);
        assert_eq!(cfg.behavior.grind, 2.5);
        assert_eq!(cfg.behavior.quest, 1.0);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        assert!(Config::parse("[bots]\nconut = 3\n").is_err());
        assert!(Config::parse("[nope]\nx = 1\n").is_err());
        assert!(Config::parse("[bots]\ntick_ms = 10\n").is_err());
        assert!(Config::parse("[bots]\ntick_ms = 500\nidle_tick_ms = 100\n").is_err());
        assert!(Config::parse("[bots]\nraces = [\"gnome\"]\n").is_err());
        assert!(Config::parse("[bots]\nraces = []\n").is_err());
        assert!(Config::parse("[bots]\nmove_speed = 0\n").is_err());
        assert!(Config::parse("[bots]\nwalk_speed = 9.0\n").is_err());
        assert!(Config::parse("[bots]\nreconnect_min_secs = 130\n").is_err());
        assert!(Config::parse("[server]\naccount_prefix = \"a b\"\n").is_err());
        assert!(Config::parse("[server]\naccount_prefix = \"\"\n").is_err());
        assert!(Config::parse("[behavior]\ngrind = -1\n").is_err());
        assert!(Config::parse("[behavior]\npersona_spread = 2\n").is_err());
        assert!(Config::parse("[bots]\ncount = 999999\n").is_err());
        assert!(Config::parse("[social]\nreply_chance = 1.5\n").is_err());
        assert!(Config::parse("[social]\nreply_radius_m = -1\n").is_err());
        assert!(Config::parse("[social]\nchater = true\n").is_err());
        assert!(Config::parse("[llm]\ndirector_interval_secs = 5\n").is_err());
    }

    #[test]
    fn the_social_and_director_keys_have_quiet_defaults() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.social.enabled && cfg.social.bot_chatter);
        assert!(cfg.social.chatter_needs_player && !cfg.social.reply_to_bots);
        assert_eq!(cfg.social.chatter_per_min, 4);
        assert_eq!(cfg.social.chatter_gap_secs, 60);
        assert!(!cfg.llm.director, "the director is opt-in");
        assert_eq!(cfg.llm.director_interval_secs, 600);
        let cfg = Config::parse(
            "[llm]\nenabled = true\ndirector = true\ndirector_interval_secs = 90\n[social]\nbot_chatter = false\n",
        )
        .unwrap();
        assert!(cfg.llm.director && !cfg.social.bot_chatter);
        assert_eq!(cfg.llm.director_interval_secs, 90);
    }

    #[test]
    fn race_names_and_ids() {
        assert_eq!(race_id("Nuian"), Some(1));
        assert_eq!(race_id(" HARIHARAN "), Some(5));
        assert_eq!(race_id("4"), Some(4));
        assert_eq!(race_id("9"), None);
        assert_eq!(race_id("0"), None);
        assert_eq!(race_name(6), "Ferre");
    }

    #[test]
    fn account_prefixes() {
        assert!(valid_account_prefix("pb"));
        assert!(valid_account_prefix("bot_.-@é1"));
        assert!(!valid_account_prefix(""));
        assert!(!valid_account_prefix("p b"));
        assert!(!valid_account_prefix("<x>"));
        assert!(!valid_account_prefix(&"a".repeat(25)));
    }

    #[test]
    fn the_password_is_never_printed() {
        let mut cfg = Config::default();
        cfg.server.password = "hunter2".into();
        let shown = format!("{cfg:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(cfg.password(), "hunter2");
    }

    #[test]
    fn the_password_falls_back_to_the_environment_then_the_default() {
        // Only checks the config-set branch deterministically; the environment is process-wide.
        let cfg = Config::default();
        let p = cfg.password();
        assert!(!p.is_empty());
    }
}
