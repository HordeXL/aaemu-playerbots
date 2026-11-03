//! The doodad functions that move a character: entering an instance dungeon
//! (`DoodadFuncEnterInstance`), entering a system instance such as Mirage Isle
//! (`DoodadFuncEnterSysInstance`), leaving one (`DoodadFuncExitIndun`) and binding the recall
//! point (`DoodadFuncBinding`, the Memory Tome).
//!
//! A doodad's functions hang off its function groups; the skill a client casts on the doodad is
//! the function's own `func_skill_id`. The server runs the function whose skill the cast names
//! (`systems/npcs/doodad/funcs/world.rs`), so `(doodad, skill)` identifies one function.

use anyhow::Result;
use rusqlite::Connection;
use rustc_hash::FxHashMap;

use crate::sql::{each_row, RowExt};

/// `DoodadFuncEnterInstance` of a doodad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EnterFunc {
    pub doodad: u32,
    /// Skill the client casts on the doodad (0 when the function names none).
    pub skill: u32,
    /// `doodad_func_enter_instances.zone_id` (a `zones.id`, not a zone key).
    pub zone_id: u32,
}

/// `DoodadFuncEnterSysInstance` of a doodad (the portals to Mirage Isle and the other system
/// instances). Same shape as [`EnterFunc`]: the function names a `zones.id`.
pub(crate) type SysEnterFunc = EnterFunc;

/// `DoodadFuncExitIndun` of a doodad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExitFunc {
    pub doodad: u32,
    pub skill: u32,
    /// `doodad_func_exit_induns.return_point_id` (0 = back to the main-world position).
    pub return_point: u32,
}

/// `DoodadFuncBinding` of a doodad (Memory Tome).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BindFunc {
    pub doodad: u32,
    pub skill: u32,
    /// `doodad_func_bindings.district_id` (never 0 here).
    pub district: u32,
}

/// The travel functions of all doodads, in `(doodad, group kind, group, function)` order with
/// at most one entry per doodad and target (first one wins).
#[derive(Debug, Default)]
pub(crate) struct DoodadTravelFuncs {
    pub enters: Vec<EnterFunc>,
    /// System instance portals. Kept apart from `enters`: those are the instance dungeons the
    /// dungeon goal runs, a system instance is no dungeon to clear.
    pub sys_enters: Vec<SysEnterFunc>,
    pub exits: Vec<ExitFunc>,
    pub binds: Vec<BindFunc>,
}

impl DoodadTravelFuncs {
    pub fn load(conn: &Connection) -> Result<DoodadTravelFuncs> {
        let mut enter_zone: FxHashMap<u32, u32> = FxHashMap::default();
        each_row(
            conn,
            "SELECT id, zone_id FROM doodad_func_enter_instances",
            |r| {
                enter_zone.insert(r.u32(0), r.u32(1));
            },
        )?;
        // (a compact without the table has no system instance portals)
        let mut sys_zone: FxHashMap<u32, u32> = FxHashMap::default();
        let _ = each_row(
            conn,
            "SELECT id, zone_id FROM doodad_func_enter_sys_instances",
            |r| {
                sys_zone.insert(r.u32(0), r.u32(1));
            },
        );
        let mut exit_point: FxHashMap<u32, u32> = FxHashMap::default();
        each_row(
            conn,
            "SELECT id, return_point_id FROM doodad_func_exit_induns",
            |r| {
                exit_point.insert(r.u32(0), r.u32(1));
            },
        )?;
        let mut bind_district: FxHashMap<u32, u32> = FxHashMap::default();
        each_row(
            conn,
            "SELECT id, district_id FROM doodad_func_bindings",
            |r| {
                bind_district.insert(r.u32(0), r.u32(1));
            },
        )?;

        let mut out = DoodadTravelFuncs::default();
        each_row(
            conn,
            "SELECT g.doodad_almighty_id, f.actual_func_type, f.actual_func_id, f.func_skill_id \
             FROM doodad_funcs f JOIN doodad_func_groups g ON g.id = f.doodad_func_group_id \
             WHERE f.actual_func_type IN \
             ('DoodadFuncEnterInstance', 'DoodadFuncEnterSysInstance', 'DoodadFuncExitIndun', \
              'DoodadFuncBinding') \
             ORDER BY g.doodad_almighty_id, g.doodad_func_group_kind_id, g.id, f.id",
            |r| {
                let (doodad, actual, skill) = (r.u32(0), r.u32(2), r.u32(3));
                match r.text(1) {
                    "DoodadFuncEnterInstance" => {
                        if let Some(&zone_id) = enter_zone.get(&actual) {
                            push_unique(
                                &mut out.enters,
                                EnterFunc {
                                    doodad,
                                    skill,
                                    zone_id,
                                },
                                |a, b| a.doodad == b.doodad && a.zone_id == b.zone_id,
                                |e| &mut e.skill,
                            );
                        }
                    }
                    "DoodadFuncEnterSysInstance" => {
                        if let Some(&zone_id) = sys_zone.get(&actual) {
                            push_unique(
                                &mut out.sys_enters,
                                SysEnterFunc {
                                    doodad,
                                    skill,
                                    zone_id,
                                },
                                |a, b| a.doodad == b.doodad && a.zone_id == b.zone_id,
                                |e| &mut e.skill,
                            );
                        }
                    }
                    "DoodadFuncExitIndun" => {
                        if let Some(&return_point) = exit_point.get(&actual) {
                            push_unique(
                                &mut out.exits,
                                ExitFunc {
                                    doodad,
                                    skill,
                                    return_point,
                                },
                                |a, b| a.doodad == b.doodad,
                                |e| &mut e.skill,
                            );
                        }
                    }
                    _ => {
                        if let Some(&district) = bind_district.get(&actual).filter(|d| **d != 0) {
                            push_unique(
                                &mut out.binds,
                                BindFunc {
                                    doodad,
                                    skill,
                                    district,
                                },
                                |a, b| a.doodad == b.doodad,
                                |e| &mut e.skill,
                            );
                        }
                    }
                }
            },
        )?;
        Ok(out)
    }

    /// The exit function of a doodad template.
    pub fn exit_of(&self, doodad: u32) -> Option<&ExitFunc> {
        self.exits.iter().find(|e| e.doodad == doodad)
    }
}

/// Adds `item` unless an equal one (by `same`) exists; an existing one without a skill takes
/// the new one's skill.
fn push_unique<T: Copy>(
    list: &mut Vec<T>,
    item: T,
    same: impl Fn(&T, &T) -> bool,
    skill: impl Fn(&mut T) -> &mut u32,
) {
    let mut item = item;
    if let Some(existing) = list.iter_mut().find(|e| same(e, &item)) {
        if *skill(existing) == 0 {
            *skill(existing) = *skill(&mut item);
        }
    } else {
        list.push(item);
    }
}

#[cfg(test)]
pub(crate) fn create_tables(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE doodad_func_groups (id INT, doodad_almighty_id INT, doodad_func_group_kind_id INT);
         CREATE TABLE doodad_funcs (id INT, doodad_func_group_id INT, actual_func_id INT,
           actual_func_type TEXT, func_skill_id INT);
         CREATE TABLE doodad_func_enter_instances (id INT, zone_id INT, item_id INT);
         CREATE TABLE doodad_func_enter_sys_instances (id INT, zone_id INT, faction_id INT, selective NUM);
         CREATE TABLE doodad_func_exit_induns (id INT, return_point_id INT);
         CREATE TABLE doodad_func_bindings (id INT, district_id INT);",
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_exit_and_bind_functions_are_collected_once_per_target() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        conn.execute_batch(
            "INSERT INTO doodad_func_groups VALUES (1, 4288, 1), (2, 4288, 2), (3, 4535, 1), (4, 841, 1), (5, 900, 1);
             INSERT INTO doodad_funcs VALUES
               (10, 1, 8, 'DoodadFuncEnterInstance', NULL),
               (11, 2, 15, 'DoodadFuncEnterInstance', 17743),
               (12, 3, 2, 'DoodadFuncExitIndun', 16560),
               (13, 4, 3, 'DoodadFuncBinding', 14602),
               (14, 5, 4, 'DoodadFuncBinding', 14602),
               (15, 5, 99, 'DoodadFuncEnterInstance', 1);
             INSERT INTO doodad_func_enter_instances VALUES (8, 170, 0), (15, 170, 0);
             INSERT INTO doodad_func_exit_induns VALUES (2, NULL);
             INSERT INTO doodad_func_bindings VALUES (3, 15), (4, 0);",
        )
        .unwrap();
        let f = DoodadTravelFuncs::load(&conn).unwrap();
        // two enter functions of 4288 into zone 170 are one entry, and take the skill that exists
        assert_eq!(
            f.enters,
            vec![EnterFunc {
                doodad: 4288,
                skill: 17743,
                zone_id: 170
            }]
        );
        assert_eq!(
            f.exits,
            vec![ExitFunc {
                doodad: 4535,
                skill: 16560,
                return_point: 0
            }]
        );
        // district 0 binds nothing
        assert_eq!(
            f.binds,
            vec![BindFunc {
                doodad: 841,
                skill: 14602,
                district: 15
            }]
        );
        assert_eq!(f.exit_of(4535).map(|e| e.skill), Some(16560));
        assert!(f.exit_of(1).is_none());
        // (no system instance portals in this data)
        assert!(f.sys_enters.is_empty());
    }

    #[test]
    fn system_instance_portals_are_kept_apart_from_the_dungeon_entrances() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        conn.execute_batch(
            "INSERT INTO doodad_func_groups VALUES (1, 4894, 1), (2, 4288, 1);
             INSERT INTO doodad_funcs VALUES
               (10, 1, 1, 'DoodadFuncEnterSysInstance', 17829),
               (11, 2, 8, 'DoodadFuncEnterInstance', 17743);
             INSERT INTO doodad_func_enter_sys_instances VALUES (1, 183, NULL, 'f');
             INSERT INTO doodad_func_enter_instances VALUES (8, 170, 0);",
        )
        .unwrap();
        let f = DoodadTravelFuncs::load(&conn).unwrap();
        assert_eq!(
            f.sys_enters,
            vec![SysEnterFunc {
                doodad: 4894,
                skill: 17829,
                zone_id: 183
            }]
        );
        assert_eq!(f.enters.len(), 1);
        assert_eq!(f.enters[0].doodad, 4288);
    }

    #[test]
    fn a_compact_without_the_system_instance_table_still_loads() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn);
        conn.execute_batch("DROP TABLE doodad_func_enter_sys_instances;")
            .unwrap();
        let f = DoodadTravelFuncs::load(&conn).unwrap();
        assert!(f.sys_enters.is_empty());
    }
}
