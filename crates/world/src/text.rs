//! English display names from `localized_texts` (`tbl_name`, `tbl_column_name`, `idx = row id`,
//! column `en_us`). One scan of the table collects every name set the world needs.

use anyhow::Result;
use rusqlite::Connection;
use rustc_hash::FxHashMap;

use crate::sql::{each_row, RowExt};

/// Which name set a `localized_texts` row belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTable {
    /// `npcs.name`
    Npc,
    /// `items.name`
    Item,
    /// `skills.name`
    Skill,
    /// `quest_contexts.name`
    Quest,
    /// `doodad_almighties.name`
    Doodad,
    /// `housings.name`
    Housing,
    /// `crafts.title`
    Craft,
    /// `zones.display_text`
    Zone,
    /// `zone_groups.display_text`
    ZoneGroup,
}

const TABLES: [TextTable; 9] = [
    TextTable::Npc,
    TextTable::Item,
    TextTable::Skill,
    TextTable::Quest,
    TextTable::Doodad,
    TextTable::Housing,
    TextTable::Craft,
    TextTable::Zone,
    TextTable::ZoneGroup,
];

fn classify(tbl: &str, col: &str) -> Option<TextTable> {
    Some(match (tbl, col) {
        ("npcs", "name") => TextTable::Npc,
        ("items", "name") => TextTable::Item,
        ("skills", "name") => TextTable::Skill,
        ("quest_contexts", "name") => TextTable::Quest,
        ("doodad_almighties", "name") => TextTable::Doodad,
        ("housings", "name") => TextTable::Housing,
        ("crafts", "title") => TextTable::Craft,
        ("zones", "display_text") => TextTable::Zone,
        ("zone_groups", "display_text") => TextTable::ZoneGroup,
        _ => return None,
    })
}

/// English texts per [`TextTable`], keyed by row id.
#[derive(Default)]
pub struct Texts {
    maps: Vec<FxHashMap<u32, Box<str>>>,
}

impl Texts {
    /// Scans `localized_texts` once.
    pub fn load(conn: &Connection) -> Result<Texts> {
        let mut maps: Vec<FxHashMap<u32, Box<str>>> =
            TABLES.iter().map(|_| FxHashMap::default()).collect();
        each_row(
            conn,
            "SELECT tbl_name, tbl_column_name, idx, en_us FROM localized_texts \
             WHERE tbl_name IN ('npcs','items','skills','quest_contexts','doodad_almighties', \
             'housings','crafts','zones','zone_groups') AND en_us IS NOT NULL AND en_us <> ''",
            |r| {
                if let Some(t) = classify(r.text(0), r.text(1)) {
                    let text = r.text(3).trim();
                    if !text.is_empty() {
                        maps[t as usize].insert(r.u32(2), text.into());
                    }
                }
            },
        )?;
        Ok(Texts { maps })
    }

    /// An empty set (tests).
    #[cfg(test)]
    pub fn empty() -> Texts {
        Texts {
            maps: TABLES.iter().map(|_| FxHashMap::default()).collect(),
        }
    }

    /// Adds a text (tests).
    #[cfg(test)]
    pub fn insert(&mut self, t: TextTable, id: u32, text: &str) {
        if self.maps.is_empty() {
            *self = Texts::empty();
        }
        self.maps[t as usize].insert(id, text.into());
    }

    /// The English text of row `id`, if any.
    pub fn get(&self, t: TextTable, id: u32) -> Option<&str> {
        self.maps.get(t as usize)?.get(&id).map(|s| &**s)
    }

    /// English text of `id`, else `fallback` (the raw column, usually Korean), as a boxed string.
    pub fn name_or(&self, t: TextTable, id: u32, fallback: &str) -> Box<str> {
        self.get(t, id).unwrap_or(fallback).into()
    }

    /// Number of texts per table, for logging.
    pub fn count(&self, t: TextTable) -> usize {
        self.maps.get(t as usize).map_or(0, FxHashMap::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_only_known_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE localized_texts (id INT, tbl_name TEXT, tbl_column_name TEXT, idx INT, ko TEXT, en_us TEXT);
             INSERT INTO localized_texts VALUES
               (1, 'npcs', 'name', 3597, '몰', 'Maude'),
               (2, 'npcs', 'description', 3597, 'x', 'ignored'),
               (3, 'items', 'name', 4058, 'k', ' Boar Meat '),
               (4, 'npcs', 'name', 9, 'k', ''),
               (5, 'zones', 'display_text', 125, 'k', 'Solzreed Peninsula');",
        )
        .unwrap();
        let t = Texts::load(&conn).unwrap();
        assert_eq!(t.get(TextTable::Npc, 3597), Some("Maude"));
        assert_eq!(t.get(TextTable::Item, 4058), Some("Boar Meat"));
        assert_eq!(t.get(TextTable::Npc, 9), None);
        assert_eq!(t.get(TextTable::Zone, 125), Some("Solzreed Peninsula"));
        assert_eq!(&*t.name_or(TextTable::Npc, 9, "fallback"), "fallback");
        assert_eq!(t.count(TextTable::Npc), 1);
    }
}
