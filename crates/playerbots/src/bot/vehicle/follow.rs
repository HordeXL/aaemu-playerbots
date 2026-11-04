//! Route following for a boat: pure pursuit along a polyline of waypoints.
//!
//! [`guide`] projects the boat on the current leg, looks [`Params::lookahead`] metres ahead along
//! the route from there (round corners too) and returns the heading to steer for, the distance
//! left to the end and whether the boat has arrived. It moves on to the next leg by itself when
//! the boat reaches a waypoint or passes it. The heading is in the bot convention (0 = +Y,
//! counter-clockwise positive).
//!
//! It also plans the speed ([`Guide::v_limit`]): a hull turns on a circle of `speed / turn rate`,
//! so the speed is held down where the route bends more sharply than the hull can follow, and it
//! falls towards the end of the route so that the hull arrives slowly. [`resample`] turns the
//! planner's long legs into points a few tens of metres apart, which is what the curvature of
//! [`guide`] needs.

use super::helm::Hull;

/// Tuning of [`guide`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// How far ahead on the route the boat aims (metres).
    pub lookahead: f32,
    /// A waypoint counts as reached within this distance (metres).
    pub reach: f32,
    /// The route's end counts as reached within this distance (metres).
    pub arrive: f32,
    /// The turn rate (rad/s) the hull can hold; 0 switches the speed limits for corners off.
    pub rate: f32,
    /// The deceleration (m/s^2) the speed plan counts on; 0 switches the speed plan off.
    pub decel: f32,
    /// Speed (m/s) wanted at the end of the route.
    pub v_end: f32,
    /// The lowest speed the plan asks for before the end (m/s): a hull needs steerage way.
    pub v_min: f32,
    /// How far ahead (m) corners are looked for.
    pub horizon: f32,
}

impl Params {
    /// A plan without speed limits.
    pub const fn basic(lookahead: f32, reach: f32, arrive: f32) -> Params {
        Params {
            lookahead,
            reach,
            arrive,
            rate: 0.0,
            decel: 0.0,
            v_end: 0.0,
            v_min: 0.0,
            horizon: 0.0,
        }
    }

    /// The tuning for a hull of `length` metres: the lookahead is the turn radius at top speed
    /// (70 m at least, the rowboat's), the speed plan stops the hull in 40 percent of its full
    /// astern deceleration. With `corners` the speed is also limited by the route's curvature (the
    /// route is then expected to be [`resample`]d, and `reach` is small); the rowboat's old
    /// coarse route is followed without.
    pub fn for_hull(hull: &Hull, length: f32, corners: bool) -> Params {
        let radius = hull.radius();
        Params {
            lookahead: (0.9 * radius).max(70.0),
            reach: if corners {
                10.0
            } else {
                (0.3 * radius).max(40.0)
            },
            arrive: (0.6 * length).max(18.0),
            rate: if corners { 0.85 * hull.cap } else { 0.0 },
            decel: 0.4 * hull.brake,
            v_end: 0.8,
            v_min: 1.2,
            horizon: hull.top * hull.top / (0.8 * hull.brake) + 150.0,
        }
    }
}

impl Default for Params {
    fn default() -> Self {
        Params::basic(45.0, 25.0, 15.0)
    }
}

/// What the boat should do now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guide {
    /// The heading to steer for.
    pub target_yaw: f32,
    /// Metres along the route to its end (from the boat).
    pub dist_end: f32,
    /// The boat is on the last leg.
    pub last_leg: bool,
    /// Distance from the route (metres).
    pub cross: f32,
    /// The boat is on the last leg and within [`Params::arrive`] of the route's end.
    pub arrived: bool,
    /// The speed (m/s) the hull should not exceed here, from the corners ahead and the end of the
    /// route; infinite without a speed plan ([`Params::decel`] 0).
    pub v_limit: f32,
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Guidance along `route` for a boat at `pos`; `leg` is the index of the leg the boat is on (the
/// segment from `route[leg]` to `route[leg + 1]`) and is advanced as the boat progresses. An empty
/// route is arrived, a one-point route is a destination.
pub fn guide(route: &[(f32, f32)], leg: &mut usize, pos: (f32, f32), p: &Params) -> Guide {
    let Some(&end) = route.last() else {
        return Guide {
            target_yaw: 0.0,
            dist_end: 0.0,
            last_leg: true,
            cross: 0.0,
            arrived: true,
            v_limit: 0.0,
        };
    };
    let to_end = dist(pos, end);
    if route.len() == 1 {
        return Guide {
            target_yaw: yaw(pos, end),
            dist_end: to_end,
            last_leg: true,
            cross: 0.0,
            arrived: to_end <= p.arrive,
            v_limit: speed_plan(&[pos, end], 0, pos, to_end, p),
        };
    }
    let n = route.len();
    *leg = (*leg).min(n - 2);
    // move on while the boat has reached or passed the end of the current leg
    let (q, t) = loop {
        let (a, b) = (route[*leg], route[*leg + 1]);
        let (sx, sy) = (b.0 - a.0, b.1 - a.1);
        let len2 = sx * sx + sy * sy;
        let t = if len2 < 1e-6 {
            1.0
        } else {
            (((pos.0 - a.0) * sx + (pos.1 - a.1) * sy) / len2).clamp(0.0, 1.0)
        };
        let q = (a.0 + sx * t, a.1 + sy * t);
        if *leg + 2 < n && (t >= 1.0 || dist(pos, b) <= p.reach) {
            *leg += 1;
            continue;
        }
        break (q, t);
    };
    let last_leg = *leg + 2 == n;
    // the arc length from the projection to the end
    let mut left = dist(q, route[*leg + 1]);
    for w in route[*leg + 1..].windows(2) {
        left += dist(w[0], w[1]);
    }
    // the point `lookahead` metres along the route from the projection
    let mut want = p.lookahead;
    let mut at = q;
    for next in &route[*leg + 1..] {
        let d = dist(at, *next);
        if d >= want && d > 1e-3 {
            let f = want / d;
            at = (at.0 + (next.0 - at.0) * f, at.1 + (next.1 - at.1) * f);
            want = 0.0;
            break;
        }
        want -= d;
        at = *next;
    }
    let _ = (want, t);
    Guide {
        target_yaw: yaw(pos, at),
        dist_end: dist(pos, q) + left,
        last_leg,
        cross: dist(pos, q),
        // (a loop ends where it starts: only the last leg can arrive)
        arrived: last_leg && to_end <= p.arrive,
        v_limit: speed_plan(route, *leg, q, left, p),
    }
}

/// The speed limit at the foot point `q` of the leg `leg` with `left` metres of route to go:
/// the speed from which the hull can still brake to the speed each corner ahead allows (the
/// corner's radius times the turn rate it can hold) and to `v_end` at the end of the route.
fn speed_plan(route: &[(f32, f32)], leg: usize, q: (f32, f32), left: f32, p: &Params) -> f32 {
    if p.decel <= 0.0 {
        return f32::INFINITY;
    }
    let brake = |v: f32, d: f32| (v * v + 2.0 * p.decel * d.max(0.0)).sqrt();
    let mut limit = brake(p.v_end, left - p.arrive);
    if p.rate <= 0.0 || route.len() < 3 {
        return limit;
    }
    // the corners ahead, vertex by vertex: the radius is the arc length per radian of turn
    let mut s = dist(q, route[leg + 1]);
    for j in leg + 1..route.len() - 1 {
        if s > p.horizon {
            break;
        }
        let (a, b, c) = (route[j - 1], route[j], route[j + 1]);
        let (len_in, len_out) = (dist(a, b), dist(b, c));
        if len_in > 1e-3 && len_out > 1e-3 {
            let cos = ((b.0 - a.0) * (c.0 - b.0) + (b.1 - a.1) * (c.1 - b.1)) / (len_in * len_out);
            let turn = cos.clamp(-1.0, 1.0).acos();
            if turn > 0.02 {
                let radius = 0.5 * (len_in + len_out) / turn;
                let v = (p.rate * radius).max(p.v_min);
                limit = limit.min(brake(v, s));
            }
        }
        s += len_out;
    }
    limit
}

/// The route as points at most `step` metres apart (the ends and every point of a long leg
/// kept in order), for the curvature of [`guide`].
pub fn resample(route: &[(f32, f32)], step: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::with_capacity(route.len());
    for (i, &b) in route.iter().enumerate() {
        if let Some(&a) = out.last().filter(|_| i > 0) {
            let d = dist(a, b);
            let n = (d / step).ceil() as usize;
            for k in 1..n {
                let t = k as f32 / n as f32;
                out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            }
        }
        out.push(b);
    }
    out
}

/// The yaw (bot convention) that faces `to` from `from`; 0 when they coincide.
fn yaw(from: (f32, f32), to: (f32, f32)) -> f32 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    if dx == 0.0 && dy == 0.0 {
        0.0
    } else {
        (-dx).atan2(dy)
    }
}

#[cfg(test)]
mod tests {
    use super::super::helm::{throttle, wanted_speed, Helm, Hull};
    use super::super::sim::Plant;
    use super::super::wrap_pi;
    use super::*;

    const P: Params = Params::basic(45.0, 25.0, 15.0);

    /// What the goal uses with a rowboat, which needs 84 m to turn round at full speed.
    const SAIL: Params = Params::basic(70.0, 40.0, 15.0);

    #[test]
    fn on_a_straight_leg_the_boat_aims_along_it() {
        let route = [(0.0, 0.0), (0.0, 1000.0)];
        let mut leg = 0;
        let g = guide(&route, &mut leg, (0.0, 100.0), &P);
        assert!(g.target_yaw.abs() < 1e-4);
        assert!((g.dist_end - 900.0).abs() < 0.01);
        assert!(g.last_leg && !g.arrived && g.cross < 0.01);
        // east of the route: the bow turns back towards it (north-west, positive yaw)
        let g = guide(&route, &mut leg, (30.0, 100.0), &P);
        assert!(g.target_yaw > 0.0 && g.target_yaw < 1.2, "{}", g.target_yaw);
        assert!((g.cross - 30.0).abs() < 0.01);
    }

    #[test]
    fn corners_advance_the_leg_and_are_cut_smoothly() {
        // north for 500 m, then east for 500 m
        let route = [(0.0, 0.0), (0.0, 500.0), (500.0, 500.0)];
        let mut leg = 0;
        let g = guide(&route, &mut leg, (0.0, 100.0), &P);
        assert_eq!(leg, 0);
        assert!(!g.last_leg);
        // near the corner the aim already bends towards the east (negative yaw)
        let g = guide(&route, &mut leg, (0.0, 480.0), &P);
        assert!(g.target_yaw < 0.0, "{}", g.target_yaw);
        // at the corner the next leg takes over
        let g = guide(&route, &mut leg, (0.0, 495.0), &P);
        assert_eq!(leg, 1);
        assert!(g.last_leg);
        assert!((g.target_yaw + std::f32::consts::FRAC_PI_2).abs() < 0.3);
        assert!((g.dist_end - 505.0).abs() < 1.0, "{}", g.dist_end);
    }

    #[test]
    fn the_end_is_reached_within_the_arrival_radius() {
        let route = [(0.0, 0.0), (0.0, 100.0)];
        let mut leg = 0;
        assert!(!guide(&route, &mut leg, (0.0, 80.0), &P).arrived);
        assert!(guide(&route, &mut leg, (0.0, 90.0), &P).arrived);
        assert!(guide(&[], &mut leg, (5.0, 5.0), &P).arrived);
        let one = [(10.0, 10.0)];
        let g = guide(&one, &mut leg, (-30.0, 10.0), &P);
        assert!(!g.arrived && (g.target_yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    }

    #[test]
    fn passing_a_waypoint_without_touching_it_still_advances() {
        let route = [(0.0, 0.0), (0.0, 500.0), (500.0, 500.0)];
        let mut leg = 0;
        // 40 m east of the corner, already beyond it on the first leg
        guide(&route, &mut leg, (40.0, 520.0), &P);
        assert_eq!(leg, 1);
    }

    /// Sails `route` with the helm and returns (worst cross-track, finish distance, seconds).
    fn sail(route: &[(f32, f32)], plant: &mut Plant, hull: Hull, secs: f32) -> (f32, f32, f32) {
        let mut helm = Helm::new(hull);
        let params = Params {
            arrive: SAIL.arrive,
            ..Params::for_hull(&hull, 3.0, false)
        };
        let mut now = 0u64;
        while let Some(c) = helm.probe(now, plant.yaw) {
            plant.request(c.throttle, c.steering);
            plant.advance(0.25);
            now += 250;
        }
        let mut leg = 0;
        let mut worst = 0.0f32;
        let end = *route.last().unwrap();
        let limit = now + (secs * 1000.0) as u64;
        while now < limit {
            let g = guide(route, &mut leg, plant.pos, &params);
            if g.arrived {
                break;
            }
            let err = wrap_pi(g.target_yaw - plant.yaw);
            let s = helm.steer(err, plant.yaw_rate_estimate(), plant.speed);
            helm.watch(now, err, s);
            let th = throttle(plant.speed, wanted_speed(&hull, err, g.v_limit));
            plant.request(th, s);
            plant.advance(0.25);
            now += 250;
            // (the leg's cross-track error, not counting the first seconds of turning round)
            if now > 60_000 {
                worst = worst.max(g.cross);
            }
        }
        (worst, dist(plant.pos, end), now as f32 / 1000.0)
    }

    #[test]
    fn the_boat_sails_a_dog_leg_route_for_either_wheel_direction() {
        // a 2.5 km route with three corners, the start heading away from the first leg
        let route = [
            (0.0, 0.0),
            (0.0, 700.0),
            (700.0, 1000.0),
            (700.0, 1700.0),
            (200.0, 2300.0),
        ];
        for sign in [1, -1] {
            let hull = Hull::ROWBOAT;
            let mut plant = Plant::new(-2.0, sign).hull(hull.accel, hull.cap);
            let (cross, left, secs) = sail(&route, &mut plant, hull, 1500.0);
            assert!(
                left <= SAIL.arrive + 1.0,
                "sign {sign}: {left} m short after {secs}s"
            );
            assert!(cross < 90.0, "sign {sign}: {cross} m off the route");
            assert!(secs < 1300.0, "sign {sign}: {secs} s");
        }
    }

    #[test]
    fn a_slow_turning_hull_and_a_fast_hull_both_arrive() {
        let route = [(0.0, 0.0), (500.0, 500.0), (0.0, 1000.0)];
        // the clipper-like hull (quick, slow to build up its turn) and a nimble one
        for (accel, cap, top) in [(0.019, 0.102, 19.5), (0.4, 0.15, 6.4)] {
            let hull = Hull {
                top,
                ..Hull::turning(accel, cap)
            };
            let mut plant = Plant::new(0.0, 1).hull(accel, cap).top(top);
            let (_, left, secs) = sail(&route, &mut plant, hull, 1500.0);
            assert!(
                left <= SAIL.arrive + 1.0,
                "hull {accel}/{cap}/{top}: {left} m short ({secs}s)"
            );
            assert!(
                plant.speed < 0.8 * top,
                "still fast at the end: {}",
                plant.speed
            );
        }
    }
}
