//! Tower defence events ("rifts"): where they are, what they ask the players to kill and for how
//! long they run.
//!
//! The server runs an event from four tables of `compact.sqlite3`: `tower_defs` (the event: when
//! the first wave comes, the time cap, the defended target), `tower_def_progs` (its waves, called
//! steps here: a timer and/or kill counts that start the next one), `tower_def_prog_spawn_targets`
//! (the spawners a wave activates) and `tower_def_prog_kill_targets` (the NPC templates, and how
//! many of each, the wave waits for). A spawner is a row of `npc_spawner_npcs`; for the events its
//! members are *controller* NPCs (grade 3, "Rift Rank N") that stand at authored positions of the
//! server's spawn files and summon the wave NPCs with their skills. Kills count for anybody in
//! the same world and zone group (G/systems/instances/tower_def.rs `on_death`), no party needed.
//!
//! [`Events`] joins these into one small record per definition: the place (the positions of the
//! defended target and of the wave controllers, with the zone group each lies in), the level range
//! (from the kill-target NPCs), the kill targets of every step and the time cap. It is read-only
//! after load and shared by all bots; about 5 KiB for the whole 1.2 data.
//!
//! The events of a zone group at war (`conflict_zones.war_tower_def_id`) are a PvP matter and are
//! marked [`EventDef::war_zone_group`]; the bots do not join them.
//!
//! `G/` is `rust/crates/aaemu-game/src/` of AAEmu-1.2-Rust.

use anyhow::Result;
use rusqlite::Connection;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::geom::{IdMap, Vec3};
use crate::npc::NpcTemplate;
use crate::spatial::SpawnIndex;
use crate::sql::{each_row, table_exists, RowExt};

/// One kill the next wave waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kill {
    /// NPC template to kill.
    pub template: u32,
    /// How many of them.
    pub count: u32,
}

/// One wave of an event (`tower_def_progs`).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// `tower_def_progs.id`.
    pub prog: u32,
    /// Seconds after the wave began that count as done for the next one (0 = no timer).
    pub timer_s: f32,
    /// All conditions (the timer and every kill target) must hold; otherwise any one does.
    pub all: bool,
    /// The kill targets.
    pub kills: Box<[Kill]>,
    /// The spawners the wave activates.
    pub spawners: Box<[u32]>,
}

impl Step {
    /// The server's `wave_complete` (G/systems/instances/tower_def.rs:`wave_complete`): whether
    /// the wave is done after `elapsed_s` seconds and `kills(template)` kills of each target.
    pub fn complete(&self, elapsed_s: f32, kills: impl Fn(u32) -> u32) -> bool {
        let timer = (self.timer_s > 0.0).then_some(elapsed_s >= self.timer_s);
        let counted = self.kills.iter().map(|k| kills(k.template) >= k.count);
        let mut conditions = timer.into_iter().chain(counted);
        if self.all {
            conditions.all(|v| v)
        } else {
            conditions.any(|v| v)
        }
    }

    /// Whether the wave has anything to wait for at all (a timer or a kill target).
    pub fn has_condition(&self) -> bool {
        self.timer_s > 0.0 || !self.kills.is_empty()
    }
}

/// A controller NPC of an event at one authored position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controller {
    /// The tower defence spawner (`npc_spawners.id`) the NPC belongs to.
    pub spawner: u32,
    /// The wave that activates it (1 is the first); 0 for the defended target, which stands from
    /// the start of the event.
    pub step: u8,
    /// NPC template.
    pub template: u32,
    /// Where it stands.
    pub pos: Vec3,
    /// The zone group of that position (0 when the zone grid is not baked).
    pub zone_group: u16,
}

/// One event definition (`tower_defs`).
#[derive(Clone, Debug, PartialEq)]
pub struct EventDef {
    /// `tower_defs.id`.
    pub id: u32,
    /// English title (the Korean name when there is none).
    pub name: Box<str>,
    /// Seconds from the start to the first wave (0 = at once).
    pub first_wave_after_s: f32,
    /// The server ends the event this many seconds after its start.
    pub force_end_s: f32,
    /// The spawner of the defended target.
    pub target_spawner: u32,
    /// The NPC (template and kills) whose death ends the event, `(0, 0)` when none does.
    pub end_kill: (u32, u32),
    /// The waves, in order.
    pub steps: Box<[Step]>,
    /// The defended target and the wave controllers, by step then position.
    pub controllers: Box<[Controller]>,
    /// The lowest level among the kill-target NPCs (0 when the waves name none).
    pub min_level: u8,
    /// The highest level among the kill-target NPCs (0 when the waves name none).
    pub max_level: u8,
    /// The zone group whose war this event is (`conflict_zones.war_tower_def_id` and
    /// `peace_tower_def_id`).
    pub war_zone_group: Option<u16>,
}

impl EventDef {
    /// Wave `step` (counted from 1).
    pub fn step(&self, step: u32) -> Option<&Step> {
        self.steps.get((step as usize).checked_sub(1)?)
    }

    /// Whether `template` is something wave `step` waits for.
    pub fn is_kill_target(&self, step: u32, template: u32) -> bool {
        self.step(step)
            .is_some_and(|s| s.kills.iter().any(|k| k.template == template))
    }

    /// Whether `template` is a kill target of any wave: what the event's monsters are.
    pub fn is_wave_npc(&self, template: u32) -> bool {
        self.steps
            .iter()
            .any(|s| s.kills.iter().any(|k| k.template == template))
    }

    /// Whether `template` is one of the event's own controllers (never to be attacked).
    pub fn is_controller(&self, template: u32) -> bool {
        self.controllers.iter().any(|c| c.template == template)
    }

    /// The controllers of wave `step` (0 for the defended target) in `zone_group`; all of them
    /// when the zone grid is not baked or none lies in the group.
    fn at(&self, step: u8, zone_group: u16) -> impl Iterator<Item = Vec3> + '_ {
        let any_in_group = self
            .controllers
            .iter()
            .any(|c| c.step == step && c.zone_group == zone_group);
        self.controllers
            .iter()
            .filter(move |c| c.step == step && (!any_in_group || c.zone_group == zone_group))
            .map(|c| c.pos)
    }

    /// The defended target's positions in `zone_group`.
    pub fn target_positions(&self, zone_group: u16) -> impl Iterator<Item = Vec3> + '_ {
        self.at(0, zone_group)
    }

    /// The position of the defended target in `zone_group` nearest to `from`.
    pub fn nearest_target(&self, zone_group: u16, from: Vec3) -> Option<Vec3> {
        self.target_positions(zone_group)
            .min_by(|a, b| a.dist2d_sq(from).total_cmp(&b.dist2d_sq(from)))
    }

    /// The positions of the controllers of wave `step` (counted from 1, like the server) in
    /// `zone_group`.
    pub fn wave_positions(&self, step: u32, zone_group: u16) -> impl Iterator<Item = Vec3> + '_ {
        self.at(u8::try_from(step).unwrap_or(u8::MAX), zone_group)
    }

    /// Whether the bots may join: a normal event with something to kill (not a war).
    pub fn joinable(&self) -> bool {
        self.war_zone_group.is_none() && self.min_level > 0
    }

    /// Whether the event ends by itself without a kill count: only the time cap ends it.
    pub fn ends_by_time(&self) -> bool {
        self.end_kill.1 == 0
    }
}

/// All event definitions. See the module docs.
#[derive(Debug, Default)]
pub struct Events {
    /// Sorted by id.
    defs: Vec<EventDef>,
    /// The templates of every controller of every event.
    controllers: FxHashSet<u32>,
}

impl Events {
    /// Reads the event tables. `zone_group_at(x, y)` names the zone group of a position (0 when
    /// unknown); a database without the tables gives no events.
    pub(crate) fn load(
        conn: &Connection,
        npcs: &IdMap<NpcTemplate>,
        npc_spawns: &SpawnIndex,
        zone_group_at: &dyn Fn(f32, f32) -> u16,
    ) -> Result<Events> {
        if !table_exists(conn, "tower_defs")? {
            return Ok(Events::default());
        }
        let titles = titles(conn)?;
        let members = members(conn)?;
        let wars = wars(conn)?;
        let mut progs: FxHashMap<u32, Vec<Step>> = FxHashMap::default();
        let mut prog_order: Vec<(u32, u32)> = Vec::new();
        each_row(
            conn,
            "SELECT id, tower_def_id, cond_to_next_time, cond_comp_by_and FROM tower_def_progs \
             ORDER BY id",
            |r| {
                prog_order.push((r.u32(1), r.u32(0)));
                progs.entry(r.u32(1)).or_default().push(Step {
                    prog: r.u32(0),
                    timer_s: r.f32(2),
                    all: r.flag(3),
                    kills: Box::default(),
                    spawners: Box::default(),
                });
            },
        )?;
        let mut kills: FxHashMap<u32, Vec<Kill>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT tower_def_prog_id, kill_target_id, kill_count FROM tower_def_prog_kill_targets \
             WHERE kill_target_type = 'Npc' ORDER BY id",
            |r| {
                kills.entry(r.u32(0)).or_default().push(Kill {
                    template: r.u32(1),
                    count: r.u32(2),
                });
            },
        )?;
        let mut spawners: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT tower_def_prog_id, spawn_target_id FROM tower_def_prog_spawn_targets \
             WHERE spawn_target_type = 'NpcSpawner' ORDER BY id",
            |r| spawners.entry(r.u32(0)).or_default().push(r.u32(1)),
        )?;

        let mut defs = Vec::new();
        each_row(
            conn,
            "SELECT id, name, first_wave_after, target_npc_spawner_id, kill_npc_id, \
             kill_npc_count, force_end_time FROM tower_defs ORDER BY id",
            |r| {
                let id = r.u32(0);
                let mut steps = progs.remove(&id).unwrap_or_default();
                for s in &mut steps {
                    s.kills = kills.remove(&s.prog).unwrap_or_default().into();
                    s.spawners = spawners.remove(&s.prog).unwrap_or_default().into();
                }
                let target_spawner = r.u32(3);
                let mut controllers = Vec::new();
                let mut add = |spawner: u32, step: u8| {
                    for &template in members.get(&spawner).into_iter().flatten() {
                        for &pos in npc_spawns.of(template) {
                            controllers.push(Controller {
                                spawner,
                                step,
                                template,
                                pos,
                                zone_group: zone_group_at(pos.x, pos.y),
                            });
                        }
                    }
                };
                add(target_spawner, 0);
                for (i, s) in steps.iter().enumerate() {
                    for &sp in &s.spawners {
                        add(sp, u8::try_from(i + 1).unwrap_or(u8::MAX));
                    }
                }
                let levels = steps
                    .iter()
                    .flat_map(|s| s.kills.iter())
                    .filter_map(|k| npcs.get(k.template).map(|n| n.level))
                    .filter(|&l| l > 0);
                let (min_level, max_level) =
                    levels.fold((u8::MAX, 0u8), |(lo, hi), l| (lo.min(l), hi.max(l)));
                let name = titles
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| r.text(1).to_owned());
                defs.push(EventDef {
                    id,
                    name: name.into(),
                    first_wave_after_s: r.f32(2),
                    force_end_s: r.f32(6),
                    target_spawner,
                    end_kill: (r.u32(4), r.u32(5)),
                    steps: steps.into(),
                    controllers: controllers.into(),
                    min_level: if max_level == 0 { 0 } else { min_level },
                    max_level,
                    war_zone_group: wars.get(&id).copied(),
                });
            },
        )?;
        let controllers = defs
            .iter()
            .flat_map(|d| d.controllers.iter().map(|c| c.template))
            .collect();
        Ok(Events { defs, controllers })
    }

    /// Definition `id`.
    pub fn get(&self, id: u32) -> Option<&EventDef> {
        self.defs
            .binary_search_by_key(&id, |d| d.id)
            .ok()
            .map(|i| &self.defs[i])
    }

    /// All definitions, by id.
    pub fn all(&self) -> &[EventDef] {
        &self.defs
    }

    /// Whether `template` is the controller NPC of some event.
    pub fn is_controller(&self, template: u32) -> bool {
        self.controllers.contains(&template)
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.defs
            .iter()
            .map(|d| {
                std::mem::size_of::<EventDef>()
                    + d.name.len()
                    + d.steps
                        .iter()
                        .map(|s| {
                            std::mem::size_of::<Step>()
                                + s.kills.len() * std::mem::size_of::<Kill>()
                                + s.spawners.len() * 4
                        })
                        .sum::<usize>()
                    + d.controllers.len() * std::mem::size_of::<Controller>()
            })
            .sum::<usize>()
            + self.controllers.capacity() * 8
    }
}

/// The English titles of the events (`localized_texts`, `tower_defs.title_msg`).
fn titles(conn: &Connection) -> Result<FxHashMap<u32, String>> {
    let mut out = FxHashMap::default();
    if !table_exists(conn, "localized_texts")? {
        return Ok(out);
    }
    each_row(
        conn,
        "SELECT idx, en_us FROM localized_texts WHERE tbl_name = 'tower_defs' \
         AND tbl_column_name = 'title_msg'",
        |r| {
            let t = r.text(1).trim();
            if !t.is_empty() {
                out.insert(r.u32(0), t.to_owned());
            }
        },
    )?;
    Ok(out)
}

/// `spawner id -> member NPC templates` of the spawners that are members lists of NPCs.
fn members(conn: &Connection) -> Result<FxHashMap<u32, Vec<u32>>> {
    let mut out: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    if !table_exists(conn, "npc_spawner_npcs")? {
        return Ok(out);
    }
    each_row(
        conn,
        "SELECT npc_spawner_id, member_id FROM npc_spawner_npcs WHERE member_type = 'Npc' \
         AND npc_spawner_id IN (SELECT target_npc_spawner_id FROM tower_defs \
         UNION SELECT spawn_target_id FROM tower_def_prog_spawn_targets \
         WHERE spawn_target_type = 'NpcSpawner')",
        |r| out.entry(r.u32(0)).or_default().push(r.u32(1)),
    )?;
    Ok(out)
}

/// `tower def id -> zone group` of the events that are a zone group's war or peace
/// (`conflict_zones`).
fn wars(conn: &Connection) -> Result<FxHashMap<u32, u16>> {
    let mut out = FxHashMap::default();
    if !table_exists(conn, "conflict_zones")? {
        return Ok(out);
    }
    each_row(
        conn,
        "SELECT zone_group_id, war_tower_def_id, peace_tower_def_id FROM conflict_zones",
        |r| {
            for col in [1, 2] {
                let def = r.u32(col);
                if def != 0 {
                    out.insert(def, r.u16(0));
                }
            }
        },
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::IdMap;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tower_defs (id INT, name TEXT, first_wave_after REAL,
                target_npc_spawner_id INT, kill_npc_id INT, kill_npc_count INT,
                force_end_time REAL);
             CREATE TABLE tower_def_progs (id INT, tower_def_id INT, cond_to_next_time REAL,
                cond_comp_by_and TEXT);
             CREATE TABLE tower_def_prog_spawn_targets (id INT, tower_def_prog_id INT,
                spawn_target_id INT, spawn_target_type TEXT, despawn_on_next_step TEXT);
             CREATE TABLE tower_def_prog_kill_targets (id INT, tower_def_prog_id INT,
                kill_target_id INT, kill_target_type TEXT, kill_count INT);
             CREATE TABLE npc_spawner_npcs (id INT, npc_spawner_id INT, member_id INT,
                member_type TEXT, weight REAL);
             CREATE TABLE conflict_zones (zone_group_id INT, war_tower_def_id INT,
                peace_tower_def_id INT);
             CREATE TABLE localized_texts (tbl_name TEXT, tbl_column_name TEXT, idx INT,
                en_us TEXT);
             INSERT INTO tower_defs VALUES (3, 'rift', 0, 100, NULL, 0, 3600),
                                           (18, 'war', 300, 200, NULL, 1, 4800);
             INSERT INTO tower_def_progs VALUES (1, 3, 170, 't'), (2, 3, 0, 't'), (3, 3, 0, 'f'),
                                                (9, 18, 0, 't');
             INSERT INTO tower_def_prog_spawn_targets VALUES (1, 2, 101, 'NpcSpawner', 'f'),
                (2, 3, 102, 'NpcSpawner', 'f'), (3, 3, 55, 'DoodadAlmighty', 'f');
             INSERT INTO tower_def_prog_kill_targets VALUES (1, 2, 500, 'Npc', 10),
                (2, 2, 501, 'Npc', 5), (3, 3, 502, 'Npc', 2), (4, 3, 503, 'Doodad', 9);
             INSERT INTO npc_spawner_npcs VALUES (1, 100, 900, 'Npc', 1), (2, 101, 901, 'Npc', 1),
                (3, 102, 902, 'Npc', 1), (4, 102, 77, 'Doodad', 1), (5, 200, 903, 'Npc', 1);
             INSERT INTO conflict_zones VALUES (22, 18, NULL), (30, NULL, NULL);
             INSERT INTO localized_texts VALUES ('tower_defs', 'title_msg', 3, 'Crimson Rift'),
                ('tower_defs', 'end_msg', 3, 'closed');",
        )
        .unwrap();
        conn
    }

    fn npc(id: u32, level: u8) -> (u32, NpcTemplate) {
        (
            id,
            NpcTemplate {
                id,
                name: format!("npc {id}").into(),
                level,
                faction_id: 115,
                kind: 1,
                grade: 1,
                aggressive: true,
                roles: crate::npc::NpcRoles::NONE,
                is_quest_giver: false,
            },
        )
    }

    fn load(conn: &Connection) -> Events {
        let npcs = IdMap::build(vec![npc(500, 40), npc(501, 42), npc(502, 45), npc(903, 50)]);
        let spawns = SpawnIndex::build(vec![
            (900, Vec3::new(1000.0, 1000.0, 10.0)),
            (900, Vec3::new(1010.0, 1000.0, 10.0)),
            (900, Vec3::new(9000.0, 9000.0, 10.0)),
            (901, Vec3::new(1500.0, 1000.0, 10.0)),
            (902, Vec3::new(500.0, 1000.0, 10.0)),
            (902, Vec3::new(9500.0, 9000.0, 10.0)),
            (903, Vec3::new(3000.0, 3000.0, 10.0)),
        ]);
        let group = |x: f32, _y: f32| if x < 5000.0 { 20 } else { 22 };
        Events::load(conn, &npcs, &spawns, &group).unwrap()
    }

    #[test]
    fn an_event_is_joined_from_its_tables() {
        let ev = load(&fixture());
        assert_eq!(ev.all().len(), 2);
        let d = ev.get(3).unwrap();
        assert_eq!(&*d.name, "Crimson Rift");
        assert_eq!((d.first_wave_after_s, d.force_end_s), (0.0, 3600.0));
        assert_eq!(d.target_spawner, 100);
        assert!(d.ends_by_time());
        assert_eq!(d.steps.len(), 3);
        assert_eq!(d.steps[0].timer_s, 170.0);
        assert!(d.steps[0].has_condition() && d.steps[0].spawners.is_empty());
        assert_eq!(d.steps[1].spawners.as_ref(), [101]);
        assert_eq!(
            d.steps[1].kills.as_ref(),
            [
                Kill {
                    template: 500,
                    count: 10
                },
                Kill {
                    template: 501,
                    count: 5
                }
            ]
        );
        // a doodad kill target and a doodad spawner are not NPC work
        assert_eq!(d.steps[2].kills.len(), 1);
        assert_eq!(d.steps[2].spawners.as_ref(), [102]);
        assert!(!d.steps[2].all);
        // levels come from the kill-target NPCs of every step
        assert_eq!((d.min_level, d.max_level), (40, 45));
        assert!(d.joinable());
        // the name of an event without a title is the database name
        assert_eq!(&*ev.get(18).unwrap().name, "war");
        assert!(ev.get(4).is_none());
    }

    #[test]
    fn places_come_from_the_controllers_and_their_zone_group() {
        let ev = load(&fixture());
        let d = ev.get(3).unwrap();
        // the target: three positions of template 900, two of them in group 20
        let near = d
            .nearest_target(20, Vec3::new(1011.0, 1001.0, 0.0))
            .unwrap();
        assert_eq!((near.x, near.y), (1010.0, 1000.0));
        assert_eq!(d.target_positions(20).count(), 2);
        assert_eq!(d.target_positions(22).count(), 1);
        // a group without a position falls back to all of them
        assert_eq!(d.target_positions(99).count(), 3);
        // step numbers are the server's: the place in the list of waves, counted from 1
        assert_eq!(d.wave_positions(2, 20).count(), 1);
        assert_eq!(d.wave_positions(3, 22).count(), 1);
        assert_eq!(d.wave_positions(3, 20).count(), 1);
        assert_eq!(d.wave_positions(1, 20).count(), 0);
        assert!(d.is_controller(901) && d.is_controller(900));
        assert!(!d.is_controller(500));
        assert!(ev.is_controller(903));
        assert!(!ev.is_controller(500));
    }

    #[test]
    fn kill_targets_and_wave_npcs() {
        let ev = load(&fixture());
        let d = ev.get(3).unwrap();
        assert!(d.is_kill_target(2, 500) && d.is_kill_target(2, 501));
        assert!(!d.is_kill_target(3, 500));
        assert!(!d.is_kill_target(1, 500));
        assert!(!d.is_kill_target(0, 500) && !d.is_kill_target(9, 500));
        assert!(d.is_wave_npc(502) && !d.is_wave_npc(900));
        assert!(d.step(0).is_none() && d.step(4).is_none());
        assert_eq!(d.step(2).unwrap().prog, 2);
    }

    #[test]
    fn a_wave_completes_like_the_server_says() {
        let ev = load(&fixture());
        let d = ev.get(3).unwrap();
        // step 1: only a timer
        let s1 = d.step(1).unwrap();
        assert!(!s1.complete(169.9, |_| 0));
        assert!(s1.complete(170.0, |_| 0));
        // step 2: all kills (and no timer)
        let s2 = d.step(2).unwrap();
        let kills = |a, b| {
            move |t| {
                if t == 500 {
                    a
                } else if t == 501 {
                    b
                } else {
                    0
                }
            }
        };
        assert!(!s2.complete(1e6, kills(10, 4)));
        assert!(s2.complete(0.0, kills(10, 5)));
        assert!(s2.complete(0.0, kills(11, 99)));
        // step 3: a single target of an "or" wave
        assert!(d
            .step(3)
            .unwrap()
            .complete(0.0, |t| if t == 502 { 2 } else { 0 }));
        assert!(!d.step(3).unwrap().complete(0.0, |_| 1));
    }

    #[test]
    fn wars_are_not_joinable() {
        let ev = load(&fixture());
        let w = ev.get(18).unwrap();
        assert_eq!(w.war_zone_group, Some(22));
        assert!(!w.joinable());
        assert_eq!(w.end_kill, (0, 1));
        assert!(!w.ends_by_time());
        assert_eq!(ev.get(3).unwrap().war_zone_group, None);
    }

    #[test]
    fn a_database_without_the_tables_has_no_events() {
        let conn = Connection::open_in_memory().unwrap();
        let ev = Events::load(
            &conn,
            &IdMap::build(vec![]),
            &SpawnIndex::build(vec![]),
            &|_, _| 0,
        )
        .unwrap();
        assert!(ev.all().is_empty());
        assert!(!ev.is_controller(1));
        assert_eq!(ev.bytes(), 0);
    }

    #[test]
    fn the_size_is_small() {
        let ev = load(&fixture());
        assert!(ev.bytes() < 4096, "{}", ev.bytes());
    }
}
