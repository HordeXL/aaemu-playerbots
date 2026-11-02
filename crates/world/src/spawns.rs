//! Static NPC and doodad spawn positions from the server's `Worlds/<world>/npc_spawns*.json` and
//! `doodad_spawns*.json`.
//!
//! Follows the server's loader (AAEmu Rust `systems/npcs/spawn_manager.rs`, GPL-3.0-or-later):
//! files matching `<prefix>*.json` are read in reverse file-name order, JSONC is accepted,
//! NPC entries closer than 2 m (per axis, same template) to an already kept one are dropped,
//! doodad entries within 1 cm likewise, entries of unknown templates are skipped, and NPC entries
//! that carry `NpcSpawnerIds` (event/tower-defense spawners) are not static spawns.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rustc_hash::FxHashMap;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::geom::Vec3;

struct RawPosition {
    x: f32,
    y: f32,
    /// `None` when the entry has no Z (the server then leaves it to the terrain).
    z: Option<f32>,
}

impl<'de> Deserialize<'de> for RawPosition {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawPosition;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a position object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawPosition, A::Error> {
                let mut out = RawPosition {
                    x: 0.0,
                    y: 0.0,
                    z: None,
                };
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key.eq_ignore_ascii_case("X") {
                        out.x = map.next_value::<Option<f32>>()?.unwrap_or(0.0);
                    } else if key.eq_ignore_ascii_case("Y") {
                        out.y = map.next_value::<Option<f32>>()?.unwrap_or(0.0);
                    } else if key.eq_ignore_ascii_case("Z") {
                        out.z = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_map(V)
    }
}

struct RawSpawn {
    unit_id: u32,
    position: Option<RawPosition>,
    npc_spawner_ids: Option<Vec<u32>>,
}

/// Newtonsoft semantics: member names match case-insensitively, unknown members are ignored and
/// a repeated member keeps its last value (the shipped `doodad_spawns.json` repeats `UnitId`).
impl<'de> Deserialize<'de> for RawSpawn {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawSpawn;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a spawn entry object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawSpawn, A::Error> {
                let mut out = RawSpawn {
                    unit_id: 0,
                    position: None,
                    npc_spawner_ids: None,
                };
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key.eq_ignore_ascii_case("UnitId") {
                        out.unit_id = map.next_value::<Option<u32>>()?.unwrap_or(0);
                    } else if key.eq_ignore_ascii_case("Position") {
                        out.position = map.next_value()?;
                    } else if key.eq_ignore_ascii_case("NpcSpawnerIds") {
                        out.npc_spawner_ids = map.next_value()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_map(V)
    }
}

/// Which spawn files and rules to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `npc_spawns*.json`.
    Npc,
    /// `doodad_spawns*.json`.
    Doodad,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Npc => "npc_spawns",
            Kind::Doodad => "doodad_spawns",
        }
    }

    /// Per-axis distance under which two same-template entries count as one.
    fn dedup_distance(self) -> f32 {
        match self {
            Kind::Npc => 2.0,
            Kind::Doodad => 0.01,
        }
    }
}

/// Counters from one load, for logging and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpawnStats {
    /// Files read.
    pub files: usize,
    /// Entries in all files.
    pub entries: usize,
    /// Entries dropped as duplicates.
    pub duplicates: usize,
    /// Entries of templates that do not exist.
    pub unknown_template: usize,
    /// Event-spawner entries (NPC only).
    pub event_spawners: usize,
    /// Entries without a Z coordinate (filled from the terrain when available).
    pub missing_z: usize,
}

/// `<dir>/<prefix>*.json` in reverse (case-insensitive) file-name order.
fn spawn_files(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            (name.starts_with(prefix) && name.ends_with(".json")).then(|| (name, e.path()))
        })
        .collect();
    files.sort();
    files.reverse();
    files.into_iter().map(|(_, p)| p).collect()
}

/// Reads the spawn files of `kind` under `world_dir` (`<server data>/Worlds/<world>`).
///
/// `exists(template)` tells whether the template is known. `ground(x, y)` supplies a Z for
/// entries that have none (return `None` to leave 0).
pub fn load(
    world_dir: &Path,
    kind: Kind,
    exists: impl Fn(u32) -> bool,
    ground: impl Fn(f32, f32) -> Option<f32>,
) -> Result<(Vec<(u32, Vec3)>, SpawnStats)> {
    let mut stats = SpawnStats::default();
    let mut out = Vec::new();
    let mut stored: FxHashMap<u32, Vec<Vec3>> = FxHashMap::default();
    let dedup = kind.dedup_distance();
    for path in spawn_files(world_dir, kind.prefix()) {
        let entries: Vec<RawSpawn> = {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            crate::jsonc::from_str(&text).with_context(|| format!("parse {}", path.display()))?
        };
        stats.files += 1;
        for e in entries {
            stats.entries += 1;
            let raw_z = e.position.as_ref().and_then(|p| p.z);
            let pos = e
                .position
                .map_or(Vec3::default(), |p| Vec3::new(p.x, p.y, p.z.unwrap_or(0.0)));
            if stored.get(&e.unit_id).is_some_and(|v| {
                v.iter().any(|s| {
                    (s.x - pos.x).abs() < dedup
                        && (s.y - pos.y).abs() < dedup
                        && (s.z - pos.z).abs() < dedup
                })
            }) {
                stats.duplicates += 1;
                continue;
            }
            if !exists(e.unit_id) {
                stats.unknown_template += 1;
                continue;
            }
            let ids = e.npc_spawner_ids.as_deref().unwrap_or(&[]);
            if kind == Kind::Npc && !ids.is_empty() && ids != [0] {
                stats.event_spawners += 1;
                continue;
            }
            stored.entry(e.unit_id).or_default().push(pos);
            let mut pos = pos;
            if raw_z.is_none() {
                stats.missing_z += 1;
                if let Some(z) = ground(pos.x, pos.y) {
                    pos.z = z;
                }
            }
            out.push((e.unit_id, pos));
        }
    }
    Ok((out, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aa-world-spawns-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn files_are_read_in_reverse_name_order_and_deduplicated() {
        let dir = temp_dir("dedup");
        write(
            &dir,
            "npc_spawns.json",
            r#"[ // main file
              { "UnitId": 500, "Position": { "X": 101, "Y": 101, "Z": 5 } },
              { "UnitId": 500, "Position": { "X": 110, "Y": 101, "Z": 5 } },
              { "UnitId": 501, "Position": { "X": 1, "Y": 2 } },
              { "UnitId": 502, "NpcSpawnerIds": [4], "Position": { "X": 1, "Y": 2, "Z": 3 } },
              { "UnitId": 9999, "Position": { "X": 1, "Y": 2, "Z": 3 } },
            ]"#,
        );
        // "npc_spawns_10x.json" sorts after "npc_spawns.json", so it is read first.
        write(
            &dir,
            "npc_spawns_10x.json",
            r#"[ { "UnitId": 500, "Position": { "X": 100, "Y": 100, "Z": 5.5 } } ]"#,
        );
        write(&dir, "doodad_spawns.json", "[]");
        write(&dir, "gimmick_spawns.json", "[{ \"UnitId\": 1 }]");

        let (spawns, stats) = load(
            &dir,
            Kind::Npc,
            |t| t != 9999,
            |x, y| Some(x + y), // fake terrain
        )
        .unwrap();
        // 500@(100,100) kept (10x first); 500@(101,101) dropped (<2 m); 500@(110,101) kept;
        // 501 has no Z -> filled from terrain; 502 is an event spawner; 9999 unknown.
        let want = vec![
            (500, Vec3::new(100.0, 100.0, 5.5)),
            (500, Vec3::new(110.0, 101.0, 5.0)),
            (501, Vec3::new(1.0, 2.0, 3.0)),
        ];
        assert_eq!(spawns, want);
        assert_eq!(
            stats,
            SpawnStats {
                files: 2,
                entries: 6,
                duplicates: 1,
                unknown_template: 1,
                event_spawners: 1,
                missing_z: 1,
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_and_differently_cased_members_are_accepted() {
        let dir = temp_dir("dupfield");
        write(
            &dir,
            "doodad_spawns.json",
            r#"[ { "UnitId": 8218, // Artistry Workbench
                   "unitid": 8219, "Extra": [1, {"a": 2}], "position": { "X": 1.0, "Y": 2.0 } } ]"#,
        );
        let (spawns, _) = load(&dir, Kind::Doodad, |_| true, |_, _| None).unwrap();
        assert_eq!(spawns, vec![(8219, Vec3::new(1.0, 2.0, 0.0))]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doodad_dedup_is_one_centimetre() {
        let dir = temp_dir("doodad");
        write(
            &dir,
            "doodad_spawns.json",
            r#"[ { "UnitId": 7, "Position": { "X": 1.0, "Y": 1.0, "Z": 1.0 } },
                 { "UnitId": 7, "Position": { "X": 1.005, "Y": 1.0, "Z": 1.0 } },
                 { "UnitId": 7, "Position": { "X": 1.5, "Y": 1.0, "Z": 1.0 } } ]"#,
        );
        let (spawns, stats) = load(&dir, Kind::Doodad, |_| true, |_, _| None).unwrap();
        assert_eq!(spawns.len(), 2);
        assert_eq!(stats.duplicates, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_is_empty() {
        let (spawns, stats) = load(
            Path::new("definitely/not/here"),
            Kind::Npc,
            |_| true,
            |_, _| None,
        )
        .unwrap();
        assert!(spawns.is_empty());
        assert_eq!(stats, SpawnStats::default());
    }
}
