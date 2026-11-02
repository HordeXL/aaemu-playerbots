//! Facts the compiled [`World`] does not carry, read once from `compact.sqlite3`:
//!
//! * the skill a client casts to use a doodad: `doodad_funcs.func_skill_id`, else the
//!   `fake_skill_id` of its `DoodadFuncFakeUse` or the `skill_id` of its `DoodadFuncUse` (the quest
//!   props such as signposts and levers only have the latter; `DoodadTemplate::use_skill` reads
//!   `func_skill_id` only and is 0 for them),
//! * the skill an item casts (`items.use_skill_id`), needed for "use the item" objectives,
//! * the unit requirements of quest start components ([`StartGates`]) and the mother factions.
//!
//! Only the doodads and items some quest objective names are kept. The tables are read once per
//! process and shared by all bots; if the file cannot be read the lookups are empty and the
//! objectives that need them count as unsupported.

use std::path::Path;
use std::sync::OnceLock;

use aa_world::{ObjKind, World};
use rusqlite::{Connection, OpenFlags};
use rustc_hash::{FxHashMap, FxHashSet};

use super::reqs::StartGates;

/// `unit_reqs.kind_id` of "the target is this NPC template".
const TARGET_NPC_KIND: i64 = 0x1B;
/// `unit_reqs.kind_id` of "a doodad of this template is within a range" (millimetres).
const DOODAD_RANGE_KIND: i64 = 0x14;
/// `unit_reqs.kind_id` of "the caster stands inside a quest sphere".
const AREA_SPHERE_KIND: i64 = 0x23;

/// Where an item skill has to be cast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Need {
    /// At an NPC of this template.
    Npc(u32),
    /// Within this many metres of a doodad of this template.
    Doodad(u32, f32),
    /// Inside a sphere of the quest.
    Sphere,
}

/// Skill lookups for quest doodads and quest items.
#[derive(Debug, Default)]
pub struct Extras {
    doodad_skill: FxHashMap<u32, u32>,
    item_skill: FxHashMap<u32, u32>,
    /// Where an item skill must be cast (`unit_reqs` of the skill).
    skill_needs: FxHashMap<u32, Vec<Need>>,
    gates: StartGates,
}

impl Extras {
    /// The skill that uses doodad template `doodad`, 0 when unknown.
    pub fn doodad_skill(&self, doodad: u32) -> u32 {
        self.doodad_skill.get(&doodad).copied().unwrap_or(0)
    }

    /// The skill that using item template `item` casts, 0 when unknown.
    pub fn item_skill(&self, item: u32) -> u32 {
        self.item_skill.get(&item).copied().unwrap_or(0)
    }

    /// Where the skill `skill` must be cast; empty when it can be cast anywhere.
    pub fn skill_needs(&self, skill: u32) -> &[Need] {
        self.skill_needs.get(&skill).map_or(&[], |v| &v[..])
    }

    /// The unit requirements of the start components of the quests.
    pub fn gates(&self) -> &StartGates {
        &self.gates
    }

    /// Builds lookups from literal tables (tests).
    #[cfg(test)]
    pub fn from_tables(
        doodads: impl IntoIterator<Item = (u32, u32)>,
        items: impl IntoIterator<Item = (u32, u32)>,
    ) -> Extras {
        Extras {
            doodad_skill: doodads.into_iter().collect(),
            item_skill: items.into_iter().collect(),
            skill_needs: FxHashMap::default(),
            gates: StartGates::default(),
        }
    }

    /// Reads the lookups for everything the quests of `world` name.
    pub fn load(compact: &Path, world: &World) -> rusqlite::Result<Extras> {
        let (doodads, items) = wanted(world);
        let conn = Connection::open_with_flags(
            compact,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA query_only = ON;")?;
        let mut out = Extras::read(&conn, &doodads, &items)?;
        match StartGates::read(&conn) {
            Ok(gates) => out.gates = gates,
            Err(e) => tracing::warn!(error = %e, "quest start requirements unavailable"),
        }
        Ok(out)
    }

    /// Reads the skills of `doodads` and `items` from an open database.
    pub fn read(
        conn: &Connection,
        doodads: &FxHashSet<u32>,
        items: &FxHashSet<u32>,
    ) -> rusqlite::Result<Extras> {
        let mut out = Extras::default();
        for (id, skill) in pairs(
            conn,
            "SELECT id, COALESCE(use_skill_id, 0) FROM items WHERE use_skill_id != 0",
        )? {
            if items.contains(&id) {
                out.item_skill.insert(id, skill);
            }
        }
        let item_skills: FxHashSet<u32> = out.item_skill.values().copied().collect();
        let mut stmt = conn.prepare(
            "SELECT owner_id, kind_id, COALESCE(value1, 0), COALESCE(value2, 0) FROM unit_reqs              WHERE owner_type = 'Skill' AND kind_id IN (?1, ?2, ?3) ORDER BY id",
        )?;
        let rows = stmt.query_map(
            [TARGET_NPC_KIND, DOODAD_RANGE_KIND, AREA_SPHERE_KIND],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?;
        for row in rows {
            let (skill, kind, v1, v2) = row?;
            let (Ok(skill), Ok(v1)) = (u32::try_from(skill), u32::try_from(v1)) else {
                continue;
            };
            if !item_skills.contains(&skill) {
                continue;
            }
            let need = match kind {
                TARGET_NPC_KIND => Need::Npc(v1),
                DOODAD_RANGE_KIND => Need::Doodad(v1, v2 as f32 / 1000.0),
                _ => Need::Sphere,
            };
            out.skill_needs.entry(skill).or_default().push(need);
        }
        drop(stmt);
        let fake: FxHashMap<u32, u32> = pairs(
            conn,
            "SELECT id, COALESCE(fake_skill_id, 0) FROM doodad_func_fake_uses",
        )?
        .into_iter()
        .collect();
        let uses: FxHashMap<u32, u32> = pairs(
            conn,
            "SELECT id, COALESCE(skill_id, 0) FROM doodad_func_uses",
        )?
        .into_iter()
        .collect();
        let mut stmt = conn.prepare(
            "SELECT g.doodad_almighty_id, f.actual_func_type, f.actual_func_id, \
             COALESCE(f.func_skill_id, 0) \
             FROM doodad_funcs f JOIN doodad_func_groups g ON g.id = f.doodad_func_group_id \
             ORDER BY g.doodad_almighty_id, g.doodad_func_group_kind_id, g.id, f.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (doodad, ty, actual, func_skill) = row?;
            let (Ok(doodad), Ok(actual)) = (u32::try_from(doodad), u32::try_from(actual)) else {
                continue;
            };
            if !doodads.contains(&doodad) || out.doodad_skill.contains_key(&doodad) {
                continue;
            }
            let skill = if func_skill > 0 {
                u32::try_from(func_skill).unwrap_or(0)
            } else {
                match ty.as_str() {
                    "DoodadFuncFakeUse" => fake.get(&actual).copied().unwrap_or(0),
                    "DoodadFuncUse" => uses.get(&actual).copied().unwrap_or(0),
                    _ => 0,
                }
            };
            if skill > 0 {
                out.doodad_skill.insert(doodad, skill);
            }
        }
        Ok(out)
    }
}

/// `(id, value)` rows of a two-column query, ids and values that do not fit `u32` skipped.
fn pairs(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<(u32, u32)>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, value) = row?;
        if let (Ok(id), Ok(value)) = (u32::try_from(id), u32::try_from(value)) {
            if value > 0 {
                out.push((id, value));
            }
        }
    }
    Ok(out)
}

/// The doodad templates and item templates some quest objective names.
fn wanted(world: &World) -> (FxHashSet<u32>, FxHashSet<u32>) {
    let mut doodads = FxHashSet::default();
    let mut items = FxHashSet::default();
    for q in world.quests() {
        for step in q.progress.iter() {
            for o in step.objectives.iter() {
                match &o.kind {
                    ObjKind::Interact { doodad, .. } => {
                        doodads.insert(*doodad);
                    }
                    ObjKind::UseItem { item, .. } => {
                        items.insert(*item);
                    }
                    ObjKind::Gather { item, .. } => {
                        doodads.extend(world.gather_doodads(*item).iter().copied());
                    }
                    _ => {}
                }
            }
        }
    }
    (doodads, items)
}

/// The process-wide lookups, loaded on first use from `compact`. Empty (and logged) when the file
/// cannot be read.
pub fn shared(compact: &Path, world: &World) -> &'static Extras {
    static EXTRAS: OnceLock<Extras> = OnceLock::new();
    EXTRAS.get_or_init(|| match Extras::load(compact, world) {
        Ok(e) => {
            tracing::info!(
                doodads = e.doodad_skill.len(),
                items = e.item_skill.len(),
                gated_quests = e.gates.len(),
                "quest extras loaded"
            );
            e
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %compact.display(),
                "quest extras unavailable; item and doodad objectives are unsupported"
            );
            Extras::default()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_default_to_zero() {
        let e = Extras::from_tables([(1451, 14907)], [(7738, 11596)]);
        assert_eq!(e.doodad_skill(1451), 14907);
        assert_eq!(e.item_skill(7738), 11596);
        assert_eq!(e.doodad_skill(1), 0);
        assert_eq!(e.item_skill(1), 0);
        assert_eq!(Extras::default().doodad_skill(1451), 0);
    }

    #[test]
    fn skills_come_from_the_func_then_the_fake_use_then_the_use_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INT, use_skill_id INT);
             CREATE TABLE doodad_func_groups (id INT, doodad_almighty_id INT, doodad_func_group_kind_id INT);
             CREATE TABLE doodad_funcs (id INT, doodad_func_group_id INT, actual_func_id INT,
               actual_func_type TEXT, func_skill_id INT);
             CREATE TABLE doodad_func_fake_uses (id INT, skill_id INT, fake_skill_id INT);
             CREATE TABLE doodad_func_uses (id INT, skill_id INT);
             CREATE TABLE unit_reqs (id INT, owner_id INT, owner_type TEXT, kind_id INT, value1 INT, value2 INT);
             INSERT INTO items VALUES (7738, 11596), (4058, NULL), (99, 5);
             INSERT INTO doodad_func_groups VALUES (1, 1451, 1), (2, 77, 1), (3, 88, 1), (4, 88, 2), (5, 55, 1);
             INSERT INTO doodad_funcs VALUES
               (10, 1, 842, 'DoodadFuncFakeUse', NULL),
               (11, 2, 5, 'DoodadFuncLootItem', 321),
               (12, 3, 6, 'DoodadFuncUse', NULL),
               (13, 4, 9, 'DoodadFuncFakeUse', NULL),
               (14, 5, 1, 'DoodadFuncQuest', NULL);
             INSERT INTO doodad_func_fake_uses VALUES (842, NULL, 14907), (9, NULL, 1);
             INSERT INTO doodad_func_uses VALUES (6, 555);
             INSERT INTO unit_reqs VALUES
               (1, 11596, 'Skill', 27, 3681, 0),
               (2, 11596, 'Skill', 32, 97, 0),
               (3, 5, 'Skill', 27, 99, 0),
               (4, 11596, 'QuestComponent', 27, 7, 0),
               (5, 11596, 'Skill', 20, 1430, 30000),
               (6, 11596, 'Skill', 35, 12, 0);",
        )
        .unwrap();
        let doodads: FxHashSet<u32> = [1451, 77, 88, 55].into_iter().collect();
        let items: FxHashSet<u32> = [7738, 4058].into_iter().collect();
        let e = Extras::read(&conn, &doodads, &items).unwrap();
        assert_eq!(e.doodad_skill(1451), 14907);
        assert_eq!(e.doodad_skill(77), 321);
        // the start group's Use row wins over the later group's fake use
        assert_eq!(e.doodad_skill(88), 555);
        assert_eq!(e.doodad_skill(55), 0);
        assert_eq!(e.item_skill(7738), 11596);
        assert_eq!(e.item_skill(4058), 0);
        // items nobody asked for are not kept
        assert_eq!(e.item_skill(99), 0);
        // where an item skill must be cast; other kinds and owners are ignored
        assert_eq!(
            e.skill_needs(11596),
            &[Need::Npc(3681), Need::Doodad(1430, 30.0), Need::Sphere]
        );
        assert!(e.skill_needs(5).is_empty());
        assert!(e.skill_needs(1).is_empty());
    }
}
