//! Bakes the navigation meshes from the real client pak.
//!
//! ```text
//! AA_PAK=<client>/game_pak [AA_NAV_OUT=<scratch dir>] cargo test --release -p aa-bake --test nav_real \
//!     -- --ignored --nocapture
//! ```
//!
//! The output goes to `AA_NAV_OUT` (kept) or, when unset, to a temporary directory next to the
//! system temp dir that is deleted afterwards.

use std::path::PathBuf;
use std::time::Instant;

use aa_bake::{bake_steps, BakeOptions, BakeSteps};

#[test]
#[ignore = "needs AA_PAK (the client game_pak)"]
fn bakes_every_navigation_mesh_of_the_pak() {
    let Some(pak) = std::env::var_os("AA_PAK").map(PathBuf::from) else {
        eprintln!("AA_PAK not set; skipping");
        return;
    };
    let keep = std::env::var_os("AA_NAV_OUT").map(PathBuf::from);
    let out = keep.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("aa-bake-nav-{}", std::process::id()))
    });
    std::fs::create_dir_all(&out).unwrap();

    let mut steps = BakeSteps::none();
    steps.nav = true;
    steps.nav_instances = true;
    let t = Instant::now();
    let report = bake_steps(&BakeOptions::new(&pak, &out, "main_world"), &steps).unwrap();
    println!("{report}");
    println!("baked in {:?}", t.elapsed());

    let main = report
        .nav
        .iter()
        .find(|n| n.world == "main_world")
        .expect("main_world mesh");
    assert!(
        main.bytes <= 250_000_000,
        "size budget: {} bytes",
        main.bytes
    );
    assert!(main.assemble.nodes_kept > 3_000_000);
    assert!(main.assemble.scc_largest > 1_500_000);
    assert_eq!(main.files_failed, 0);
    let instances: Vec<&str> = report
        .nav
        .iter()
        .map(|n| n.world.as_str())
        .filter(|w| *w != "main_world")
        .collect();
    println!("instance worlds: {instances:?}");
    for w in [
        "instance_cuttingwind_deadmine",
        "instance_nachashgar",
        "instance_library",
        "arche_mall_world",
    ] {
        assert!(instances.contains(&w), "{w} missing");
    }
    assert!(
        report.checks.iter().all(|c| c.ok),
        "failed checks: {:?}",
        report.checks.iter().filter(|c| !c.ok).collect::<Vec<_>>()
    );
    let instance_bytes: u64 = report
        .nav
        .iter()
        .filter(|n| n.world != "main_world")
        .map(|n| n.bytes)
        .sum();
    println!(
        "instance meshes together: {:.1} MB",
        instance_bytes as f64 / 1e6
    );
    assert!(instance_bytes < 40_000_000);

    if keep.is_none() {
        std::fs::remove_dir_all(&out).unwrap();
    }
}
