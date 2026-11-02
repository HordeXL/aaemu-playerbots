//! Crafting recipes and trade-pack ("specialty") selling.
//!
//! A craft is a *pack* craft when its product is a specialty good that some specialty NPC buys
//! (`specialty_bundle_items.item_id`); those are the recipes a trade run needs. Benches are the
//! doodad templates that offer a recipe (`DoodadFuncCraftPack` / `DoodadFuncCraftStart`
//! functions, plus the recipe's own `req_doodad_id`).
//!
//! Table semantics follow the server's `systems/economy/{craft_data,specialty}.rs` and
//! `systems/npcs/bridges.rs` (AAEmu Rust, GPL-3.0-or-later).

use anyhow::Result;
use rusqlite::Connection;
use rustc_hash::FxHashMap;

use crate::geom::IdMap;
use crate::skill::Skills;
use crate::sql::{each_row, RowExt};
use crate::text::{TextTable, Texts};

/// One crafting recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Craft {
    /// `crafts.id`.
    pub id: u32,
    /// English title (Korean if there is no translation).
    pub name: Box<str>,
    /// `crafts.skill_id`: the skill whose `labor` the craft costs.
    pub skill_id: u32,
    /// Labor power spent.
    pub labor: u16,
    /// The first product `(item id, count)`.
    pub product: (u32, u32),
    /// Required materials `(item id, count)`.
    pub materials: Box<[(u32, u32)]>,
    /// Produces a specialty trade pack that specialty NPCs buy.
    pub is_pack: bool,
    /// `crafts.req_doodad_id`: the doodad template the recipe requires (0 = none).
    pub req_doodad: u32,
    /// Doodad templates that offer this recipe.
    pub benches: Box<[u32]>,
}

/// Recipes and specialty selling.
#[derive(Debug, Default)]
pub struct Economy {
    crafts: IdMap<Craft>,
    /// Pack crafts by bench doodad template.
    pack_at: FxHashMap<u32, Box<[u32]>>,
    /// Specialty NPC templates by pack item.
    buyers: FxHashMap<u32, Box<[u32]>>,
}

impl Economy {
    /// Loads `crafts`, `craft_products`, `craft_materials`, `specialty_bundle_items` and
    /// `specialty_npcs`. `bench_pairs` are `(doodad template, craft)` pairs from the doodad loader.
    pub fn load(
        conn: &Connection,
        texts: &Texts,
        skills: &Skills,
        bench_pairs: &[(u32, u32)],
    ) -> Result<Economy> {
        let mut head: Vec<(u32, Box<str>, u32, u32)> = Vec::new();
        each_row(
            conn,
            "SELECT id, title, skill_id, req_doodad_id FROM crafts",
            |r| {
                let id = r.u32(0);
                head.push((
                    id,
                    texts.name_or(TextTable::Craft, id, r.text(1)),
                    r.u32(2),
                    r.u32(3),
                ));
            },
        )?;
        let mut products: FxHashMap<u32, (u32, u32)> = FxHashMap::default();
        each_row(
            conn,
            "SELECT craft_id, item_id, amount FROM craft_products ORDER BY id",
            |r| {
                products
                    .entry(r.u32(0))
                    .or_insert((r.u32(1), r.u32(2).max(1)));
            },
        )?;
        let mut materials: FxHashMap<u32, Vec<(u32, u32)>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT craft_id, item_id, amount FROM craft_materials ORDER BY id",
            |r| {
                materials
                    .entry(r.u32(0))
                    .or_default()
                    .push((r.u32(1), r.u32(2).max(1)))
            },
        )?;

        // Specialty: pack item -> bundles, bundle -> npcs.
        let mut bundles_of_item: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT item_id, specialty_bundle_id FROM specialty_bundle_items",
            |r| bundles_of_item.entry(r.u32(0)).or_default().push(r.u32(1)),
        )?;
        let mut npcs_of_bundle: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        each_row(
            conn,
            "SELECT specialty_bundle_id, npc_id FROM specialty_npcs",
            |r| npcs_of_bundle.entry(r.u32(0)).or_default().push(r.u32(1)),
        )?;
        let buyers: FxHashMap<u32, Box<[u32]>> = bundles_of_item
            .iter()
            .filter_map(|(item, bundles)| {
                let mut npcs: Vec<u32> = bundles
                    .iter()
                    .filter_map(|b| npcs_of_bundle.get(b))
                    .flatten()
                    .copied()
                    .collect();
                npcs.sort_unstable();
                npcs.dedup();
                (!npcs.is_empty()).then(|| (*item, npcs.into_boxed_slice()))
            })
            .collect();

        // Benches per craft.
        let mut benches: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for &(doodad, craft) in bench_pairs {
            benches.entry(craft).or_default().push(doodad);
        }
        for (id, _, _, req) in &head {
            if *req != 0 {
                benches.entry(*id).or_default().push(*req);
            }
        }

        let crafts: Vec<(u32, Craft)> = head
            .into_iter()
            .filter_map(|(id, name, skill_id, req_doodad)| {
                let product = *products.get(&id)?;
                let mut b = benches.remove(&id).unwrap_or_default();
                b.sort_unstable();
                b.dedup();
                Some((
                    id,
                    Craft {
                        id,
                        name,
                        skill_id,
                        labor: skills.get(skill_id).map_or(0, |s| s.labor),
                        product,
                        materials: materials.remove(&id).unwrap_or_default().into_boxed_slice(),
                        is_pack: bundles_of_item.contains_key(&product.0),
                        req_doodad,
                        benches: b.into_boxed_slice(),
                    },
                ))
            })
            .collect();
        Ok(Economy::from_parts(IdMap::build(crafts), buyers))
    }

    fn from_parts(crafts: IdMap<Craft>, buyers: FxHashMap<u32, Box<[u32]>>) -> Economy {
        let mut at: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for c in crafts.values().iter().filter(|c| c.is_pack) {
            for b in c.benches.iter() {
                at.entry(*b).or_default().push(c.id);
            }
        }
        let pack_at = at
            .into_iter()
            .map(|(b, mut v)| {
                v.sort_unstable();
                v.dedup();
                (b, v.into_boxed_slice())
            })
            .collect();
        Economy {
            crafts,
            pack_at,
            buyers,
        }
    }

    /// One recipe.
    pub fn craft(&self, id: u32) -> Option<&Craft> {
        self.crafts.get(id)
    }

    /// Number of recipes with a product.
    pub fn craft_count(&self) -> usize {
        self.crafts.len()
    }

    /// Trade-pack recipes offered by a bench doodad template.
    pub fn pack_crafts_at(&self, doodad_template: u32) -> &[u32] {
        self.pack_at.get(&doodad_template).map_or(&[], |v| &v[..])
    }

    /// NPC templates that buy `pack_item`.
    pub fn specialty_buyers(&self, pack_item: u32) -> &[u32] {
        self.buyers.get(&pack_item).map_or(&[], |v| &v[..])
    }

    /// Pack items that at least one specialty NPC buys.
    pub fn pack_items(&self) -> impl Iterator<Item = u32> + '_ {
        self.buyers.keys().copied()
    }

    /// Every pack recipe.
    pub fn pack_crafts(&self) -> impl Iterator<Item = &Craft> {
        self.crafts.values().iter().filter(|c| c.is_pack)
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        let crafts: usize = self
            .crafts
            .values()
            .iter()
            .map(|c| c.name.len() + c.materials.len() * 8 + c.benches.len() * 4)
            .sum();
        let idx = |m: &FxHashMap<u32, Box<[u32]>>| {
            m.values().map(|v| v.len() * 4 + 16 + 8).sum::<usize>()
        };
        self.crafts.shallow_bytes() + crafts + idx(&self.pack_at) + idx(&self.buyers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::SkillInfo;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE crafts (id INT, title TEXT, skill_id INT, req_doodad_id INT);
             CREATE TABLE craft_products (id INT, craft_id INT, item_id INT, amount INT);
             CREATE TABLE craft_materials (id INT, craft_id INT, item_id INT, amount INT);
             CREATE TABLE specialty_bundle_items (id INT, item_id INT, specialty_bundle_id INT);
             CREATE TABLE specialty_npcs (id INT, name TEXT, npc_id INT, specialty_bundle_id INT);
             INSERT INTO crafts VALUES (6226, 'pack', 16766, 4233), (6227, 'other', 0, 0), (6228, 'no product', 0, 0);
             INSERT INTO craft_products VALUES (6989, 6226, 31855, 1), (6990, 6226, 5, 1), (6991, 6227, 100, 3);
             INSERT INTO craft_materials VALUES (1, 6226, 4747, 1), (2, 6226, 30901, 50), (3, 6226, 14627, 8);
             INSERT INTO specialty_bundle_items VALUES (1, 31855, 15), (2, 31855, 16), (3, 31855, 99);
             INSERT INTO specialty_npcs VALUES (1, 'a', 14766, 15), (2, 'b', 14767, 16), (3, 'c', 14766, 16);",
        )
        .unwrap();
        conn
    }

    fn skills() -> Skills {
        Skills::from_infos(vec![SkillInfo {
            id: 16766,
            ability_id: 0,
            ability_level: 0,
            cooldown_time: 0,
            casting_time: 0,
            mana_cost: 0,
            min_range: 0,
            max_range: 0,
            target_type_id: 0,
            target_relation_id: 0,
            labor: 50,
            auto_learn: false,
            need_learn: false,
            name: "".into(),
        }])
    }

    #[test]
    fn pack_craft_bench_materials_and_buyers() {
        let conn = fixture();
        // Craft 6226 is offered at 4233 via a craft pack and at 4300 by a craft start.
        let eco = Economy::load(
            &conn,
            &Texts::empty(),
            &skills(),
            &[(4233, 6226), (4300, 6226), (4300, 6227)],
        )
        .unwrap();
        let c = eco.craft(6226).unwrap();
        assert!(c.is_pack);
        assert_eq!(c.product, (31855, 1));
        assert_eq!(&c.materials[..], &[(4747, 1), (30901, 50), (14627, 8)]);
        assert_eq!(c.labor, 50);
        assert_eq!(&c.benches[..], &[4233, 4300]);
        assert_eq!(c.req_doodad, 4233);
        assert_eq!(eco.pack_crafts_at(4233), &[6226]);
        assert_eq!(eco.pack_crafts_at(4300), &[6226]);
        assert!(eco.pack_crafts_at(1).is_empty());
        assert_eq!(eco.specialty_buyers(31855), &[14766, 14767]);
        assert!(eco.specialty_buyers(5).is_empty());
        // 6227 is no pack, 6228 has no product row.
        assert!(!eco.craft(6227).unwrap().is_pack);
        assert_eq!(eco.craft(6227).unwrap().product, (100, 3));
        assert!(eco.craft(6228).is_none());
        assert_eq!(eco.craft_count(), 2);
    }
}
