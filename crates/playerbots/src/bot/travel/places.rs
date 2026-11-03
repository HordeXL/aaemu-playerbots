//! Where a bot records a Teleport Book entry, and what it calls it.
//!
//! A spot is worth recording when the bot is somewhere it will want to come back to: a town (a
//! recall point is within 120 m), a cluster of quest givers, a buyer of trade packs, a dungeon
//! entrance or its own house. The questions are cheap spawn-index lookups (a few hundred metres at
//! most), asked at most every few seconds.

use aa_world::{Vec3, World};

use super::book::MAX_NAME_CHARS;

/// A recall point this close makes the spot a town.
pub const TOWN_M: f32 = 120.0;
/// Quest givers are counted within this radius.
pub const GIVERS_M: f32 = 60.0;
/// This many quest givers make a cluster.
pub const GIVERS_MIN: usize = 2;
/// A buyer of trade packs this close makes the spot worth it.
pub const BUYER_M: f32 = 40.0;
/// A dungeon entrance this close does.
pub const DUNGEON_M: f32 = 40.0;
/// The bot's own house this close does.
pub const HOME_M: f32 = 60.0;
/// A recall point this close lends its name to the entry.
pub const NAME_M: f32 = 400.0;

/// Why a spot is worth a book entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// A town or village (a recall point is near).
    Town,
    /// Several quest givers.
    Givers,
    /// A buyer of trade packs.
    Buyer,
    /// A dungeon entrance.
    Dungeon,
    /// The bot's own house.
    Home,
}

impl Spot {
    /// A word for logs.
    pub fn word(self) -> &'static str {
        match self {
            Spot::Town => "town",
            Spot::Givers => "quest givers",
            Spot::Buyer => "pack buyer",
            Spot::Dungeon => "dungeon entrance",
            Spot::Home => "home",
        }
    }
}

/// Whether `pos` is worth a book entry, and why. `homes` are the positions of the bot's houses.
pub fn worth(world: &World, pos: [f32; 3], homes: &[[f32; 3]]) -> Option<Spot> {
    let here = Vec3::new(pos[0], pos[1], pos[2]);
    if homes
        .iter()
        .any(|h| (h[0] - pos[0]).hypot(h[1] - pos[1]) <= HOME_M)
    {
        return Some(Spot::Home);
    }
    if world
        .travel()
        .portals
        .recalls_within(here, TOWN_M)
        .next()
        .is_some()
    {
        return Some(Spot::Town);
    }
    let (mut givers, mut buyer) = (0usize, false);
    world.npc_spawns_within(here, GIVERS_M.max(BUYER_M), |t, p| {
        let Some(n) = world.npc(t) else { return };
        if n.is_quest_giver && p.dist2d(here) <= GIVERS_M {
            givers += 1;
        }
        if n.is_specialty() && p.dist2d(here) <= BUYER_M {
            buyer = true;
        }
    });
    if buyer {
        return Some(Spot::Buyer);
    }
    if givers >= GIVERS_MIN {
        return Some(Spot::Givers);
    }
    let entrance = world.dungeons().iter().any(|d| {
        d.entrances
            .iter()
            .flat_map(|e| e.spawns.iter())
            .any(|p| p.dist2d(here) <= DUNGEON_M)
    });
    entrance.then_some(Spot::Dungeon)
}

/// Two entries closer than this are one place: no new record near an existing one.
pub const ENTRY_NEAR_M: f32 = 600.0;
/// The most entries the bot keeps in its book.
pub const MAX_ENTRIES: usize = super::book::MAX_PRIVATE;

/// What the recording duty knows when it decides.
#[derive(Debug, Clone, Copy)]
pub struct RecordIn {
    /// Bot clock.
    pub now_ms: u64,
    /// Not before this bot clock.
    pub next_record_ms: u64,
    /// The place was last looked at before this bot clock.
    pub next_probe_ms: u64,
    /// A journey or another portal job is under way.
    pub busy: bool,
    /// The bot is calm (no fight, standing, not in an instance, not mid-trade, ...).
    pub calm: bool,
    /// Distance to the nearest book entry whose position is known.
    pub nearest_entry_m: Option<f32>,
    /// Memory Ink in the bag.
    pub ink: i64,
    /// GM commands may top up the ink.
    pub gm: bool,
    /// Entries in the book.
    pub entries: usize,
}

/// What the recording duty does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not now (too early, busy, not calm, nothing worth recording here, an entry is near).
    Wait,
    /// Add Memory Ink with a GM command, then try again.
    TopUpInk,
    /// There is no ink and none can be had: not for a long while.
    NoInk,
    /// The book is full: delete the least useful entry first.
    MakeRoom,
    /// Record the spot.
    Record,
}

/// The recording rules: at most one record per ~10 minutes, only when calm, only at a spot that is
/// worth it and not within [`ENTRY_NEAR_M`] of an entry, with ink (a GM top-up when allowed), and
/// never more than [`MAX_ENTRIES`] entries in the book. `spot` is asked last (it is the costly
/// question).
pub fn decide(i: &RecordIn, spot: impl FnOnce() -> Option<Spot>) -> Decision {
    if i.now_ms < i.next_record_ms || i.now_ms < i.next_probe_ms || i.busy || !i.calm {
        return Decision::Wait;
    }
    if spot().is_none() {
        return Decision::Wait;
    }
    if i.nearest_entry_m.is_some_and(|d| d < ENTRY_NEAR_M) {
        return Decision::Wait;
    }
    if i.ink < 1 {
        return if i.gm {
            Decision::TopUpInk
        } else {
            Decision::NoInk
        };
    }
    if i.entries >= MAX_ENTRIES {
        return Decision::MakeRoom;
    }
    Decision::Record
}

/// The name for a new entry at `pos`: the nearest recall point's name (a town), else the zone's,
/// made unique among `taken`, at most [`MAX_NAME_CHARS`] characters.
pub fn name_for<'a>(
    world: &World,
    pos: [f32; 3],
    spot: Spot,
    taken: impl Iterator<Item = &'a str> + Clone,
) -> String {
    let here = Vec3::new(pos[0], pos[1], pos[2]);
    let town = world
        .nearest_recall(here)
        .filter(|r| r.pos.dist2d(here) <= NAME_M)
        .map(|r| r.name.to_string());
    let zone = || {
        world
            .zone_key_at(pos[0], pos[1])
            .and_then(|k| world.zone_name(k))
            .map(str::to_owned)
    };
    let base = match spot {
        Spot::Home => "Home".to_string(),
        _ => town.or_else(zone).unwrap_or_else(|| "Spot".to_string()),
    };
    unique_name(&base, taken)
}

/// `base` cut to [`MAX_NAME_CHARS`], with a number appended when `taken` has it already.
pub fn unique_name<'a>(base: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let clean = clean.trim().to_string();
    let clean = if clean.is_empty() {
        "Spot".to_string()
    } else {
        clean
    };
    if !taken.clone().any(|t| t == clean) {
        return clean;
    }
    for n in 2..100 {
        let suffix = format!(" {n}");
        let keep = MAX_NAME_CHARS - suffix.chars().count();
        let cut: String = clean.chars().take(keep).collect();
        let name = format!("{}{suffix}", cut.trim_end());
        if !taken.clone().any(|t| t == name) {
            return name;
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> RecordIn {
        RecordIn {
            now_ms: 1_000_000,
            next_record_ms: 0,
            next_probe_ms: 0,
            busy: false,
            calm: true,
            nearest_entry_m: None,
            ink: 2,
            gm: false,
            entries: 3,
        }
    }

    const TOWN: fn() -> Option<Spot> = || Some(Spot::Town);

    #[test]
    fn a_calm_bot_at_a_good_spot_with_ink_records() {
        assert_eq!(decide(&input(), TOWN), Decision::Record);
    }

    #[test]
    fn records_are_rate_limited_and_need_a_calm_bot() {
        let mut i = input();
        i.next_record_ms = i.now_ms + 1;
        assert_eq!(decide(&i, TOWN), Decision::Wait);
        let mut i = input();
        i.next_probe_ms = i.now_ms + 1;
        assert_eq!(decide(&i, TOWN), Decision::Wait);
        let mut i = input();
        i.busy = true;
        assert_eq!(decide(&i, TOWN), Decision::Wait);
        let mut i = input();
        i.calm = false;
        assert_eq!(decide(&i, TOWN), Decision::Wait);
        // the place is only looked up when everything else allows it
        let mut i = input();
        i.calm = false;
        decide(&i, || panic!("the spot is the last question"));
    }

    #[test]
    fn only_worthwhile_spots_far_from_an_entry_are_recorded() {
        assert_eq!(decide(&input(), || None), Decision::Wait);
        let mut i = input();
        i.nearest_entry_m = Some(ENTRY_NEAR_M - 1.0);
        assert_eq!(decide(&i, TOWN), Decision::Wait);
        i.nearest_entry_m = Some(ENTRY_NEAR_M + 1.0);
        assert_eq!(decide(&i, TOWN), Decision::Record);
    }

    #[test]
    fn ink_comes_from_gm_or_the_bot_waits_long() {
        let mut i = input();
        i.ink = 0;
        assert_eq!(decide(&i, TOWN), Decision::NoInk);
        i.gm = true;
        assert_eq!(decide(&i, TOWN), Decision::TopUpInk);
    }

    #[test]
    fn a_full_book_makes_room_first() {
        let mut i = input();
        i.entries = MAX_ENTRIES;
        assert_eq!(decide(&i, TOWN), Decision::MakeRoom);
        i.entries = MAX_ENTRIES - 1;
        assert_eq!(decide(&i, TOWN), Decision::Record);
    }

    #[test]
    fn names_are_cut_cleaned_and_unique() {
        let none = std::iter::empty::<&str>();
        assert_eq!(unique_name("Marianople", none.clone()), "Marianople");
        assert_eq!(unique_name("  ", none.clone()), "Spot");
        assert_eq!(unique_name("a\nb\u{7}c", none.clone()), "abc");
        let long = "x".repeat(60);
        assert_eq!(unique_name(&long, none).chars().count(), MAX_NAME_CHARS);
        let taken = ["Marianople", "Marianople 2"];
        assert_eq!(
            unique_name("Marianople", taken.iter().copied()),
            "Marianople 3"
        );
        // a full-length name still has room for the number
        let name = "y".repeat(MAX_NAME_CHARS);
        let taken = [name.as_str()];
        let second = unique_name(&name, taken.iter().copied());
        assert_eq!(second.chars().count(), MAX_NAME_CHARS);
        assert!(second.ends_with(" 2"));
        // Hangul counts as characters, not bytes
        let korean = "가".repeat(40);
        assert_eq!(
            unique_name(&korean, std::iter::empty::<&str>())
                .chars()
                .count(),
            MAX_NAME_CHARS
        );
    }
}
