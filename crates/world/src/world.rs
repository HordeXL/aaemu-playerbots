//! The [`World`]: everything static the bots need to know, loaded once and shared read-only
//! (`Arc<World>`).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rustc_hash::FxHashMap;

use crate::baked::{Baked, QuestSphere};
use crate::chars::Chars;
use crate::doodad::{DoodadData, DoodadTemplate};
use crate::economy::{Craft, Economy};
use crate::faction::{Factions, Relation};
use crate::geom::{IdMap, StrTable, Vec3};
use crate::housing::{AreaIndex, Housing, HousingDesign};
use crate::npc::{self, NpcTemplate};
use crate::quest::{Quest, QuestStats, Quests};
use crate::skill::{SkillInfo, Skills};
use crate::spatial::SpawnIndex;
use crate::spawns::{self, Kind, SpawnStats};
use crate::sql::{each_row, open_readonly, RowExt};
use crate::text::{TextTable, Texts};

/// Where the world data lives.
#[derive(Clone, Debug)]
pub struct WorldPaths {
    /// The server's `compact.sqlite3` (opened read-only).
    pub compact: PathBuf,
    /// The server's `app/Data` directory (`Worlds/<world>/*.json`, `CharTemplates.json`).
    pub server_data: PathBuf,
    /// Directory written by `playerbots bake` (terrain, map, housing areas, quest spheres,
    /// zones); `None` runs without baked data.
    pub baked: Option<PathBuf>,
    /// World name, e.g. `"main_world"`.
    pub world: String,
}

/// Zone names and the zone id / zone key mapping.
#[derive(Debug, Default)]
struct Zones {
    /// `(zone_key, English name)` sorted by key.
    names: Vec<(u16, Box<str>)>,
    /// `zones.id -> zone_key`.
    key_of_id: IdMap<u16>,
}

impl Zones {
    fn load(conn: &Connection, texts: &Texts) -> Result<Zones> {
        let mut names: Vec<(u16, Box<str>)> = Vec::new();
        let mut ids = Vec::new();
        each_row(
            conn,
            "SELECT id, name, zone_key, group_id FROM zones",
            |r| {
                let (id, key) = (r.u32(0), r.u16(2));
                let name: Box<str> = texts
                    .get(TextTable::Zone, id)
                    .or_else(|| texts.get(TextTable::ZoneGroup, r.u32(3)))
                    .unwrap_or_else(|| r.text(1))
                    .into();
                ids.push((id, key));
                if !names.iter().any(|n| n.0 == key) {
                    names.push((key, name));
                }
            },
        )?;
        names.sort_by_key(|n| n.0);
        Ok(Zones {
            names,
            key_of_id: IdMap::build(ids),
        })
    }

    fn name(&self, key: u16) -> Option<&str> {
        self.names
            .binary_search_by_key(&key, |n| n.0)
            .ok()
            .map(|i| &*self.names[i].1)
    }

    fn bytes(&self) -> usize {
        self.names.capacity() * std::mem::size_of::<(u16, Box<str>)>()
            + self.names.iter().map(|n| n.1.len()).sum::<usize>()
            + self.key_of_id.shallow_bytes()
    }
}

/// Static world knowledge, immutable after [`World::load`].
pub struct World {
    baked: Baked,
    factions: Factions,
    chars: Chars,
    skills: Skills,
    npcs: IdMap<NpcTemplate>,
    doodads: DoodadData,
    npc_spawns: SpawnIndex,
    doodad_spawns: SpawnIndex,
    npc_spawn_stats: SpawnStats,
    doodad_spawn_stats: SpawnStats,
    quests: Quests,
    economy: Economy,
    housing: Housing,
    areas: AreaIndex,
    items: StrTable,
    zones: Zones,
    /// Doodad templates (that spawn) yielding a gather item.
    gather_doodads: FxHashMap<u32, Box<[u32]>>,
    /// `(quest, component) -> index into baked.quest_spheres`.
    spheres: FxHashMap<(u32, u32), u32>,
}

// Bots share the world through `Arc<World>` across tasks.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<World>();
};

impl World {
    /// Loads the world: opens `compact.sqlite3` read-only, reads the spawn files and the baked
    /// extras, compiles quests and builds the indexes. Logs counts and the load time.
    ///
    /// Blocking (about half a second with the full 1.2 data); call it from `spawn_blocking` inside
    /// an async runtime.
    pub fn load(paths: &WorldPaths) -> Result<World> {
        let started = Instant::now();
        let conn = open_readonly(&paths.compact)?;
        let texts = Texts::load(&conn).context("read localized_texts")?;

        let baked = match &paths.baked {
            Some(dir) => Baked::load(dir, &paths.world),
            None => Baked::default(),
        };

        let factions = Factions::load(&conn).context("load factions")?;
        let chars = Chars::load(&conn, &paths.server_data).context("load character templates")?;
        let skills = Skills::load(&conn, &texts).context("load skills")?;
        let mut npcs = npc::load(&conn, &texts).context("load npcs")?;
        let doodads = crate::doodad::load(&conn, &texts).context("load doodads")?;
        let items = load_items(&conn, &texts).context("load items")?;

        // Spawn files.
        let world_dir: PathBuf = paths.server_data.join("Worlds").join(&paths.world);
        let ground = |x: f32, y: f32| baked.terrain.as_ref().and_then(|t| t.height(x, y));
        let (npc_raw, npc_spawn_stats) =
            spawns::load(&world_dir, Kind::Npc, |t| npcs.contains(t), ground)
                .context("load npc spawns")?;
        let (doodad_raw, doodad_spawn_stats) = spawns::load(
            &world_dir,
            Kind::Doodad,
            |t| doodads.templates.contains(t),
            ground,
        )
        .context("load doodad spawns")?;
        let npc_spawns = SpawnIndex::build(npc_raw);
        let doodad_spawns = SpawnIndex::build(doodad_raw);

        let quests =
            Quests::load(&conn, &texts, &|n| npc_spawns.has(n)).context("compile quests")?;
        for npc_id in quests.npc_starters() {
            if let Some(n) = npcs.get_mut(npc_id) {
                n.is_quest_giver = true;
            }
        }
        let gather_doodads = doodads
            .item_sources(&conn, &quests.gather_items(), |d| doodad_spawns.has(d))
            .context("resolve gather item sources")?;
        let economy =
            Economy::load(&conn, &texts, &skills, &doodads.bench_pairs).context("load crafts")?;
        let housing =
            Housing::load(&conn, &texts, &|i| items.get(i).is_some()).context("load housing")?;
        let areas = AreaIndex::build(&baked.housing_areas);
        let zones = Zones::load(&conn, &texts).context("load zones")?;
        let spheres = baked
            .quest_spheres
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.quest, s.component), i as u32))
            .collect();

        let world = World {
            baked,
            factions,
            chars,
            skills,
            npcs,
            doodads,
            npc_spawns,
            doodad_spawns,
            npc_spawn_stats,
            doodad_spawn_stats,
            quests,
            economy,
            housing,
            areas,
            items,
            zones,
            gather_doodads,
            spheres,
        };
        let q = world.quests.stats();
        tracing::info!(
            world = %paths.world,
            elapsed_ms = started.elapsed().as_millis() as u64,
            npcs = world.npcs.len(),
            doodads = world.doodads.templates.len(),
            npc_spawns = world.npc_spawns.len(),
            npc_spawn_ids = world.npc_spawns.template_count(),
            doodad_spawns = world.doodad_spawns.len(),
            quests = q.quests,
            quests_with_objectives = q.with_objectives,
            skills = world.skills.len(),
            crafts = world.economy.craft_count(),
            designs = world.housing.designs().len(),
            items = world.items.len(),
            factions = world.factions.len(),
            terrain = world.baked.terrain.is_some(),
            memory_kib = world.memory_estimate() / 1024,
            "world loaded"
        );
        Ok(world)
    }

    /// Approximate heap bytes held (memory-mapped terrain is not counted).
    pub fn memory_estimate(&self) -> usize {
        self.memory_breakdown().iter().map(|(_, n)| n).sum()
    }

    /// Heap bytes per component, largest first.
    pub fn memory_breakdown(&self) -> Vec<(&'static str, usize)> {
        let baked = self
            .baked
            .housing_areas
            .iter()
            .map(|p| p.points.capacity() * 8 + std::mem::size_of_val(p))
            .sum::<usize>()
            + self.baked.quest_spheres.capacity() * std::mem::size_of::<QuestSphere>()
            + self
                .baked
                .zones
                .as_ref()
                .map_or(0, |z| z.keys.capacity() * 2);
        let gather = self
            .gather_doodads
            .values()
            .map(|v| v.len() * 4 + 24)
            .sum::<usize>();
        let mut v = vec![
            ("npc spawns + grid", self.npc_spawns.bytes()),
            ("doodad spawns + grid", self.doodad_spawns.bytes()),
            ("npc templates", npc::bytes(&self.npcs)),
            ("doodad templates", self.doodads.bytes()),
            ("quests", self.quests.bytes()),
            ("skills", self.skills.bytes()),
            ("item names", self.items.bytes()),
            ("crafts + specialty", self.economy.bytes()),
            ("housing", self.housing.bytes() + self.areas.bytes()),
            ("factions", self.factions.bytes()),
            ("characters", self.chars.bytes()),
            ("zones", self.zones.bytes()),
            ("baked (areas, spheres, zones)", baked),
            ("indexes", gather + self.spheres.capacity() * 16),
        ];
        v.sort_by_key(|e| std::cmp::Reverse(e.1));
        v
    }

    /// The baked extras (terrain, map calibration, housing areas, quest spheres, zones).
    pub fn baked(&self) -> &Baked {
        &self.baked
    }

    /// Terrain height at (x, y) from the baked grid; `None` without baked terrain or outside it.
    pub fn ground_z(&self, x: f32, y: f32) -> Option<f32> {
        self.baked.terrain.as_ref()?.height(x, y)
    }

    // ---- NPCs ----------------------------------------------------------------------------

    /// An NPC template.
    pub fn npc(&self, template: u32) -> Option<&NpcTemplate> {
        self.npcs.get(template)
    }

    /// All NPC templates in id order.
    pub fn npc_templates(&self) -> &[NpcTemplate] {
        self.npcs.values()
    }

    /// Static spawn positions of an NPC template.
    pub fn npc_spawns(&self, template: u32) -> &[Vec3] {
        self.npc_spawns.of(template)
    }

    /// The spawn of `template` horizontally closest to `from`.
    pub fn nearest_npc_spawn(&self, template: u32, from: Vec3) -> Option<Vec3> {
        self.npc_spawns.nearest(template, from)
    }

    /// Calls `f(template, position)` for every NPC spawn within `radius` metres (horizontal).
    pub fn npc_spawns_within(&self, center: Vec3, radius: f32, f: impl FnMut(u32, Vec3)) {
        self.npc_spawns.within(center, radius, f)
    }

    /// Whether the NPC template spawns anywhere in this world.
    pub fn npc_spawns_anywhere(&self, template: u32) -> bool {
        self.npc_spawns.has(template)
    }

    /// Counters from reading the NPC spawn files.
    pub fn npc_spawn_stats(&self) -> SpawnStats {
        self.npc_spawn_stats
    }

    // ---- Doodads -------------------------------------------------------------------------

    /// A doodad template.
    pub fn doodad(&self, template: u32) -> Option<&DoodadTemplate> {
        self.doodads.templates.get(template)
    }

    /// Static spawn positions of a doodad template.
    pub fn doodad_spawns(&self, template: u32) -> &[Vec3] {
        self.doodad_spawns.of(template)
    }

    /// The spawn of `template` horizontally closest to `from`.
    pub fn nearest_doodad_spawn(&self, template: u32, from: Vec3) -> Option<Vec3> {
        self.doodad_spawns.nearest(template, from)
    }

    /// Calls `f(template, position)` for every doodad spawn within `radius` metres (horizontal).
    pub fn doodad_spawns_within(&self, center: Vec3, radius: f32, f: impl FnMut(u32, Vec3)) {
        self.doodad_spawns.within(center, radius, f)
    }

    /// Whether the doodad template spawns anywhere in this world.
    pub fn doodad_spawns_anywhere(&self, template: u32) -> bool {
        self.doodad_spawns.has(template)
    }

    /// Counters from reading the doodad spawn files.
    pub fn doodad_spawn_stats(&self) -> SpawnStats {
        self.doodad_spawn_stats
    }

    /// Doodad templates (that spawn here) which yield `item` when used.
    pub fn gather_doodads(&self, item: u32) -> &[u32] {
        self.gather_doodads.get(&item).map_or(&[], |v| &v[..])
    }

    // ---- Factions and characters ---------------------------------------------------------

    /// How faction `a` regards faction `b`.
    pub fn relation(&self, faction_a: u32, faction_b: u32) -> Relation {
        self.factions.relation(faction_a, faction_b)
    }

    /// Where a new character of `race` starts.
    pub fn race_start(&self, race: u8) -> Option<Vec3> {
        self.chars.race_start(race)
    }

    /// Whether `race` / `gender` can be created.
    pub fn creatable(&self, race: u8, gender: u8) -> bool {
        self.chars.creatable(race, gender)
    }

    /// The faction a player of `race` has.
    pub fn race_faction(&self, race: u8) -> Option<u32> {
        self.chars.race_faction(race)
    }

    // ---- Quests --------------------------------------------------------------------------

    /// A compiled quest.
    pub fn quest(&self, id: u32) -> Option<&Quest> {
        self.quests.get(id)
    }

    /// All quests in id order.
    pub fn quests(&self) -> impl Iterator<Item = &Quest> {
        self.quests.iter()
    }

    /// Quests started by talking to this NPC template.
    pub fn quests_from_npc(&self, npc_template: u32) -> &[u32] {
        self.quests.from_npc(npc_template)
    }

    /// Quests started by using this doodad template.
    pub fn quests_from_doodad(&self, doodad_template: u32) -> &[u32] {
        self.quests.from_doodad(doodad_template)
    }

    /// Aggregate numbers about the compiled quests.
    pub fn quest_stats(&self) -> QuestStats {
        self.quests.stats()
    }

    /// The baked sign sphere of a quest component (start / objective position).
    pub fn quest_sphere(&self, quest: u32, component: u32) -> Option<&QuestSphere> {
        let i = *self.spheres.get(&(quest, component))?;
        self.baked.quest_spheres.get(i as usize)
    }

    // ---- Skills --------------------------------------------------------------------------

    /// A skill.
    pub fn skill(&self, id: u32) -> Option<&SkillInfo> {
        self.skills.get(id)
    }

    /// Skill ids of class ability `ability` (1..=10) ordered by required ability level.
    pub fn ability_skills(&self, ability: u8) -> &[u32] {
        self.skills.ability_skills(ability)
    }

    // ---- Economy -------------------------------------------------------------------------

    /// A crafting recipe.
    pub fn craft(&self, id: u32) -> Option<&Craft> {
        self.economy.craft(id)
    }

    /// Trade-pack recipes offered by a bench doodad template.
    pub fn pack_crafts_at(&self, doodad_template: u32) -> &[u32] {
        self.economy.pack_crafts_at(doodad_template)
    }

    /// NPC templates that buy `pack_item`.
    pub fn specialty_buyers(&self, pack_item: u32) -> &[u32] {
        self.economy.specialty_buyers(pack_item)
    }

    /// Every trade-pack recipe.
    pub fn pack_crafts(&self) -> impl Iterator<Item = &Craft> {
        self.economy.pack_crafts()
    }

    /// An item's English name.
    pub fn item_name(&self, id: u32) -> Option<&str> {
        self.items.get(id)
    }

    // ---- Housing -------------------------------------------------------------------------

    /// Buildable house designs, cheapest first.
    pub fn housing_designs(&self) -> &[HousingDesign] {
        self.housing.designs()
    }

    /// Whether housing area `area_id` accepts houses of `category`.
    pub fn housing_area_allows(&self, area_id: u32, category: u32) -> bool {
        self.housing.area_allows(area_id, category)
    }

    /// The housing area (`housing_areas.id`) containing (x, y), from the baked outlines.
    pub fn housing_area_at(&self, x: f32, y: f32) -> Option<u32> {
        self.areas.at(&self.baked.housing_areas, x, y)
    }

    // ---- Zones ---------------------------------------------------------------------------

    /// The zone key at (x, y) from the baked zone grid.
    pub fn zone_key_at(&self, x: f32, y: f32) -> Option<u16> {
        self.baked.zones.as_ref()?.zone_key_at(x, y)
    }

    /// English zone name for a zone key.
    pub fn zone_name(&self, zone_key: u16) -> Option<&str> {
        self.zones.name(zone_key)
    }

    /// Zone key for a `zones.id` (quests use ids, the client and baked grid use keys).
    pub fn zone_key_for_id(&self, zone_id: u32) -> Option<u16> {
        self.zones.key_of_id.get(zone_id).copied()
    }
}

/// `items` ids with English names.
fn load_items(conn: &Connection, texts: &Texts) -> Result<StrTable> {
    let mut entries: Vec<(u32, Box<str>)> = Vec::new();
    each_row(conn, "SELECT id, name FROM items", |r| {
        let id = r.u32(0);
        entries.push((id, texts.name_or(TextTable::Item, id, r.text(1))));
    })?;
    Ok(StrTable::build(entries))
}
