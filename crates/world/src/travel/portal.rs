//! Portals: the Teleport Book, the recall (Memory Tome) and the places they lead to.
//!
//! How the server (`aaemu-game/src/systems/npcs/portal/`) works, as far as a bot needs it:
//!
//! * **Recall points** (`Data/Portal/recalls.json`, `Id == ReturnPointId`): a character's
//!   Teleport Book lists the recall points of the sub zones it has visited (`CSNotifySubZone`,
//!   `SubZoneId`), plus private entries it recorded itself (skill 11215, one Memory Ink).
//! * **Open a portal** (Teleport Book 4045, skill 11216, 10 s cast): `OpenPortalEffect` consumes
//!   one reagent stone ([`Portals::reagents`]: same continent = the `inland` table, otherwise the
//!   `outland` table; the first the character can afford) and summons the entrance (NPC 3891) at
//!   the caster and the exit (NPC 6949) at the book entry; both vanish after 30 s and a character
//!   steps through by using the entrance (`mod.rs:39-43`, `open_portal` / `make_portal`).
//! * **Recall** (skill 10594, 10 s cast, 30 min cooldown): the `Return` special effect with
//!   `value1 == 0` takes the character to the recall point of its *bound district*
//!   (`district_return_points[district, faction]`). The district is bound by using a Memory Tome
//!   doodad (`DoodadFuncBinding`, skill 14602, [`MemoryTome`]).
//! * **World gates** (`worldgates.json`) are destinations of `Return` effects with `value1 != 0`
//!   (quest and event teleports, mostly), **respawn points** (`respawns.json`) are where the
//!   server resurrects a dead character (the nearest allowed one).

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rustc_hash::FxHashSet;

use super::funcs::DoodadTravelFuncs;
use super::{
    localized, member, member_f32, member_str, member_u32, read_object_array, str_bytes,
    warn_missing, SkillTiming, TravelInputs,
};
use crate::geom::Vec3;
use crate::sql::{each_row, RowExt};

/// Teleport Book (item): its use skill opens a portal.
pub const TELEPORT_BOOK_ITEM: u32 = 4045;
/// "Opening the gate": the Teleport Book's use skill (`OpenPortalEffect`).
pub const OPEN_PORTAL_SKILL: u32 = 11216;
/// "Recording the position in the Teleport Book": adds a private entry; reagent Memory Ink.
pub const RECORD_PORTAL_SKILL: u32 = 11215;
/// Memory Ink (item): the reagent of [`RECORD_PORTAL_SKILL`] (`skill_reagents`).
pub const MEMORY_INK_ITEM: u32 = 4053;
/// "Editing a Teleport Book record": renames a private entry (`CSChangePortalName`).
pub const RENAME_PORTAL_SKILL: u32 = 16841;
/// Recall ("Faint memory of space"): back to the bound recall point.
pub const RECALL_SKILL: u32 = 10594;
/// "Recording the position in a Memory Tome": the skill cast on a Memory Tome doodad.
pub const BIND_SKILL: u32 = 14602;
/// Bound Hereafter Stone, the first reagent of an open portal.
pub const BOUND_STONE_ITEM: u32 = 417;
/// Hereafter Stone, the second reagent of an open portal.
pub const STONE_ITEM: u32 = 4052;
/// NPC template of the green portal entrance (`PORTAL_ENTRANCE_TEMPLATE_ID`, `mod.rs:39`).
pub const PORTAL_ENTRANCE_NPC: u32 = 3891;
/// NPC template of the yellow portal exit (`PORTAL_EXIT_TEMPLATE_ID`, `mod.rs:41`).
pub const PORTAL_EXIT_NPC: u32 = 6949;
/// How long an open portal stays (`PORTAL_LIFETIME`, `mod.rs:43`), seconds.
pub const PORTAL_LIFETIME_SECS: u32 = 30;
/// Extra metres the server allows on the gate point of an open portal
/// (`OPEN_PORTAL_RANGE_SLACK`), on top of [`PortalKit::open_distance`].
pub const OPEN_PORTAL_RANGE_SLACK: f32 = 2.0;

/// The fixed items, skills and NPCs of the portal system, with the timings from the database.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PortalKit {
    /// Teleport Book item ([`TELEPORT_BOOK_ITEM`]).
    pub book_item: u32,
    /// Opens a portal (the book's use skill).
    pub open: SkillTiming,
    /// Records the current position as a private book entry.
    pub record: SkillTiming,
    /// Renames a private book entry.
    pub rename: SkillTiming,
    /// Recall.
    pub recall: SkillTiming,
    /// Bind the recall district at a Memory Tome doodad.
    pub bind: SkillTiming,
    /// Memory Ink item, consumed by [`PortalKit::record`] (0 when the data names none).
    pub ink_item: u32,
    /// Memory Ink consumed per record.
    pub ink_amount: u32,
    /// Entrance NPC template of an open portal.
    pub entrance_npc: u32,
    /// Exit NPC template of an open portal.
    pub exit_npc: u32,
    /// `open_portal_effects.distance`: metres in front of the caster the gate point may be
    /// (the server adds [`OPEN_PORTAL_RANGE_SLACK`]).
    pub open_distance: f32,
}

/// One reagent an open portal can consume (`open_portal_{inland,outland}_reagents`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalReagent {
    /// Item template.
    pub item: u32,
    /// How many are consumed.
    pub amount: u32,
    /// `priority` column (the server tries the rows in table order).
    pub priority: u32,
}

/// A recall point (`recalls.json`); also a Teleport Book entry for visited sub zones.
#[derive(Clone, Debug, PartialEq)]
pub struct RecallPoint {
    /// `Id`, which is also the return point id `district_return_points` names.
    pub id: u32,
    /// English name (`localized_texts` `return_points`, else the file's name).
    pub name: Box<str>,
    /// Zone key of the destination.
    pub zone_key: u16,
    /// Position.
    pub pos: Vec3,
    /// `ZRot`, degrees.
    pub z_rot: f32,
    /// The sub zone whose visit adds this entry to the book.
    pub sub_zone: u32,
}

/// A respawn point (`respawns.json`).
#[derive(Clone, Debug, PartialEq)]
pub struct RespawnPoint {
    /// `Id`.
    pub id: u32,
    /// Name, e.g. "Respawn: Southern White Arden".
    pub name: Box<str>,
    /// Position.
    pub pos: Vec3,
    /// `SubZoneId`.
    pub sub_zone: u32,
}

/// A world gate (`worldgates.json`), the destination of `Return` effects with a gate id.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldGate {
    /// `Id`, the `value1` of the `Return` special effect.
    pub id: u32,
    /// Name.
    pub name: Box<str>,
    /// Zone key of the destination.
    pub zone_key: u16,
    /// Position.
    pub pos: Vec3,
    /// Facing, degrees.
    pub yaw: f32,
    /// Skills whose `Return` effect leads here.
    pub skills: Box<[u32]>,
}

/// A Memory Tome doodad spawn in the main world: using it with [`BIND_SKILL`] binds the recall
/// district.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryTome {
    /// Doodad template.
    pub doodad: u32,
    /// Skill to cast on it (the function's own skill, normally [`BIND_SKILL`]).
    pub skill: u32,
    /// The district it binds; see [`Portals::recall_target`].
    pub district: u32,
    /// Where it stands.
    pub pos: Vec3,
}

/// Portal data.
#[derive(Debug, Default)]
pub struct Portals {
    kit: PortalKit,
    recalls: Vec<RecallPoint>,
    respawns: Vec<RespawnPoint>,
    /// Sorted by id.
    gates: Vec<WorldGate>,
    inland: Box<[PortalReagent]>,
    outland: Box<[PortalReagent]>,
    tomes: Vec<MemoryTome>,
    /// `(district, faction, return point)` of the districts a Memory Tome binds, sorted.
    tome_returns: Vec<(u32, u32, u32)>,
    /// `(zone key, zone_groups.target_id)` sorted: the "continent" the reagent choice uses.
    continents: Vec<(u16, u8)>,
}

impl Portals {
    pub(crate) fn load(inp: &TravelInputs<'_>, funcs: &DoodadTravelFuncs) -> Result<Portals> {
        let conn = inp.conn;
        let dir = inp.server_data.join("Portal");

        let recall_names = localized(conn, "return_points", "name");
        let recalls = load_recalls(&dir.join("recalls.json"), |id| {
            recall_names.get(&id).map(|n| &**n)
        })?;
        let respawns = load_respawns(&dir.join("respawns.json"))?;
        let mut gates = load_gates(&dir.join("worldgates.json"))?;
        attach_gate_skills(conn, &mut gates).context("read Return effects")?;

        let inland = load_reagents(conn, "open_portal_inland_reagents")?;
        let outland = load_reagents(conn, "open_portal_outland_reagents")?;
        let kit = load_kit(inp, &inland, &outland)?;

        // Memory Tomes: every spawn of a doodad with a binding function.
        let mut tomes = Vec::new();
        for f in &funcs.binds {
            let skill = if f.skill != 0 {
                f.skill
            } else {
                inp.doodads
                    .templates
                    .get(f.doodad)
                    .map_or(0, |d| d.use_skill)
            };
            if skill == 0 {
                continue;
            }
            for &pos in inp.doodad_spawns.of(f.doodad) {
                tomes.push(MemoryTome {
                    doodad: f.doodad,
                    skill,
                    district: f.district,
                    pos,
                });
            }
        }
        let districts: FxHashSet<u32> = tomes.iter().map(|t| t.district).collect();
        let mut tome_returns = Vec::new();
        each_row(
            conn,
            "SELECT district_id, faction_id, return_point_id FROM district_return_points",
            |r| {
                if districts.contains(&r.u32(0)) {
                    tome_returns.push((r.u32(0), r.u32(1), r.u32(2)));
                }
            },
        )
        .context("read district_return_points")?;
        tome_returns.sort_unstable();
        tome_returns.dedup();

        let mut continents = Vec::new();
        each_row(
            conn,
            "SELECT z.zone_key, g.target_id FROM zones z JOIN zone_groups g ON g.id = z.group_id",
            |r| continents.push((r.u16(0), r.u8(1))),
        )
        .context("read zone continents")?;
        continents.sort_unstable();
        continents.dedup_by_key(|c| c.0);

        Ok(Portals {
            kit,
            recalls,
            respawns,
            gates,
            inland,
            outland,
            tomes,
            tome_returns,
            continents,
        })
    }

    /// The fixed portal items / skills / NPCs.
    pub fn kit(&self) -> &PortalKit {
        &self.kit
    }

    /// Recall points in file order.
    pub fn recall_points(&self) -> &[RecallPoint] {
        &self.recalls
    }

    /// The recall point with this id (the first when the file repeats an id).
    pub fn recall(&self, id: u32) -> Option<&RecallPoint> {
        self.recalls.iter().find(|r| r.id == id)
    }

    /// The recall point nearest to `pos` (horizontal distance).
    pub fn nearest_recall(&self, pos: Vec3) -> Option<&RecallPoint> {
        self.recalls
            .iter()
            .min_by(|a, b| a.pos.dist2d_sq(pos).total_cmp(&b.pos.dist2d_sq(pos)))
    }

    /// Recall points whose sub zone is `sub_zone` (the entries a visit adds to the book).
    pub fn recalls_in_sub_zone(&self, sub_zone: u32) -> impl Iterator<Item = &RecallPoint> {
        self.recalls.iter().filter(move |r| r.sub_zone == sub_zone)
    }

    /// Recall points within `radius` metres (horizontal) of `pos`.
    pub fn recalls_within(&self, pos: Vec3, radius: f32) -> impl Iterator<Item = &RecallPoint> {
        let r2 = radius * radius;
        self.recalls
            .iter()
            .filter(move |r| r.pos.dist2d_sq(pos) <= r2)
    }

    /// Respawn points.
    pub fn respawn_points(&self) -> &[RespawnPoint] {
        &self.respawns
    }

    /// The respawn point nearest to `pos` (horizontal); the server resurrects at the nearest
    /// one the character may use.
    pub fn nearest_respawn(&self, pos: Vec3) -> Option<&RespawnPoint> {
        self.respawns
            .iter()
            .min_by(|a, b| a.pos.dist2d_sq(pos).total_cmp(&b.pos.dist2d_sq(pos)))
    }

    /// World gates ordered by id.
    pub fn world_gates(&self) -> &[WorldGate] {
        &self.gates
    }

    /// A world gate by id.
    pub fn world_gate(&self, id: u32) -> Option<&WorldGate> {
        self.gates
            .binary_search_by_key(&id, |g| g.id)
            .ok()
            .map(|i| &self.gates[i])
    }

    /// What an open portal consumes: the `inland` table when source and destination are on the
    /// same continent, else the `outland` table. The server takes the first row the character
    /// can pay in full.
    pub fn reagents(&self, same_continent: bool) -> &[PortalReagent] {
        if same_continent {
            &self.inland
        } else {
            &self.outland
        }
    }

    /// The continent id of a zone key: `zone_groups.target_id` of the zone's group, 0 for an
    /// unknown zone (the server's `target_id_by_zone`).
    pub fn continent_of_zone(&self, zone_key: u16) -> u8 {
        self.continents
            .binary_search_by_key(&zone_key, |c| c.0)
            .map_or(0, |i| self.continents[i].1)
    }

    /// Whether the server treats two zones as one continent for the open-portal reagent
    /// (equal `target_id`, where an unknown zone counts as 0).
    pub fn same_continent(&self, zone_a: u16, zone_b: u16) -> bool {
        self.continent_of_zone(zone_a) == self.continent_of_zone(zone_b)
    }

    /// Memory Tome spawns (the doodad spawns of all binding doodads in the main world).
    pub fn memory_tomes(&self) -> &[MemoryTome] {
        &self.tomes
    }

    /// The recall point reached by a recall after binding `district` as a character of
    /// `faction`: `district_return_points[district, faction]` resolved in `recalls.json`.
    /// `None` when the faction has no return point there (the server then ignores the bind and
    /// the recall).
    pub fn recall_target(&self, district: u32, faction: u32) -> Option<&RecallPoint> {
        let i = self
            .tome_returns
            .binary_search_by(|e| (e.0, e.1).cmp(&(district, faction)))
            .ok()?;
        self.recall(self.tome_returns[i].2)
    }

    /// The Memory Tome nearest to `pos` that binds a usable recall point for `faction`.
    pub fn nearest_memory_tome(&self, pos: Vec3, faction: u32) -> Option<&MemoryTome> {
        self.tomes
            .iter()
            .filter(|t| self.recall_target(t.district, faction).is_some())
            .min_by(|a, b| a.pos.dist2d_sq(pos).total_cmp(&b.pos.dist2d_sq(pos)))
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.recalls.capacity() * std::mem::size_of::<RecallPoint>()
            + str_bytes(self.recalls.iter().map(|r| &*r.name))
            + self.respawns.capacity() * std::mem::size_of::<RespawnPoint>()
            + str_bytes(self.respawns.iter().map(|r| &*r.name))
            + self.gates.capacity() * std::mem::size_of::<WorldGate>()
            + str_bytes(self.gates.iter().map(|g| &*g.name))
            + self.gates.iter().map(|g| g.skills.len() * 4).sum::<usize>()
            + (self.inland.len() + self.outland.len()) * std::mem::size_of::<PortalReagent>()
            + self.tomes.capacity() * std::mem::size_of::<MemoryTome>()
            + self.tome_returns.capacity() * 12
            + self.continents.capacity() * std::mem::size_of::<(u16, u8)>()
    }
}

/// Metres per zone origin cell (the wire offsets of a book entry count from `origin * 1024`).
pub const ZONE_CELL_M: f32 = 1024.0;

/// The origin cell `(cx, cy)` that turns the wire offset `off` of a Teleport Book entry into the
/// world position `world` (`world = off + cell * 1024`). `None` when the difference is not a whole
/// number of cells within `tolerance` metres: then one of the two is not the same spot.
pub fn origin_cell(world: (f32, f32), off: (f32, f32), tolerance: f32) -> Option<(i32, i32)> {
    let axis = |w: f32, o: f32| {
        let cells = ((w - o) / ZONE_CELL_M).round();
        if !cells.is_finite() || (w - o - cells * ZONE_CELL_M).abs() > tolerance {
            return None;
        }
        Some(cells as i32)
    };
    Some((axis(world.0, off.0)?, axis(world.1, off.1)?))
}

/// The zone origin cells learned so far (`zone key -> (cell x, cell y)`).
///
/// The server sends a Teleport Book entry as an offset from the origin cell of its zone
/// (`world.xml` of the client, which the bots do not read), so a bot learns each zone's cell from
/// a spot whose world position it knows: the place it stood when it recorded the entry, or a recall
/// point (`recalls.json` has world positions). The table is tiny (one entry per zone) and is kept
/// in a text form (`zone x y` per line) so a run can keep it for the next one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ZoneOrigins {
    cells: Vec<(u16, i32, i32)>,
}

impl ZoneOrigins {
    /// Most zones kept (the main world has about a hundred).
    pub const MAX: usize = 512;

    /// A table that knows no zone (usable in a `static`).
    pub const EMPTY: ZoneOrigins = ZoneOrigins { cells: Vec::new() };

    /// The origin cell of `zone`.
    pub fn get(&self, zone: u16) -> Option<(i32, i32)> {
        self.cells
            .binary_search_by_key(&zone, |c| c.0)
            .ok()
            .map(|i| (self.cells[i].1, self.cells[i].2))
    }

    /// Remembers the origin cell of `zone`; returns whether that changed anything.
    pub fn learn(&mut self, zone: u16, cell: (i32, i32)) -> bool {
        match self.cells.binary_search_by_key(&zone, |c| c.0) {
            Ok(i) => {
                let changed = (self.cells[i].1, self.cells[i].2) != cell;
                self.cells[i] = (zone, cell.0, cell.1);
                changed
            }
            Err(i) if self.cells.len() < Self::MAX => {
                self.cells.insert(i, (zone, cell.0, cell.1));
                true
            }
            Err(_) => false,
        }
    }

    /// Learns the cell of `zone` from an entry with the wire offset `off` that is known to be at
    /// world position `world`; `None` when the two do not differ by whole cells within
    /// `tolerance` metres (nothing is learned), else whether the table changed.
    pub fn learn_from(
        &mut self,
        zone: u16,
        world: (f32, f32),
        off: (f32, f32),
        tolerance: f32,
    ) -> Option<bool> {
        let cell = origin_cell(world, off, tolerance)?;
        Some(self.learn(zone, cell))
    }

    /// World `(x, y)` of an entry of `zone` with the wire offset `off`. Zone 0 sends world
    /// coordinates already; an unknown zone gives `None`.
    pub fn world_xy(&self, zone: u16, off: (f32, f32)) -> Option<(f32, f32)> {
        if zone == 0 {
            return Some(off);
        }
        let (cx, cy) = self.get(zone)?;
        Some((
            off.0 + cx as f32 * ZONE_CELL_M,
            off.1 + cy as f32 * ZONE_CELL_M,
        ))
    }

    /// Number of zones known.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether no zone is known.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// One `zone x y` line per zone.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for (zone, x, y) in &self.cells {
            s.push_str(&format!("{zone} {x} {y}\n"));
        }
        s
    }

    /// Reads the text form; lines that do not parse are skipped.
    pub fn from_text(text: &str) -> ZoneOrigins {
        let mut o = ZoneOrigins::default();
        for line in text.lines() {
            let mut it = line.split_whitespace().map(str::parse::<i64>);
            if let (Some(Ok(z)), Some(Ok(x)), Some(Ok(y)), None) =
                (it.next(), it.next(), it.next(), it.next())
            {
                if let (Ok(z), Ok(x), Ok(y)) =
                    (u16::try_from(z), i32::try_from(x), i32::try_from(y))
                {
                    o.learn(z, (x, y));
                }
            }
        }
        o
    }
}

fn pos_of(m: &serde_json::Map<String, serde_json::Value>) -> Vec3 {
    Vec3::new(member_f32(m, "X"), member_f32(m, "Y"), member_f32(m, "Z"))
}

fn zone_key_of(m: &serde_json::Map<String, serde_json::Value>) -> u16 {
    member_u32(m, "ZoneId").min(u32::from(u16::MAX)) as u16
}

fn load_recalls<'a>(
    path: &Path,
    localized_name: impl Fn(u32) -> Option<&'a str>,
) -> Result<Vec<RecallPoint>> {
    Ok(read_object_array(path)?
        .iter()
        .map(|m| {
            let id = member_u32(m, "Id");
            RecallPoint {
                id,
                name: localized_name(id)
                    .unwrap_or_else(|| member_str(m, "Name"))
                    .into(),
                zone_key: zone_key_of(m),
                pos: pos_of(m),
                z_rot: member_f32(m, "ZRot"),
                sub_zone: member_u32(m, "SubZoneId"),
            }
        })
        .collect())
}

fn load_respawns(path: &Path) -> Result<Vec<RespawnPoint>> {
    Ok(read_object_array(path)?
        .iter()
        .map(|m| RespawnPoint {
            id: member_u32(m, "Id"),
            name: member_str(m, "Name").into(),
            pos: pos_of(m),
            sub_zone: member_u32(m, "SubZoneId"),
        })
        .collect())
}

fn load_gates(path: &Path) -> Result<Vec<WorldGate>> {
    let mut gates: Vec<WorldGate> = read_object_array(path)?
        .iter()
        .map(|m| WorldGate {
            id: member_u32(m, "Id"),
            name: member_str(m, "Name").into(),
            zone_key: zone_key_of(m),
            pos: pos_of(m),
            yaw: member(m, "Yaw")
                .or_else(|| member(m, "ZRot"))
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0) as f32,
            skills: Box::default(),
        })
        .collect();
    gates.sort_by_key(|g| g.id); // stable: the first gate with an id stays first
    gates.dedup_by_key(|g| g.id);
    Ok(gates)
}

/// Fills `WorldGate::skills` from the `Return` special effects (type 25) with a gate id.
fn attach_gate_skills(conn: &Connection, gates: &mut [WorldGate]) -> Result<()> {
    let mut pairs: Vec<(u32, u32)> = Vec::new(); // (gate, skill)
    each_row(
        conn,
        "SELECT se.value1, sk.skill_id FROM special_effects se \
         JOIN effects e ON e.actual_id = se.id AND e.actual_type = 'SpecialEffect' \
         JOIN skill_effects sk ON sk.effect_id = e.id \
         WHERE se.special_effect_type_id = 25 AND se.value1 != 0",
        |r| pairs.push((r.u32(0), r.u32(1))),
    )?;
    pairs.sort_unstable();
    pairs.dedup();
    for g in gates.iter_mut() {
        let skills: Vec<u32> = pairs.iter().filter(|p| p.0 == g.id).map(|p| p.1).collect();
        g.skills = skills.into_boxed_slice();
    }
    Ok(())
}

fn load_reagents(conn: &Connection, table: &str) -> Result<Box<[PortalReagent]>> {
    let mut out = Vec::new();
    // `table` is one of two literals of the caller.
    each_row(
        conn,
        &format!("SELECT item_id, amount, priority FROM {table}"),
        |r| {
            out.push(PortalReagent {
                item: r.u32(0),
                amount: r.u32(1),
                priority: r.u32(2),
            })
        },
    )
    .with_context(|| format!("read {table}"))?;
    Ok(out.into_boxed_slice())
}

fn load_kit(
    inp: &TravelInputs<'_>,
    inland: &[PortalReagent],
    outland: &[PortalReagent],
) -> Result<PortalKit> {
    let conn = inp.conn;
    let skill = |id: u32| SkillTiming::of(inp.skills, id, "portal");

    let mut items = vec![
        TELEPORT_BOOK_ITEM,
        MEMORY_INK_ITEM,
        BOUND_STONE_ITEM,
        STONE_ITEM,
    ];
    items.extend(inland.iter().chain(outland).map(|r| r.item));
    items.sort_unstable();
    items.dedup();
    warn_missing(conn, "items", "portal item", &items);
    warn_missing(
        conn,
        "npcs",
        "portal gate npc",
        &[PORTAL_ENTRANCE_NPC, PORTAL_EXIT_NPC],
    );

    // The book's use skill must be the open skill; the ink the record skill's reagent.
    let mut book_skill = 0;
    each_row(
        conn,
        &format!("SELECT use_skill_id FROM items WHERE id = {TELEPORT_BOOK_ITEM}"),
        |r| book_skill = r.u32(0),
    )
    .context("read the Teleport Book")?;
    if book_skill != OPEN_PORTAL_SKILL {
        tracing::warn!(
            "travel: item {TELEPORT_BOOK_ITEM} casts skill {book_skill}, expected {OPEN_PORTAL_SKILL}"
        );
    }
    let mut reagent = (0, 0);
    each_row(
        conn,
        &format!(
            "SELECT item_id, amount FROM skill_reagents WHERE skill_id = {RECORD_PORTAL_SKILL}"
        ),
        |r| {
            if reagent.0 == 0 {
                reagent = (r.u32(0), r.u32(1));
            }
        },
    )
    .context("read skill_reagents")?;
    if reagent.0 != MEMORY_INK_ITEM {
        tracing::warn!(
            "travel: skill {RECORD_PORTAL_SKILL} consumes item {} instead of {MEMORY_INK_ITEM}",
            reagent.0
        );
    }

    // The distance of the OpenPortalEffect the open skill carries.
    let mut open_distance = None;
    each_row(
        conn,
        &format!(
            "SELECT o.distance FROM skill_effects sk \
             JOIN effects e ON e.id = sk.effect_id AND e.actual_type = 'OpenPortalEffect' \
             JOIN open_portal_effects o ON o.id = e.actual_id WHERE sk.skill_id = {OPEN_PORTAL_SKILL}"
        ),
        |r| open_distance = open_distance.or(Some(r.f32(0))),
    )
    .context("read open_portal_effects")?;
    if open_distance.is_none() {
        tracing::warn!("travel: skill {OPEN_PORTAL_SKILL} has no OpenPortalEffect");
    }

    Ok(PortalKit {
        book_item: TELEPORT_BOOK_ITEM,
        open: skill(OPEN_PORTAL_SKILL),
        record: skill(RECORD_PORTAL_SKILL),
        rename: skill(RENAME_PORTAL_SKILL),
        recall: skill(RECALL_SKILL),
        bind: skill(BIND_SKILL),
        ink_item: reagent.0,
        ink_amount: reagent.1,
        entrance_npc: PORTAL_ENTRANCE_NPC,
        exit_npc: PORTAL_EXIT_NPC,
        open_distance: open_distance.unwrap_or(3.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doodad::DoodadData;
    use crate::geom::{IdMap, StrTable};
    use crate::skill::{SkillInfo, Skills};
    use crate::spatial::SpawnIndex;
    use crate::text::Texts;
    use crate::travel::funcs;
    use crate::travel::testutil::{temp_dir, write};

    fn skill(id: u32, cast: u32, cooldown: u32) -> SkillInfo {
        SkillInfo {
            id,
            ability_id: 0,
            ability_level: 0,
            cooldown_time: cooldown,
            casting_time: cast,
            mana_cost: 0,
            mana_level_md: 0.0,
            min_range: 0,
            max_range: 0,
            target_type_id: 0,
            target_relation_id: 0,
            labor: 0,
            auto_learn: false,
            need_learn: false,
            name: "".into(),
        }
    }

    /// A compact with the portal tables, the Teleport Book data and two Memory Tomes.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        funcs::create_tables(&conn);
        conn.execute_batch(
            "CREATE TABLE items (id INT, use_skill_id INT);
             CREATE TABLE npcs (id INT);
             CREATE TABLE skill_reagents (id INT, skill_id INT, item_id INT, amount INT);
             CREATE TABLE skill_effects (id INT, skill_id INT, effect_id INT);
             CREATE TABLE effects (id INT, actual_id INT, actual_type TEXT);
             CREATE TABLE open_portal_effects (id INT, distance REAL);
             CREATE TABLE special_effects (id INT, special_effect_type_id INT, value1 INT);
             CREATE TABLE open_portal_inland_reagents (id INT, open_portal_effect_id INT, item_id INT, amount INT, priority INT);
             CREATE TABLE open_portal_outland_reagents (id INT, open_portal_effect_id INT, item_id INT, amount INT, priority INT);
             CREATE TABLE district_return_points (id INT, district_id INT, faction_id INT, return_point_id INT);
             CREATE TABLE zones (id INT, zone_key INT, group_id INT);
             CREATE TABLE zone_groups (id INT, target_id INT);
             CREATE TABLE localized_texts (id INT, tbl_name TEXT, tbl_column_name TEXT, idx INT, en_us TEXT);
             INSERT INTO items VALUES (4045, 11216), (4053, 0), (417, 0), (4052, 0);
             INSERT INTO npcs VALUES (3891);
             INSERT INTO skill_reagents VALUES (809, 11215, 4053, 1);
             INSERT INTO skill_effects VALUES (1, 11216, 100), (2, 20000, 101), (3, 20001, 101);
             INSERT INTO effects VALUES (100, 1, 'OpenPortalEffect'), (101, 7, 'SpecialEffect');
             INSERT INTO open_portal_effects VALUES (1, 3.0);
             INSERT INTO special_effects VALUES (7, 25, 372), (8, 25, 0);
             INSERT INTO open_portal_inland_reagents VALUES (5, 1, 417, 1, 1), (6, 1, 4052, 1, 2);
             INSERT INTO open_portal_outland_reagents VALUES (4, 1, 417, 3, 1), (6, 1, 4052, 3, 2);
             INSERT INTO district_return_points VALUES (1, 15, 101, 243), (2, 15, 103, 999), (3, 99, 101, 240);
             INSERT INTO zones VALUES (1, 179, 1), (2, 187, 2), (3, 300, NULL);
             INSERT INTO zone_groups VALUES (1, 3), (2, 4);
             INSERT INTO localized_texts VALUES (1, 'return_points', 'name', 243, 'Nuian Start');
             INSERT INTO doodad_func_groups VALUES (1, 841, 1), (2, 842, 1);
             INSERT INTO doodad_funcs VALUES (10, 1, 3, 'DoodadFuncBinding', 14602), (11, 2, 4, 'DoodadFuncBinding', 0);
             INSERT INTO doodad_func_bindings VALUES (3, 15), (4, 99);",
        )
        .unwrap();
        conn
    }

    fn data_dir() -> std::path::PathBuf {
        let dir = temp_dir("portal");
        write(
            &dir,
            "Portal/recalls.json",
            r#"[ // recall points
              { "Id": 243, "Name": "Nuian Starting Area", "X": 15578.04, "Y": 15382.12, "Z": 126.48,
                "ZoneId": 179, "ZRot": 58, "SubZoneId": 324, "Owner": 0 },
              { "Id": 240, "Name": "Harani Starting Area", "X": 19304.87, "Y": 7646.06, "Z": 212.7,
                "ZoneId": 187, "ZRot": 79, "SubZoneId": 188 },
              { "Id": 243, "Name": "dup", "X": 1, "Y": 1, "Z": 1, "ZoneId": 179, "SubZoneId": 324 },
            ]"#,
        );
        write(
            &dir,
            "Portal/respawns.json",
            r#"[ { "Id": 86, "Name": "Respawn: Southern White Arden", "X": 11052.4, "Y": 12858.1, "Z": 160.1, "SubZoneId": 160 } ]"#,
        );
        write(
            &dir,
            "Portal/worldgates.json",
            r#"[ { "Id": 372, "Name": "Serpent's Pass", "ZoneId": 188, "X": 21182.3, "Y": 6627.0, "Z": 144.3, "Yaw": 12.5, "SubZoneId": 196 },
                 { "Id": 221, "Name": "Escape", "ZoneId": 188, "X": 1.0, "Y": 2.0, "Z": 3.0, "SubZoneId": 852 } ]"#,
        );
        dir
    }

    fn load(conn: &Connection, dir: &std::path::Path) -> Portals {
        let texts = Texts::empty();
        let npcs = IdMap::<crate::npc::NpcTemplate>::build(Vec::new());
        let doodads = DoodadData::default();
        let skills = Skills::from_infos(vec![
            skill(OPEN_PORTAL_SKILL, 10_000, 10_000),
            skill(RECALL_SKILL, 10_000, 1_800_000),
            skill(BIND_SKILL, 3_000, 5_000),
        ]);
        let spawns = SpawnIndex::build(vec![
            (841, Vec3::new(15500.0, 15400.0, 120.0)),
            (841, Vec3::new(100.0, 100.0, 1.0)),
            (842, Vec3::new(5.0, 5.0, 5.0)),
        ]);
        let items = StrTable::default();
        let inp = TravelInputs {
            conn,
            texts: &texts,
            server_data: dir,
            main_world: "main_world",
            npcs: &npcs,
            doodads: &doodads,
            skills: &skills,
            doodad_spawns: &spawns,
            items: &items,
        };
        let funcs = DoodadTravelFuncs::load(conn).unwrap();
        Portals::load(&inp, &funcs).unwrap()
    }

    #[test]
    fn recalls_gates_respawns_and_reagents_are_read() {
        let conn = fixture();
        let dir = data_dir();
        let p = load(&conn, &dir);

        // 3 entries (an id may repeat); the localized name wins, the first of a repeated id is used
        assert_eq!(p.recall_points().len(), 3);
        let nuia = p.recall(243).unwrap();
        assert_eq!(&*nuia.name, "Nuian Start");
        assert_eq!((nuia.zone_key, nuia.sub_zone, nuia.z_rot), (179, 324, 58.0));
        assert_eq!(&*p.recall(240).unwrap().name, "Harani Starting Area");
        assert_eq!(
            p.nearest_recall(Vec3::new(19000.0, 7000.0, 0.0))
                .map(|r| r.id),
            Some(240)
        );
        assert_eq!(p.recalls_in_sub_zone(324).count(), 2);

        assert_eq!(p.respawn_points().len(), 1);
        assert_eq!(
            p.nearest_respawn(Vec3::new(0.0, 0.0, 0.0)).map(|r| r.id),
            Some(86)
        );

        // gates sorted by id, with the skills that return there; value1 0 is the recall
        let ids: Vec<u32> = p.world_gates().iter().map(|g| g.id).collect();
        assert_eq!(ids, vec![221, 372]);
        let g = p.world_gate(372).unwrap();
        assert_eq!((g.zone_key, g.yaw), (188, 12.5));
        assert_eq!(&*g.skills, &[20000, 20001]);
        assert!(p.world_gate(221).unwrap().skills.is_empty());

        // reagents per continent, in table order
        let inland = p.reagents(true);
        assert_eq!(
            inland
                .iter()
                .map(|r| (r.item, r.amount))
                .collect::<Vec<_>>(),
            vec![(417, 1), (4052, 1)]
        );
        let outland = p.reagents(false);
        assert_eq!(
            outland
                .iter()
                .map(|r| (r.item, r.amount))
                .collect::<Vec<_>>(),
            vec![(417, 3), (4052, 3)]
        );
        assert_eq!(p.continent_of_zone(179), 3);
        assert_eq!(p.continent_of_zone(187), 4);
        assert_eq!(
            p.continent_of_zone(9999),
            0,
            "unknown zones are continent 0"
        );
        assert!(p.same_continent(9998, 9999));
        assert!(!p.same_continent(179, 187));
        assert!(p.bytes() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kit_has_ids_timings_ink_and_distance() {
        let conn = fixture();
        let dir = data_dir();
        let k = *load(&conn, &dir).kit();
        assert_eq!(k.book_item, 4045);
        assert_eq!(
            k.open,
            SkillTiming {
                id: 11216,
                cast_ms: 10_000,
                cooldown_ms: 10_000
            }
        );
        assert_eq!(k.recall.cooldown_ms, 1_800_000);
        assert_eq!(k.bind.cast_ms, 3_000);
        assert_eq!(
            k.record.id, 11215,
            "skills missing from compact keep their id"
        );
        assert_eq!((k.ink_item, k.ink_amount), (4053, 1));
        assert_eq!((k.entrance_npc, k.exit_npc), (3891, 6949));
        assert_eq!(k.open_distance, 3.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_tomes_bind_districts_with_a_recall_per_faction() {
        let conn = fixture();
        let dir = data_dir();
        let p = load(&conn, &dir);
        // doodad 841 (skill 14602, district 15) spawns twice; 842 has no skill anywhere
        assert_eq!(p.memory_tomes().len(), 2);
        assert!(p
            .memory_tomes()
            .iter()
            .all(|t| t.doodad == 841 && t.skill == 14602));
        // faction 101 recalls to 243, faction 103 to 999 which recalls.json lacks, 104 nowhere
        assert_eq!(p.recall_target(15, 101).map(|r| r.id), Some(243));
        assert!(p.recall_target(15, 103).is_none());
        assert!(p.recall_target(15, 104).is_none());
        assert!(
            p.recall_target(99, 101).is_none(),
            "district 99 has no tome spawn"
        );
        let near = p
            .nearest_memory_tome(Vec3::new(90.0, 90.0, 0.0), 101)
            .unwrap();
        assert_eq!(near.pos, Vec3::new(100.0, 100.0, 1.0));
        assert!(p.nearest_memory_tome(Vec3::default(), 103).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_portal_files_load_as_empty_lists() {
        let conn = fixture();
        let dir = temp_dir("portal-empty");
        let p = load(&conn, &dir);
        assert!(p.recall_points().is_empty() && p.respawn_points().is_empty());
        assert!(p.world_gates().is_empty());
        assert!(p.nearest_recall(Vec3::default()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recalls_within_a_radius() {
        let conn = fixture();
        let dir = data_dir();
        let p = load(&conn, &dir);
        let here = Vec3::new(15500.0, 15400.0, 0.0);
        let ids: Vec<u32> = p.recalls_within(here, 200.0).map(|r| r.id).collect();
        assert_eq!(ids, vec![243]);
        assert_eq!(p.recalls_within(here, 10.0).count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_origin_cell_is_the_whole_cells_between_world_and_offset() {
        // a spot at world (15578.04, 15382.12) with the wire offset (218.04, -1001.88)
        let cell = origin_cell((15578.04, 15382.12), (218.04, -1001.88), 0.5);
        assert_eq!(cell, Some((15, 16)));
        // a spot that is not a whole number of cells away is no origin
        assert_eq!(origin_cell((100.0, 100.0), (50.0, 50.0), 2.0), None);
        // the tolerance covers a position that is a little off
        assert_eq!(origin_cell((2048.8, 1024.0), (0.0, 0.0), 1.0), Some((2, 1)));
        assert_eq!(origin_cell((2051.0, 1024.0), (0.0, 0.0), 1.0), None);
        assert_eq!(origin_cell((f32::NAN, 0.0), (0.0, 0.0), 1.0), None);
        // negative offsets are fine
        assert_eq!(
            origin_cell((10.0, 10.0), (-1014.0, -1014.0), 0.1),
            Some((1, 1))
        );
    }

    #[test]
    fn zone_origins_learn_resolve_and_round_trip() {
        let mut o = ZoneOrigins::default();
        assert!(o.is_empty());
        assert_eq!(o.world_xy(179, (1.0, 2.0)), None, "unknown zone");
        assert_eq!(
            o.world_xy(0, (1.0, 2.0)),
            Some((1.0, 2.0)),
            "zone 0 is world"
        );
        assert_eq!(
            o.learn_from(179, (15578.0, 15382.0), (218.0, 100.0), 3.0),
            None,
            "the second axis is no whole cell"
        );
        assert_eq!(
            o.learn_from(179, (15578.0, 15382.0), (218.0, 22.0), 3.0),
            Some(true)
        );
        assert_eq!(o.get(179), Some((15, 15)));
        assert_eq!(
            o.learn_from(179, (15578.0, 15382.0), (218.0, 22.0), 3.0),
            Some(false)
        );
        assert_eq!(
            o.world_xy(179, (10.0, 20.0)),
            Some((10.0 + 15360.0, 20.0 + 15360.0))
        );
        o.learn(5, (-1, 2));
        o.learn(900, (3, 4));
        let text = o.to_text();
        assert_eq!(ZoneOrigins::from_text(&text), o);
        // bad lines are skipped
        let back = ZoneOrigins::from_text(
            "7 1 2
nope
8 1
9 1 2 3
70000 1 1
",
        );
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(7), Some((1, 2)));
    }
}
