//! Choosing a trip: where to launch, where to land, and which waters to stay out of.
//!
//! The launch is the nearest beach with open water in front of it ([`find_shore`]); the landing is
//! a beach near a random open-water point 1 to 3 km from the launch whose sea route is not much
//! longer than the straight line; failing that the trip is a sightseeing loop out to a point at
//! sea and back to the launch beach. Everything is computed on the shared sea map and the terrain
//! grid, with the places hostile sea monsters spawn closed to the planner ([`danger_zones`]).

use aa_world::water::{find_shore, route, Disc, RouteCfg, ShoreSpot, WaterMap};
use aa_world::World;

/// How far from the chosen sea point a landing beach is looked for.
pub const LANDING_SEARCH_M: f32 = 350.0;
/// Radius of the area closed around an aggressive sea monster's spawn (140 m shuts the whole of
/// Castaway Strait to a trip, 100 m leaves routes).
pub const DANGER_R: f32 = 100.0;
/// Most discs the planner is given (they are rasterised once per search, so this only bounds the
/// set's size).
pub const MAX_DANGER: usize = 400;
/// Candidate destinations tried before the trip becomes a loop.
const ATTEMPTS: u32 = 16;
/// The water route of a trip is this much longer than the straight line at most.
const DETOUR: f32 = 2.2;
/// A trip's water route is at most this much longer than the configured longest crossing (a boat
/// does 3 m/s; the whole trip has 40 minutes).
const MAX_ROUTE: f32 = 1.2;
/// Cells of water a sea route keeps around its path.
pub const ROUTE_CLEAR: i32 = 2;
/// The search bound of the planning routes (cells reached).
const PLAN_NODES: u32 = 30_000;

/// How long a trip is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TripCfg {
    /// How far from the bot a launch beach is looked for.
    pub launch_m: f32,
    /// Shortest crossing (metres from the launch water to the destination).
    pub min_m: f32,
    /// Longest crossing.
    pub max_m: f32,
}

impl Default for TripCfg {
    fn default() -> Self {
        TripCfg {
            launch_m: 900.0,
            min_m: 1000.0,
            max_m: 3000.0,
        }
    }
}

/// What kind of trip it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// From the launch beach to another beach.
    Cross,
    /// Out to a point at sea and back to the launch beach.
    Loop {
        /// The turning point.
        via: (f32, f32),
    },
}

/// A planned trip.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Where the bot stands, faces and summons.
    pub launch: ShoreSpot,
    /// Where the boat is sailed to and the bot comes ashore.
    pub landing: ShoreSpot,
    /// What kind of trip.
    pub kind: Kind,
    /// Length of the planned water route in metres.
    pub route_m: f32,
}

/// Why no trip was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanError {
    /// No launch beach near the bot.
    NoCoast,
    /// A beach, but no crossing or loop over open water from it.
    NoTrip,
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Plans a trip for a bot at `from` (see the module docs). `avoid` are the closed areas.
pub fn plan_trip(
    map: &WaterMap,
    ground: &dyn Fn(f32, f32) -> Option<f32>,
    rng: &mut fastrand::Rng,
    from: (f32, f32),
    cfg: &TripCfg,
    avoid: &[Disc],
) -> Result<Plan, PlanError> {
    let launch = find_shore(map, ground, from, cfg.launch_m, avoid).ok_or(PlanError::NoCoast)?;
    let rc = RouteCfg {
        clear: ROUTE_CLEAR,
        max_nodes: PLAN_NODES,
        avoid,
        ..RouteCfg::default()
    };
    // a crossing to another beach
    for _ in 0..ATTEMPTS {
        let ang = rng.f32() * std::f32::consts::TAU;
        let d = cfg.min_m + rng.f32() * (cfg.max_m - cfg.min_m);
        let t = (
            launch.water.0 + ang.cos() * d,
            launch.water.1 + ang.sin() * d,
        );
        if !map.open_at(t.0, t.1, ROUTE_CLEAR) {
            continue;
        }
        let Some(landing) = find_shore(map, ground, t, LANDING_SEARCH_M, avoid) else {
            continue;
        };
        let straight = dist(launch.water, landing.water);
        if straight < 0.6 * cfg.min_m {
            continue;
        }
        if let Ok(r) = route(map, launch.water, landing.water, &rc) {
            if r.length <= DETOUR * straight && r.length <= MAX_ROUTE * cfg.max_m {
                return Ok(Plan {
                    launch,
                    landing,
                    kind: Kind::Cross,
                    route_m: r.length,
                });
            }
        }
    }
    // a loop out to sea and back
    for _ in 0..ATTEMPTS {
        let ang = rng.f32() * std::f32::consts::TAU;
        let d = cfg.min_m * 0.5 + rng.f32() * cfg.min_m * 0.4;
        let via = (
            launch.water.0 + ang.cos() * d,
            launch.water.1 + ang.sin() * d,
        );
        if !map.open_at(via.0, via.1, ROUTE_CLEAR) {
            continue;
        }
        let (Ok(out), Ok(back)) = (
            route(map, launch.water, via, &rc),
            route(map, via, launch.water, &rc),
        ) else {
            continue;
        };
        if out.length <= DETOUR * d && out.length + back.length <= MAX_ROUTE * cfg.max_m {
            return Ok(Plan {
                launch,
                landing: launch,
                kind: Kind::Loop { via },
                route_m: out.length + back.length,
            });
        }
    }
    Err(PlanError::NoTrip)
}

/// The sea routes of `plan`, from `start` (the boat's position): the waypoints to sail, ending at
/// the landing water. `None` when no route exists even with a narrower margin to the shore.
pub fn sail_route(
    map: &WaterMap,
    start: (f32, f32),
    plan: &Plan,
    avoid: &[Disc],
) -> Option<Vec<(f32, f32)>> {
    for clear in [ROUTE_CLEAR, 1] {
        let rc = RouteCfg {
            clear,
            max_nodes: PLAN_NODES * 2,
            avoid,
            ..RouteCfg::default()
        };
        let mut pts: Vec<(f32, f32)> = Vec::new();
        let legs: Vec<(f32, f32)> = match plan.kind {
            Kind::Cross => vec![plan.landing.water],
            Kind::Loop { via } => vec![via, plan.landing.water],
        };
        let mut at = start;
        let mut ok = true;
        for to in legs {
            match route(map, at, to, &rc) {
                Ok(r) => {
                    let skip = usize::from(!pts.is_empty());
                    pts.extend(r.points.into_iter().skip(skip));
                    at = to;
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && pts.len() >= 2 {
            return Some(pts);
        }
    }
    None
}

/// Circles around the spawns of aggressive monsters that live on the water within `radius` of
/// `center`: sea monsters the boat must stay clear of. A monster counts when the bot's
/// `faction` is hostile to its faction (any aggressive monster when the faction is unknown); at
/// most [`MAX_DANGER`] discs, nearest to `center` first, spawns inside a disc already made add
/// none.
pub fn danger_zones(
    world: &World,
    map: &WaterMap,
    faction: u32,
    center: (f32, f32),
    radius: f32,
) -> Vec<Disc> {
    let mut found: Vec<(f32, Disc)> = Vec::new();
    world.npc_spawns_within(
        aa_world::Vec3::new(center.0, center.1, 0.0),
        radius,
        |template, pos| {
            if !map.sea_at(pos.x, pos.y) {
                return;
            }
            let Some(t) = world.npc(template) else {
                return;
            };
            // (a monster that does not attack on sight leaves a boat alone)
            let hostile = t.aggressive
                && (faction == 0
                    || world.relation(faction, t.faction_id) == aa_world::Relation::Hostile);
            if hostile {
                found.push((
                    dist(center, (pos.x, pos.y)),
                    Disc {
                        x: pos.x,
                        y: pos.y,
                        r: DANGER_R,
                    },
                ));
            }
        },
    );
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut discs: Vec<Disc> = Vec::new();
    for (_, d) in found {
        if discs.len() >= MAX_DANGER {
            break;
        }
        if !discs.iter().any(|e| e.contains(d.x, d.y)) {
            discs.push(d);
        }
    }
    discs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sea 100 x 60 cells (1600 x 960 m) with a west coast (land cells 0..=7) and an east coast
    /// (land cells 80..), terrain 110 m on land and 50 m at sea; with `island` a block of land in
    /// the middle too.
    fn two_coasts(island: bool) -> (WaterMap, impl Fn(f32, f32) -> Option<f32>) {
        let land = move |c: i32, r: i32| {
            c <= 7 || c >= 80 || (island && (40..=50).contains(&c) && (15..=45).contains(&r))
        };
        let map = WaterMap::from_cells((0.0, 0.0), 100, 60, |c, r| !land(c as i32, r as i32));
        let ground = move |x: f32, y: f32| {
            let (c, r) = ((x / 16.0).floor() as i32, (y / 16.0).floor() as i32);
            if !(0..100).contains(&c) || !(0..60).contains(&r) {
                return None;
            }
            Some(if land(c, r) { 110.0 } else { 50.0 })
        };
        (map, ground)
    }

    const SHORT: TripCfg = TripCfg {
        launch_m: 900.0,
        min_m: 700.0,
        max_m: 1300.0,
    };

    #[test]
    fn a_crossing_goes_from_one_coast_to_the_other() {
        let (map, ground) = two_coasts(false);
        // (the crossing is not the only possible trip: a landing further along the same coast is
        // one too, so look at a few seeds)
        let mut crossings = 0;
        for seed in 0..8 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let p = plan_trip(&map, &ground, &mut rng, (60.0, 480.0), &SHORT, &[]).unwrap();
            assert!(p.launch.stand.0 < 128.0, "{:?}", p.launch);
            let straight = dist(p.launch.water, p.landing.water);
            if p.kind == Kind::Cross {
                assert!(p.route_m >= straight - 0.01 && p.route_m < 2.2 * straight);
            }
            if p.kind == Kind::Cross && p.landing.stand.0 > 1250.0 {
                crossings += 1;
                assert!(p.route_m > 600.0, "{}", p.route_m);
            }
        }
        assert!(crossings > 0, "no seed crossed the sea");
    }

    #[test]
    fn no_beach_near_the_bot_is_no_coast() {
        let (map, ground) = two_coasts(false);
        let mut rng = fastrand::Rng::with_seed(1);
        // the middle of the sea, 650 m from either coast, with a search radius of 300 m
        let cfg = TripCfg {
            launch_m: 300.0,
            ..SHORT
        };
        assert_eq!(
            plan_trip(&map, &ground, &mut rng, (800.0, 480.0), &cfg, &[]),
            Err(PlanError::NoCoast)
        );
    }

    #[test]
    fn a_lonely_island_gives_a_loop() {
        // an 80 m island in an open sea: there is no other beach to cross to
        let island = |c: i32, r: i32| (20..=24).contains(&c) && (28..=32).contains(&r);
        let map = WaterMap::from_cells((0.0, 0.0), 100, 60, |c, r| !island(c as i32, r as i32));
        let ground = |x: f32, y: f32| {
            let (c, r) = ((x / 16.0).floor() as i32, (y / 16.0).floor() as i32);
            Some(if island(c, r) { 110.0 } else { 50.0 })
        };
        let mut rng = fastrand::Rng::with_seed(3);
        let p = plan_trip(&map, &ground, &mut rng, (360.0, 488.0), &SHORT, &[]).unwrap();
        assert!(matches!(p.kind, Kind::Loop { .. }), "{:?}", p.kind);
        assert_eq!(p.landing, p.launch);
        assert!(p.route_m > 400.0, "{}", p.route_m);
        // the way back leads to the launch water
        let r = sail_route(&map, p.launch.water, &p, &[]).unwrap();
        assert!(dist(*r.last().unwrap(), p.launch.water) < 60.0);
    }

    #[test]
    fn the_sail_route_leads_from_the_boat_to_the_landing_water() {
        let (map, ground) = two_coasts(true);
        let mut rng = fastrand::Rng::with_seed(11);
        let p = plan_trip(&map, &ground, &mut rng, (60.0, 480.0), &SHORT, &[]).unwrap();
        let start = (p.launch.water.0 + 8.0, p.launch.water.1 - 5.0);
        let r = sail_route(&map, start, &p, &[]).unwrap();
        assert_eq!(r[0], start);
        let end = *r.last().unwrap();
        assert!(
            dist(end, p.landing.water) < 60.0,
            "{end:?} vs {:?}",
            p.landing
        );
        // the whole way over open water except at its ends
        for w in r.windows(2) {
            for s in 1..20 {
                let t = s as f32 / 20.0;
                let (x, y) = (
                    w[0].0 + (w[1].0 - w[0].0) * t,
                    w[0].1 + (w[1].1 - w[0].1) * t,
                );
                assert!(map.sea_at(x, y), "({x}, {y}) is land");
            }
        }
    }

    #[test]
    fn closed_areas_keep_trips_out_of_them() {
        let (map, ground) = two_coasts(false);
        // a monster's water in the middle of the sea
        let disc = Disc {
            x: 800.0,
            y: 480.0,
            r: DANGER_R,
        };
        for seed in 0..6 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let p = plan_trip(&map, &ground, &mut rng, (60.0, 480.0), &SHORT, &[disc]).unwrap();
            let r = sail_route(&map, p.launch.water, &p, &[disc]).unwrap();
            for w in r.windows(2) {
                for s in 0..=40 {
                    let t = s as f32 / 40.0;
                    let (x, y) = (
                        w[0].0 + (w[1].0 - w[0].0) * t,
                        w[0].1 + (w[1].1 - w[0].1) * t,
                    );
                    assert!(
                        !Disc {
                            r: DANGER_R - 1.0,
                            ..disc
                        }
                        .contains(x, y),
                        "seed {seed}: ({x}, {y}) is inside the closed area"
                    );
                }
            }
        }
    }

    #[test]
    fn the_planning_does_not_repeat_itself() {
        let (map, ground) = two_coasts(false);
        let plan = |seed| {
            let mut rng = fastrand::Rng::with_seed(seed);
            plan_trip(&map, &ground, &mut rng, (60.0, 480.0), &SHORT, &[])
        };
        assert_eq!(plan(5), plan(5), "the same seed, the same trip");
        let landings: std::collections::HashSet<i32> = (0..12)
            .filter_map(|s| plan(s).ok())
            .map(|p| p.landing.stand.1.round() as i32 / 32)
            .collect();
        assert!(landings.len() > 1, "{landings:?}");
    }
}
