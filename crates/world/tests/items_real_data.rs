//! Checks the item trade facts against the real `items` table of the server.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   cargo test -p aa-world --test items_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use aa_world::items::bind;
use aa_world::{World, WorldPaths};

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
fn item_facts_of_known_items() {
    let Some(paths) = paths() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let w = World::load(&paths).expect("load main_world");

    // Copper Ore: a plain, sellable, unbound material that stacks to 1000.
    let ore = w.item_facts(3411).expect("copper ore");
    assert_eq!(w.item_name(3411), Some("Copper Ore"));
    assert_eq!((ore.price, ore.refund, ore.max_stack), (40, 20, 1000));
    assert_eq!(ore.bind, bind::NORMAL);
    assert_eq!(ore.auction_cat, [7, 24, 36]);
    assert_eq!(ore.level, 1);
    assert!(ore.sellable && ore.tradable());

    assert!(w.item_facts(0).is_none());
    assert!(w.item_facts(u32::MAX).is_none());

    // The table covers every template the name table has, and the binding kinds are the ones
    // the server defines (1..=6).
    let table = w.item_table();
    let mut tradable = 0usize;
    let mut kinds = [0usize; 8];
    for (id, f) in table.iter() {
        assert!(w.item_name(id).is_some(), "no name for item {id}");
        assert!((1..=6).contains(&f.bind), "item {id}: bind {}", f.bind);
        kinds[usize::from(f.bind)] += 1;
        tradable += usize::from(f.tradable());
    }
    assert_eq!(table.len(), kinds.iter().sum::<usize>());
    assert!(tradable > 1000 && tradable < table.len(), "{tradable}");

    println!(
        "item facts: {} templates, {} tradable, bind kinds {:?}, {} KiB of heap ({} bytes per item)",
        table.len(),
        tradable,
        &kinds[1..=6],
        table.bytes() / 1024,
        table.bytes() / table.len()
    );
}
