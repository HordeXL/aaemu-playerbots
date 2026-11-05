//! The zone and pack doodad facts the pvp features read, against the real data.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> AA_BAKED=<data dir> \
//!   cargo test -p aa-world --test pvp_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;

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
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real data); AA_BAKED for the zone grid"]
fn zones_conflict_groups_and_pack_doodads() {
    let Some(w) = load() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // `conflict_zones` rows that are not closed: ten land groups and the five sea groups
    let groups = w.conflict_groups();
    println!("conflict groups: {groups:?}");
    for g in [14, 15, 16, 17, 19, 20, 22, 23, 26, 27, 30, 36, 39, 59, 60] {
        assert!(w.is_conflict_group(g), "group {g}");
    }
    assert!(!w.is_conflict_group(78), "closed in the data");
    assert!(!w.is_conflict_group(1), "Gweonid Forest is a home zone");

    // the 335 doodads a put-down backpack turns into, e.g. 6059 for the first row
    assert!(w.is_pack_doodad(6059));
    assert!(!w.is_pack_doodad(1));

    // a home zone has its alliance faction, a conflict zone none (the server's `zones` rows)
    let gweonid = w.zone_info(129).expect("w_gweonid_forest_1");
    assert_eq!((gweonid.group, gweonid.faction), (1, 148));
    let sunrise = w.zone_info(141).expect("e_sunrise_peninsula_1");
    assert_eq!(sunrise.faction, 149);
    let cross = w.zone_info(148).expect("w_cross_plains_1");
    assert_eq!((cross.group, cross.faction), (20, 0));

    if w.baked().zones.is_some() {
        // Gweonid Forest (9681, 15189) is Nuian home, Cinderstone Moor (15437, 11483) a conflict
        // zone of the Cross Plains group
        let home = w.zone_info_at(9681.0, 15189.0).expect("zone at the bench");
        assert_eq!(home.faction, 148);
        let moor = w.zone_info_at(15437.0, 11483.0).expect("zone at the moor");
        assert_eq!((moor.group, moor.faction), (20, 0));
        for g in [20u16, 17, 15] {
            let spots = w.zone_group_spots(g);
            println!("group {g}: {} spots", spots.len());
            assert!(spots.len() > 20, "group {g}");
            for s in spots.iter().step_by(7) {
                let i = w.zone_info_at(s[0], s[1]).expect("a spot is inside a zone");
                assert_eq!(i.group, g);
            }
        }
        assert!(w.zone_group_spots(1).is_empty(), "home zones have no spots");
    }
}
