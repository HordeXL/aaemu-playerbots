//! Which bots a filtered spawn or despawn means, and which running bots may be stopped.
//!
//! A [`Selector`] is the checked form of the web filter. It picks roster entries for a spawn (a
//! bot's race decides its side and home continent) and running bots for a despawn (where the bot
//! is now). [`stop_rank`] says how willingly a running bot is stopped: never in the middle of a
//! trade, a duel, a dungeon or a meeting with a buyer, first when it only stands or rests.

use pb_web::{Activity, Alliance, Continent, SpawnFilter};

use super::region::{alliance_of_race, home_continent};
use crate::config::race_id;

/// A checked [`SpawnFilter`]. `Copy`, so it can ride in a [`SupCmd`](crate::hub::SupCmd).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selector {
    /// The side (of the race for a spawn, of the bot's faction for a despawn).
    pub alliance: Option<Alliance>,
    /// A race id.
    pub race: Option<u8>,
    /// The home continent of the race for a spawn, the continent the bot stands on for a despawn.
    pub continent: Option<Continent>,
}

/// What the supervisor may stop when a despawn comes from the director.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StopRule {
    /// Only bots the director started.
    pub owned_only: bool,
    /// Only bots that have been up this long (seconds).
    pub min_age_secs: u32,
}

impl Selector {
    /// Every bot.
    pub const ALL: Selector = Selector {
        alliance: None,
        race: None,
        continent: None,
    };

    /// The selector for the side `alliance`.
    pub fn side(alliance: Alliance) -> Selector {
        Selector {
            alliance: Some(alliance),
            ..Selector::ALL
        }
    }

    /// Reads a web filter. The web layer checks the words already; this refuses what it cannot
    /// turn into ids.
    pub fn from_filter(f: &SpawnFilter) -> Result<Selector, String> {
        let alliance = match &f.alliance {
            Some(a) => Some(Alliance::parse(a).ok_or_else(|| format!("unknown alliance {a:?}"))?),
            None => None,
        };
        let race = match &f.race {
            Some(r) => Some(race_id(r).ok_or_else(|| format!("unknown race {r:?}"))?),
            None => None,
        };
        let continent = match &f.continent {
            Some(c) => Some(Continent::parse(c).ok_or_else(|| format!("unknown continent {c:?}"))?),
            None => None,
        };
        Ok(Selector {
            alliance,
            race,
            continent,
        })
    }

    /// Whether the selector names nothing.
    pub fn is_all(&self) -> bool {
        *self == Selector::ALL
    }

    /// Whether a roster bot of race `race` is meant by a spawn.
    pub fn matches_race(&self, race: u8) -> bool {
        let side = alliance_of_race(race);
        self.race.is_none_or(|r| r == race)
            && self.alliance.is_none_or(|a| a == side)
            && self.continent.is_none_or(|c| c == home_continent(side))
    }

    /// Whether a running bot is meant by a despawn: its race, the side it plays now and the
    /// continent it stands on now. A bot whose continent is not known yet (still logging in)
    /// counts as being at home.
    pub fn matches_live(&self, race: u8, alliance: Alliance, continent: Continent) -> bool {
        let side = if alliance == Alliance::Unknown {
            alliance_of_race(race)
        } else {
            alliance
        };
        let on = if continent == Continent::Unknown {
            home_continent(alliance_of_race(race))
        } else {
            continent
        };
        self.race.is_none_or(|r| r == race)
            && self.alliance.is_none_or(|a| a == side)
            && self.continent.is_none_or(|c| c == on)
    }

    /// The races a new roster entry may get so that a spawn with this selector finds it: the
    /// configured races that match. Empty when none does.
    pub fn races(&self, configured: &[u8]) -> Vec<u8> {
        configured
            .iter()
            .copied()
            .filter(|r| self.matches_race(*r))
            .collect()
    }
}

/// How willingly a running bot is stopped; lower is sooner, `None` means not now.
///
/// Never while the bot is in a window or a meeting with another character: trading, dueling,
/// fighting, dead or connecting; the goals `trade`, `duel`, `dungeon`, `market`, `auction`,
/// `event`, `companion`, `warzone`, `gank`, `guild` and `sail` (a party waits for it, a deal is
/// open, a boat floats). Bots that stand, rest or wander go first, then the ones that walk to a
/// quest, grind or farm, last the ones that carry a trade pack.
pub fn stop_rank(activity: Activity, goal: &str) -> Option<u8> {
    if matches!(
        activity,
        Activity::Offline
            | Activity::Connecting
            | Activity::Dead
            | Activity::Fighting
            | Activity::Dueling
            | Activity::Trading
    ) {
        return None;
    }
    match goal {
        "trade" | "duel" | "dungeon" | "market" | "auction" | "event" | "companion" | "warzone"
        | "gank" | "guild" | "sail" => None,
        "rest" | "wander" | "" | "auto" => Some(0),
        "quest" | "grind" | "farm" | "housing" => Some(1),
        "trade_run" => Some(2),
        _ => Some(1),
    }
}

/// A running bot, as a despawn sees it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub id: u32,
    pub race: u8,
    pub alliance: Alliance,
    pub continent: Continent,
    pub activity: Activity,
    /// The bot's goal name.
    pub goal: &'a str,
    /// Seconds in the world this session.
    pub online_secs: u32,
    /// The population director started the bot.
    pub owned: bool,
}

/// The ids to stop for a despawn of `n` bots of `sel` under `rule`: only bots that are safe to
/// stop ([`stop_rank`]), the most harmless first, and among equals the ones that have been up
/// longest (then the lowest id).
pub fn pick_stops(
    candidates: &[Candidate<'_>],
    sel: &Selector,
    rule: StopRule,
    n: usize,
) -> Vec<u32> {
    let mut ranked: Vec<(u8, std::cmp::Reverse<u32>, u32)> = candidates
        .iter()
        .filter(|c| sel.matches_live(c.race, c.alliance, c.continent))
        .filter(|c| !rule.owned_only || c.owned)
        .filter(|c| c.online_secs >= rule.min_age_secs)
        .filter_map(|c| {
            stop_rank(c.activity, c.goal).map(|r| (r, std::cmp::Reverse(c.online_secs), c.id))
        })
        .collect();
    ranked.sort_unstable();
    ranked.into_iter().take(n).map(|(_, _, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(
        id: u32,
        race: u8,
        activity: Activity,
        goal: &'static str,
        secs: u32,
    ) -> Candidate<'static> {
        Candidate {
            id,
            race,
            alliance: alliance_of_race(race),
            continent: home_continent(alliance_of_race(race)),
            activity,
            goal,
            online_secs: secs,
            owned: id.is_multiple_of(2),
        }
    }

    #[test]
    fn a_despawn_picks_the_most_harmless_bots_of_the_wanted_kind() {
        let all = [
            cand(1, 1, Activity::Questing, "quest", 500),
            cand(2, 1, Activity::Resting, "rest", 100),
            cand(3, 4, Activity::Idle, "wander", 900),
            cand(4, 1, Activity::Trading, "trade", 900),
            cand(5, 5, Activity::Idle, "wander", 900),
            cand(6, 1, Activity::Idle, "dungeon", 900),
            cand(7, 1, Activity::Traveling, "trade_run", 900),
            cand(8, 4, Activity::Idle, "rest", 100),
        ];
        let nuia = Selector::side(Alliance::Nuia);
        let any = StopRule::default();
        // idle ones first (longest up first), then the quester, then the pack carrier; the trader
        // and the dungeon bot never; the Haranya bot is not asked for
        assert_eq!(pick_stops(&all, &nuia, any, 10), vec![3, 2, 8, 1, 7]);
        assert_eq!(pick_stops(&all, &nuia, any, 2), vec![3, 2]);
        assert!(pick_stops(&all, &nuia, any, 0).is_empty());
        // only what the director started (the even ids), and up for a while
        let owned = StopRule {
            owned_only: true,
            min_age_secs: 300,
        };
        assert!(pick_stops(&all, &nuia, owned, 10).is_empty());
        let owned = StopRule {
            owned_only: true,
            min_age_secs: 100,
        };
        assert_eq!(pick_stops(&all, &nuia, owned, 10), vec![2, 8]);
        // by race
        let elf = Selector {
            race: Some(4),
            ..Selector::ALL
        };
        assert_eq!(pick_stops(&all, &elf, any, 10), vec![3, 8]);
        // nobody matches
        let ferre = Selector {
            race: Some(6),
            ..Selector::ALL
        };
        assert!(pick_stops(&all, &ferre, any, 10).is_empty());
    }

    fn filter(a: Option<&str>, r: Option<&str>, c: Option<&str>) -> SpawnFilter {
        SpawnFilter {
            alliance: a.map(str::to_owned),
            race: r.map(str::to_owned),
            continent: c.map(str::to_owned),
        }
    }

    #[test]
    fn filters_become_selectors() {
        let s = Selector::from_filter(&filter(Some("Nuia"), None, None)).unwrap();
        assert_eq!(s.alliance, Some(Alliance::Nuia));
        let s = Selector::from_filter(&filter(None, Some("Ferre"), Some("east"))).unwrap();
        assert_eq!((s.race, s.continent), (Some(6), Some(Continent::East)));
        assert!(Selector::from_filter(&SpawnFilter::default())
            .unwrap()
            .is_all());
        assert!(Selector::from_filter(&filter(Some("orcs"), None, None)).is_err());
        assert!(Selector::from_filter(&filter(None, Some("gnome"), None)).is_err());
        assert!(Selector::from_filter(&filter(None, None, Some("moon"))).is_err());
    }

    #[test]
    fn a_spawn_matches_the_race_its_side_and_its_home() {
        let nuia = Selector::side(Alliance::Nuia);
        assert!(nuia.matches_race(1) && nuia.matches_race(4));
        assert!(!nuia.matches_race(5) && !nuia.matches_race(6));
        let east = Selector {
            continent: Some(Continent::East),
            ..Selector::ALL
        };
        assert!(east.matches_race(5) && east.matches_race(6) && !east.matches_race(1));
        let elf = Selector {
            race: Some(4),
            ..Selector::ALL
        };
        assert!(elf.matches_race(4) && !elf.matches_race(1));
        // a side and a continent that disagree match nobody
        let odd = Selector {
            alliance: Some(Alliance::Nuia),
            continent: Some(Continent::East),
            ..Selector::ALL
        };
        assert!((1..=8).all(|r| !odd.matches_race(r)));
        assert!((1..=8).all(|r| Selector::ALL.matches_race(r)));
    }

    #[test]
    fn the_races_of_a_new_bot_are_the_configured_ones_that_match() {
        let configured = [1, 4, 5, 6];
        assert_eq!(Selector::side(Alliance::Nuia).races(&configured), [1, 4]);
        assert_eq!(Selector::side(Alliance::Haranya).races(&configured), [5, 6]);
        assert_eq!(Selector::ALL.races(&configured), configured);
        assert!(Selector::side(Alliance::Haranya).races(&[1, 4]).is_empty());
    }

    #[test]
    fn a_despawn_goes_by_where_the_bot_is_now() {
        let west_now = Selector {
            continent: Some(Continent::West),
            ..Selector::ALL
        };
        // a Ferre who travelled to the west continent is a west bot now
        assert!(west_now.matches_live(6, Alliance::Haranya, Continent::West));
        assert!(!west_now.matches_live(6, Alliance::Haranya, Continent::East));
        // a bot that is still logging in counts as being at home
        assert!(west_now.matches_live(1, Alliance::Unknown, Continent::Unknown));
        assert!(!west_now.matches_live(5, Alliance::Unknown, Continent::Unknown));
        let haranya = Selector::side(Alliance::Haranya);
        assert!(haranya.matches_live(5, Alliance::Haranya, Continent::East));
        assert!(!haranya.matches_live(1, Alliance::Nuia, Continent::West));
        assert!(haranya.matches_live(5, Alliance::Unknown, Continent::Unknown));
    }

    #[test]
    fn busy_bots_are_never_stopped_and_idle_ones_go_first() {
        for goal in [
            "trade",
            "duel",
            "dungeon",
            "market",
            "auction",
            "event",
            "companion",
            "warzone",
            "gank",
            "guild",
            "sail",
        ] {
            assert_eq!(stop_rank(Activity::Idle, goal), None, "{goal}");
        }
        for a in [
            Activity::Offline,
            Activity::Connecting,
            Activity::Dead,
            Activity::Fighting,
            Activity::Dueling,
            Activity::Trading,
        ] {
            assert_eq!(stop_rank(a, "rest"), None, "{a:?}");
        }
        let idle = stop_rank(Activity::Resting, "rest").unwrap();
        let questing = stop_rank(Activity::Questing, "quest").unwrap();
        let carrying = stop_rank(Activity::TradeRun, "trade_run").unwrap();
        assert!(idle < questing && questing < carrying);
        assert_eq!(stop_rank(Activity::Idle, "wander"), Some(idle));
        assert_eq!(stop_rank(Activity::Idle, ""), Some(idle));
        assert_eq!(stop_rank(Activity::Traveling, "quest"), Some(questing));
    }
}
