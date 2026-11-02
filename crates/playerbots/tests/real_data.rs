//! Runs the brain, the goals, the combat controller and the motion controller against the real
//! `main_world` data, without a server: the bot's state is fed by hand the way the server's packets
//! would feed it.
//!
//! ```text
//! AA_COMPACT=<.../app/Data/compact.sqlite3> AA_SERVER_DATA=<.../app/Data> [AA_BAKED=<data dir>] \
//!   cargo test -p playerbots --test real_data -- --ignored --nocapture
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use aa_proto::cs::Out;
use aa_proto::opcodes::cs as op;
use aa_proto::sc::ServerEvent;
use aa_proto::sc_world::{EquipSlot, LootItem, WorldEvent};
use aa_world::{Relation, World, WorldPaths};
use playerbots::archetype::Archetypes;
use playerbots::bot::brain::Brain;
use playerbots::bot::cmd::{BotCmd, GoalChoice, GoalKind};
use playerbots::bot::combat::Combat;
use playerbots::bot::coordinator::CoordHandle;
use playerbots::bot::ctx::{Ctx, Persona};
use playerbots::bot::motion::{Motion, MotionCfg, MotionEnv};
use playerbots::bot::state::{BotState, Unit, UnitKind};
use playerbots::config::Config;
use playerbots::hub::{Hub, HubParams, Logger};
use playerbots::roster::Temperament;

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

const ME: u32 = 0x400;

/// One bot without a network connection.
struct Rig {
    world: Arc<World>,
    cfg: Config,
    hub: Hub,
    log: Logger,
    coord: CoordHandle,
    persona: Persona,
    state: BotState,
    motion: Motion,
    combat: Combat,
    brain: Brain,
    out: Vec<Out>,
    rng: fastrand::Rng,
    now_ms: u64,
}

impl Rig {
    fn new(world: Arc<World>, archetype: &str, seed: u64) -> Rig {
        let cfg = Config::default();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let hub = Hub::new(HubParams {
            world: &world,
            baked_dir: None,
            supervisor: tx,
            llm: None,
        });
        let archetypes = Archetypes::builtin().unwrap();
        let archetype = archetypes.get(archetype).unwrap().clone();
        let slot = hub.register(pb_web::BotSnapshot {
            id: 1,
            name: "Tester".into(),
            ..pb_web::BotSnapshot::default()
        });
        let persona = Persona {
            id: 1,
            name: "Tester".into(),
            archetype,
            temper: Temperament::NEUTRAL,
        };
        let start = world.race_start(1).expect("Nuian start");
        let z = world.ground_z(start.x, start.y).unwrap_or(start.z);
        let mut state = BotState::new();
        state.me.char_id = 500;
        state.me.obj = ME;
        state.me.spawned = true;
        state.me.level = 1;
        state.me.hp = 1000;
        state.me.hp_max = 1000;
        state.me.mp = 400;
        state.me.mp_max = 400;
        state.me.faction = world.race_faction(1).expect("Nuian faction");
        state.me.pos = [start.x, start.y, z];
        state.skills = vec![(persona.archetype.starter_skill, 1)];
        let mut motion = Motion::new(MotionCfg::default());
        motion.enter_world(ME, state.me.pos, 0.0, 1);
        let brain = Brain::new(&cfg.behavior, &persona.temper);
        Rig {
            log: Logger::new(slot),
            coord: CoordHandle::detached(),
            cfg,
            hub,
            persona,
            state,
            motion,
            combat: Combat::new(),
            brain,
            out: Vec::new(),
            rng: fastrand::Rng::with_seed(seed),
            now_ms: 1,
            world,
        }
    }

    fn tick(&mut self, dt: u64) {
        self.now_ms += dt;
        self.state.now_ms = self.now_ms;
        self.state.me.pos = self.motion.pos();
        {
            let mut ctx = Ctx {
                world: &self.world,
                cfg: &self.cfg,
                persona: &self.persona,
                state: &mut self.state,
                motion: &mut self.motion,
                combat: &mut self.combat,
                out: &mut self.out,
                coord: &self.coord,
                hub: &self.hub,
                log: &self.log,
                rng: &mut self.rng,
                now: tokio::time::Instant::now(),
                now_ms: self.now_ms,
                real_player_near: false,
            };
            self.brain.tick(&mut ctx);
        }
        let mut env = MotionEnv {
            ground: &*self.world,
            now_ms: self.now_ms,
            out: &mut self.out,
            rng: &mut self.rng,
            send_gap_ms: 250,
            combat: self.combat.target().is_some(),
        };
        self.motion.tick(&mut env);
        self.state.me.pos = self.motion.pos();
    }

    /// Feeds an event the way the bot task does.
    fn feed(&mut self, ev: WorldEvent) {
        let ev = ServerEvent::World(ev);
        self.state.apply(&ev);
        self.combat
            .on_event(&ev, self.state.me.obj, self.now_ms, &mut self.out);
    }

    fn sent(&self, opcode: u16) -> usize {
        self.out.iter().filter(|o| o.opcode == opcode).count()
    }

    fn force(&mut self, kind: GoalKind) {
        self.brain.command(&BotCmd::Goal(GoalChoice::Forced(kind)));
    }
}

fn dist2d(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn wandering_stays_near_home_on_land_and_sends_movement() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world.clone(), "Darkrunner", 1);
    let home = rig.state.me.pos;
    rig.force(GoalKind::Wander);
    let mut farthest = 0.0f32;
    let has_terrain = world.baked().terrain.is_some();
    for _ in 0..1200 {
        rig.tick(250);
        farthest = farthest.max(dist2d(home, rig.motion.pos()));
        if has_terrain {
            let p = rig.motion.pos();
            let g = world.ground_z(p[0], p[1]).expect("inside the terrain grid");
            assert!(
                g >= 99.0 || (p[2] - 98.5).abs() < 0.01,
                "walked into water at {p:?}"
            );
        }
    }
    assert!(farthest > 8.0, "never left home ({farthest} m)");
    assert!(farthest < 160.0, "strayed {farthest} m");
    assert!(rig.sent(op::CSMoveUnitPacket) > 10);
    assert!(rig.brain.goal().is_some() || rig.brain.goal_name() == "-");
}

/// A weak monster hostile to the Nuian faction.
fn weak_hostile(world: &World, faction: u32) -> u32 {
    world
        .npc_templates()
        .iter()
        .find(|t| {
            t.grade == 1
                && (1..=3).contains(&t.level)
                && world.relation(faction, t.faction_id) == Relation::Hostile
                && world.npc_spawns_anywhere(t.id)
        })
        .map(|t| t.id)
        .expect("a weak hostile monster")
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn grinding_fights_a_monster_in_view_and_loots_it() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world.clone(), "Darkrunner", 2);
    let template = weak_hostile(&world, rig.state.me.faction);
    let level = world.npc(template).unwrap().level;
    let mob = 0x900;
    let me = rig.state.me.pos;
    rig.state.units.insert(
        mob,
        Unit {
            kind: UnitKind::Npc,
            id: template,
            pos: [me[0] + 14.0, me[1], me[2]],
            hp: 100,
            faction: 0,
            level,
            flags: 0,
            name: None,
        },
    );
    rig.force(GoalKind::Grind);
    // The bot walks into range, faces the monster and attacks.
    for _ in 0..80 {
        rig.tick(250);
        if rig.sent(op::CSStartSkillPacket) > 0 {
            break;
        }
    }
    assert_eq!(
        rig.combat.target(),
        Some(mob),
        "the bot should have engaged"
    );
    assert!(rig.sent(op::CSChangeTargetPacket) >= 1);
    assert!(rig.sent(op::CSStartSkillPacket) >= 1, "no attack was sent");
    assert!(
        dist2d(rig.state.me.pos, rig.state.units[&mob].pos) <= 4.0,
        "a melee archetype should be next to the monster"
    );

    // The monster dies by the bot's hand; the corpse has loot.
    rig.feed(WorldEvent::UnitDeath {
        obj: mob,
        killer: ME,
        respawn_wait_ms: 0,
        lost_exp: 0,
        reason: 0,
    });
    rig.feed(WorldEvent::LootableState {
        owner_obj: mob,
        owner_type: 1,
        has_loot: true,
    });
    for _ in 0..20 {
        rig.tick(250);
        if rig.sent(op::CSLootOpenBagPacket) > 0 {
            break;
        }
    }
    assert_eq!(
        rig.sent(op::CSLootOpenBagPacket),
        1,
        "the corpse was not looted"
    );
    rig.feed(WorldEvent::LootBag {
        owner_obj: mob,
        owner_type: 1,
        items: [LootItem {
            index: 0,
            template: 500,
            count: 1,
        }]
        .into_iter()
        .collect(),
        loot_all: true,
    });
    assert_eq!(rig.sent(op::CSLootItemPacket), 1);
    assert_eq!(rig.combat.kills(), 1);
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn the_automatic_brain_runs_for_half_an_hour_without_trouble() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world, "Reaper", 3);
    let mut seen = BTreeSet::new();
    for _ in 0..(30 * 60 * 4) {
        rig.tick(250);
        seen.insert(rig.brain.goal_name());
        rig.out.clear();
    }
    println!("goals seen: {seen:?}");
    println!(
        "stubs switched off: {:?}",
        GoalKind::ALL
            .iter()
            .filter(|g| rig.brain.is_disabled(**g))
            .map(|g| g.name())
            .collect::<Vec<_>>()
    );
    assert!(
        seen.contains("wander") || seen.contains("grind") || seen.contains("rest"),
        "no implemented goal ever ran: {seen:?}"
    );
    // Every goal is implemented now: none may be switched off as a stub.
    for kind in GoalKind::ALL {
        assert!(
            !rig.brain.is_disabled(kind),
            "{kind:?} was switched off as not implemented"
        );
    }
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_rest_that_cut_a_trade_run_short_gives_it_back_while_a_pack_is_carried() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // a pack that some NPC buys, carried by a bot 150 m from the nearest buyer
    let pack = world
        .pack_crafts()
        .map(|c| c.product.0)
        .find(|&p| {
            world
                .specialty_buyers(p)
                .iter()
                .any(|&b| !world.npc_spawns(b).is_empty())
        })
        .expect("a pack with a placed buyer");
    let buyer = *world
        .specialty_buyers(pack)
        .iter()
        .find(|&&b| !world.npc_spawns(b).is_empty())
        .unwrap();
    let spawn = world.npc_spawns(buyer)[0];
    let mut rig = Rig::new(world.clone(), "Darkrunner", 5);
    let start = [spawn.x + 150.0, spawn.y, spawn.z];
    rig.state.me.pos = start;
    rig.motion.enter_world(ME, start, 0.0, rig.now_ms);
    rig.state.equipment.push(EquipSlot {
        slot: 26,
        template: pack,
        id: 77,
        grade: 0,
    });

    rig.brain.queue(GoalKind::TradeRun, None);
    for _ in 0..8 {
        rig.tick(250);
    }
    assert_eq!(
        rig.brain.goal_name(),
        "trade_run",
        "the run carries the pack"
    );

    // badly hurt: the brain rests instead
    rig.state.me.hp = 200;
    rig.tick(250);
    assert_eq!(rig.brain.goal_name(), "rest");
    // recovered: the rest ends after its ten seconds and the run goes on
    rig.state.me.hp = 1000;
    let mut resumed_after = None;
    for i in 0..(100 * 4) {
        rig.tick(250);
        if rig.brain.goal_name() == "trade_run" {
            resumed_after = Some(i * 250);
            break;
        }
    }
    let ms = resumed_after.expect("the rest gave the trade run back");
    assert!(ms >= 9_000, "after the minimum rest, not at once: {ms} ms");
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_trade_run_only_fights_back_against_close_attackers() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    // a bot with a pack, 400 m from the nearest buyer
    let pack = world
        .pack_crafts()
        .map(|c| c.product.0)
        .find(|&p| {
            world
                .specialty_buyers(p)
                .iter()
                .any(|&b| !world.npc_spawns(b).is_empty())
        })
        .expect("a pack with a placed buyer");
    let buyer = *world
        .specialty_buyers(pack)
        .iter()
        .find(|&&b| !world.npc_spawns(b).is_empty())
        .unwrap();
    let spawn = world.npc_spawns(buyer)[0];
    let mut rig = Rig::new(world.clone(), "Darkrunner", 6);
    let start = [spawn.x + 400.0, spawn.y, spawn.z];
    rig.state.me.pos = start;
    rig.motion.enter_world(ME, start, 0.0, rig.now_ms);
    rig.state.equipment.push(EquipSlot {
        slot: 26,
        template: pack,
        id: 77,
        grade: 0,
    });
    rig.brain.queue(GoalKind::TradeRun, None);
    for _ in 0..8 {
        rig.tick(250);
    }
    assert_eq!(rig.brain.goal_name(), "trade_run");
    assert!(rig.motion.is_moving(), "on the way to the buyer");

    // a monster attacks from far behind: the bot keeps going
    let template = weak_hostile(&world, rig.state.me.faction);
    let level = world.npc(template).unwrap().level;
    let mob = 0x900;
    let place = |rig: &mut Rig, dx: f32| {
        let me = rig.state.me.pos;
        rig.state.units.insert(
            mob,
            Unit {
                kind: UnitKind::Npc,
                id: template,
                pos: [me[0] + dx, me[1], me[2]],
                hp: 100,
                faction: 0,
                level,
                flags: 0,
                name: None,
            },
        );
        let now = rig.now_ms;
        rig.state.threats.clear();
        rig.state.threats.push((mob, now));
    };
    for _ in 0..8 {
        place(&mut rig, 40.0);
        rig.tick(250);
        assert_eq!(rig.combat.target(), None, "no detour for a far attacker");
        assert_eq!(rig.brain.goal_name(), "trade_run");
    }
    assert!(rig.motion.is_moving());

    // a close attacker cannot be outrun: the controller fights back
    place(&mut rig, 8.0);
    rig.tick(250);
    assert_eq!(rig.combat.target(), Some(mob));

    // and lets go of it once it falls far behind
    place(&mut rig, 40.0);
    rig.tick(250);
    assert_eq!(
        rig.combat.target(),
        None,
        "a defence is dropped when the attacker is far"
    );
}
