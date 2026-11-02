//! Debug helper: list or extract files of a game_pak.
//!
//! `pakls <pak> <prefix> [--extract <name> <out-file>] [--count]`

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "usage: pakls <pak> <prefix> [--extract <name> <out>] [--count]"
    );
    let prefix = args[1].to_lowercase();
    let t = std::time::Instant::now();
    let mut pak =
        aa_bake::pak::Pak::open_filtered(Path::new(&args[0]), |n| n.starts_with(&prefix))?;
    eprintln!(
        "{} entries under {prefix:?} in {:?}",
        pak.len(),
        t.elapsed()
    );
    if let Some(i) = args.iter().position(|a| a == "--extract") {
        let bytes = pak
            .read(&args[i + 1])?
            .ok_or_else(|| anyhow::anyhow!("not in pak"))?;
        std::fs::write(&args[i + 2], &bytes)?;
        eprintln!("wrote {} bytes", bytes.len());
    } else if !args.iter().any(|a| a == "--count") {
        for n in pak.names_with_prefix(&prefix) {
            let e = pak.entry(n).unwrap();
            println!("{n}\t{}", e.size);
        }
    }
    Ok(())
}
