//! The sail machine against a scripted server: a rowboat on a synthetic sea (two coasts), the
//! server's physics from [`Plant`], the slave events through the real [`VehicleState`], and the
//! server behaviours that go wrong (a refused place, no boat, a bind nobody answers, a despawn that
//! is ignored, a hull that does not move) switched on one at a time.

use std::cell::RefCell;

use aa_proto::sc_travel::{error_message as emsg, MySlaveInfo};
use aa_proto::sc_world::WorldEvent;
use aa_world::travel::ShipClass;
use aa_world::water::{find_shore, yaw_of, Disc, ShoreSpot, WaterMap};

use super::machine::{hull_gap, Act, Input, Machine, Ops, Phase, Pick};
use super::trip::{plan_trip, sail_route, Plan, PlanError, TripCfg};
use crate::bot::motion::MotionStatus;
use crate::bot::state::StateError;
use crate::bot::vehicle::helm::Cmd;
use crate::bot::vehicle::sim::Plant;
use crate::bot::vehicle::VehicleState;
use crate::bot::watch::classify::skill_result;
use crate::bot::watch::Finding;

use super::super::{FailReason, GoalStatus};

const ME: u32 = 0x400;
const CHAR: u32 = 77;
const BOAT: u32 = 0x600;
const TL: u16 = 3;
const SCROLL: u32 = 17906;
const SKILL: u32 = 15802;
const CAST_MS: u32 = 3000;
/// One tick of the rig (a multiple of the 40 ms the plant is stepped by).
const DT: u64 = 240;
const WALK: f32 = 5.4;

// ---- the world ---------------------------------------------------------------------------------

/// A sea of 100 x 60 cells (1600 x 960 m) between a west coast (cells 0..=7) and an east coast
/// (cells 80..): terrain 110 m on land, 50 m at sea.
fn is_land(c: i32, _r: i32) -> bool {
    c <= 7 || c >= 80
}

fn ground(x: f32, y: f32) -> Option<f32> {
    ground_in(&NARROW, x, y)
}

/// A synthetic sea: `cols` x `rows` cells of 16 m, land where `land` says, the bot starts at `start`.
#[derive(Clone, Copy)]
struct Sea {
    cols: i32,
    rows: i32,
    land: fn(i32, i32) -> bool,
    start: [f32; 3],
}

/// The sea of most tests: 1600 x 960 m between two coasts.
const NARROW: Sea = Sea {
    cols: 100,
    rows: 60,
    land: is_land,
    start: [60.0, 480.0, 110.0],
};

/// A wide sea of 3520 x 3200 m between two coasts (cols 0..=7 and 212..) with a wall of land from
/// row 50 to the north edge in the middle: a trip from the west coast north of the wall's end to
/// the east coast has to round its southern end.
const WIDE: Sea = Sea {
    cols: 220,
    rows: 200,
    land: wide_land,
    start: [60.0, 1300.0, 110.0],
};

fn wide_land(c: i32, r: i32) -> bool {
    c <= 7 || c >= 212 || ((100..=108).contains(&c) && r >= 50)
}

fn ground_in(sea: &Sea, x: f32, y: f32) -> Option<f32> {
    let (c, r) = ((x / 16.0).floor() as i32, (y / 16.0).floor() as i32);
    if !(0..sea.cols).contains(&c) || !(0..sea.rows).contains(&r) {
        return None;
    }
    Some(if (sea.land)(c, r) { 110.0 } else { 50.0 })
}

const SHORT: TripCfg = TripCfg {
    launch_m: 900.0,
    min_m: 700.0,
    max_m: 1300.0,
    ship: ShipClass::ROWBOAT,
};

struct TestOps {
    map: WaterMap,
    sea: Sea,
    rng: RefCell<fastrand::Rng>,
    cfg: TripCfg,
}

impl TestOps {
    fn new(seed: u64) -> TestOps {
        TestOps::on(NARROW, seed)
    }

    fn on(sea: Sea, seed: u64) -> TestOps {
        TestOps {
            map: WaterMap::from_cells((0.0, 0.0), sea.cols as u32, sea.rows as u32, |c, r| {
                !(sea.land)(c as i32, r as i32)
            }),
            sea,
            rng: RefCell::new(fastrand::Rng::with_seed(seed)),
            cfg: SHORT,
        }
    }
}

impl Ops for TestOps {
    fn danger(&self, _from: (f32, f32)) -> Vec<Disc> {
        Vec::new()
    }

    fn plan(&self, from: (f32, f32), avoid: &[Disc]) -> Result<Plan, PlanError> {
        plan_trip(
            &self.map,
            &|x, y| ground_in(&self.sea, x, y),
            &mut self.rng.borrow_mut(),
            from,
            &self.cfg,
            avoid,
        )
    }

    fn route(&self, start: (f32, f32), plan: &Plan, avoid: &[Disc]) -> Option<Vec<(f32, f32)>> {
        sail_route(&self.map, start, plan, avoid, &self.cfg.ship)
    }

    fn shore_near(&self, from: (f32, f32), max_r: f32) -> Option<ShoreSpot> {
        find_shore(
            &self.map,
            &|x, y| ground_in(&self.sea, x, y),
            from,
            max_r,
            &[],
        )
    }

    fn ground(&self, x: f32, y: f32) -> Option<f32> {
        ground_in(&self.sea, x, y)
    }
}

// ---- the scripted server -----------------------------------------------------------------------

/// What the server does wrong (everything off = a server that works).
#[derive(Default, Clone)]
struct Script {
    /// GM commands are allowed (and `/item add self` works).
    gm: bool,
    /// The scroll is in the bag from the start.
    scroll: bool,
    /// The GM item never arrives.
    scroll_never: bool,
    /// The wheel's true direction (0 is read as +1, the documented one).
    sign: i8,
    /// Errors that answer the casts, one per cast in order.
    summon_errors: Vec<StateError>,
    /// The cast never makes a boat, and no error comes.
    no_slave: bool,
    /// The bind is acknowledged (`SCSlaveBound`) but no seat comes.
    no_attach: bool,
    /// The bind is not answered at all.
    ignore_bind: bool,
    /// The bind is always answered "too far".
    always_too_far: bool,
    ignore_discard: bool,
    ignore_despawn: bool,
    /// The hull has no engine.
    dead_engine: bool,
    /// The hull cannot get past this x (it is aground there).
    wall_x: Option<f32>,
    /// The server does not say the hull is aground (it never does).
    silent_wall: bool,
}

enum Later {
    Scroll,
    Boat,
}

struct Rig {
    now: u64,
    m: Machine,
    veh: VehicleState,
    errors: Vec<(u64, StateError)>,
    held: Option<(u64, u32)>,
    pos: [f32; 3],
    yaw: f32,
    motion: MotionStatus,
    dest: Option<((f32, f32), f32)>,
    plant: Option<Plant>,
    /// Metres the boat sailed and the farthest it was from where it started.
    travelled: f32,
    farthest: f32,
    /// Ticks the hull's box was not over the sea, the top speed reached, and the speed at the
    /// last tick before the helm was left.
    aground: u32,
    top_seen: f32,
    last_speed: f32,
    /// When the hull first ran into the wall, and when the machine said it was stuck.
    hit_ms: u64,
    stuck_ms: u64,
    script: Script,
    ops: TestOps,
    pick: Pick,
    later: Vec<(u64, Later)>,
    casts: usize,
    gm_lines: usize,
    // what the tests look at
    phases: Vec<Phase>,
    helms: Vec<(u64, Cmd)>,
    keys: Vec<String>,
    finds: Vec<Finding>,
    says: Vec<String>,
}

impl Rig {
    fn new(script: Script, seed: u64) -> Rig {
        Rig::on(NARROW, script, seed)
    }

    fn on(sea: Sea, script: Script, seed: u64) -> Rig {
        let start = sea.start;
        Rig {
            now: 1000,
            m: Machine::new(1000, start),
            veh: VehicleState::default(),
            errors: Vec::new(),
            held: script.scroll.then_some((9001, SCROLL)),
            pos: start,
            yaw: 0.0,
            motion: MotionStatus::Idle,
            dest: None,
            plant: None,
            travelled: 0.0,
            farthest: 0.0,
            aground: 0,
            top_seen: 0.0,
            last_speed: 0.0,
            hit_ms: 0,
            stuck_ms: 0,
            ops: TestOps::on(sea, seed),
            pick: Pick {
                item: SCROLL,
                skill: SKILL,
                cast_ms: CAST_MS,
                class: ShipClass::ROWBOAT,
            },
            later: Vec::new(),
            casts: 0,
            gm_lines: 0,
            phases: Vec::new(),
            helms: Vec::new(),
            keys: Vec::new(),
            finds: Vec::new(),
            says: Vec::new(),
            script,
        }
    }

    /// A rig that sails a boat of `class` (a bigger hull than the rowboat).
    fn with_class(script: Script, seed: u64, class: ShipClass) -> Rig {
        let mut rig = Rig::new(script, seed);
        rig.pick.class = class;
        rig.ops.cfg = TripCfg {
            ship: class,
            ..SHORT
        };
        rig
    }

    fn seated(&self) -> bool {
        self.veh.seat().is_some()
    }

    fn gone(&self) -> bool {
        self.veh.boat().is_none() && !self.seated() && self.plant.is_none()
    }

    /// One tick of the world, then one of the machine.
    fn tick(&mut self) -> GoalStatus {
        self.now += DT;
        self.server();
        let mut acts = Vec::new();
        let boat_unit = self
            .plant
            .as_ref()
            .filter(|_| self.veh.boat().is_some())
            .map(|p| [p.pos.0, p.pos.1, 100.5]);
        let io = Input {
            now_ms: self.now,
            char_id: CHAR,
            pos: self.pos,
            gm: self.script.gm,
            pick: self.pick,
            held: self.held,
            sailor: self.veh.sailor(),
            boat_unit,
            errors: &self.errors,
            motion: self.motion,
            ops: &self.ops,
        };
        let status = self.m.step(&io, &mut acts);
        if self.phases.last() != Some(&self.m.phase()) {
            self.phases.push(self.m.phase());
        }
        self.apply(acts);
        status
    }

    /// Runs until the machine ends or `limit_ms` of bot time have passed.
    fn run(&mut self, limit_ms: u64) -> GoalStatus {
        let end = self.now + limit_ms;
        while self.now < end {
            let st = self.tick();
            if st != GoalStatus::Running {
                return st;
            }
        }
        GoalStatus::Running
    }

    /// Runs until `ms` of bot time have passed or the machine ends.
    fn run_until(&mut self, at_ms: u64) -> GoalStatus {
        while self.now < at_ms {
            let st = self.tick();
            if st != GoalStatus::Running {
                return st;
            }
        }
        GoalStatus::Running
    }

    /// The goal is interrupted: `Machine::stop`, and the server carries out what it asked.
    fn interrupt(&mut self) {
        let mut acts = Vec::new();
        let io = Input {
            now_ms: self.now,
            char_id: CHAR,
            pos: self.pos,
            gm: self.script.gm,
            pick: self.pick,
            held: self.held,
            sailor: self.veh.sailor(),
            boat_unit: None,
            errors: &self.errors,
            motion: self.motion,
            ops: &self.ops,
        };
        self.m.stop(&io, &mut acts);
        self.apply(acts);
    }

    /// The server's side of the tick: walking, the due events, the boat.
    fn server(&mut self) {
        if self.motion == MotionStatus::Moving {
            if let Some((d, radius)) = self.dest {
                let (dx, dy) = (d.0 - self.pos[0], d.1 - self.pos[1]);
                let dist = dx.hypot(dy);
                let step = WALK * DT as f32 / 1000.0;
                if dist - radius <= step {
                    let k = ((dist - radius).max(0.0)) / dist.max(0.001);
                    self.pos[0] += dx * k;
                    self.pos[1] += dy * k;
                    self.motion = MotionStatus::Arrived;
                    self.dest = None;
                } else {
                    self.pos[0] += dx / dist * step;
                    self.pos[1] += dy / dist * step;
                }
            }
        }
        // the due events
        let now = self.now;
        let due: Vec<Later> = {
            let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.later)
                .into_iter()
                .partition(|(t, _)| *t <= now);
            self.later = rest;
            due.into_iter().map(|(_, l)| l).collect()
        };
        for l in due {
            match l {
                Later::Scroll => self.held = Some((9001, SCROLL)),
                Later::Boat => self.spawn_boat(),
            }
        }
        // the boat: it reports while somebody sits on it
        if self.seated() {
            let wall = self.script.wall_x;
            let origin = self.m.plan().map(|p| p.launch.water);
            if let Some(p) = &mut self.plant {
                for _ in 0..(DT / 40) {
                    let before = p.pos;
                    p.advance(0.04);
                    self.travelled += (p.pos.0 - before.0).hypot(p.pos.1 - before.1);
                    if let Some(o) = origin {
                        self.farthest = self.farthest.max((p.pos.0 - o.0).hypot(p.pos.1 - o.1));
                    }
                    if let Some(w) = wall {
                        if p.pos.0 > w {
                            p.pos.0 = w;
                            p.speed = 0.0;
                        }
                    }
                    self.top_seen = self.top_seen.max(p.speed);
                    self.last_speed = p.speed;
                    if !hull_over_sea(&self.ops.map, &self.pick.class, p.pos, p.yaw) {
                        self.aground += 1;
                    }
                    let mut m = p.motion(BOAT);
                    m.stuck = !self.script.silent_wall && wall.is_some_and(|w| p.pos.0 >= w);
                    if wall.is_some_and(|w| p.pos.0 >= w) && self.hit_ms == 0 {
                        self.hit_ms = self.now;
                    }
                    self.veh.on_event(&WorldEvent::ShipMoved(Box::new(m)), ME);
                }
                self.pos = [p.pos.0, p.pos.1, 100.5];
            }
        }
    }

    fn spawn_boat(&mut self) {
        let Some(plan) = self.m.plan() else {
            return;
        };
        let at = plan.launch.water;
        let sign = if self.script.sign == 0 {
            1
        } else {
            self.script.sign
        };
        let mut plant = Plant::new(self.yaw, sign).at(at.0, at.1);
        let class = self.pick.class;
        if class.size != aa_world::travel::HullSize::Small {
            // the hull is laid across the caster's view, bow to the caster's left, on the first
            // spot of the server's search that has room for it
            let depth = if class.size == aa_world::travel::HullSize::Medium {
                super::trip::LAUNCH_DEPTH_MEDIUM_M
            } else {
                super::trip::LAUNCH_DEPTH_LARGE_M
            };
            let sea = self.ops.sea;
            let at = aa_world::water::launch_ok(
                &self.ops.map,
                &|x, y| ground_in(&sea, x, y),
                &plan.launch,
                &class.launch_req(depth),
            )
            .expect("the planner launched from a spot with room for the hull");
            plant = Plant::of_class(&class, wrap(self.yaw + std::f32::consts::FRAC_PI_2), sign)
                .at(at.0, at.1);
        }
        if self.script.dead_engine {
            plant = plant.top(0.0);
        }
        self.plant = Some(plant);
        self.held = None;
        self.veh.on_event(
            &WorldEvent::SlaveCreated {
                owner_obj: ME,
                tl: TL,
                slave_obj: BOAT,
                hide_effect: false,
                creator: "bot".into(),
            },
            ME,
        );
        self.veh.on_event(
            &WorldEvent::MySlave(Box::new(MySlaveInfo {
                obj: BOAT,
                tl: TL,
                name: "Rowboat".into(),
                template: 3,
                hp: 100,
                max_hp: 100,
                pos: (
                    self.plant.as_ref().map_or(at.0, |p| p.pos.0),
                    self.plant.as_ref().map_or(at.1, |p| p.pos.1),
                    100.5,
                ),
            })),
            ME,
        );
    }

    fn remove_boat(&mut self) {
        self.plant = None;
        self.veh
            .on_event(&WorldEvent::SlaveDespawned { obj: BOAT }, ME);
        self.veh
            .on_event(&WorldEvent::SlaveRemoved { obj: ME, tl: TL }, ME);
    }

    /// The server carries out what the bot sent.
    fn apply(&mut self, acts: Vec<Act>) {
        for act in acts {
            match act {
                Act::Say(s) => {
                    if s.contains("stuck at") && self.stuck_ms == 0 {
                        self.stuck_ms = self.now;
                    }
                    self.says.push(s);
                }
                Act::Gm(line) => {
                    self.gm_lines += 1;
                    if self.script.gm
                        && !self.script.scroll_never
                        && line.starts_with("/item add self")
                    {
                        self.later.push((self.now + 300, Later::Scroll));
                    }
                }
                Act::Travel { x, y, radius } => {
                    self.motion = MotionStatus::Moving;
                    self.dest = Some(((x, y), radius));
                }
                Act::Face { x, y } => {
                    self.yaw = yaw_of(x - self.pos[0], y - self.pos[1]);
                }
                Act::StopMotion => {
                    if self.motion == MotionStatus::Moving {
                        self.motion = MotionStatus::Idle;
                    }
                    self.dest = None;
                }
                Act::Sync(_) | Act::Travelling | Act::Pin => {}
                Act::Summon { .. } => {
                    self.casts += 1;
                    if let Some(e) = self.script.summon_errors.get(self.casts - 1) {
                        self.errors.push((self.now + 100, *e));
                    } else if !self.script.no_slave {
                        self.later
                            .push((self.now + u64::from(CAST_MS) + 200, Later::Boat));
                    }
                }
                Act::Bind(_) => {
                    if self.script.ignore_bind {
                        continue;
                    }
                    if self.script.always_too_far {
                        self.errors
                            .push((self.now + 100, StateError::Msg(emsg::TOO_FAR_AWAY)));
                        continue;
                    }
                    let class = self.pick.class;
                    let near = self.plant.as_ref().is_some_and(|p| {
                        if class.size == aa_world::travel::HullSize::Small {
                            (p.pos.0 - self.pos[0]).hypot(p.pos.1 - self.pos[1]) <= 4.0
                        } else {
                            // (the server measures to the hull box)
                            hull_gap(&class, p.pos, self.yaw, (self.pos[0], self.pos[1])).0 <= 3.0
                        }
                    });
                    if !near {
                        self.errors
                            .push((self.now + 100, StateError::Msg(emsg::TOO_FAR_AWAY)));
                        continue;
                    }
                    self.veh.on_event(
                        &WorldEvent::SlaveBound {
                            master_id: CHAR,
                            slave_obj: BOAT,
                        },
                        ME,
                    );
                    if !self.script.no_attach {
                        self.veh.on_event(
                            &WorldEvent::UnitAttached {
                                child: ME,
                                point: 1,
                                parent: BOAT,
                                reason: 6,
                            },
                            ME,
                        );
                    }
                }
                Act::Discard(_) => {
                    if !self.script.ignore_discard && self.seated() {
                        self.veh
                            .on_event(&WorldEvent::UnitDetached { obj: ME, reason: 5 }, ME);
                    }
                }
                Act::Despawn(_) => {
                    if !self.script.ignore_despawn && self.plant.is_some() {
                        self.remove_boat();
                    }
                }
                Act::Helm { cmd, .. } => {
                    self.helms.push((self.now, cmd));
                    if let Some(p) = &mut self.plant {
                        p.request(cmd.throttle, cmd.steering);
                    }
                }
                Act::Report(r) => self.keys.push(r.key.clone()),
                Act::Find(f) => self.finds.push(f),
            }
        }
    }
}

/// An angle wrapped to `(-pi, pi]`.
fn wrap(a: f32) -> f32 {
    crate::bot::vehicle::wrap_pi(a)
}

/// Whether the hull box of `class` at `pos` heading `yaw` lies over sea cells (nine points).
fn hull_over_sea(map: &WaterMap, class: &ShipClass, pos: (f32, f32), yaw: f32) -> bool {
    let (fx, fy) = (-yaw.sin(), yaw.cos());
    let (rx, ry) = (yaw.cos(), yaw.sin());
    [-1.0f32, 0.0, 1.0].iter().all(|a| {
        [-1.0f32, 0.0, 1.0].iter().all(|b| {
            let (l, w) = (a * class.length / 2.0, b * class.beam / 2.0);
            map.sea_at(
                pos.0 + fx * (l + class.center_y) + rx * w,
                pos.1 + fy * (l + class.center_y) + ry * w,
            )
        })
    })
}

fn working() -> Script {
    Script {
        gm: true,
        ..Script::default()
    }
}

const TRIP_MS: u64 = 25 * 60_000;

// ---- a whole trip --------------------------------------------------------------------------------

#[test]
fn a_trip_runs_from_the_scroll_to_the_beach() {
    for (seed, sign) in [(1, 1), (2, -1), (3, 1), (4, -1)] {
        let mut rig = Rig::new(Script { sign, ..working() }, seed);
        let st = rig.run(TRIP_MS);
        assert_eq!(
            st,
            GoalStatus::Done,
            "seed {seed} sign {sign}: {:?}",
            rig.says
        );
        assert_eq!(
            rig.phases,
            [
                Phase::Gear,
                Phase::Plan,
                Phase::Shore,
                Phase::Summon,
                Phase::Board,
                Phase::Sail,
                Phase::Land
            ],
            "seed {seed}"
        );
        assert!(rig.m.sailed);
        assert!(rig.gone(), "seed {seed}: boat or seat left behind");
        assert_eq!(rig.gm_lines, 1, "the scroll is asked for once");
        assert!(rig.keys.is_empty(), "seed {seed}: reports {:?}", rig.keys);
        assert!(rig.finds.is_empty());
        // the bot is on the landing beach
        let plan = rig.m.plan().unwrap().clone();
        let d = (rig.pos[0] - plan.landing.stand.0).hypot(rig.pos[1] - plan.landing.stand.1);
        assert!(
            d < 6.0,
            "seed {seed}: {d:.1} m from the landing, kind {:?}",
            plan.kind
        );
        // the wheel's direction was found
        assert_eq!(rig.m.helm().sign(), sign, "seed {seed}");
        assert!(rig.m.helm().known() || sign == 1, "seed {seed}");
        assert!(rig.m.helm().flips() <= 1, "seed {seed}");
    }
}

#[test]
fn a_loop_goes_out_to_sea_and_comes_back_to_the_beach() {
    let mut loops = 0;
    for seed in 0..12 {
        let mut rig = Rig::new(working(), seed);
        let st = rig.run(TRIP_MS);
        assert_eq!(st, GoalStatus::Done, "seed {seed}: {:?}", rig.says);
        let plan = rig.m.plan().unwrap().clone();
        let d = (rig.pos[0] - plan.landing.stand.0).hypot(rig.pos[1] - plan.landing.stand.1);
        assert!(d < 6.0, "seed {seed}: {d:.1} m from the landing");
        assert!(
            rig.travelled > 0.7 * plan.route_m,
            "seed {seed}: sailed {:.0} m of {:.0}",
            rig.travelled,
            plan.route_m
        );
        if matches!(plan.kind, super::trip::Kind::Loop { .. }) {
            loops += 1;
            assert!(
                rig.farthest > 250.0,
                "seed {seed}: {:.0} m out",
                rig.farthest
            );
        }
    }
    assert!(loops > 0, "no seed made a loop");
}

#[test]
fn a_boat_slower_than_its_data_is_reported_once() {
    let mut rig = Rig::new(working(), 5);
    // the data says 6.4 m/s; the hull does 3.2
    rig.pick.class.top_speed = 6.4;
    assert_eq!(rig.run(TRIP_MS), GoalStatus::Done);
    assert_eq!(rig.keys, [format!("sail:slow_boat:{SCROLL}")]);
}

#[test]
fn the_planned_routes_fit_the_trip_limit() {
    for seed in 0..40 {
        let ops = TestOps::new(seed);
        let Ok(plan) = ops.plan((60.0, 480.0), &[]) else {
            continue;
        };
        assert!(
            plan.route_m <= 1.2 * SHORT.max_m + 1.0,
            "seed {seed}: {}",
            plan.route_m
        );
    }
}

#[test]
fn helm_packets_go_out_on_change_and_as_a_heartbeat() {
    let mut rig = Rig::new(working(), 5);
    assert_eq!(rig.run(TRIP_MS), GoalStatus::Done);
    let sail_ticks = {
        // (the trip takes a few minutes of ticks; every tick would have sent one)
        (rig.now - 1000) / DT
    };
    let n = rig.helms.len() as u64;
    assert!(n > 20, "{n}");
    assert!(n < sail_ticks, "{n} packets in {sail_ticks} ticks");
    // never two requests within one tick period, never a silence of over 1.3 s while sailing
    let mut gaps: Vec<u64> = rig.helms.windows(2).map(|w| w[1].0 - w[0].0).collect();
    gaps.sort_unstable();
    assert!(*gaps.last().unwrap() < 4000, "{:?}", gaps.last());
    assert!(*gaps.first().unwrap() >= DT);
}

#[test]
fn the_trip_ends_within_its_limit_and_the_helm_is_quiet_afterwards() {
    let mut rig = Rig::new(working(), 6);
    assert_eq!(rig.run(TRIP_MS), GoalStatus::Done);
    let last = rig.helms.last().unwrap().1;
    assert_eq!(last, Cmd::default(), "the last request stops the hull");
}

// ---- the scroll ------------------------------------------------------------------------------------

#[test]
fn without_gm_and_without_a_scroll_the_trip_does_not_start() {
    let mut rig = Rig::new(Script::default(), 1);
    assert_eq!(rig.tick(), GoalStatus::Failed(FailReason::NoTarget));
    assert_eq!(rig.gm_lines, 0);
    assert!(rig.helms.is_empty());
}

#[test]
fn a_scroll_in_the_bag_needs_no_gm() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            ..Script::default()
        },
        1,
    );
    assert_eq!(rig.run(TRIP_MS), GoalStatus::Done);
    assert_eq!(rig.gm_lines, 0);
}

#[test]
fn a_scroll_that_never_arrives_ends_the_trip_after_one_request() {
    let mut rig = Rig::new(
        Script {
            scroll_never: true,
            ..working()
        },
        1,
    );
    assert_eq!(rig.run(60_000), GoalStatus::Failed(FailReason::Timeout));
    assert_eq!(rig.gm_lines, 1);
}

// ---- the summon --------------------------------------------------------------------------------------

#[test]
fn a_refused_place_tries_another_beach() {
    for first in [
        StateError::Skill {
            skill: SKILL,
            result: skill_result::INVALID_LOCATION,
        },
        StateError::Msg(emsg::SLAVE_SPAWN_SHIP_NEED_MORE_SPACE),
        StateError::Msg(583),
    ] {
        let mut rig = Rig::new(
            Script {
                scroll: true,
                summon_errors: vec![first],
                ..Script::default()
            },
            9,
        );
        let st = rig.run(TRIP_MS);
        assert_eq!(st, GoalStatus::Done, "{first:?}: {:?}", rig.says);
        assert_eq!(rig.casts, 2, "{first:?}");
        assert!(rig.says.iter().any(|s| s.contains("another beach")));
        assert!(rig.gone());
        assert!(rig.keys.is_empty(), "{:?}", rig.keys);
    }
}

#[test]
fn a_place_that_is_always_refused_gives_up() {
    let e = StateError::Msg(emsg::SLAVE_SPAWN_SHIP_NEED_MORE_SPACE);
    let mut rig = Rig::new(
        Script {
            scroll: true,
            summon_errors: vec![e; 8],
            ..Script::default()
        },
        9,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(
        st,
        GoalStatus::Failed(FailReason::Other("no room to summon a boat"))
    );
    assert_eq!(rig.casts, 3);
    assert!(rig.gone());
}

#[test]
fn an_unexpected_refusal_is_a_finding_and_ends_the_trip() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            summon_errors: vec![StateError::Skill {
                skill: SKILL,
                result: skill_result::NO_PERM,
            }],
            ..Script::default()
        },
        2,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(
        st,
        GoalStatus::Failed(FailReason::Other("the summon was refused"))
    );
    assert_eq!(
        rig.finds,
        [Finding::SkillRefused {
            skill: SKILL,
            code: skill_result::NO_PERM
        }]
    );
}

#[test]
fn a_cast_that_makes_no_boat_is_reported_once() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            no_slave: true,
            ..Script::default()
        },
        2,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(st, GoalStatus::Failed(FailReason::Timeout));
    assert_eq!(rig.keys, [format!("sail:no_slave:{SKILL}")]);
    assert_eq!(rig.casts, 1, "one cast per scroll, no hammering");
}

#[test]
fn an_unexpected_error_code_after_the_cast_is_reported() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            summon_errors: vec![StateError::Msg(9)],
            ..Script::default()
        },
        2,
    );
    let _ = rig.run(40_000);
    assert!(
        rig.keys.contains(&"sail:error_code:9".to_string()),
        "{:?}",
        rig.keys
    );
}

// ---- boarding --------------------------------------------------------------------------------------

#[test]
fn a_bind_that_gets_no_seat_is_reported_and_the_boat_is_cleared() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            no_attach: true,
            ..Script::default()
        },
        3,
    );
    let st = rig.run(TRIP_MS);
    assert!(matches!(st, GoalStatus::Failed(_)), "{st:?}");
    assert!(
        rig.keys.contains(&"sail:bind_no_attach".to_string()),
        "{:?}",
        rig.keys
    );
    assert!(rig.gone());
}

#[test]
fn a_bind_nobody_answers_is_reported_and_the_boat_is_cleared() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            ignore_bind: true,
            ..Script::default()
        },
        3,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(
        st,
        GoalStatus::Failed(FailReason::Other("the bind got no answer"))
    );
    assert_eq!(rig.keys, ["sail:bind_no_response"]);
    assert!(rig.gone());
}

#[test]
fn a_server_that_always_says_too_far_is_a_bot_side_report() {
    let mut rig = Rig::new(
        Script {
            scroll: true,
            always_too_far: true,
            ..Script::default()
        },
        3,
    );
    let st = rig.run(TRIP_MS);
    assert!(matches!(st, GoalStatus::Failed(_)), "{st:?}");
    assert!(
        rig.keys.contains(&"sail:bind_reach".to_string()),
        "{:?}",
        rig.keys
    );
    assert!(rig.gone());
}

// ---- trouble at sea --------------------------------------------------------------------------------

#[test]
fn a_hull_without_an_engine_is_reported_and_the_trip_ends_ashore() {
    let mut rig = Rig::new(
        Script {
            dead_engine: true,
            ..working()
        },
        4,
    );
    let st = rig.run(TRIP_MS);
    assert!(matches!(st, GoalStatus::Failed(_)), "{st:?} {:?}", rig.says);
    assert!(
        rig.keys.contains(&"sail:no_thrust".to_string()),
        "{:?}",
        rig.keys
    );
    assert!(rig.gone());
}

#[test]
fn a_boat_aground_backs_off_and_gives_up_after_a_few_tries() {
    let mut rig = Rig::new(
        Script {
            wall_x: Some(500.0),
            ..working()
        },
        4,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(st, GoalStatus::Failed(FailReason::Stuck), "{:?}", rig.says);
    assert!(
        rig.says.iter().filter(|s| s.contains("stuck")).count() >= 2,
        "{:?}",
        rig.says
    );
    assert!(rig.gone());
}

// ---- the way out ------------------------------------------------------------------------------------

#[test]
fn a_despawn_that_is_ignored_is_reported_and_the_trip_still_ends() {
    let mut rig = Rig::new(
        Script {
            ignore_despawn: true,
            ..working()
        },
        5,
    );
    let st = rig.run(TRIP_MS);
    assert_eq!(st, GoalStatus::Done, "{:?}", rig.says);
    assert!(rig.keys.contains(&"sail:despawn_ignored".to_string()));
    assert!(!rig.seated());
}

#[test]
fn a_discard_that_is_ignored_is_reported_and_the_boat_is_removed() {
    let mut rig = Rig::new(
        Script {
            ignore_discard: true,
            ..working()
        },
        5,
    );
    let st = rig.run(TRIP_MS);
    assert!(matches!(st, GoalStatus::Failed(_)), "{st:?} {:?}", rig.says);
    assert!(rig.keys.contains(&"sail:discard_ignored".to_string()));
    assert!(rig.gone(), "the despawn takes the seat with it");
}

#[test]
fn the_goal_never_leaves_a_boat_behind() {
    // the goal is interrupted (a fight it lost, a rest, the operator) at every point of the trip
    let mut interrupted_with_boat = 0;
    let mut t = 3000;
    while t < 420_000 {
        let mut rig = Rig::new(working(), 7);
        let st = rig.run_until(1000 + t);
        if st != GoalStatus::Running {
            break;
        }
        let had_boat = rig.veh.boat().is_some();
        let cast_in_flight = rig.later.iter().any(|(_, l)| matches!(l, Later::Boat));
        rig.interrupt();
        // the server settles
        for _ in 0..8 {
            rig.now += DT;
            rig.server();
        }
        if cast_in_flight {
            // (the cast completes after the goal is gone: the next trip clears the boat)
            t += 7000;
            continue;
        }
        assert!(
            rig.gone(),
            "interrupted at +{t} ms in {:?}: boat {:?} seat {:?}",
            rig.m.phase(),
            rig.veh.boat(),
            rig.veh.seat()
        );
        interrupted_with_boat += usize::from(had_boat);
        t += 7000;
    }
    assert!(interrupted_with_boat > 10, "{interrupted_with_boat}");
}

#[test]
fn a_boat_left_by_an_interrupted_cast_is_cleared_by_the_next_trip() {
    let mut rig = Rig::new(working(), 8);
    // run until the cast is in flight, interrupt, let the boat appear
    while !rig.later.iter().any(|(_, l)| matches!(l, Later::Boat)) {
        assert_eq!(rig.tick(), GoalStatus::Running);
    }
    rig.interrupt();
    for _ in 0..40 {
        rig.now += DT;
        rig.server();
    }
    assert!(rig.veh.boat().is_some(), "the late boat is on the water");
    // the next trip: the old boat goes first, then everything runs as usual
    rig.m = Machine::new(rig.now, rig.pos);
    rig.phases.clear();
    rig.held = Some((9002, SCROLL));
    rig.casts = 0;
    let st = rig.run(TRIP_MS);
    assert_eq!(st, GoalStatus::Done, "{:?}", rig.says);
    assert!(rig.gone());
}

#[test]
fn stopping_a_goal_that_never_had_a_boat_sends_no_packet_but_the_stop() {
    let mut rig = Rig::new(working(), 1);
    let _ = rig.run(2000);
    let mut acts = Vec::new();
    let io = Input {
        now_ms: rig.now,
        char_id: CHAR,
        pos: rig.pos,
        gm: true,
        pick: rig.pick,
        held: rig.held,
        sailor: rig.veh.sailor(),
        boat_unit: None,
        errors: &rig.errors,
        motion: rig.motion,
        ops: &rig.ops,
    };
    rig.m.stop(&io, &mut acts);
    assert!(
        acts.iter().all(|a| matches!(a, Act::StopMotion)),
        "{acts:?}"
    );
}

#[test]
fn the_goal_owns_the_fight_only_at_sea() {
    let mut rig = Rig::new(working(), 1);
    assert!(!rig.m.owns_combat());
    let mut at_sea = false;
    while rig.tick() == GoalStatus::Running {
        let phase = rig.m.phase();
        assert_eq!(rig.m.owns_combat(), phase.at_sea(), "{phase:?}");
        at_sea |= rig.m.owns_combat();
    }
    assert!(at_sea);
}

#[test]
fn a_trip_that_runs_too_long_is_given_up() {
    // a world where nothing ever happens: the scroll is never added and the machine waits
    let mut rig = Rig::new(
        Script {
            scroll: true,
            ignore_bind: true,
            ..Script::default()
        },
        1,
    );
    rig.m = Machine::new(rig.now, rig.pos);
    // jump the clock past the trip limit while the machine is mid-trip
    let _ = rig.run(20_000);
    rig.now += 31 * 60_000;
    let st = rig.tick();
    assert!(
        matches!(st, GoalStatus::Running | GoalStatus::Failed(_)),
        "{st:?}"
    );
    let st = rig.run(300_000);
    assert!(matches!(st, GoalStatus::Failed(_)), "{st:?}");
}

// ---- bigger hulls ------------------------------------------------------------------------------------

/// The hulls of the real data (see `ShipClass`), by name.
fn hulls() -> [(&'static str, ShipClass); 2] {
    [
        ("clipper", ShipClass::CLIPPER),
        ("cutter", ShipClass::CUTTER),
    ]
}

#[test]
fn a_clipper_and_a_cutter_sail_a_trip_without_touching_the_shore() {
    for (name, class) in hulls() {
        for (seed, sign) in [(1, 1), (2, -1), (3, 1)] {
            let mut rig = Rig::with_class(Script { sign, ..working() }, seed, class);
            let st = rig.run(TRIP_MS);
            assert_eq!(st, GoalStatus::Done, "{name} seed {seed}: {:?}", rig.says);
            assert!(rig.gone(), "{name} seed {seed}: boat or seat left behind");
            assert_eq!(rig.aground, 0, "{name} seed {seed}: hull over land");
            assert!(rig.keys.is_empty(), "{name} seed {seed}: {:?}", rig.keys);
            assert!(rig.finds.is_empty());
            assert!(
                !rig.says.iter().any(|s| s.contains("stuck")),
                "{name} seed {seed}: {:?}",
                rig.says
            );
            // the hull reached its real top speed and was almost stopped at the berth
            assert!(
                rig.top_seen > 0.85 * class.top_speed && rig.top_seen < 1.05 * class.top_speed,
                "{name} seed {seed}: top speed {} of {}",
                rig.top_seen,
                class.top_speed
            );
            assert!(
                rig.last_speed < 1.5,
                "{name} seed {seed}: {}",
                rig.last_speed
            );
            // the bot swam to the landing's stand point
            let plan = rig.m.plan().unwrap().clone();
            let d = (rig.pos[0] - plan.landing.stand.0).hypot(rig.pos[1] - plan.landing.stand.1);
            assert!(d < 6.0, "{name} seed {seed}: {d:.1} m from the landing");
            // the hull berthed out at sea, clear of the beach by the planner's margin
            let berth = plan.moor;
            assert!(rig
                .ops
                .map
                .open_at(berth.0, berth.1, rig.ops.cfg.plan_clear()));
            assert_eq!(rig.m.helm().sign(), sign, "{name} seed {seed}");
        }
    }
}

#[test]
fn a_big_hull_is_launched_where_the_server_has_room_for_it() {
    let mut rig = Rig::with_class(working(), 3, ShipClass::CUTTER);
    assert_eq!(rig.run(TRIP_MS), GoalStatus::Done, "{:?}", rig.says);
    let plan = rig.m.plan().unwrap().clone();
    // the launch is a beach whose water runs on for the hull: the planner's own finder agrees
    let req = ShipClass::CUTTER.launch_req(super::trip::LAUNCH_DEPTH_LARGE_M);
    assert!(
        aa_world::water::launch_ok(&rig.ops.map, &ground, &plan.launch, &req).is_some(),
        "{:?}",
        plan.launch
    );
    // the boat was summoned and boarded from its side: the bot bound within the box's reach
    assert!(rig.says.iter().any(|s| s.contains("at the helm")));
}

#[test]
fn a_big_hull_stops_dead_and_backs_off_at_once_without_the_servers_stuck_flag() {
    // the server never says a hull is aground; a hull that ran and now stands with its engine on is
    for (name, class) in hulls() {
        let mut rig = Rig::with_class(
            Script {
                wall_x: Some(700.0),
                silent_wall: true,
                ..working()
            },
            4,
            class,
        );
        let st = rig.run(TRIP_MS);
        assert_eq!(
            st,
            GoalStatus::Failed(FailReason::Stuck),
            "{name}: {:?}",
            rig.says
        );
        assert!(rig.hit_ms > 0 && rig.stuck_ms > rig.hit_ms, "{name}");
        let reaction = rig.stuck_ms - rig.hit_ms;
        assert!(
            reaction < 4000,
            "{name}: {reaction} ms from the impact to backing off"
        );
        assert!(rig.gone());
    }
}

#[test]
fn a_cutter_rounds_the_end_of_a_wall_inside_the_water() {
    // the route from the west coast to the east coast goes round the southern end of the wall
    let mut rounded = 0;
    for seed in 0..6 {
        let mut rig = Rig::on(WIDE, working(), seed);
        rig.pick.class = ShipClass::CUTTER;
        rig.ops.cfg = TripCfg {
            min_m: 2200.0,
            max_m: 4200.0,
            ship: ShipClass::CUTTER,
            ..SHORT
        };
        let st = rig.run(60 * 60_000);
        assert_eq!(st, GoalStatus::Done, "seed {seed}: {:?}", rig.says);
        assert_eq!(rig.aground, 0, "seed {seed}: hull over land");
        assert!(rig.keys.is_empty(), "seed {seed}: {:?}", rig.keys);
        assert!(rig.gone());
        let plan = rig.m.plan().unwrap().clone();
        if plan.landing.stand.0 > 3000.0 {
            rounded += 1;
            // it went down to the passage below the wall and back up on the other side
            assert!(rig.travelled > 1.05 * (plan.landing.stand.0 - plan.launch.stand.0));
        }
    }
    assert!(rounded > 0, "no seed crossed the wall's end");
}
