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
