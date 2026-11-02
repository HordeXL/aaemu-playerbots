//! Faction relations, precomputed into a dense matrix.
//!
//! Ported from the server's `faction_relation` (AAEmu Rust `systems/skills/relation.rs`,
//! GPL-3.0-or-later): `system_factions.mother_id` inheritance, the three root factions
//! (1 friendly, 2 neutral, 3 hostile) and `system_faction_relations` rows (state 1 hostile,
//! 2 neutral, 3 friendly, matched in either direction, first row wins).

use anyhow::Result;
use rusqlite::Connection;
use rustc_hash::FxHashMap;

use crate::sql::{each_row, RowExt};

/// How one faction regards another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    /// Attackable / attacks on sight.
    Hostile,
    /// Neither; also the answer for unknown faction ids.
    Neutral,
    /// Same side.
    Friendly,
}

const ROOT_FRIENDLY: u32 = 1;
const ROOT_NEUTRAL: u32 = 2;
const ROOT_HOSTILE: u32 = 3;

fn from_state(state: u8) -> Relation {
    match state {
        1 => Relation::Hostile,
        3 => Relation::Friendly,
        _ => Relation::Neutral,
    }
}

fn encode(r: Relation) -> u8 {
    match r {
        Relation::Hostile => 1,
        Relation::Neutral => 2,
        Relation::Friendly => 3,
    }
}

/// All `n * n` ordered relations between the known factions.
#[derive(Debug, Default)]
pub struct Factions {
    /// Sorted faction ids; index = row/column of `matrix`.
    ids: Vec<u32>,
    matrix: Vec<u8>,
}

impl Factions {
    /// Loads `system_factions` and `system_faction_relations`.
    pub fn load(conn: &Connection) -> Result<Factions> {
        let mut factions = Vec::new();
        each_row(conn, "SELECT id, mother_id FROM system_factions", |r| {
            factions.push((r.u32(0), r.u32(1)))
        })?;
        let mut relations = Vec::new();
        each_row(
            conn,
            "SELECT faction1_id, faction2_id, state_id FROM system_faction_relations ORDER BY rowid",
            |r| relations.push((r.u32(0), r.u32(1), r.u8(2))),
        )?;
        Ok(Factions::build(&factions, &relations))
    }

    /// Builds the matrix from `(id, mother_id)` factions and `(faction1, faction2, state)`
    /// relation rows (in table order).
    pub fn build(factions: &[(u32, u32)], relations: &[(u32, u32, u8)]) -> Factions {
        let mut sorted: Vec<(u32, u32)> = factions.to_vec();
        sorted.sort_by_key(|f| f.0);
        sorted.dedup_by_key(|f| f.0);
        let ids: Vec<u32> = sorted.iter().map(|f| f.0).collect();
        let mothers: Vec<u32> = sorted.iter().map(|f| f.1).collect();
        let n = ids.len();

        // Both orientations of a stored pair, the first row seen wins.
        let mut stored: FxHashMap<(u32, u32), u8> = FxHashMap::default();
        for &(a, b, state) in relations {
            stored.entry((a, b)).or_insert(state);
            stored.entry((b, a)).or_insert(state);
        }
        let stored = |a: u32, b: u32| stored.get(&(a, b)).map(|s| from_state(*s));
        let known = |id: u32| ids.binary_search(&id).is_ok();

        let mut matrix = vec![encode(Relation::Neutral); n * n];
        for (i, &own) in ids.iter().enumerate() {
            for (j, &other) in ids.iter().enumerate() {
                let own_mother = mothers[i];
                let faction_id = if own_mother != 0 { own_mother } else { own };
                let other_id = if mothers[j] != 0 { mothers[j] } else { other };
                let rel = match (faction_id, other_id) {
                    (ROOT_NEUTRAL, _) => Relation::Neutral,
                    (ROOT_FRIENDLY, _) => Relation::Friendly,
                    (ROOT_HOSTILE, _) => Relation::Hostile,
                    (_, ROOT_NEUTRAL) => Relation::Neutral,
                    (_, ROOT_FRIENDLY) => Relation::Friendly,
                    (_, ROOT_HOSTILE) => Relation::Hostile,
                    (a, b) if a == b => Relation::Friendly,
                    _ if own_mother != 0 && known(own_mother) => {
                        stored(own_mother, other_id).unwrap_or(Relation::Friendly)
                    }
                    _ => stored(own, other_id).unwrap_or(Relation::Neutral),
                };
                matrix[i * n + j] = encode(rel);
            }
        }
        Factions { ids, matrix }
    }

    /// How faction `a` regards faction `b`; [`Relation::Neutral`] if either is unknown.
    pub fn relation(&self, a: u32, b: u32) -> Relation {
        let (Ok(i), Ok(j)) = (self.ids.binary_search(&a), self.ids.binary_search(&b)) else {
            return Relation::Neutral;
        };
        from_state(self.matrix[i * self.ids.len() + j])
    }

    /// Number of known factions.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no factions are known.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.ids.capacity() * 4 + self.matrix.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Factions {
        // 1/2/3 roots; 10 and 11 belong to 20 (mother); 30 and 40 are plain.
        Factions::build(
            &[
                (1, 0),
                (2, 0),
                (3, 0),
                (10, 20),
                (11, 20),
                (20, 0),
                (30, 0),
                (40, 0),
            ],
            &[(20, 30, 1), (30, 40, 3), (40, 30, 1)],
        )
    }

    #[test]
    fn roots_decide_first() {
        let f = sample();
        assert_eq!(f.relation(1, 30), Relation::Friendly);
        assert_eq!(f.relation(30, 1), Relation::Friendly);
        assert_eq!(f.relation(2, 30), Relation::Neutral);
        assert_eq!(f.relation(3, 1), Relation::Hostile);
        assert_eq!(f.relation(30, 3), Relation::Hostile);
    }

    #[test]
    fn members_inherit_the_mother_relations() {
        let f = sample();
        // 10 is a member of 20; 20 is hostile to 30 either way round.
        assert_eq!(f.relation(10, 30), Relation::Hostile);
        assert_eq!(f.relation(30, 10), Relation::Hostile);
        assert_eq!(f.relation(11, 30), Relation::Hostile);
    }

    #[test]
    fn siblings_and_self_are_friendly() {
        let f = sample();
        assert_eq!(f.relation(10, 11), Relation::Friendly);
        assert_eq!(f.relation(11, 10), Relation::Friendly);
        assert_eq!(f.relation(30, 30), Relation::Friendly);
        assert_eq!(f.relation(10, 20), Relation::Friendly);
    }

    #[test]
    fn member_without_stored_relation_defaults_to_friendly_plain_to_neutral() {
        let f = sample();
        // 10 (member of 20) vs 40: no (20,40) row -> Friendly (the server's quirk).
        assert_eq!(f.relation(10, 40), Relation::Friendly);
        // 30 (no mother) vs 20: the (20,30) row matches in reverse -> Hostile.
        assert_eq!(f.relation(30, 20), Relation::Hostile);
        // 40 vs 20: no row -> Neutral.
        assert_eq!(f.relation(40, 20), Relation::Neutral);
    }

    #[test]
    fn first_row_wins_for_conflicting_directions() {
        let f = sample();
        // rows: (30,40,3) then (40,30,1): the first matching row decides both directions.
        assert_eq!(f.relation(30, 40), Relation::Friendly);
        assert_eq!(f.relation(40, 30), Relation::Friendly);
    }

    #[test]
    fn unknown_factions_are_neutral() {
        let f = sample();
        assert_eq!(f.relation(999, 30), Relation::Neutral);
        assert_eq!(f.relation(30, 999), Relation::Neutral);
        assert_eq!(f.relation(0, 0), Relation::Neutral);
    }
}
