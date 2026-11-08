//! The explore planner against the real `main_world` data (ignored by default): the landmarks the
//! world gives and the trips a new bot plans from each race's start, level by level.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> AA_BAKED=<data dir> \
//!   cargo test -p playerbots --test explore_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aa_world::landmarks::LandmarkKind;
use aa_world::{World, WorldPaths};
use playerbots::bot::goals::explore::plan::{self, Env, NoTrip, Query, Why, WorldChecks};
use playerbots::journal::Seen;

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

/// The world's checks, no portals and no war (the goal adds both from the bot's context).
struct Checks<'w>(WorldChecks<'w>);

impl Env for Checks<'_> {
    fn at_war(&mut self, _x: f32, _y: f32) -> bool {
        false
    }
    fn unreachable(&mut self, from: [f32; 2], to: [f32; 2]) -> bool {
        self.0.cut_off(from, to)
    }
    fn dangerous(&mut self, from: [f32; 2], to: [f32; 2]) -> bool {
        self.0.dangerous(from, to)
    }
}

fn zone_name(w: &World, key: Option<u16>) -> String {
    key.and_then(|k| w.zone_name(k))
        .map_or_else(|| format!("{key:?}"), str::to_string)
}

#[test]
#[ignore = "needs the real server data (AA_COMPACT, AA_SERVER_DATA, AA_BAKED)"]
fn the_world_has_landmarks_in_most_zones() {
    let Some(w) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let t = std::time::Instant::now();
    let lm = w.landmarks();
    let kinds = |k: LandmarkKind| lm.all().iter().filter(|m| m.kind == k).count();
    eprintln!(
        "landmarks: {} zones, {} marks ({} centres, {} hills, {} coasts, {} recall points), {} KiB, built in {:?}",
        lm.zones().len(),
        lm.all().len(),
        kinds(LandmarkKind::Centre),
        kinds(LandmarkKind::Hill),
        kinds(LandmarkKind::Coast),
        kinds(LandmarkKind::Recall),
        lm.bytes() / 1024,
        t.elapsed()
    );
    if w.baked().zones.is_none() {
        eprintln!("no baked zone grid (AA_BAKED): no landmarks");
        return;
    }
    assert!(lm.zones().len() > 30, "{} zones", lm.zones().len());
    assert!(kinds(LandmarkKind::Recall) > 10);
    if w.baked().terrain.is_some() {
        assert!(kinds(LandmarkKind::Hill) > 5);
        assert!(kinds(LandmarkKind::Coast) > 5);
        // every landmark of the terrain's kinds stands on dry land
        for m in lm.all().iter().filter(|m| m.kind != LandmarkKind::Recall) {
            let z = w.ground_z(m.pos[0], m.pos[1]).unwrap_or(0.0);
            assert!(z >= 99.0, "{m:?} at {z}");
        }
    }
}

#[test]
#[ignore = "needs the real server data (AA_COMPACT, AA_SERVER_DATA, AA_BAKED)"]
fn a_new_bot_plans_sensible_trips_from_its_start() {
    let Some(w) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let lm = w.landmarks();
    if lm.all().is_empty() {
        eprintln!("no landmarks (no baked zone grid); skipped");
        return;
    }
    let mut rng = fastrand::Rng::with_seed(11);
    let mut planned = 0;
    for race in 1..=8u8 {
        let (Some(start), Some(faction)) = (w.race_start(race), w.race_faction(race)) else {
            continue;
        };
        let me = [start.x, start.y, start.z];
        let home = w.zone_key_at(me[0], me[1]);
        for level in [1u8, 10, 20, 30] {
            let mut seen = Seen::default();
            if let Some(k) = home {
                seen.add_zone(k);
            }
            let checks = WorldChecks {
                world: &w,
                faction,
                level,
                level_gap: 5,
            };
            let q = Query {
                me,
                max_trip_m: 6_000.0,
                now_unix: 1_000_000,
                seen: &seen,
                zone: None,
            };
            let novel = plan::novelty(lm, &q, 50);
            let mut trips = Vec::new();
            for _ in 0..4 {
                match plan::choose(lm, &q, &mut Checks(checks), &mut rng) {
                    Ok(p) => {
                        assert!(p.dist >= plan::MIN_TRIP_M && p.dist <= 6_000.0, "{p:?}");
                        assert!(!checks.dangerous([me[0], me[1]], [p.pos[0], p.pos[1]]));
                        assert!(!checks.cut_off([me[0], me[1]], [p.pos[0], p.pos[1]]));
                        trips.push(format!(
                            "{} {} {:.1} km ({})",
                            p.kind.map_or("point", |k| k.name()),
                            if p.name.is_empty() {
                                zone_name(&w, p.zone_key)
                            } else {
                                p.name.to_string()
                            },
                            p.dist / 1000.0,
                            p.why.name()
                        ));
                        if level == 1 {
                            assert_ne!(p.why, Why::Again, "a new bot has new places around");
                        }
                        planned += 1;
                    }
                    Err(e) => {
                        assert!(
                            matches!(e, NoTrip::AllUnsafe | NoTrip::NothingNear),
                            "{e:?}"
                        );
                        trips.push(format!("none: {}", e.text()));
                    }
                }
            }
            eprintln!(
                "race {race} level {level:>2} from {} ({novel} new places within 6 km): {}",
                zone_name(&w, home),
                trips.join("; ")
            );
        }
    }
    assert!(planned > 0, "no race start planned a trip");
}
