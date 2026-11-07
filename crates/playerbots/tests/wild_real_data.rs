//! The wild-farm spot finder against the real `main_world` data (ignored by default): from each
//! race's start and a few recall points, does a secluded, dry, flat patch turn up, and how far
//! away is it?
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> AA_BAKED=<data dir> \
//!   cargo test -p playerbots --test wild_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aa_world::{Vec3, World, WorldPaths};
use playerbots::bot::farm::site::{self, WildEnv};
use playerbots::bot::goals::quest::progress;

fn world() -> Option<Arc<World>> {
    static WORLD: OnceLock<Option<Arc<World>>> = OnceLock::new();
    WORLD
        .get_or_init(|| {
            let compact = PathBuf::from(std::env::var_os("AA_COMPACT")?);
            let server_data = PathBuf::from(std::env::var_os("AA_SERVER_DATA")?);
            let baked = std::env::var_os("AA_BAKED").map(PathBuf::from);
            let paths = WorldPaths {
                compact,
                server_data,
                baked,
                world: "main_world".into(),
            };
            Some(Arc::new(World::load(&paths).expect("load main_world")))
        })
        .clone()
}

/// The farm goal's checks with the static data only (no players in view, no war).
struct Data<'w> {
    world: &'w World,
    faction: u32,
}

impl WildEnv for Data<'_> {
    fn ground(&self, x: f32, y: f32) -> Option<f32> {
        playerbots::bot::goals::walkable(self.world, x, y)
            .then(|| self.world.ground_z(x, y).unwrap_or(0.0))
    }
    fn protected(&self, x: f32, y: f32) -> bool {
        self.world.housing_area_at(x, y).is_some() || self.world.farm().area_at(x, y).is_some()
    }
    fn npcs(&self, x: f32, y: f32, radius: f32, cap: u32) -> u32 {
        playerbots::bot::goals::farm::townsfolk(self.world, self.faction, x, y, radius).min(cap)
    }
    fn quest_giver(&self, x: f32, y: f32, radius: f32) -> bool {
        let mut found = false;
        let w = self.world;
        w.npc_spawns_within(Vec3::new(x, y, 0.0), radius, |t, _| {
            found = found || w.npc(t).is_some_and(|n| n.is_quest_giver);
        });
        found
    }
    fn monster(&self, x: f32, y: f32, radius: f32) -> bool {
        progress::worst_near(self.world, self.faction, x, y, radius, u8::MAX) > 0
    }
    fn town(&self, x: f32, y: f32, radius: f32) -> bool {
        let near = |p: Vec3| (p.x - x).powi(2) + (p.y - y).powi(2) <= radius * radius;
        let portals = &self.world.travel().portals;
        portals.recall_points().iter().any(|r| near(r.pos))
            || portals.respawn_points().iter().any(|r| near(r.pos))
    }
    fn player(&self, _x: f32, _y: f32, _radius: f32) -> bool {
        false
    }
    fn bad(&self, _x: f32, _y: f32) -> bool {
        false
    }
}

#[test]
#[ignore = "needs the real server data (AA_COMPACT, AA_SERVER_DATA, AA_BAKED)"]
fn secluded_patches_turn_up_near_the_starts_and_the_recall_points() {
    let Some(w) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let mut rng = fastrand::Rng::with_seed(21);
    let mut froms: Vec<(String, [f32; 3], u32)> = Vec::new();
    for race in 1..=8u8 {
        if let (Some(p), Some(f)) = (w.race_start(race), w.race_faction(race)) {
            froms.push((format!("race {race} start"), [p.x, p.y, p.z], f));
        }
    }
    let faction = froms.first().map_or(0, |f| f.2);
    for r in w.recall_points().iter().step_by(9).take(12) {
        froms.push((r.name.to_string(), [r.pos.x, r.pos.y, r.pos.z], faction));
    }
    let mut found = 0;
    for (name, from, faction) in &froms {
        let env = Data {
            world: &w,
            faction: *faction,
        };
        let mut hits = Vec::new();
        for _ in 0..5 {
            if let Some(s) = site::wild_patch(&env, *from, &mut rng) {
                let [x, y] = s.centre;
                assert!(site::wild_ok(&env, x, y));
                assert!(w.housing_area_at(x, y).is_none() && w.farm().area_at(x, y).is_none());
                let d = ((x - from[0]).powi(2) + (y - from[1]).powi(2)).sqrt();
                hits.push(format!("({x:.0}, {y:.0}) {d:.0} m"));
            }
        }
        if !hits.is_empty() {
            found += 1;
        }
        eprintln!("{name}: {} of 5: {}", hits.len(), hits.join("; "));
    }
    assert!(
        found * 2 >= froms.len(),
        "a patch turned up from {found} of {} places",
        froms.len()
    );
}

#[test]
#[ignore = "needs the real server data (AA_COMPACT, AA_SERVER_DATA)"]
fn most_seeds_cost_labor_out_in_the_wild() {
    let Some(w) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let crops = w.farm().crops();
    let costs: Vec<u16> = crops
        .iter()
        .filter(|c| c.plantable_on_land())
        .map(|c| w.skill(w.item_use_skill(c.seed)).map_or(0, |s| s.labor))
        .collect();
    let paid = costs.iter().filter(|&&c| c > 0).count();
    eprintln!(
        "{} plantable crops, {paid} cost labor (most {:?})",
        costs.len(),
        costs.iter().max()
    );
    assert!(paid * 2 > costs.len(), "{paid} of {}", costs.len());
}
