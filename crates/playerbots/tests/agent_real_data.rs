//! The LLM director's options and the agent API against the real `main_world` data, without a
//! server and without an LLM.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> [AA_BAKED=<data dir>] \
//!   cargo test -p playerbots --test agent_real_data -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aa_world::{World, WorldPaths};
use pb_web::BotHub;
use playerbots::archetype::Archetypes;
use playerbots::bot::cmd::GoalKind;
use playerbots::bot::combat::Combat;
use playerbots::bot::coordinator::CoordHandle;
use playerbots::bot::ctx::{Ctx, Persona};
use playerbots::bot::director::{self, What, MAX_OPTIONS};
use playerbots::bot::motion::{Motion, MotionCfg};
use playerbots::bot::state::BotState;
use playerbots::config::Config;
use playerbots::hub::{Hub, HubParams, Logger};
use playerbots::roster::{Profile, Temperament};

fn world() -> Option<Arc<World>> {
    static WORLD: OnceLock<Option<Arc<World>>> = OnceLock::new();
    WORLD
        .get_or_init(|| {
            let compact = PathBuf::from(std::env::var_os("AA_COMPACT")?);
            let server_data = PathBuf::from(std::env::var_os("AA_SERVER_DATA")?);
            let baked = std::env::var_os("AA_BAKED").map(PathBuf::from);
            let paths = WorldPaths {
                compact,
                server_data,
                baked,
                world: "main_world".into(),
            };
            Some(Arc::new(World::load(&paths).expect("load main_world")))
        })
        .clone()
}

fn hub(world: &World) -> Hub {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    Hub::new(HubParams {
        world,
        baked_dir: None,
        supervisor: tx,
        llm: None,
    })
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA"]
fn zone_names_of_the_real_world_resolve() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let hub = hub(&world);
    let zones = hub.zones();
    println!("{} zone keys with a name", zones.len());
    assert!(zones.len() > 20, "{}", zones.len());
    // every name resolves to a key that has that name
    for z in zones.iter().take(200) {
        let key = hub.zones.resolve(&z.name).expect(&z.name);
        assert_eq!(hub.zones.name(key), Some(z.name.as_str()), "{}", z.name);
        assert_eq!(hub.zones.resolve(&z.name.to_uppercase()), Ok(key));
        assert_eq!(hub.zones.resolve(&z.key.to_string()), Ok(z.key));
    }
    let solz = hub.zones.resolve("solzreed");
    println!("solzreed -> {solz:?}");
    assert!(solz.is_ok(), "{solz:?}");
    assert!(hub.zones.resolve("no such zone at all").is_err());
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA"]
fn the_director_offers_concrete_options_a_bot_can_do() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let mut cfg = Config::default();
    cfg.farm.enabled = true;
    cfg.farm.wild = true;
    let hub = hub(&world);
    let slot = hub.register(pb_web::BotSnapshot {
        id: 1,
        name: "Tester".into(),
        ..pb_web::BotSnapshot::default()
    });
    let log = Logger::new(slot);
    let archetypes = Archetypes::builtin().unwrap();
    let archetype = archetypes.all().first().unwrap().clone();
    let persona = Persona {
        id: 1,
        name: "Tester".into(),
        archetype,
        temper: Temperament::NEUTRAL,
        profile: Profile {
            outlaw: 0.9,
            ..Profile::NEUTRAL
        },
    };
    let start = world.race_start(1).expect("Nuian start");
    let mut state = BotState::new();
    state.me.spawned = true;
    state.me.level = 12;
    state.me.hp = 1000;
    state.me.hp_max = 1000;
    state.me.faction = world.race_faction(1).expect("Nuian faction");
    state.me.pos = [start.x, start.y, start.z];
    let mut motion = Motion::new(MotionCfg::default());
    let mut combat = Combat::new();
    let mut out = Vec::new();
    let coord = CoordHandle::detached();
    let mut rng = fastrand::Rng::with_seed(1);
    let ctx = Ctx {
        world: &world,
        cfg: &cfg,
        persona: &persona,
        state: &mut state,
        motion: &mut motion,
        combat: &mut combat,
        out: &mut out,
        coord: &coord,
        hub: &hub,
        log: &log,
        rng: &mut rng,
        now: tokio::time::Instant::now(),
        now_ms: 1,
        real_player_near: false,
    };
    let scored = [
        (GoalKind::Quest, 2.0),
        (GoalKind::Explore, 1.5),
        (GoalKind::Farm, 1.0),
        (GoalKind::Grind, 0.9),
        (GoalKind::Wander, 0.5),
        (GoalKind::Rest, 0.1),
    ];
    let started = std::time::Instant::now();
    let menu = director::menu(&ctx, &scored);
    println!("menu built in {:?}", started.elapsed());
    let name = |k: u16| world.zone_name(k).map(str::to_owned);
    for o in &menu {
        println!(
            "  {} | {}",
            o.label(&name),
            o.line(&name, [start.x, start.y])
        );
    }
    assert!(menu.len() <= MAX_OPTIONS);
    assert!(menu.len() >= scored.len(), "every eligible goal is offered");
    assert!(
        menu.iter().any(|o| o.what == What::Wild),
        "an outlaw with [farm] wild may farm wild"
    );
    let zones: Vec<u16> = menu
        .iter()
        .filter_map(|o| match o.what {
            What::Zone { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    println!("zones to explore: {zones:?}");
    let here = world.zone_key_at(start.x, start.y);
    assert!(
        !zones.contains(&here.unwrap_or(0)),
        "not the zone it stands in"
    );
    for o in &menu {
        assert!(!o.line(&name, [start.x, start.y]).is_empty());
        let args = o.args("test");
        match o.what {
            What::Zone { key, .. } => assert_eq!(args.zone, Some(key)),
            What::Point { at, .. } => assert_eq!(args.dest, Some(at)),
            What::Wild => assert!(args.wild),
            What::Plain => assert!(args.zone.is_none() && args.dest.is_none()),
        }
    }
    // without the outlaw streak there is no wild farm
    let tame = Persona {
        profile: Profile::NEUTRAL,
        ..persona.clone()
    };
    let ctx = Ctx {
        world: &world,
        cfg: &cfg,
        persona: &tame,
        state: &mut state,
        motion: &mut motion,
        combat: &mut combat,
        out: &mut out,
        coord: &coord,
        hub: &hub,
        log: &log,
        rng: &mut rng,
        now: tokio::time::Instant::now(),
        now_ms: 1,
        real_player_near: false,
    };
    assert!(!director::menu(&ctx, &scored)
        .iter()
        .any(|o| o.what == What::Wild));
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA"]
fn an_observation_and_a_note_go_through_the_real_hub() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipped");
        return;
    };
    let hub = hub(&world);
    let slot = hub.register(pb_web::BotSnapshot {
        id: 7,
        name: "Aria".into(),
        level: 9,
        activity: pb_web::Activity::Idle,
        ..pb_web::BotSnapshot::default()
    });
    slot.push_log("goal: wander".into());
    hub.command(
        Some(7),
        pb_web::Command::Note {
            text: "Bob owes us a duel".into(),
        },
    )
    .unwrap();
    assert!(hub
        .command(
            Some(7),
            pb_web::Command::Note {
                text: "see www.example.com".into(),
            },
        )
        .is_err());
    let o = hub.observe(7).unwrap();
    println!("{}", serde_json::to_string_pretty(&o).unwrap());
    assert_eq!(o.name, "Aria");
    assert_eq!(o.notes, vec!["Bob owes us a duel".to_string()]);
    assert!(o.log.iter().any(|l| l.contains("note: Bob owes us a duel")));
    assert!(hub.observe(8).is_none());
    // an act with a zone name reaches the bot with the key
    let mut rx = slot.attach();
    let name = hub.zones().first().unwrap().name.clone();
    hub.command(
        Some(7),
        pb_web::Command::Act(pb_web::ActArgs {
            goal: "explore".into(),
            zone: Some(name.clone()),
            ..pb_web::ActArgs::default()
        }),
    )
    .unwrap();
    let Ok(playerbots::bot::cmd::BotCmd::GoalWith { kind, args }) = rx.try_recv() else {
        panic!("no act")
    };
    assert_eq!(kind, GoalKind::Explore);
    assert_eq!(args.zone, hub.zones.resolve(&name).ok());
}
