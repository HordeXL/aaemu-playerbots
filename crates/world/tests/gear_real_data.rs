//! Loads the real server data and checks the gear facts (equippable items, lunagems, sockets,
//! socket chances, gear merchants, quest rewards) against what the server data says.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   cargo test -p aa-world --test gear_real_data -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use aa_world::gear::{armor_kind, equip_slot, slot_type, GearClass, GRADE_COMMON, NO_GRADE};
use aa_world::{World, WorldPaths};

fn load() -> Option<World> {
    let compact = PathBuf::from(std::env::var_os("AA_COMPACT")?);
    let server_data = PathBuf::from(std::env::var_os("AA_SERVER_DATA")?);
    let baked = std::env::var_os("AA_BAKED").map(PathBuf::from);
    Some(
        World::load(&WorldPaths {
            compact,
            server_data,
            baked,
            world: "main_world".into(),
        })
        .expect("load main_world"),
    )
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn the_gear_tables_match_the_server_data() {
    let Some(w) = load() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let g = w.gear();
    println!(
        "{} gear templates, {} lunagems, {} merchant offers, {} KB",
        g.len(),
        g.gems().count(),
        g.sold().len(),
        g.bytes() / 1024
    );
    assert!(g.len() > 8_000, "{} gear templates", g.len());

    // Tree Shade Hood (22229): cloth helmet, level 13, requirement 11, graded at creation
    let hood = g.item(22_229).expect("the Tree Shade Hood is gear");
    assert_eq!(hood.class, GearClass::Armor);
    assert_eq!(
        (hood.slot_type, hood.kind, hood.level, hood.req),
        (slot_type::HEAD, armor_kind::CLOTH, 13, 11)
    );
    assert_eq!(hood.fixed_grade, NO_GRADE);
    assert!(!hood.wearable_by(10, 1) && hood.wearable_by(11, 1));
    // Executioner (21907): a greatsword (holdable 3), two-handed, level 7
    let sword = g.item(21_907).expect("the Executioner is gear");
    assert_eq!(sword.class, GearClass::Weapon);
    assert!(sword.two_handed());
    assert_eq!((sword.kind, sword.level, sword.req), (3, 7, 6));
    assert_eq!(sword.slots(), [equip_slot::MAINHAND]);
    // Memory Cutter (21851): a dagger for either hand
    let dagger = g.item(21_851).unwrap();
    assert_eq!(dagger.slot_type, slot_type::ONE_HANDED);
    assert_eq!(dagger.slots(), [equip_slot::MAINHAND, equip_slot::OFFHAND]);

    // merchants sell the hood at the common grade, for copper
    let offers = g.sold_of(22_229);
    assert!(!offers.is_empty(), "somebody sells the Tree Shade Hood");
    assert_eq!(offers[0].grade, GRADE_COMMON);
    for npc in offers[0].npcs.iter() {
        assert!(w.npc_spawns_anywhere(*npc), "vendor {npc} spawns");
    }

    // lunagems: Faint Blue Crescent Stone (30915) goes into armor, Faint Red (30907) into one-
    // handed main-hand weapons, bows and instruments; the dawnstone (327) is no lunagem
    let blue = g.gem(30_915).expect("30915 is a lunagem");
    assert!(blue.fits(slot_type::CHEST, 13) && blue.fits(slot_type::HEAD, 1));
    assert!(!blue.fits(slot_type::ONE_HANDED, 13));
    let red = g.gem(30_907).expect("30907 is a lunagem");
    assert!(red.fits(slot_type::ONE_HANDED, 13) && red.fits(slot_type::RANGED, 13));
    assert!(!red.fits(slot_type::CHEST, 13));
    // a clear (선명한) gem needs item level 40
    let clear = g.gem(30_918).expect("30918 is a lunagem");
    assert_eq!(clear.min_level, 40);
    assert!(g.gem(327).is_none());
    assert!(g.gems().count() >= 100);
    assert_eq!(w.item_use_skill(30_915), 23_728, "the socketing skill");

    // sockets: a rare chest has two, a common one none; a heroic one-handed weapon four
    assert_eq!(g.sockets(slot_type::CHEST, 3), 2);
    assert_eq!(g.sockets(slot_type::CHEST, 0), 0);
    assert_eq!(g.sockets(slot_type::ONE_HANDED, 5), 4);
    assert_eq!(g.socket_chance(0), 10_000);
    assert_eq!(g.socket_chance(1), 5_000);
    // grades: common is order 1 and the stat base, poor is below it
    assert_eq!(g.grade(0).order, 1);
    assert_eq!(g.grade_factor(0), 1.0);
    assert!(g.grade_factor(1) < 1.0 && g.grade_factor(11) == 2.0);
    // the plain regrade scrolls
    let s = g.scrolls();
    assert_eq!((s.weapon, s.armor, s.accessory), (28_298, 28_299, 31_928));

    // quest 13 offers a bow, a shield, a lute and a greatclub, in that order
    assert_eq!(g.quest_rewards(13), [33_579, 33_586, 33_590, 35_837]);

    // the pet helmets merchants sell are no character gear
    let pet = g.item(23_093).expect("the Light Pet Helm has an armor row");
    assert_eq!(pet.kind, armor_kind::PET);
    assert!(!pet.character_gear() && hood.character_gear() && sword.character_gear());

    // what the merchants sell to characters, by slot type: the starter sets (levels 7 to 14) of
    // every armor kind and weapons; the honor gear is not for copper
    let mut by_slot: BTreeMap<u8, usize> = BTreeMap::new();
    for s in g.sold() {
        if let Some(item) = g.item(s.item).filter(|i| i.character_gear()) {
            *by_slot.entry(item.slot_type).or_default() += 1;
            assert!(
                item.level <= 20,
                "copper gear {} is level {}",
                s.item,
                item.level
            );
        }
    }
    println!("merchant gear by slot type: {by_slot:?}");
    assert_eq!(by_slot.get(&slot_type::CHEST).copied(), Some(3));
    assert!(g.sold_of(18_395).is_empty(), "honor gear");
}
