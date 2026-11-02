//! Playable races: which race/gender combinations can be created (compact `characters`), their
//! faction, and where a new character of each race starts (`CharTemplates.json`).
//!
//! Table semantics follow the server's `data/character_templates.rs` (AAEmu Rust,
//! GPL-3.0-or-later).

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Deserialize;

use crate::geom::Vec3;
use crate::sql::{each_row, RowExt};

/// One `characters` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharRow {
    /// `char_race_id`: 1 Nuian, 2 Fairy, 3 Dwarf, 4 Elf, 5 Hariharan, 6 Ferre, 7 Returned, 8 Warborn.
    pub race: u8,
    /// `char_gender_id`: 1 male, 2 female.
    pub gender: u8,
    /// Whether the character creation screen offers it.
    pub creatable: bool,
    /// Faction of a player of this race.
    pub faction_id: u32,
}

/// Race templates.
#[derive(Debug, Default)]
pub struct Chars {
    rows: Vec<CharRow>,
    starts: Vec<(u8, Vec3)>,
}

#[derive(Deserialize)]
struct RawPos {
    #[serde(rename = "X")]
    x: f32,
    #[serde(rename = "Y")]
    y: f32,
    #[serde(rename = "Z", default)]
    z: f32,
}

#[derive(Deserialize)]
struct RawTemplate {
    #[serde(rename = "Id")]
    id: u32,
    #[serde(rename = "Pos")]
    pos: RawPos,
}

impl Chars {
    /// Loads `characters` from the compact database and `CharTemplates.json` from the server's
    /// data directory (a missing or unreadable file is a warning: no start positions).
    pub fn load(conn: &Connection, server_data: &Path) -> Result<Chars> {
        let mut rows = Vec::new();
        each_row(
            conn,
            "SELECT char_race_id, char_gender_id, creatable, faction_id FROM characters ORDER BY id",
            |r| {
                rows.push(CharRow {
                    race: r.u8(0),
                    gender: r.u8(1),
                    creatable: r.flag(2),
                    faction_id: r.u32(3),
                })
            },
        )?;
        let path = server_data.join("CharTemplates.json");
        let starts = match std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .and_then(|text| crate::jsonc::from_str::<Vec<RawTemplate>>(&text))
        {
            Ok(list) => Chars::starts_from(list),
            Err(e) => {
                tracing::warn!("no character start positions: {e:#}");
                Vec::new()
            }
        };
        Ok(Chars { rows, starts })
    }

    fn starts_from(list: Vec<RawTemplate>) -> Vec<(u8, Vec3)> {
        list.into_iter()
            .filter_map(|t| {
                let race = u8::try_from(t.id).ok()?;
                Some((race, Vec3::new(t.pos.x, t.pos.y, t.pos.z)))
            })
            .collect()
    }

    /// Builds from parts (tests).
    #[cfg(test)]
    pub fn from_parts(rows: Vec<CharRow>, starts: Vec<(u8, Vec3)>) -> Chars {
        Chars { rows, starts }
    }

    /// Where a new character of `race` appears in the world.
    pub fn race_start(&self, race: u8) -> Option<Vec3> {
        self.starts.iter().find(|s| s.0 == race).map(|s| s.1)
    }

    /// Whether `race` / `gender` can be created.
    pub fn creatable(&self, race: u8, gender: u8) -> bool {
        self.rows
            .iter()
            .any(|r| r.race == race && r.gender == gender && r.creatable)
    }

    /// The faction of a player of `race`.
    pub fn race_faction(&self, race: u8) -> Option<u32> {
        self.rows
            .iter()
            .find(|r| r.race == race)
            .map(|r| r.faction_id)
    }

    /// All `characters` rows.
    pub fn rows(&self) -> &[CharRow] {
        &self.rows
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.rows.capacity() * std::mem::size_of::<CharRow>()
            + self.starts.capacity() * std::mem::size_of::<(u8, Vec3)>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_char_templates() {
        let list: Vec<RawTemplate> = crate::jsonc::from_str(
            r#"[{"Id": 1, "Name": "Nuian", "Pos": {"X": 1.5, "Y": 2.5, "Z": 3.5, "Yaw": 160}},
                {"Id": 4, "Pos": {"X": 9, "Y": 8}}, ]"#,
        )
        .unwrap();
        let starts = Chars::starts_from(list);
        let chars = Chars::from_parts(Vec::new(), starts);
        assert_eq!(chars.race_start(1), Some(Vec3::new(1.5, 2.5, 3.5)));
        assert_eq!(chars.race_start(4), Some(Vec3::new(9.0, 8.0, 0.0)));
        assert_eq!(chars.race_start(2), None);
    }

    #[test]
    fn creatable_needs_race_gender_and_flag() {
        let chars = Chars::from_parts(
            vec![
                CharRow {
                    race: 1,
                    gender: 1,
                    creatable: true,
                    faction_id: 101,
                },
                CharRow {
                    race: 1,
                    gender: 2,
                    creatable: false,
                    faction_id: 101,
                },
            ],
            Vec::new(),
        );
        assert!(chars.creatable(1, 1));
        assert!(!chars.creatable(1, 2));
        assert!(!chars.creatable(2, 1));
        assert_eq!(chars.race_faction(1), Some(101));
    }
}
