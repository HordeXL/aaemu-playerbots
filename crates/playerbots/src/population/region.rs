//! Where things are: the side (alliance) of a faction or a race, the continent of a position.
//!
//! Pure lookups over the world data. The alliance of a character is the mother faction of its
//! faction (148 Nuia, 149 Haranya); the continent of a position is `zone_groups.target_id` of its
//! zone (3 west, 4 east, 5 Auroria, 1, 2 and 6 the seas).

use aa_world::World;
use pb_web::{Alliance, Continent};

/// What the population code needs to know about the map and the factions: a [`World`] in the
/// bots, a table in the tests.
pub trait Geo {
    /// `mother_id` of a faction, 0 when it has none.
    fn mother_faction(&self, faction: u32) -> u32;
    /// The zone key at a map position.
    fn zone_key_at(&self, x: f32, y: f32) -> Option<u16>;
    /// `zone_groups.target_id` of a zone: the continent, 0 when unknown.
    fn continent_target(&self, zone_key: u16) -> u8;
    /// English name of a zone.
    fn zone_name(&self, zone_key: u16) -> Option<String>;
}

impl Geo for World {
    fn mother_faction(&self, faction: u32) -> u32 {
        World::mother_faction(self, faction)
    }
    fn zone_key_at(&self, x: f32, y: f32) -> Option<u16> {
        World::zone_key_at(self, x, y)
    }
    fn continent_target(&self, zone_key: u16) -> u8 {
        self.travel().portals.continent_of_zone(zone_key)
    }
    fn zone_name(&self, zone_key: u16) -> Option<String> {
        World::zone_name(self, zone_key).map(str::to_owned)
    }
}

/// The mother faction of the Nuia alliance.
pub const NUIA_FACTION: u32 = 148;
/// The mother faction of the Haranya alliance.
pub const HARANYA_FACTION: u32 = 149;

/// The side of a mother faction id.
pub fn alliance_of_mother(mother: u32) -> Alliance {
    match mother {
        0 => Alliance::Unknown,
        NUIA_FACTION => Alliance::Nuia,
        HARANYA_FACTION => Alliance::Haranya,
        _ => Alliance::Other,
    }
}

/// The side a faction belongs to. A faction that is a mother itself (148, 149) is its own side;
/// 0 and factions without a mother (monsters, system factions) are `Unknown`.
pub fn alliance_of_faction(world: &dyn Geo, faction: u32) -> Alliance {
    if faction == 0 {
        return Alliance::Unknown;
    }
    if matches!(faction, NUIA_FACTION | HARANYA_FACTION) {
        return alliance_of_mother(faction);
    }
    match world.mother_faction(faction) {
        0 => Alliance::Unknown,
        m => alliance_of_mother(m),
    }
}

/// The side a race plays (race ids as in `config::race_id`): Nuian, Fairy, Dwarf, Elf and the
/// Returned are Nuia's, Hariharan, Ferre and the Warborn are Haranya's.
pub fn alliance_of_race(race: u8) -> Alliance {
    match race {
        1..=4 | 7 => Alliance::Nuia,
        5 | 6 | 8 => Alliance::Haranya,
        _ => Alliance::Unknown,
    }
}

/// The continent a side starts on.
pub fn home_continent(alliance: Alliance) -> Continent {
    match alliance {
        Alliance::Nuia => Continent::West,
        Alliance::Haranya => Continent::East,
        _ => Continent::Unknown,
    }
}

/// The continent of a `zone_groups.target_id`.
pub fn continent_of_target(target: u8) -> Continent {
    match target {
        3 => Continent::West,
        4 => Continent::East,
        5 => Continent::Auroria,
        1 | 2 | 6 => Continent::Sea,
        _ => Continent::Unknown,
    }
}

/// The continent of a zone key.
pub fn continent_of_zone(world: &dyn Geo, zone_key: u16) -> Continent {
    if zone_key == 0 {
        return Continent::Unknown;
    }
    continent_of_target(world.continent_target(zone_key))
}

/// A position's zone and continent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Place {
    /// Zone key, 0 where the baked zone grid has none.
    pub zone_key: u16,
    pub continent: Continent,
}

/// The zone and continent at map position `(x, y)`.
pub fn place_at(world: &dyn Geo, x: f32, y: f32) -> Place {
    let zone_key = world.zone_key_at(x, y).unwrap_or(0);
    Place {
        zone_key,
        continent: continent_of_zone(world, zone_key),
    }
}

/// A tiny fake map for the tests of the population code.
#[cfg(test)]
pub mod fake {
    use super::Geo;

    /// A tiny map: x below 1000 is zone 5 (west), 1000 and above zone 9 (east), 3000 and above
    /// zone 30 (sea); factions 101 and 108 belong to the two alliances.
    pub struct Table;

    impl Geo for Table {
        fn mother_faction(&self, faction: u32) -> u32 {
            match faction {
                101..=107 => 148,
                108..=113 => 149,
                _ => 0,
            }
        }
        fn zone_key_at(&self, x: f32, _y: f32) -> Option<u16> {
            match x {
                x if x < 0.0 => None,
                x if x < 1000.0 => Some(5),
                x if x < 3000.0 => Some(9),
                _ => Some(30),
            }
        }
        fn continent_target(&self, zone_key: u16) -> u8 {
            match zone_key {
                5 => 3,
                9 => 4,
                30 => 2,
                _ => 0,
            }
        }
        fn zone_name(&self, zone_key: u16) -> Option<String> {
            Some(format!("zone{zone_key}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Table;
    use super::*;

    #[test]
    fn mothers_and_races_name_the_two_sides() {
        assert_eq!(alliance_of_mother(148), Alliance::Nuia);
        assert_eq!(alliance_of_mother(149), Alliance::Haranya);
        assert_eq!(alliance_of_mother(0), Alliance::Unknown);
        assert_eq!(alliance_of_mother(150), Alliance::Other);
        for (race, side) in [
            (1, Alliance::Nuia),
            (2, Alliance::Nuia),
            (3, Alliance::Nuia),
            (4, Alliance::Nuia),
            (5, Alliance::Haranya),
            (6, Alliance::Haranya),
            (7, Alliance::Nuia),
            (8, Alliance::Haranya),
            (0, Alliance::Unknown),
            (9, Alliance::Unknown),
        ] {
            assert_eq!(alliance_of_race(race), side, "race {race}");
        }
        assert_eq!(home_continent(Alliance::Nuia), Continent::West);
        assert_eq!(home_continent(Alliance::Haranya), Continent::East);
        assert_eq!(home_continent(Alliance::Other), Continent::Unknown);
    }

    #[test]
    fn zone_group_targets_are_continents() {
        assert_eq!(continent_of_target(3), Continent::West);
        assert_eq!(continent_of_target(4), Continent::East);
        assert_eq!(continent_of_target(5), Continent::Auroria);
        for sea in [1, 2, 6] {
            assert_eq!(continent_of_target(sea), Continent::Sea);
        }
        assert_eq!(continent_of_target(0), Continent::Unknown);
        assert_eq!(continent_of_target(77), Continent::Unknown);
    }

    #[test]
    fn positions_and_factions_are_placed_through_the_geo_table() {
        let w = Table;
        assert_eq!(
            place_at(&w, 500.0, 0.0),
            Place {
                zone_key: 5,
                continent: Continent::West
            }
        );
        assert_eq!(place_at(&w, 1500.0, 0.0).continent, Continent::East);
        assert_eq!(place_at(&w, 4000.0, 0.0).continent, Continent::Sea);
        assert_eq!(place_at(&w, -5.0, 0.0), Place::default());
        assert_eq!(alliance_of_faction(&w, 0), Alliance::Unknown);
        assert_eq!(alliance_of_faction(&w, 101), Alliance::Nuia);
        assert_eq!(alliance_of_faction(&w, 110), Alliance::Haranya);
        assert_eq!(alliance_of_faction(&w, 148), Alliance::Nuia);
        assert_eq!(alliance_of_faction(&w, 149), Alliance::Haranya);
        // monsters and the flags of a duel have no mother
        assert_eq!(alliance_of_faction(&w, 115), Alliance::Unknown);
        assert_eq!(continent_of_zone(&w, 0), Continent::Unknown);
        assert_eq!(continent_of_zone(&w, 77), Continent::Unknown);
    }
}
