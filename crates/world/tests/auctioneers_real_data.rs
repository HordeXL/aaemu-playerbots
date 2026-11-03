//! Checks the auctioneer list against the real server data.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   cargo test -p aa-world --test auctioneers_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use aa_world::{Vec3, World, WorldPaths};

fn paths() -> Option<WorldPaths> {
    Some(WorldPaths {
        compact: PathBuf::from(std::env::var_os("AA_COMPACT")?),
        server_data: PathBuf::from(std::env::var_os("AA_SERVER_DATA")?),
        baked: None,
        world: "main_world".into(),
    })
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn the_auctioneers_of_the_main_world() {
    let Some(paths) = paths() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let w = World::load(&paths).expect("load main_world");
    let all = w.auctioneers();
    println!(
        "{} auctioneer spawns of {} templates",
        all.len(),
        all.all()
            .iter()
            .map(|a| a.template)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    assert!(all.len() > 100, "{}", all.len());
    // the NPC next to every Mirage portal
    assert!(all.all().iter().any(|a| a.template == 10857));

    // Marianople: the capital's auctioneers around (11240, 12110)
    let marianople = Vec3::new(11240.0, 12112.0, 145.0);
    let near = w
        .nearest_auctioneer(marianople, 500.0, |_| false)
        .expect("an auctioneer near Marianople");
    println!("nearest to Marianople: {near:?}");
    assert!(near.pos.dist2d(marianople) < 40.0);
    // a skip rule moves on to the next one
    let other = w
        .nearest_auctioneer(marianople, 5000.0, |a| a.pos == near.pos)
        .expect("another auctioneer");
    assert_ne!(other.pos, near.pos);
    // nothing at the bottom of the sea
    assert!(w
        .nearest_auctioneer(Vec3::new(1.0, 1.0, 0.0), 100.0, |_| false)
        .is_none());
}

/// The server matches a search keyword against `localized_texts` in lower case; the bots search by
/// the exact English name `World::item_name` gives them, so it has to be one name per template and
/// free of whitespace the server would not match.
#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn item_names_are_safe_exact_search_keywords() {
    let Some(paths) = paths() else {
        return;
    };
    let w = World::load(&paths).expect("load main_world");
    let mut odd = 0;
    for (id, f) in w.item_table().iter() {
        if !f.tradable() {
            continue;
        }
        let name = w.item_name(id).expect("a name");
        if name.trim() != name || name.is_empty() || name.contains("  ") {
            odd += 1;
            if odd <= 5 {
                println!("odd name for item {id}: {name:?}");
            }
        }
    }
    println!("{odd} tradable items with odd names");
    assert!(odd < 50, "{odd}");
}
