//! Tests of the parts of the event goal that need no tick context: what it shows, which goals it
//! pushes aside, where it thinks the target is. The decisions are tested in `plan` and `pick`, the
//! board in `crate::events`; the walk and the fight are checked live.

use aa_world::event::{Controller, EventDef};

use super::*;

fn id() -> EventId {
    EventId {
        def: 3,
        zone_group: 20,
    }
}

fn run(phase: Phase) -> Box<Run> {
    Box::new(Run {
        id: id(),
        name: "Crimson Rift".into(),
        phase,
        rally: [100.0, 200.0, 10.0],
        spot: [110.0, 200.0],
        step: 2,
        dist_m: 480.0,
        deadline_ms: 0,
        go_ms: 0,
        legs: 0,
        leg_from: 0.0,
        walking: false,
        kills: 4,
        target: None,
        idle_since_ms: 0,
        last_observe_ms: 0,
        ignore: SmallVec::new(),
        left: false,
    })
}

fn view(target: Option<(u32, [f32; 3])>) -> View {
    View {
        id: id(),
        zone_key: 257,
        step: 2,
        step_since_ms: 0,
        first_seen_ms: 0,
        target,
        joined: 0,
    }
}

fn def() -> EventDef {
    let c = |x: f32, group: u16| Controller {
        spawner: 1,
        step: 0,
        template: 900,
        pos: Vec3::new(x, 50.0, 7.0),
        zone_group: group,
    };
    EventDef {
        id: 3,
        name: "Crimson Rift".into(),
        first_wave_after_s: 0.0,
        force_end_s: 3600.0,
        target_spawner: 1,
        end_kill: (0, 0),
        steps: Box::default(),
        controllers: vec![c(1000.0, 20), c(2000.0, 20), c(-5000.0, 22)].into(),
        min_level: 40,
        max_level: 45,
        war_zone_group: None,
    }
}

#[test]
fn a_goal_without_an_event_says_so_and_owns_nothing() {
    let g = Event::new();
    assert!(!g.owns_combat());
    let mut text = String::new();
    g.describe(&mut text);
    assert_eq!(text, "event (no event to join)");
    assert_eq!(g.activity(false, false), Activity::Idle);
    assert_eq!(g.activity(true, false), Activity::Traveling);
    assert_eq!(g.activity(true, true), Activity::Fighting);
}

#[test]
fn a_bot_in_the_event_owns_its_fights_and_says_where_it_is() {
    let mut g = Event::new();
    g.run = Some(run(Phase::Go));
    assert!(g.owns_combat());
    let mut text = String::new();
    g.describe(&mut text);
    assert_eq!(
        text,
        "event: going to the Crimson Rift (wave 2, 480 m to go)"
    );

    let r = g.run.as_deref_mut().unwrap();
    r.phase = Phase::Hold;
    text.clear();
    g.describe(&mut text);
    assert_eq!(text, "event: holding the Crimson Rift (wave 2), 4 kills");

    g.run.as_deref_mut().unwrap().target = Some(77);
    text.clear();
    g.describe(&mut text);
    assert_eq!(
        text,
        "event: fighting at the Crimson Rift (wave 2), 4 kills"
    );
}

#[test]
fn the_event_goal_pushes_aside_only_the_small_goals() {
    let taken: Vec<GoalKind> = GoalKind::ALL
        .into_iter()
        .filter(|&k| preemptible(Some(k)))
        .collect();
    assert_eq!(
        taken,
        [
            GoalKind::Wander,
            GoalKind::Rest,
            GoalKind::Grind,
            GoalKind::Quest
        ]
    );
    assert!(preemptible(None));
    // not the event itself, and nothing that has a partner, a pack, a party or a boat
    for k in [
        GoalKind::Event,
        GoalKind::Duel,
        GoalKind::Trade,
        GoalKind::TradeRun,
        GoalKind::Dungeon,
        GoalKind::Sail,
    ] {
        assert!(!preemptible(Some(k)), "{k:?}");
    }
}

#[test]
fn an_eligible_bot_outscores_the_everyday_goals() {
    let b = BehaviorCfg::default();
    let sum = b.quest + b.grind + b.duel + b.trade + b.trade_run + b.housing + b.rest + b.wander;
    assert!(SCORE > sum * 2.0, "{SCORE} against {sum}");
    assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
}

#[test]
fn the_target_is_where_the_server_says_else_the_nearest_in_the_data() {
    let d = def();
    // the list names it
    let named = view(Some((77, [14_800.0, 12_060.0, 180.0])));
    assert_eq!(
        rally_of(&d, &named, [0.0, 0.0, 0.0]),
        Some([14_800.0, 12_060.0, 180.0])
    );
    // otherwise the controller of the event's zone group nearest to the bot
    let none = view(None);
    assert_eq!(
        rally_of(&d, &none, [1900.0, 50.0, 0.0]),
        Some([2000.0, 50.0, 7.0])
    );
    assert_eq!(
        rally_of(&d, &none, [900.0, 50.0, 0.0]),
        Some([1000.0, 50.0, 7.0])
    );
    // the one in another zone group is not an option while this group has its own
    assert_eq!(
        rally_of(&d, &none, [-5000.0, 50.0, 0.0]),
        Some([1000.0, 50.0, 7.0])
    );
    // an event without any position in the data and none from the server
    let mut empty = d.clone();
    empty.controllers = Box::default();
    assert_eq!(rally_of(&empty, &none, [0.0, 0.0, 0.0]), None);
}

#[test]
fn ignored_monsters_come_back_and_the_table_is_bounded() {
    let mut r = run(Phase::Hold);
    r.ignore_for(7, 5_000);
    assert!(r.ignored(7, 4_999));
    assert!(!r.ignored(7, 5_000));
    assert!(!r.ignored(8, 0));
    for obj in 0..20 {
        r.ignore_for(obj, 1_000);
    }
    assert_eq!(r.ignore.len(), 4);
}

#[test]
fn the_goal_costs_a_pointer_until_the_bot_joins() {
    assert_eq!(std::mem::size_of::<Event>(), std::mem::size_of::<usize>());
    assert!(
        std::mem::size_of::<Run>() <= 256,
        "{}",
        std::mem::size_of::<Run>()
    );
}
