//! Instance worlds: every `Worlds/<name>/` directory of the server data except the main world
//! (`instance_*`, `arche_mall_world`, `racing_test_world`).
//!
//! The server maps the zone key an `SCLoadInstance` carries to a world through the client's
//! `game/worlds/<name>/world.xml` (its `<Zone id=...>` entries); that file is not part of the
//! server data. The instance worlds are named after their zones in `compact.zones`
//! (`zones.name`, e.g. zone 262 = `instance_cuttingwind_deadmine`), so a zone belongs to the
//! world of the same name; the zones of a world with several (`instance_library` holds
//! `instance_library_1`, `_2`, `_boss_1`, ...) and `arche_mall` (world `arche_mall_world`) are
//! matched by name prefix. See [`resolve_world`].
//!
//! The spawn point of a world is the `Worlds/world_spawns.json` entry named like the world or
//! like one of its zones (`systems/instances/worlds.rs: zone_spawn_position`); its `ZoneId` is
//! the zone key.

use std::path::Path;

use anyhow::{Context, Result};

use super::funcs::DoodadTravelFuncs;
use super::{
    member, member_f32, member_str, member_u32, read_object_array, str_bytes, TravelInputs,
};
use crate::geom::Vec3;
use crate::spawns::{self, Kind, SpawnStats};
use crate::sql::{each_row, RowExt};

/// An NPC spawn of an instance world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceNpc {
    /// NPC template.
    pub template: u32,
    /// Spawn position.
    pub pos: Vec3,
    /// Template level.
    pub level: u8,
    /// Raw `npcs.npc_grade_id`. The data has no boss flag: 1 is the open-world normal, the
    /// ordinary mobs of dungeons are mostly 6 and 7, and the named bosses are 2 or 7 (the
    /// Sharpwind Mines' Nerta is 7, its Stonebrother Colossus 2), so tell bosses apart by
    /// template id, not by grade.
    pub grade: u8,
    /// The template attacks players that come near (`npcs.aggression`); whether the NPC is an
    /// enemy of a given player is `World::relation(player faction, npc faction)`.
    pub aggressive: bool,
}

/// A usable doodad spawn of an instance world (it has a use skill or a function a bot can
/// trigger), other than the exits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceDoodad {
    /// Doodad template.
    pub template: u32,
    /// Spawn position.
    pub pos: Vec3,
    /// Skill a client casts to use it (0 = the default interaction).
    pub skill: u32,
}

/// An exit doodad (`DoodadFuncExitIndun`) of an instance world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceExit {
    /// Doodad template.
    pub template: u32,
    /// Spawn position.
    pub pos: Vec3,
    /// Skill to cast on it.
    pub skill: u32,
    /// `doodad_func_exit_induns.return_point_id`; 0 sends the character back to where it
    /// entered from (`MainWorldPosition`).
    pub return_point: u32,
}

/// A spawn point of an instance world (`world_spawns.json`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceSpawn {
    /// The entry's name (the world's or one of its zones').
    pub name: Box<str>,
    /// Zone key of the point (`SpawnPosition.ZoneId`).
    pub zone_key: u16,
    /// Position.
    pub pos: Vec3,
    /// Facing, degrees.
    pub yaw: f32,
}

/// One instance world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstanceWorld {
    /// Directory name under `Worlds/`.
    pub name: Box<str>,
    /// Zone keys that load this world.
    pub zone_keys: Box<[u16]>,
    /// Spawn points (`world_spawns.json` entries of the world or its zones).
    pub spawns: Box<[InstanceSpawn]>,
    npcs: Box<[InstanceNpc]>,
    doodads: Box<[InstanceDoodad]>,
    exits: Box<[InstanceExit]>,
    /// Counters from reading `npc_spawns*.json` (unknown templates, event spawners, ...).
    pub npc_stats: SpawnStats,
    /// Counters from reading `doodad_spawns*.json`.
    pub doodad_stats: SpawnStats,
}

impl InstanceWorld {
    /// NPC spawns (static ones; event spawners are not included, see `npc_stats`).
    pub fn npcs(&self) -> &[InstanceNpc] {
        &self.npcs
    }

    /// Usable doodad spawns other than the exits.
    pub fn doodads(&self) -> &[InstanceDoodad] {
        &self.doodads
    }

    /// Exit doodad spawns.
    pub fn exits(&self) -> &[InstanceExit] {
        &self.exits
    }

    /// The exit doodad nearest to `from` (horizontal distance).
    pub fn nearest_exit(&self, from: Vec3) -> Option<&InstanceExit> {
        self.exits
            .iter()
            .min_by(|a, b| a.pos.dist2d_sq(from).total_cmp(&b.pos.dist2d_sq(from)))
    }

    /// NPC spawns of exactly this `npc_grade_id` (see [`InstanceNpc::grade`]).
    pub fn npcs_with_grade(&self, grade: u8) -> impl Iterator<Item = &InstanceNpc> {
        self.npcs.iter().filter(move |n| n.grade == grade)
    }

    /// The spawn point for a character that arrives for `zone_key`: the entry of that zone,
    /// else the entry named like the world, else the first one.
    pub fn spawn_for_zone(&self, zone_key: u16) -> Option<&InstanceSpawn> {
        self.spawns
            .iter()
            .find(|s| s.zone_key == zone_key)
            .or_else(|| self.spawns.iter().find(|s| *s.name == *self.name))
            .or_else(|| self.spawns.first())
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.name.len()
            + self.zone_keys.len() * 2
            + self.spawns.len() * std::mem::size_of::<InstanceSpawn>()
            + str_bytes(self.spawns.iter().map(|s| &*s.name))
            + std::mem::size_of_val(&*self.npcs)
            + std::mem::size_of_val(&*self.doodads)
            + std::mem::size_of_val(&*self.exits)
    }
}

/// All instance worlds.
#[derive(Debug, Default)]
pub struct InstanceWorlds {
    /// Sorted by name.
    worlds: Vec<InstanceWorld>,
    /// `(zone key, index into worlds)` sorted by zone key.
    by_zone: Vec<(u16, u16)>,
}

impl InstanceWorlds {
    pub(crate) fn load(
        inp: &TravelInputs<'_>,
        funcs: &DoodadTravelFuncs,
    ) -> Result<InstanceWorlds> {
        let root = inp.server_data.join("Worlds");
        let names = world_dirs(&root, inp.main_world);

        let mut zones: Vec<(u16, String)> = Vec::new();
        each_row(inp.conn, "SELECT zone_key, name FROM zones", |r| {
            zones.push((r.u16(0), r.text(1).trim().to_owned()));
        })
        .context("read zones")?;

        // zone -> world, and the world_spawns.json entries per world
        let mut zone_keys: Vec<Vec<u16>> = vec![Vec::new(); names.len()];
        for (key, name) in &zones {
            if let Some(i) = resolve_world(&names, name) {
                zone_keys[i].push(*key);
            }
        }
        let mut spawns: Vec<Vec<InstanceSpawn>> = vec![Vec::new(); names.len()];
        for s in read_world_spawns(&root.join("world_spawns.json"))? {
            if let Some(i) = resolve_world(&names, &s.name) {
                spawns[i].push(s);
            }
        }

        let mut worlds = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let dir = root.join(name);
            let mut keys = std::mem::take(&mut zone_keys[i]);
            keys.sort_unstable();
            keys.dedup();
            let mut world = load_world(inp, funcs, name, &dir)?;
            world.zone_keys = keys.into_boxed_slice();
            world.spawns = std::mem::take(&mut spawns[i]).into_boxed_slice();
            worlds.push(world);
        }

        let mut by_zone: Vec<(u16, u16)> = worlds
            .iter()
            .enumerate()
            .flat_map(|(i, w)| w.zone_keys.iter().map(move |k| (*k, i as u16)))
            .collect();
        by_zone.sort_unstable();
        by_zone.dedup_by_key(|e| e.0);
        Ok(InstanceWorlds { worlds, by_zone })
    }

    /// All instance worlds ordered by name.
    pub fn all(&self) -> &[InstanceWorld] {
        &self.worlds
    }

    /// The world with directory name `name`.
    pub fn by_name(&self, name: &str) -> Option<&InstanceWorld> {
        self.worlds
            .binary_search_by(|w| (*w.name).cmp(name))
            .ok()
            .map(|i| &self.worlds[i])
    }

    /// Index of the world that loads `zone_key`.
    pub(crate) fn index_of_zone(&self, zone_key: u16) -> Option<usize> {
        self.by_zone
            .binary_search_by_key(&zone_key, |e| e.0)
            .ok()
            .map(|i| usize::from(self.by_zone[i].1))
    }

    /// The world that loads `zone_key` (the zone key of an `SCLoadInstance`).
    pub fn by_zone_key(&self, zone_key: u16) -> Option<&InstanceWorld> {
        self.index_of_zone(zone_key).map(|i| &self.worlds[i])
    }

    pub(crate) fn get(&self, index: usize) -> Option<&InstanceWorld> {
        self.worlds.get(index)
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.worlds.capacity() * std::mem::size_of::<InstanceWorld>()
            + self.worlds.iter().map(InstanceWorld::bytes).sum::<usize>()
            + self.by_zone.capacity() * 4
    }
}

/// The directory names under `root` other than the main world, sorted.
fn world_dirs(root: &Path, main_world: &str) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != main_world)
        .collect();
    names.sort();
    names
}

/// The world a zone (or a `world_spawns.json` entry) name belongs to.
///
/// 1. the world of exactly that name;
/// 2. else the longest world name the zone name extends (`instance_library_boss_1` ->
///    `instance_library`);
/// 3. else the only world whose name extends the zone name (`arche_mall` ->
///    `arche_mall_world`).
pub(crate) fn resolve_world(worlds: &[String], zone_name: &str) -> Option<usize> {
    let z = zone_name.trim();
    if z.is_empty() {
        return None;
    }
    if let Some(i) = worlds.iter().position(|w| w == z) {
        return Some(i);
    }
    let longest = worlds
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            z.strip_prefix(w.as_str())
                .is_some_and(|r| r.starts_with('_'))
        })
        .max_by_key(|(_, w)| w.len());
    if let Some((i, _)) = longest {
        return Some(i);
    }
    let mut extending = worlds
        .iter()
        .enumerate()
        .filter(|(_, w)| w.strip_prefix(z).is_some_and(|r| r.starts_with('_')));
    match (extending.next(), extending.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// `Worlds/world_spawns.json`.
fn read_world_spawns(path: &Path) -> Result<Vec<InstanceSpawn>> {
    Ok(read_object_array(path)?
        .iter()
        .filter_map(|m| {
            let name = member_str(m, "Name");
            let p = member(m, "SpawnPosition")?.as_object()?;
            (!name.is_empty()).then(|| InstanceSpawn {
                name: name.into(),
                zone_key: member_u32(p, "ZoneId").min(u32::from(u16::MAX)) as u16,
                pos: Vec3::new(member_f32(p, "X"), member_f32(p, "Y"), member_f32(p, "Z")),
                yaw: member_f32(p, "Yaw"),
            })
        })
        .collect())
}

/// Reads the spawn files of one world directory.
fn load_world(
    inp: &TravelInputs<'_>,
    funcs: &DoodadTravelFuncs,
    name: &str,
    dir: &Path,
) -> Result<InstanceWorld> {
    // No terrain for instances: spawns without a Z stay at 0.
    let (npc_raw, npc_stats) = spawns::load(dir, Kind::Npc, |t| inp.npcs.contains(t), |_, _| None)
        .with_context(|| format!("load npc spawns of {name}"))?;
    let (doodad_raw, doodad_stats) = spawns::load(
        dir,
        Kind::Doodad,
        |t| inp.doodads.templates.contains(t),
        |_, _| None,
    )
    .with_context(|| format!("load doodad spawns of {name}"))?;

    let npcs: Vec<InstanceNpc> = npc_raw
        .into_iter()
        .filter_map(|(template, pos)| {
            let t = inp.npcs.get(template)?;
            Some(InstanceNpc {
                template,
                pos,
                level: t.level,
                grade: t.grade,
                aggressive: t.aggressive,
            })
        })
        .collect();

    let mut doodads = Vec::new();
    let mut exits = Vec::new();
    for (template, pos) in doodad_raw {
        let Some(t) = inp.doodads.templates.get(template) else {
            continue;
        };
        if let Some(exit) = funcs.exit_of(template) {
            exits.push(InstanceExit {
                template,
                pos,
                skill: if exit.skill != 0 {
                    exit.skill
                } else {
                    t.use_skill
                },
                return_point: exit.return_point,
            });
        } else if t.use_skill != 0 || t.flags.0 != 0 {
            doodads.push(InstanceDoodad {
                template,
                pos,
                skill: t.use_skill,
            });
        }
    }

    Ok(InstanceWorld {
        name: name.into(),
        zone_keys: Box::default(),
        spawns: Box::default(),
        npcs: npcs.into_boxed_slice(),
        doodads: doodads.into_boxed_slice(),
        exits: exits.into_boxed_slice(),
        npc_stats,
        doodad_stats,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::doodad::{DoodadData, DoodadFlags, DoodadTemplate};
    use crate::geom::{IdMap, StrTable};
    use crate::npc::{NpcRoles, NpcTemplate};
    use crate::skill::Skills;
    use crate::spatial::SpawnIndex;
    use crate::text::Texts;
    use crate::travel::funcs;
    use crate::travel::testutil::{temp_dir, write};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn zone_names_resolve_to_worlds() {
        let worlds = names(&[
            "arche_mall_world",
            "instance_howling_abyss",
            "instance_howling_abyss_2",
            "instance_library",
        ]);
        let r = |z: &str| resolve_world(&worlds, z).map(|i| worlds[i].as_str());
        assert_eq!(r("instance_howling_abyss"), Some("instance_howling_abyss"));
        assert_eq!(
            r("instance_howling_abyss_2"),
            Some("instance_howling_abyss_2")
        );
        assert_eq!(
            r("instance_library_1 "),
            Some("instance_library"),
            "names are trimmed"
        );
        assert_eq!(r("instance_library_boss_2"), Some("instance_library"));
        assert_eq!(r("arche_mall"), Some("arche_mall_world"));
        assert_eq!(
            r("instance_libraryx"),
            None,
            "a prefix must end at an underscore"
        );
        assert_eq!(r("main_world_0_0"), None);
        assert_eq!(r("o_library_1"), None);
        assert_eq!(r(""), None);
        // two worlds extend the name: ambiguous
        let two = names(&["arche_mall_a", "arche_mall_b"]);
        assert_eq!(resolve_world(&two, "arche_mall"), None);
    }

    fn npc(id: u32, level: u8, grade: u8, aggressive: bool) -> (u32, NpcTemplate) {
        (
            id,
            NpcTemplate {
                id,
                name: "n".into(),
                level,
                faction_id: 115,
                kind: 1,
                grade,
                aggressive,
                roles: NpcRoles::NONE,
                is_quest_giver: false,
            },
        )
    }

    fn doodad(id: u32, flags: u16, use_skill: u32) -> (u32, DoodadTemplate) {
        (
            id,
            DoodadTemplate {
                id,
                name: "d".into(),
                flags: DoodadFlags(flags),
                use_skill,
            },
        )
    }

    #[test]
    fn worlds_load_spawns_zones_exits_and_spawn_points() {
        let dir = temp_dir("instance");
        write(
            &dir,
            "Worlds/world_spawns.json",
            "\u{feff}[\n // spawn points\n\
             { \"Name\": \"instance_a\", \"SpawnPosition\": { \"ZoneId\": 10, \"X\": 796.3, \"Y\": 325.4, \"Z\": 249.5, \"Yaw\": 90.0 } },\n\
             { \"Name\": \"instance_a_boss\", \"SpawnPosition\": { \"ZoneId\": 11, \"X\": 1, \"Y\": 2, \"Z\": 3 } },\n\
             { \"Name\": \"elsewhere\", \"SpawnPosition\": { \"ZoneId\": 99, \"X\": 1, \"Y\": 2, \"Z\": 3 } }\n]",
        );
        write(
            &dir,
            "Worlds/instance_a/npc_spawns.json",
            r#"[ { "UnitId": 100, "Position": { "X": 10, "Y": 10, "Z": 5 } },
                 { "UnitId": 101, "Position": { "X": 20, "Y": 10, "Z": 5 } },
                 { "UnitId": 555, "Position": { "X": 30, "Y": 10, "Z": 5 } },
                 { "UnitId": 100, "NpcSpawnerIds": [3], "Position": { "X": 50, "Y": 10, "Z": 5 } } ]"#,
        );
        write(
            &dir,
            "Worlds/instance_a/doodad_spawns.json",
            r#"[ { "UnitId": 5723, "Position": { "X": 804.1, "Y": 327.7, "Z": 248.1 } },
                 { "UnitId": 5723, "Position": { "X": 739.4, "Y": 326.7, "Z": 166.2 } },
                 { "UnitId": 6000, "Position": { "X": 1, "Y": 1, "Z": 1 } },
                 { "UnitId": 6001, "Position": { "X": 2, "Y": 2, "Z": 2 } },
                 { "UnitId": 6002, "Position": { "X": 3, "Y": 3, "Z": 3 } } ]"#,
        );
        write(&dir, "Worlds/instance_b/npc_spawns.json", "[]");
        write(&dir, "Worlds/main_world/npc_spawns.json", "[]");
        write(&dir, "Worlds/readme.txt", "not a world");

        let conn = Connection::open_in_memory().unwrap();
        funcs::create_tables(&conn);
        conn.execute_batch(
            "CREATE TABLE zones (id INT, zone_key INT, name TEXT);
             INSERT INTO zones VALUES (1, 10, 'instance_a'), (2, 11, 'instance_a_boss'), (3, 12, 'instance_b'), (4, 13, 'main_world_0_0'), (5, 14, 'instance_a ');
             INSERT INTO doodad_func_groups VALUES (1, 5723, 1);
             INSERT INTO doodad_funcs VALUES (10, 1, 9, 'DoodadFuncExitIndun', 19754);
             INSERT INTO doodad_func_exit_induns VALUES (9, NULL);",
        )
        .unwrap();

        let npcs = IdMap::build(vec![npc(100, 20, 1, true), npc(101, 22, 3, false)]);
        let mut doodads = DoodadData::default();
        doodads.templates = IdMap::build(vec![
            doodad(5723, 0, 0),
            doodad(6000, 0, 0),   // decoration: dropped
            doodad(6001, 0, 777), // has a use skill
            doodad(6002, DoodadFlags::LOOT.0, 0),
        ]);
        let texts = Texts::empty();
        let skills = Skills::default();
        let spawns = SpawnIndex::default();
        let items = StrTable::default();
        let inp = TravelInputs {
            conn: &conn,
            texts: &texts,
            server_data: &dir,
            main_world: "main_world",
            npcs: &npcs,
            doodads: &doodads,
            skills: &skills,
            doodad_spawns: &spawns,
            items: &items,
        };
        let f = DoodadTravelFuncs::load(&conn).unwrap();
        let w = InstanceWorlds::load(&inp, &f).unwrap();

        // main_world and plain files are not instance worlds
        let all: Vec<&str> = w.all().iter().map(|w| &*w.name).collect();
        assert_eq!(all, vec!["instance_a", "instance_b"]);

        let a = w.by_name("instance_a").unwrap();
        assert_eq!(&*a.zone_keys, &[10, 11, 14]);
        assert_eq!(w.by_zone_key(11).map(|w| &*w.name), Some("instance_a"));
        assert_eq!(w.by_zone_key(12).map(|w| &*w.name), Some("instance_b"));
        assert!(w.by_zone_key(13).is_none() && w.by_zone_key(99).is_none());

        // 100 and 101 are static, 555 is unknown, the spawner entry is an event spawner
        let spawned: Vec<(u32, u8, u8, bool)> = a
            .npcs()
            .iter()
            .map(|n| (n.template, n.level, n.grade, n.aggressive))
            .collect();
        assert_eq!(spawned, vec![(100, 20, 1, true), (101, 22, 3, false)]);
        assert_eq!(a.npc_stats.unknown_template, 1);
        assert_eq!(a.npc_stats.event_spawners, 1);
        assert_eq!(a.npcs_with_grade(3).count(), 1);
        assert_eq!(a.npcs_with_grade(1).count(), 1);

        // exits split off, decorations dropped
        assert_eq!(a.exits().len(), 2);
        assert!(a
            .exits()
            .iter()
            .all(|e| e.template == 5723 && e.skill == 19754 && e.return_point == 0));
        let templates: Vec<u32> = a.doodads().iter().map(|d| d.template).collect();
        assert_eq!(templates, vec![6001, 6002]);
        assert_eq!(a.doodads()[0].skill, 777);
        assert_eq!(
            a.nearest_exit(Vec3::new(700.0, 320.0, 0.0)).map(|e| e.pos),
            Some(Vec3::new(739.4, 326.7, 166.2))
        );

        // spawn points: the world's own entry and the zone's, the foreign one is dropped
        assert_eq!(a.spawns.len(), 2);
        let s = a.spawn_for_zone(10).unwrap();
        assert_eq!((s.zone_key, s.yaw), (10, 90.0));
        assert_eq!(s.pos, Vec3::new(796.3, 325.4, 249.5));
        assert_eq!(
            a.spawn_for_zone(11).map(|s| &*s.name),
            Some("instance_a_boss")
        );
        assert_eq!(
            a.spawn_for_zone(14).map(|s| &*s.name),
            Some("instance_a"),
            "an unlisted zone falls back to the world's entry"
        );
        assert!(w
            .by_name("instance_b")
            .unwrap()
            .spawn_for_zone(12)
            .is_none());
        assert!(w.bytes() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_worlds_directory_is_no_worlds() {
        let dir = temp_dir("instance-none");
        let conn = Connection::open_in_memory().unwrap();
        funcs::create_tables(&conn);
        conn.execute_batch("CREATE TABLE zones (id INT, zone_key INT, name TEXT);")
            .unwrap();
        let npcs = IdMap::<NpcTemplate>::build(Vec::new());
        let doodads = DoodadData::default();
        let texts = Texts::empty();
        let skills = Skills::default();
        let spawns = SpawnIndex::default();
        let items = StrTable::default();
        let inp = TravelInputs {
            conn: &conn,
            texts: &texts,
            server_data: &dir,
            main_world: "main_world",
            npcs: &npcs,
            doodads: &doodads,
            skills: &skills,
            doodad_spawns: &spawns,
            items: &items,
        };
        let w = InstanceWorlds::load(&inp, &DoodadTravelFuncs::default()).unwrap();
        assert!(w.all().is_empty() && w.by_zone_key(1).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
