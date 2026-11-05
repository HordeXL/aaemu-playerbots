//! The provenance ledger against the real item table: what a bot may offer for sale.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   cargo test -p playerbots --test market_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use aa_proto::sc_activity::{quest_status, QuestRow};
use aa_world::items::bind;
use aa_world::{ObjKind, World, WorldPaths};
use playerbots::bot::goals::trade::{choose_offer, keep_of, GOODS};
use playerbots::bot::market::ledger::{keep, reserve, RESERVE};
use playerbots::bot::market::needs::QuestNeeds;
use playerbots::bot::market::MarketState;
use playerbots::bot::state::{Item, QuestLog};
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
    let needs = QuestNeeds::default();
    let got: Vec<(u32, u32)> = market.sellable_surplus(&world, &bag, &needs).collect();
    // only the tradable ones, the clean part (50 of 70), less the reserve of what can be used
    assert_eq!(
        got,
        [(ORE, 50 - reserve(&world, ORE)), (consumable, 50 - RESERVE)],
        "bound {bound}, consumable {consumable}"
    );
    println!("surplus: {got:?} (bound item {bound}, consumable {consumable})");
}

/// A quest log with `template` in progress.
fn log_with(template: u32) -> QuestLog {
    QuestLog {
        active: vec![QuestRow {
            id: i64::from(template),
            template,
            status: quest_status::PROGRESS,
            objectives: [0; 5],
            left_time_ms: -1,
            component: 0,
            acceptor_type: 0,
            acceptor_id: 0,
        }],
        completed: Vec::new(),
    }
}

fn bag_of(items: &[(u32, i32)]) -> FxHashMap<u16, Item> {
    items
        .iter()
        .enumerate()
        .map(|(i, &(template, count))| {
            (
                i as u16,
                Item {
                    id: 100 + i as u64,
                    template,
                    count,
                },
            )
        })
        .collect()
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn quest_items_are_not_for_sale_and_not_given_away() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // Dark Shadow Talisman: sellable and unbound in the data, but it exists for a quest
    const TALISMAN: u32 = 7716;
    const ORE: u32 = 3411;
    const HAY: u32 = 3712;
    let f = world.item_facts(TALISMAN).expect("the talisman");
    assert!(f.sellable && f.bind == bind::NORMAL && f.quest && !f.tradable());
    // the trade goods every bot mints and wants stay tradable
    for t in GOODS {
        assert!(world.item_facts(t).expect("a good").tradable(), "{t}");
    }

    let mut market = MarketState::default();
    {
        let ledger = &mut market.inner_mut().ledger;
        ledger.add(TALISMAN, 50);
        ledger.add(ORE, 50);
    }
    let bag = bag_of(&[(TALISMAN, 50), (ORE, 50)]);
    let none = QuestNeeds::default();
    let got: Vec<_> = market.sellable_surplus(&world, &bag, &none).collect();
    assert_eq!(got, [(ORE, 50 - reserve(&world, ORE))]);

    // random trades: neither the talisman nor an unknown item is offered, the ore is
    assert_eq!(keep_of(&world, &none, TALISMAN), None);
    assert_eq!(keep_of(&world, &none, 0xFFFF_FFF0), None);
    assert_eq!(keep_of(&world, &none, ORE), Some(0));
    let rule = |t: u32| keep_of(&world, &none, t);
    let only_talisman = bag_of(&[(TALISMAN, 50)]);
    let mut rng = fastrand::Rng::with_seed(3);
    assert_eq!(choose_offer(&only_talisman, 0, true, &rule, &mut rng), None);
    let p = choose_offer(&bag, 0, true, &rule, &mut rng).expect("the ore");
    assert_eq!(p.template, ORE);

    // Hay Bale is a gather target of quests, not a quest item: still tradable
    assert!(world.item_facts(HAY).expect("hay bale").tradable());
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn what_an_active_quest_gathers_is_kept_back() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // a quest that gathers more than the reserve of a tradable item
    let (quest, item, count) = world
        .quests()
        .find_map(|q| {
            let mut found = None;
            for step in q.progress.iter() {
                for o in step.objectives.iter() {
                    if let ObjKind::Gather { item, count, .. } = o.kind {
                        let tradable = world.item_facts(item).is_some_and(|f| f.tradable());
                        if tradable && (RESERVE + 1..=40).contains(&count) {
                            found = Some((q.id, item, count));
                        }
                    }
                }
            }
            found
        })
        .expect("a quest that gathers a tradable item");
    let needs = QuestNeeds::of(&world, &log_with(quest));
    assert!(needs.count(item) >= count, "quest {quest}, item {item}");
    assert_eq!(QuestNeeds::of(&world, &QuestLog::default()).count(item), 0);
    assert_eq!(
        keep(&world, &needs, item),
        reserve(&world, item).max(needs.count(item))
    );

    // 50 clean units: all but what the quest needs can go
    let mut market = MarketState::default();
    market.inner_mut().ledger.add(item, 50);
    let bag = bag_of(&[(item, 50)]);
    let got: Vec<_> = market.sellable_surplus(&world, &bag, &needs).collect();
    let kept = keep(&world, &needs, item);
    // (more than the reserve of a consumable: the quest's claim decides)
    assert!(kept > RESERVE && kept >= count);
    let expected: Vec<(u32, u32)> = if kept < 50 {
        vec![(item, 50 - kept)]
    } else {
        Vec::new()
    };
    assert_eq!(got, expected);
    // as many as it needs: nothing is for sale, and nothing is offered in a trade
    let mut market = MarketState::default();
    market.inner_mut().ledger.add(item, kept);
    let held = bag_of(&[(item, i32::try_from(kept).unwrap())]);
    assert_eq!(market.sellable_surplus(&world, &held, &needs).count(), 0);
    let rule = |t: u32| keep_of(&world, &needs, t);
    let mut rng = fastrand::Rng::with_seed(9);
    let trade_bag = bag_of(&[(item, i32::try_from(needs.count(item)).unwrap())]);
    assert_eq!(choose_offer(&trade_bag, 0, true, &rule, &mut rng), None);
    println!("quest {quest} gathers {count} of item {item}: keeps {kept}");
}
