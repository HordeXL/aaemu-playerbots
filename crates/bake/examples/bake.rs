//! Runs the bake from the command line.
//!
//! ```text
//! cargo run -p aa-bake --release --example bake -- --pak D:\AA1.2\game_pak --out D:\aaemu-playerbots-data \
//!     --world main_world --step 4 --compact <server>\app\Data\compact.sqlite3 [--only heights,zones,housing,spheres,map]
//! ```

use std::path::PathBuf;

use aa_bake::{bake_steps, BakeOptions, BakeSteps};

fn main() -> anyhow::Result<()> {
    let mut pak = None;
    let mut out = None;
    let mut world = "main_world".to_string();
    let mut step = 4.0f32;
    let mut compact = None;
    let mut only: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{name} needs a value"))
        };
        match a.as_str() {
            "--pak" => pak = Some(PathBuf::from(value("--pak")?)),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--world" => world = value("--world")?,
            "--step" => step = value("--step")?.parse()?,
            "--compact" => compact = Some(PathBuf::from(value("--compact")?)),
            "--only" => only = Some(value("--only")?),
            other => anyhow::bail!(
                "unknown argument {other}; usage: --pak <game_pak> --out <dir> [--world main_world] [--step 4] [--compact <compact.sqlite3>] [--only heights,zones,housing,spheres,map]"
            ),
        }
    }
    let opts = BakeOptions {
        pak: pak.ok_or_else(|| anyhow::anyhow!("--pak is required"))?,
        out: out.ok_or_else(|| anyhow::anyhow!("--out is required"))?,
        world,
        step,
        compact,
    };
    let mut steps = BakeSteps::all();
    if let Some(list) = only {
        steps = BakeSteps::none();
        for s in list.split(',') {
            match s.trim() {
                "heights" => steps.heights = true,
                "zones" => steps.zones = true,
                "housing" => steps.housing = true,
                "spheres" => steps.spheres = true,
                "map" => steps.map = true,
                other => anyhow::bail!("unknown step {other}"),
            }
        }
    }
    let started = std::time::Instant::now();
    let report = bake_steps(&opts, &steps)?;
    print!("{report}");
    println!("total: {:.1} s", started.elapsed().as_secs_f32());
    if report.checks.iter().any(|c| !c.ok) {
        std::process::exit(2);
    }
    Ok(())
}
