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
use aa_proto::sc_activity::ActivityEvent;
use aa_proto::sc_world::{EquipSlot, LootItem, WorldEvent};
use aa_world::{Relation, World, WorldPaths};
use playerbots::archetype::Archetypes;
use playerbots::bot::brain::Brain;
use playerbots::bot::cmd::{BotCmd, GoalChoice, GoalKind, PairKind, PairOrder, PairRole};
use playerbots::bot::combat::{Combat, CombatStatus, LostReason, FIGHT_TIMEOUT_MS, NO_DAMAGE_MS};
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

    /// Feeds an activity event (duels, quests).
    fn feed_activity(&mut self, ev: ActivityEvent) {
        let ev = ServerEvent::Activity(ev);
        self.state.apply(&ev);
        self.combat
            .on_event(&ev, self.state.me.obj, self.now_ms, &mut self.out);
    }

    /// The goals the brain leaves out of its choice for this bot right now.
    fn gate(&mut self) -> u16 {
        let ctx = Ctx {
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
        playerbots::bot::brain::gate(&ctx)
    }

    /// Whether the bot's event log (the web UI's) has a line with `needle`.
    fn said(&self, needle: &str) -> bool {
        self.hub
            .slot(1)
            .is_some_and(|s| s.with(|g| g.log.iter().any(|l| l.text.contains(needle))))
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

/// A monster of `template` in view, `dx` metres east of the bot and not hurt by anything.
fn place_mob(rig: &mut Rig, template: u32, obj: u32, dx: f32) {
    let level = rig.world.npc(template).unwrap().level;
    let me = rig.state.me.pos;
    rig.state.units.insert(
        obj,
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
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_bot_at_zero_hit_points_is_dead_without_a_death_event_and_never_rests() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world, "Darkrunner", 21);
    rig.force(GoalKind::Wander);
    for _ in 0..8 {
        rig.tick(250);
    }
    assert_eq!(rig.brain.goal_name(), "wander");
    // the server reports the hit points only
    rig.feed(WorldEvent::UnitPoints {
        obj: ME,
        hp: 0,
        mp: 100,
    });
    assert!(rig.state.me.dead);
    assert!(
        std::mem::take(&mut rig.state.reactions.died),
        "the bot task is told to ask for the resurrection"
    );
    assert!(rig.state.me.respawn_at_ms > rig.now_ms);
    for _ in 0..(120 * 4) {
        rig.tick(250);
        assert_eq!(
            rig.brain.goal_name(),
            "-",
            "a dead bot has no goal, least of all a rest"
        );
    }
    // the hit points come back with no resurrection notice
    rig.feed(WorldEvent::UnitPoints {
        obj: ME,
        hp: 100,
        mp: 100,
    });
    assert!(!rig.state.me.dead);
    assert!(rig.state.reactions.resurrected.is_some());
    rig.tick(250);
    rig.tick(250);
    assert_ne!(rig.brain.goal_name(), "-", "alive again, the brain goes on");
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn goals_that_cannot_succeed_are_left_out_of_the_choice() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let both = (1u16 << GoalKind::TradeRun.index()) | (1u16 << GoalKind::Housing.index());
    let mut rig = Rig::new(world.clone(), "Reaper", 22);
    assert!(!rig.cfg.bots.gm_bootstrap);
    assert_eq!(
        rig.gate(),
        both,
        "level 1 without GM help: no trade run (level), no housing (design item)"
    );
    // with GM help both can be tried
    rig.cfg.bots.gm_bootstrap = true;
    assert_eq!(rig.gate(), 0);
    rig.cfg.bots.gm_bootstrap = false;

    // a long automatic run never picks them
    let mut seen = BTreeSet::new();
    for _ in 0..(20 * 60 * 4) {
        rig.tick(250);
        seen.insert(rig.brain.goal_name());
        rig.out.clear();
    }
    println!("goals seen: {seen:?}");
    assert!(
        !seen.contains("trade_run") && !seen.contains("housing"),
        "{seen:?}"
    );

    // a bot that carries a pack can always try to sell it (housing stays out)
    let pack = world
        .pack_crafts()
        .map(|c| c.product.0)
        .find(|&p| !world.specialty_buyers(p).is_empty())
        .expect("a pack with a buyer");
    let mut rig = Rig::new(world, "Reaper", 23);
    rig.state.equipment.push(EquipSlot {
        slot: 26,
        template: pack,
        id: 77,
        grade: 0,
    });
    assert_eq!(rig.gate(), 1u16 << GoalKind::Housing.index());
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_monster_nobody_can_hurt_is_given_up_early_and_left_alone_longer_than_a_fight_lasts() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world.clone(), "Darkrunner", 31);
    let template = weak_hostile(&world, rig.state.me.faction);
    let mob = 0x900;
    place_mob(&mut rig, template, mob, 6.0);
    rig.force(GoalKind::Grind);

    // The monster stays at full hit points and never hits back (a wall between, a ledge): the
    // controller gives it up long before the fight timeout.
    let mut engaged_at = None;
    let mut lost_at = None;
    for _ in 0..(3 * 60 * 4) {
        rig.tick(250);
        if engaged_at.is_none() && rig.combat.target() == Some(mob) {
            engaged_at = Some(rig.now_ms);
        }
        if engaged_at.is_some() && lost_at.is_none() && rig.combat.target().is_none() {
            lost_at = Some(rig.now_ms);
            break;
        }
    }
    let (engaged, lost) = (
        engaged_at.expect("the bot engaged the monster"),
        lost_at.expect("the bot gave the monster up"),
    );
    let fought = lost - engaged;
    assert!(
        fought > NO_DAMAGE_MS && fought < NO_DAMAGE_MS + 15_000,
        "gave up after {fought} ms"
    );
    let loss = rig.combat.last_loss().expect("a loss was recorded");
    assert_eq!(loss.reason, LostReason::Stalled);
    assert!(!loss.dealt);
    assert!(loss.unreachable());

    // And the goal does not walk back to it: not within the time a whole fight takes, not
    // within the rest of the 10 minutes it is left alone for.
    assert!(rig.state.avoids(mob, template, rig.now_ms));
    for _ in 0..(((FIGHT_TIMEOUT_MS + 60_000) / 250) as usize) {
        rig.tick(250);
        assert_ne!(
            rig.combat.target(),
            Some(mob),
            "went back to the monster at {} ms after the loss",
            rig.now_ms - lost
        );
    }
    assert!(rig.state.avoids(mob, template, rig.now_ms));
}

/// A bot that stands next to a monster and has cast at it.
fn rig_in_a_fight(world: &Arc<World>, seed: u64) -> (Rig, u32, u32) {
    let mut rig = Rig::new(world.clone(), "Darkrunner", seed);
    let template = weak_hostile(world, rig.state.me.faction);
    let mob = 0x900;
    place_mob(&mut rig, template, mob, 10.0);
    rig.force(GoalKind::Grind);
    for _ in 0..120 {
        rig.tick(250);
        if rig.sent(op::CSStartSkillPacket) > 0 {
            break;
        }
    }
    assert_eq!(rig.combat.target(), Some(mob));
    assert!(rig.sent(op::CSStartSkillPacket) > 0, "no attack was sent");
    (rig, mob, template)
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_refused_line_of_sight_makes_the_bot_take_another_position() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let (mut rig, mob, _) = rig_in_a_fight(&world, 32);
    let skill = rig.persona.archetype.starter_skill;
    let before = rig.motion.pos();
    rig.out.clear();
    // 12 = skill_obstacle
    rig.feed(WorldEvent::SkillStarted {
        caster_obj: ME,
        skill,
        tl: 1,
        target_obj: mob,
        result: 12,
    });
    assert_eq!(rig.combat.casts_obstructed(), 1);
    assert_eq!(rig.combat.casts_refused(), 0, "not a failure of the skill");
    // for the next second the bot casts nothing but walks to another place
    for _ in 0..5 {
        rig.tick(250);
    }
    assert_eq!(
        rig.sent(op::CSStartSkillPacket),
        0,
        "no cast while the position is changed"
    );
    let moved = dist2d(before, rig.motion.pos());
    assert!(moved > 0.5, "moved only {moved} m");
    // then the fight goes on from there
    for _ in 0..80 {
        rig.tick(250);
    }
    assert!(
        rig.sent(op::CSStartSkillPacket) > 0,
        "the bot attacks again"
    );
    assert_eq!(rig.combat.target(), Some(mob));
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_target_that_keeps_refusing_for_line_of_sight_is_given_up_and_avoided() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let (mut rig, mob, template) = rig_in_a_fight(&world, 33);
    let skill = rig.persona.archetype.starter_skill;
    let mut lost = false;
    for _ in 0..6 {
        rig.feed(WorldEvent::SkillStarted {
            caster_obj: ME,
            skill,
            tl: 1,
            target_obj: mob,
            result: 12,
        });
        // wait out the hold; the target may be given up on the way
        for _ in 0..8 {
            rig.tick(250);
        }
        if rig.combat.target() != Some(mob) {
            lost = true;
            break;
        }
    }
    assert!(lost, "the target was never given up");
    let loss = rig.combat.last_loss().unwrap();
    assert_eq!(loss.reason, LostReason::Blocked);
    assert!(loss.unreachable());
    assert_eq!(rig.combat.casts_refused(), 0);
    assert!(rig.state.avoids(mob, template, rig.now_ms));
    assert_ne!(rig.combat.status(), CombatStatus::Fighting);
}

fn start_duel(rig: &mut Rig, foe: u32) {
    let me = rig.state.me.pos;
    rig.state.units.insert(
        foe,
        Unit {
            kind: UnitKind::Character,
            id: 900,
            pos: [me[0] + 8.0, me[1], me[2]],
            hp: 1000,
            faction: rig.state.me.faction,
            level: 1,
            flags: 0,
            name: Some("Foe".into()),
        },
    );
    rig.brain.command(&BotCmd::Pair(PairOrder {
        kind: PairKind::Duel,
        role: PairRole::Initiator,
        partner_bot: 2,
        partner_char: 900,
        partner_name: "Foe".into(),
        meet_at: (me[0], me[1]),
    }));
    rig.tick(250);
    assert_eq!(rig.brain.goal_name(), "duel");
    rig.feed_activity(ActivityEvent::DuelStartCountdown);
    rig.feed_activity(ActivityEvent::DuelStarted {
        challenger_obj: ME,
        challenged_obj: foe,
    });
    rig.tick(250);
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_duel_nobody_loses_hit_points_in_is_nudged_and_then_left_by_walking_away() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world, "Darkrunner", 41);
    let foe = 0x500;
    start_duel(&mut rig, foe);
    assert_eq!(rig.combat.target(), Some(foe));
    let started = rig.now_ms;
    let me = rig.state.me.pos;
    let flag = [(me[0] + rig.state.units[&foe].pos[0]) / 2.0, me[1], 0.0];

    // nobody is hurt: first the nudge, 45 s into the fight
    let run_to = |rig: &mut Rig, ms: u64| {
        while rig.now_ms < started + ms {
            rig.tick(250);
            rig.out.clear();
        }
    };
    run_to(&mut rig, 44_000);
    assert!(!rig.said("closing in on the opponent"));
    run_to(&mut rig, 47_000);
    assert!(rig.said("closing in on the opponent"), "no nudge");
    assert!(!rig.combat.duel.withdrawing());
    assert_eq!(rig.combat.target(), Some(foe), "still fighting");

    // still nothing: 30 s later the bot leaves
    run_to(&mut rig, 74_000);
    assert!(!rig.combat.duel.withdrawing());
    run_to(&mut rig, 77_000);
    assert!(rig.said("leaving it"), "no walk away");
    assert!(rig.combat.duel.withdrawing());
    assert_ne!(rig.combat.target(), Some(foe), "the fight is dropped");

    // and walks out of the duel area (the server counts 75 m from the flag as a surrender)
    run_to(&mut rig, 77_000 + 60_000);
    let away = dist2d(rig.motion.pos(), flag);
    assert!(away > 75.0, "only {away:.0} m from the flag");
    assert_ne!(rig.combat.target(), Some(foe));
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_duel_with_hits_landing_is_left_alone() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world, "Darkrunner", 42);
    let foe = 0x500;
    start_duel(&mut rig, foe);
    let started = rig.now_ms;
    let mut hp = 1000;
    let mut next_hit = started + 20_000;
    while rig.now_ms < started + 200_000 {
        rig.tick(250);
        rig.out.clear();
        if rig.now_ms >= next_hit {
            hp -= 20;
            next_hit += 20_000;
            // the foe loses hit points, then the bot does
            rig.feed(WorldEvent::UnitPoints {
                obj: foe,
                hp,
                mp: 100,
            });
            rig.state.units.get_mut(&foe).unwrap().hp = hp;
            rig.feed(WorldEvent::UnitPoints {
                obj: ME,
                hp: hp + 5,
                mp: 100,
            });
        }
    }
    assert!(!rig.said("closing in"), "nudged a duel that was going on");
    assert!(!rig.combat.duel.withdrawing());
    assert_eq!(rig.combat.target(), Some(foe));
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn the_hub_reports_the_byte_rates() {
    use pb_web::BotHub;
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let rig = Rig::new(world, "Darkrunner", 51);
    rig.hub.count(10, 6_000, 4, 1_000);
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    rig.hub.tick();
    let s = rig.hub.stats();
    assert!(
        (3_000.0..7_000.0).contains(&s.bytes_in_per_sec),
        "{}",
        s.bytes_in_per_sec
    );
    assert!(
        (500.0..1_500.0).contains(&s.bytes_out_per_sec),
        "{}",
        s.bytes_out_per_sec
    );
    assert!(s.packets_in_per_sec > 0.0 && s.packets_out_per_sec > 0.0);
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_target_the_server_does_not_know_is_dropped_after_three_refusals() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let (mut rig, mob, template) = rig_in_a_fight(&world, 34);
    let skill = rig.persona.archetype.starter_skill;
    for i in 0..3 {
        // 9 = no_target
        rig.feed(WorldEvent::SkillStarted {
            caster_obj: ME,
            skill,
            tl: 1,
            target_obj: mob,
            result: 9,
        });
        rig.tick(250);
        if i < 2 {
            assert_eq!(
                rig.combat.target(),
                Some(mob),
                "not yet after {} refusals",
                i + 1
            );
        }
    }
    assert_ne!(rig.combat.target(), Some(mob));
    let loss = rig.combat.last_loss().expect("a loss was recorded");
    assert_eq!(loss.reason, LostReason::Gone);
    assert!(
        !rig.state.units.contains_key(&mob),
        "the unit the server does not know is forgotten"
    );
    assert!(rig.state.avoids(mob, template, rig.now_ms));
}

#[test]
#[ignore = "needs AA_COMPACT and AA_SERVER_DATA (real server data)"]
fn a_defence_against_a_unit_nobody_can_hurt_is_not_taken_up_again() {
    let Some(world) = world() else {
        eprintln!("AA_COMPACT / AA_SERVER_DATA not set; skipping");
        return;
    };
    let mut rig = Rig::new(world.clone(), "Darkrunner", 35);
    let template = weak_hostile(&world, rig.state.me.faction);
    let mob = 0x900;
    place_mob(&mut rig, template, mob, 6.0);
    rig.force(GoalKind::Wander);
    // the monster "targets" the bot all the time and neither side ever loses a hit point
    let threaten = |rig: &mut Rig| {
        let now = rig.now_ms;
        rig.state.threats.clear();
        rig.state.threats.push((mob, now));
    };
    let mut lost_at = None;
    for _ in 0..(3 * 60 * 4) {
        threaten(&mut rig);
        rig.tick(250);
        if rig.combat.last_loss().is_some() {
            lost_at = Some(rig.now_ms);
            break;
        }
    }
    let lost_at = lost_at.expect("the defence was given up");
    assert_eq!(rig.combat.last_loss().unwrap().reason, LostReason::Stalled);
    // it keeps targeting the bot, but the controller does not go back to it for longer than a
    // fight lasts (this is the loop that cost 30 s of every 45 s)
    while rig.now_ms < lost_at + FIGHT_TIMEOUT_MS {
        threaten(&mut rig);
        rig.tick(250);
        assert_ne!(
            rig.combat.target(),
            Some(mob),
            "defended again {} ms after giving up",
            rig.now_ms - lost_at
        );
    }
    // a monster that does hurt the bot is another matter
    rig.feed(WorldEvent::UnitPoints {
        obj: ME,
        hp: 900,
        mp: 400,
    });
    threaten(&mut rig);
    rig.tick(250);
    assert_eq!(rig.combat.target(), Some(mob), "defends when hurt");
}
