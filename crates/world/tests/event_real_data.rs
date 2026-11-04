//! Loads the real server data and checks the tower defence events (rifts) against what the
//! tables say.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> \
//!   AA_BAKED=<baked data dir> \
//!   cargo test -p aa-world --test event_real_data -- --ignored --nocapture
//! ```
//!
//! `AA_BAKED` is optional: with it the controllers know their zone group.

use std::path::PathBuf;

use aa_world::{Vec3, World, WorldPaths};

fn load() -> Option<World> {
    let compact = PathBuf::from(std::env::var_os("AA_COMPACT")?);
    let server_data = PathBuf::from(std::env::var_os("AA_SERVER_DATA")?);
    let baked = std::env::var_os("AA_BAKED").map(PathBuf::from);
    let _ = tracing_subscriber::fmt()
        .with_env_filter("aa_world=info")
        .with_test_writer()
        .try_init();
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
fn the_crimson_rift() {
    let Some(w) = load() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let ev = w.events();
    for d in ev.all() {
        println!(
            "def {:>2} {:<28} levels {:>2}-{:<2} steps {} controllers {:>3} cap {:>5.0} s war {:?}",
            d.id,
            d.name,
            d.min_level,
            d.max_level,
            d.steps.len(),
            d.controllers.len(),
            d.force_end_s,
            d.war_zone_group,
        );
    }
    assert!(ev.bytes() < 64 * 1024, "{} bytes", ev.bytes());

    // def 3: the Crimson Rift, five waves, 3600 s cap
    let d = ev.get(3).expect("tower def 3");
    assert_eq!(&*d.name, "Crimson Rift");
    assert_eq!((d.first_wave_after_s, d.force_end_s), (0.0, 3600.0));
    assert_eq!(d.target_spawner, 9846);
    assert!(d.ends_by_time());
    assert_eq!(d.steps.len(), 5);
    // step 1 is a 170 s timer, the next three are kill counts, the last a 3600 s timer
    assert_eq!(d.step(1).unwrap().timer_s, 170.0);
    assert!(d.step(1).unwrap().kills.is_empty());
    let kills = |step: u32| -> Vec<(u32, u32)> {
        d.step(step)
            .unwrap()
            .kills
            .iter()
            .map(|k| (k.template, k.count))
            .collect()
    };
    assert_eq!(
        kills(2),
        [(8834, 23), (8826, 23)],
        "Archers and Infantrymen"
    );
    assert_eq!(kills(3), [(8835, 18), (8827, 18)]);
    let mut k4 = kills(4);
    k4.sort_unstable();
    assert_eq!(k4, [(8825, 8), (8836, 18)], "Commanders and Spearmen");
    assert!(kills(5).is_empty());
    assert_eq!(d.step(5).unwrap().timer_s, 3600.0);
    assert_eq!(
        [2, 3, 4, 5].map(|s| d.step(s).unwrap().spawners.to_vec()),
        [vec![9848], vec![9849], vec![9865], vec![9866]]
    );
    // levels 40 to 45 from the kill targets
    assert_eq!((d.min_level, d.max_level), (40, 45));
    assert!(d.joinable());
    for t in [8834, 8826, 8835, 8827, 8825, 8836] {
        assert!(d.is_wave_npc(t), "{t}");
        assert!(!d.is_controller(t), "{t}");
        assert!(w.npc(t).is_some_and(|n| n.faction_id == 115), "{t}");
    }
    // the controllers: the target "Rift" (8828) and the four wave summoners
    for t in [8828, 8830, 8831, 8847, 8848] {
        assert!(d.is_controller(t), "{t}");
        assert!(ev.is_controller(t), "{t}");
    }
    assert!(!ev.is_controller(8834));

    // three Rifts stand near (14808, 12063); the wave controllers a long walk away
    let me = Vec3::new(14800.0, 12070.0, 180.0);
    let target = d.nearest_target(20, me).expect("a target position");
    assert!(
        target.dist2d(me) < 60.0,
        "target at {target:?} is {} m away",
        target.dist2d(me)
    );
    let waves: Vec<_> = d.wave_positions(2, 20).collect();
    assert_eq!(waves.len(), 3, "three summoners per wave");
    for p in &waves {
        let far = p.dist2d(target);
        assert!((300.0..1500.0).contains(&far), "{far} m from the target");
    }

    // the war events are marked
    let war = ev.get(18).expect("the Golden Plains war");
    assert_eq!(war.war_zone_group, Some(22));
    assert!(!war.joinable());

    // def 13 is a level 50 event with a 20 s head start
    let d13 = ev.get(13).expect("tower def 13");
    assert_eq!(d13.first_wave_after_s, 20.0);
    assert_eq!((d13.min_level, d13.max_level), (50, 50));
    assert!(d13.joinable());
}

#[test]
#[ignore = "needs AA_COMPACT, AA_SERVER_DATA and AA_BAKED (real data)"]
fn the_controllers_know_their_zone_group() {
    let (Some(w), Some(_)) = (load(), std::env::var_os("AA_BAKED")) else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA / AA_BAKED not set; skipping");
        return;
    };
    let d = w.events().get(3).expect("tower def 3");
    // Cinderstone Moor is zone group 20; every controller of the Crimson Rift lies in it
    assert!(
        d.controllers.iter().all(|c| c.zone_group == 20),
        "{:?}",
        d.controllers
            .iter()
            .map(|c| (c.template, c.zone_group))
            .collect::<Vec<_>>()
    );
    assert_eq!(w.zone_key_at(14808.0, 12063.0), Some(257));
}
