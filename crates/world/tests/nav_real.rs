//! Path finding on the real baked navigation meshes. Run in release mode for meaningful timings:
//!
//! ```text
//! AA_BAKED=<data dir with nav_*.bin> cargo test --release -p aa-world --test nav_real \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! (`playerbots bake` or `cargo run -p aa-bake --release --example bake -- --only nav,nav_instances`
//! writes the files.)

use std::path::PathBuf;
use std::time::Instant;

use aa_world::nav::{NavMesh, NodeRef, PathQuery, PathResult};
use aa_world::Vec3;

fn baked_dir() -> Option<PathBuf> {
    std::env::var_os("AA_BAKED").map(PathBuf::from)
}

fn open(name: &str) -> Option<NavMesh> {
    let dir = baked_dir()?;
    let path = dir.join(name);
    if !path.is_file() {
        eprintln!("{} not found; skipping", path.display());
        return None;
    }
    Some(NavMesh::open(&path).unwrap_or_else(|e| panic!("{e:#}")))
}

fn rss_mb() -> f64 {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(pid) = sysinfo::get_current_pid() else {
        return 0.0;
    };
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map_or(0.0, |p| p.memory() as f64 / 1e6)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn describe(r: &PathResult) -> String {
    match r {
        PathResult::Found(p) => format!(
            "Found: {} points, {:.1} m, {} expansions",
            p.points.len(),
            p.length,
            p.expansions
        ),
        PathResult::Partial(p) => format!(
            "Partial: {} points, {:.1} m, {} expansions",
            p.points.len(),
            p.length,
            p.expansions
        ),
        other => format!("{other:?}"),
    }
}

/// Every sample along the walkable (non-indoor) segments lies on a triangle.
fn off_mesh_samples(mesh: &NavMesh, path: &aa_world::nav::Path) -> usize {
    let mut bad = 0;
    for (i, w) in path.points.windows(2).enumerate() {
        if path.indoor[i] {
            continue;
        }
        let len = w[0].dist2d(w[1]);
        let steps = (len / 0.25).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let p = Vec3::new(
                w[0].x + (w[1].x - w[0].x) * t,
                w[0].y + (w[1].y - w[0].y) * t,
                w[0].z + (w[1].z - w[0].z) * t,
            );
            // the segments of the first / last piece start at the query's own z; look at x/y
            // 5 cm of slack: strings hug triangle edges and f32 positions at x = 15 000 are
            // only good to ~1 mm
            let ok = mesh.locate(p, 0.05).is_some();
            if !ok {
                bad += 1;
            }
        }
    }
    bad
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
#[ignore = "needs AA_BAKED with nav_main_world.bin"]
fn main_world_open_and_known_routes() {
    let rss0 = rss_mb();
    let t = Instant::now();
    let Some(mesh) = open("nav_main_world.bin") else {
        return;
    };
    println!(
        "open: {:?}; file {:.1} MB, {} nodes, {} vertices, {} links, {} components; rss {:.1} -> {:.1} MB",
        t.elapsed(),
        mesh.file_bytes() as f64 / 1e6,
        mesh.node_count(),
        mesh.vertex_count(),
        mesh.link_count(),
        mesh.component_count(),
        rss0,
        rss_mb()
    );
    assert!(mesh.file_bytes() <= 250_000_000, "size budget");

    let start = Vec3::new(15578.0, 15382.0, 126.0);
    let loc = mesh
        .locate_detailed(start, 8.0)
        .expect("Nuian start on the mesh");
    println!("Nuian start: {loc:?}");
    for (name, to) in [
        ("Desireen", Vec3::new(15667.9, 15166.7, 120.7)),
        ("Wardton", Vec3::new(14809.2, 14295.8, 119.9)),
    ] {
        for w in [1.0f32, 1.5, 2.0] {
            let mut q = PathQuery::new(start, to);
            q.weight = w;
            q.max_expansions = 500_000;
            let t = Instant::now();
            let r = mesh.find_path(&q);
            println!("{name} w={w}: {} in {:?}", describe(&r), t.elapsed());
            let p = r.path().unwrap_or_else(|| panic!("{name}: {r:?}"));
            assert!(matches!(r, PathResult::Found(_)), "{name}");
            assert_eq!(p.points[0], start);
            assert_eq!(*p.points.last().unwrap(), to);
            assert_eq!(
                off_mesh_samples(&mesh, p),
                0,
                "{name} w={w}: the string leaves the mesh"
            );
        }
    }
    println!("rss after the queries {:.1} MB", rss_mb());
}

#[test]
#[ignore = "needs AA_BAKED with nav_main_world.bin"]
fn main_world_random_pairs_timing() {
    let Some(mesh) = open("nav_main_world.bin") else {
        return;
    };
    let n = mesh.node_count();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // pairs inside the largest strongly connected component, 50 m .. 2 km apart
    let mut pairs: Vec<(NodeRef, NodeRef, f32)> = Vec::new();
    let mut tries = 0;
    while pairs.len() < 1000 && tries < 5_000_000 {
        tries += 1;
        let a = NodeRef(rng.below(n) as u32);
        if mesh.component_of(a) != 0 {
            continue;
        }
        let pa = mesh.node_pos(a);
        // a second node nearby in space: any node of the component within the distance band
        // (random probing in a window of ids, which are Z-order ranked)
        let lo = (a.0 as usize).saturating_sub(200_000);
        let hi = (a.0 as usize + 200_000).min(n);
        let b = NodeRef((lo + rng.below(hi - lo)) as u32);
        if mesh.component_of(b) != 0 {
            continue;
        }
        let d = pa.dist2d(mesh.node_pos(b));
        if (50.0..=2000.0).contains(&d) {
            pairs.push((a, b, d));
        }
    }
    println!("{} pairs in {tries} tries", pairs.len());
    assert!(pairs.len() >= 900);

    let rss_before = rss_mb();
    for w in [1.5f32, 2.0, 1.0] {
        let (mut times, mut exps, mut per_m) = (Vec::new(), Vec::new(), Vec::new());
        let (mut found, mut partial, mut other) = (0, 0, 0);
        let (mut length_ratio, mut off_mesh) = (Vec::new(), 0usize);
        let mut partials: Vec<(NodeRef, NodeRef, f32)> = Vec::new();
        let sample = if w == 1.0 { 150 } else { pairs.len() };
        for &(a, b, d) in pairs.iter().take(sample) {
            let (pa, pb) = (mesh.node_pos(a), mesh.node_pos(b));
            let mut q = PathQuery::new(pa, pb);
            q.weight = w;
            q.max_expansions = 300_000;
            let t = Instant::now();
            let r = mesh.find_path(&q);
            let dt = t.elapsed().as_secs_f64() * 1e3;
            match &r {
                PathResult::Found(p) => {
                    found += 1;
                    times.push(dt);
                    exps.push(f64::from(p.expansions));
                    per_m.push(f64::from(p.expansions) / f64::from(d));
                    length_ratio.push(f64::from(p.length) / f64::from(d));
                    if found <= 60 {
                        off_mesh += off_mesh_samples(&mesh, p);
                    }
                }
                PathResult::Partial(_) => {
                    partial += 1;
                    if w == 1.5 && partials.len() < 6 {
                        partials.push((a, b, d));
                    }
                }
                _ => other += 1,
            }
        }
        times.sort_by(f64::total_cmp);
        exps.sort_by(f64::total_cmp);
        per_m.sort_by(f64::total_cmp);
        length_ratio.sort_by(f64::total_cmp);
        println!(
            "w={w}: found {found}, partial {partial}, other {other} | ms p50 {:.2} p90 {:.2} p99 {:.2} max {:.2} | expansions p50 {:.0} p99 {:.0} max {:.0} | per m p50 {:.1} | length/straight p50 {:.2} p90 {:.2} p99 {:.2} | off-mesh samples (60 paths) {off_mesh}",
            pct(&times, 0.5),
            pct(&times, 0.9),
            pct(&times, 0.99),
            times.last().copied().unwrap_or(0.0),
            pct(&exps, 0.5),
            pct(&exps, 0.99),
            exps.last().copied().unwrap_or(0.0),
            pct(&per_m, 0.5),
            pct(&length_ratio, 0.5),
            pct(&length_ratio, 0.9),
            pct(&length_ratio, 0.99),
        );
        for (a, b, d) in partials {
            let mut q = PathQuery::new(mesh.node_pos(a), mesh.node_pos(b));
            q.max_expansions = 3_000_000;
            let t = Instant::now();
            let r = mesh.find_path(&q);
            println!(
                "  partial pair {} -> {} ({d:.0} m straight): with 3M expansions {} in {:?}",
                a.0,
                b.0,
                describe(&r),
                t.elapsed()
            );
        }
        assert_eq!(off_mesh, 0);
    }
    println!(
        "scratch of this thread: {:.2} MB; rss {:.1} -> {:.1} MB",
        aa_world::nav::thread_scratch_bytes() as f64 / 1e6,
        rss_before,
        rss_mb()
    );
}

#[test]
#[ignore = "needs AA_BAKED with nav_main_world.bin"]
fn main_world_unreachable_is_fast_and_towns_work() {
    let Some(mesh) = open("nav_main_world.bin") else {
        return;
    };
    // a pair in different weakly connected pieces
    let n = mesh.node_count();
    let a = NodeRef(
        (0..n as u32)
            .find(|&i| mesh.component_of(NodeRef(i)) == 0)
            .unwrap(),
    );
    let b = (0..n as u32)
        .map(NodeRef)
        .find(|&b| mesh.may_reach(a, b) == Some(false))
        .expect("a node the largest component cannot reach");
    let t = Instant::now();
    let r = mesh.find_path(&PathQuery::new(mesh.node_pos(a), mesh.node_pos(b)));
    let dt = t.elapsed();
    println!("unreachable pair: {r:?} in {dt:?}");
    assert_eq!(r, PathResult::Unreachable);
    assert!(dt.as_millis() < 5, "{dt:?}");

    // inside towns (waypoints, stairs, narrow streets) between NPC spawn positions
    let mut found = 0;
    for (from, to) in [
        ((13590.59, 10465.689, 242.8), (13626.48, 10429.55, 245.9)),
        ((13827.33, 15887.25, 237.47), (13800.6, 15828.05, 224.0)),
        ((15005.72, 22905.09, 101.6), (15109.0, 22983.0, 104.24)),
        // the second spawn stands in a pocket (a 159 node component) the main part has no way into
        ((8301.0, 10695.0, 247.86), (8320.07, 10795.01, 247.99)),
    ] {
        let (from, to) = (
            Vec3::new(from.0, from.1, from.2),
            Vec3::new(to.0, to.1, to.2),
        );
        let q = PathQuery::new(from, to);
        let t = Instant::now();
        let r = mesh.find_path(&q);
        println!(
            "town {from:?} -> {to:?}: {} in {:?}",
            describe(&r),
            t.elapsed()
        );
        if let PathResult::Found(p) = &r {
            found += 1;
            let indoor = p.indoor.iter().filter(|&&i| i).count();
            println!("  {} segments, {indoor} indoor", p.indoor.len());
            assert_eq!(off_mesh_samples(&mesh, p), 0);
            // every corner is on (or within the agent radius of) the mesh
            for c in &p.points {
                assert!(
                    mesh.locate(*c, 1.0).is_some(),
                    "corner {c:?} is off the mesh"
                );
            }
        }
    }
    assert!(found >= 3, "only {found} of 4 town routes found");
}

#[test]
#[ignore = "needs AA_BAKED with the instance navigation meshes"]
fn instance_worlds_open_and_deadmine_route() {
    let Some(dir) = baked_dir() else {
        return;
    };
    let mut total = 0u64;
    let mut names = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("nav_") && name.ends_with(".bin") && name != "nav_main_world.bin" {
            names.push(name);
        }
    }
    names.sort();
    assert!(!names.is_empty(), "no instance meshes in {}", dir.display());
    for name in &names {
        let t = Instant::now();
        let m = NavMesh::open(&dir.join(name)).unwrap();
        m.verify().unwrap();
        total += m.file_bytes() as u64;
        let largest = m.component_size(0);
        println!(
            "{name}: {:.2} MB, {} nodes, {} links, {} components (largest {}), checked in {:?}",
            m.file_bytes() as f64 / 1e6,
            m.node_count(),
            m.link_count(),
            m.component_count(),
            largest,
            t.elapsed()
        );
    }
    println!("instance meshes together: {:.1} MB", total as f64 / 1e6);

    if let Some(m) = open("nav_instance_cuttingwind_deadmine.bin") {
        let from = Vec3::new(796.3, 325.4, 249.5);
        let to = Vec3::new(614.4, 423.4, 153.7);
        let lf = m.locate_detailed(from, 8.0);
        let lt = m.locate_detailed(to, 8.0);
        println!("deadmine spawn: {lf:?}\n deadmine target: {lt:?}");
        let mut q = PathQuery::new(from, to);
        q.max_expansions = 200_000;
        let r = m.find_path(&q);
        println!("deadmine {from:?} -> {to:?}: {}", describe(&r));
        // the spawn is outside the waypoint network: how far is the nearest node?
        for snap in [16.0f32, 32.0, 64.0, 128.0] {
            if let Some(l) = m.locate_detailed(from, snap) {
                println!("  nearest node to the spawn within {snap} m: {l:?}");
                let mut q = PathQuery::new(from, to);
                q.snap_dist = snap;
                println!("  path with snap {snap}: {}", describe(&m.find_path(&q)));
                break;
            }
        }
        if let (Some(a), Some(b)) = (lf, lt) {
            println!(
                "components {} / {} (sizes {} / {}), may_reach {:?}",
                m.component_of(a.node),
                m.component_of(b.node),
                m.component_size(m.component_of(a.node)),
                m.component_size(m.component_of(b.node)),
                m.may_reach(a.node, b.node)
            );
        }
        if let Some(p) = r.path() {
            println!("  corners: {:?}", p.points);
        }
    }
}

/// Share of the walkable path samples that lie on a `ROAD` triangle.
fn road_share(mesh: &NavMesh, path: &aa_world::nav::Path) -> f64 {
    let (mut on, mut all) = (0usize, 0usize);
    for (i, w) in path.points.windows(2).enumerate() {
        if path.indoor[i] {
            continue;
        }
        let steps = (w[0].dist2d(w[1]) / 2.0).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let p = Vec3::new(
                w[0].x + (w[1].x - w[0].x) * t,
                w[0].y + (w[1].y - w[0].y) * t,
                w[0].z + (w[1].z - w[0].z) * t,
            );
            if let Some(l) = mesh.locate_detailed(p, 0.5) {
                all += 1;
                if mesh.node_flags(l.node) & 2 != 0 {
                    on += 1;
                }
            }
        }
    }
    on as f64 / all.max(1) as f64
}

#[test]
#[ignore = "needs AA_BAKED with nav_main_world.bin"]
fn main_world_roads_are_flagged_and_preferred() {
    let Some(mesh) = open("nav_main_world.bin") else {
        return;
    };
    let roads = (0..mesh.node_count() as u32)
        .filter(|&n| mesh.node_flags(NodeRef(n)) & 2 != 0)
        .count();
    println!(
        "road nodes: {roads} of {} ({:.2} %)",
        mesh.node_count(),
        roads as f64 * 100.0 / mesh.node_count() as f64
    );
    assert!(roads > 10_000, "only {roads} road nodes");

    // pairs of road triangles 300 m .. 1.5 km apart: the biased search should stay on roads
    // more, at a small cost in length
    let road_nodes: Vec<u32> = (0..mesh.node_count() as u32)
        .filter(|&n| mesh.node_flags(NodeRef(n)) & 2 != 0)
        .collect();
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let (mut pairs, mut share_off, mut share_on, mut len_off, mut len_on) = (0, 0.0, 0.0, 0.0, 0.0);
    let (mut exp_off, mut exp_on) = (0u64, 0u64);
    let mut tries = 0;
    while pairs < 60 && tries < 5000 {
        tries += 1;
        let a = NodeRef(road_nodes[rng.below(road_nodes.len())]);
        let b = NodeRef(road_nodes[rng.below(road_nodes.len())]);
        let (pa, pb) = (mesh.node_pos(a), mesh.node_pos(b));
        let d = pa.dist2d(pb);
        if !(300.0..=1500.0).contains(&d) || !mesh.same_component(a, b) {
            continue;
        }
        let mut off = PathQuery::new(pa, pb);
        off.road_bias = 1.0;
        let (PathResult::Found(x), PathResult::Found(y)) = (
            mesh.find_path(&off),
            mesh.find_path(&PathQuery::new(pa, pb)),
        ) else {
            continue;
        };
        pairs += 1;
        share_off += road_share(&mesh, &x);
        share_on += road_share(&mesh, &y);
        len_off += f64::from(x.length);
        len_on += f64::from(y.length);
        exp_off += u64::from(x.expansions);
        exp_on += u64::from(y.expansions);
    }
    assert!(pairs >= 20, "only {pairs} road pairs");
    let n = f64::from(pairs);
    println!(
        "{pairs} road pairs: bias off {:.1} % on roads, {:.0} m, {:.0} exp; bias on {:.1} % on roads, {:.0} m, {:.0} exp",
        share_off / n * 100.0,
        len_off / n,
        exp_off as f64 / n,
        share_on / n * 100.0,
        len_on / n,
        exp_on as f64 / n
    );
    assert!(share_on > share_off, "the bias must raise the road share");
    assert!(len_on <= len_off * 1.2, "detours too long");
}
