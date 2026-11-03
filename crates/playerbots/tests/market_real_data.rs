//! The provenance ledger against the real item table: what a bot may offer for sale.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   cargo test -p playerbots --test market_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use aa_world::items::bind;
use aa_world::{World, WorldPaths};
use playerbots::bot::market::ledger::{reserve, RESERVE};
use playerbots::bot::market::MarketState;
use playerbots::bot::state::Item;
use rustc_hash::FxHashMap;

fn world() -> Option<World> {
    let paths = WorldPaths {
        compact: PathBuf::from(std::env::var_os("AA_COMPACT")?),
        server_data: PathBuf::from(std::env::var_os("AA_SERVER_DATA")?),
        baked: None,
        world: "main_world".into(),
    };
    Some(World::load(&paths).expect("load main_world"))
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn the_surplus_follows_the_real_item_table() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // copper ore: sellable and unbound
    const ORE: u32 = 3411;
    // something that binds when picked up, and a consumable that can be traded
    let bound = world
        .item_table()
        .iter()
        .find(|(_, f)| f.sellable && f.bind == bind::ON_PICKUP)
        .map(|(id, _)| id)
        .expect("a sellable bind-on-pickup item");
    let consumable = world
        .item_table()
        .iter()
        .find(|(id, f)| f.tradable() && world.item_use_skill(*id) != 0)
        .map(|(id, _)| id)
        .expect("a tradable item that casts a skill");
    assert_eq!(reserve(&world, consumable), RESERVE);

    let mut market = MarketState::default();
    {
        let ledger = &mut market.inner_mut().ledger;
        ledger.add(ORE, 50);
        ledger.add(bound, 50);
        ledger.add(consumable, 50);
        ledger.add(0xFFFF_FFF0, 50); // a template the table does not know
    }
    let bag: FxHashMap<u16, Item> = [ORE, bound, consumable, 0xFFFF_FFF0]
        .into_iter()
        .enumerate()
        .map(|(i, template)| {
            (
                i as u16,
                Item {
                    id: 100 + i as u64,
                    template,
                    count: 70,
                },
            )
        })
        .collect();
    let got: Vec<(u32, u32)> = market.sellable_surplus(&world, &bag).collect();
    // only the tradable ones, the clean part (50 of 70), less the reserve of what can be used
    assert_eq!(
        got,
        [(ORE, 50 - reserve(&world, ORE)), (consumable, 50 - RESERVE)],
        "bound {bound}, consumable {consumable}"
    );
    println!("surplus: {got:?} (bound item {bound}, consumable {consumable})");
}
