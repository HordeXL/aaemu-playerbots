//! Builds the sea map from the real baked height grid and plans routes on it.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> AA_BAKED=<data dir> \
//!   cargo test -p aa-world --test water_real -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use aa_world::water::{find_shore, route, RouteCfg, RouteError};
use aa_world::{World, WorldPaths};

fn load() -> Option<World> {
    let compact = PathBuf::from(std::env::var_os("AA_COMPACT")?);
    let server_data = PathBuf::from(std::env::var_os("AA_SERVER_DATA")?);
    let baked = PathBuf::from(std::env::var_os("AA_BAKED")?);
    let _ = tracing_subscriber::fmt()
        .with_env_filter("aa_world=info")
        .with_test_writer()
        .try_init();
    Some(
        World::load(&WorldPaths {
            compact,
            server_data,
            baked: Some(baked),
            world: "main_world".into(),
        })
        .expect("load main_world"),
    )
}

/// Points the server's own test client launches boats from (aaemu-testclient `busy.rs`).
const KNOWN_WATER: [(f32, f32); 2] = [(15458.0, 10366.0), (15331.0, 10494.0)];

#[test]
#[ignore = "needs AA_COMPACT, AA_SERVER_DATA and AA_BAKED (real data)"]
fn the_real_sea_map_and_routes() {
    let Some(w) = load() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA / AA_BAKED not set; skipping");
        return;
    };
    let t = Instant::now();
    let map = w.water().expect("baked terrain");
    println!(
        "water map: {} x {} cells, {} sea cells, {} KiB, built in {:?}",
        map.cols(),
        map.rows(),
        map.sea_count(),
        map.bytes() / 1024,
        t.elapsed()
    );
    assert!(map.bytes() < 800_000);
    assert!(map.sea_count() > 100_000);
    // built once: the second call is the same map
    assert!(std::ptr::eq(map, w.water().unwrap()));

    for (x, y) in KNOWN_WATER {
        assert!(map.sea_at(x, y), "the known launch water ({x}, {y}) is sea");
    }

    // a route between the two known water points
    let cfg = RouteCfg::default();
    let t = Instant::now();
    let r = route(map, KNOWN_WATER[0], KNOWN_WATER[1], &cfg).expect("route between known water");
    println!(
        "route {:?} -> {:?}: {} points, {:.0} m, {} cells reached, {:?}",
        KNOWN_WATER[0],
        KNOWN_WATER[1],
        r.points.len(),
        r.length,
        r.reached,
        t.elapsed()
    );

    // a longer one: 2 km north-west along open water, found by walking out from the first point
    let ground = |x: f32, y: f32| w.ground_z(x, y);
    let mut found = 0;
    for dist in [1000.0f32, 2000.0, 3000.0] {
        for a in 0..16 {
            let ang = a as f32 * std::f32::consts::TAU / 16.0;
            let to = (
                KNOWN_WATER[0].0 + ang.cos() * dist,
                KNOWN_WATER[0].1 + ang.sin() * dist,
            );
            let t = Instant::now();
            match route(map, KNOWN_WATER[0], to, &cfg) {
                Ok(r) => {
                    found += 1;
                    println!(
                        "  {dist:>5.0} m at {:>3.0} deg: {:>3} points, {:>6.0} m, {:>5} cells, {:?}",
                        ang.to_degrees(),
                        r.points.len(),
                        r.length,
                        r.reached,
                        t.elapsed()
                    );
                }
                Err(RouteError::GoalBlocked) => {}
                Err(e) => println!("  {dist:>5.0} m at {:>3.0} deg: {e:?}", ang.to_degrees()),
            }
        }
    }
    assert!(found > 0, "some route over open water");

    // launch spots near the known water
    for (x, y) in KNOWN_WATER {
        let t = Instant::now();
        let s = find_shore(map, &ground, (x, y), 1500.0, &[]);
        println!("shore near ({x}, {y}): {s:?} in {:?}", t.elapsed());
    }
}
