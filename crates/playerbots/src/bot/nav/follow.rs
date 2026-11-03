//! The geometry of walking a navmesh route: a polyline of corners made smooth.
//!
//! [`Route`] is a parametric curve over the corner points of a `Path`. The bot is a point that
//! moves along it by arc length ([`Route::advance`]); nothing else about the bot enters, so the
//! walk is deterministic and testable without a world.
//!
//! * **Corners are rounded.** Every outdoor corner is replaced by a quadratic curve that starts up
//!   to [`FILLET_MAX_M`] before the corner. How much of the corner it may cut is limited
//!   ([`FILLET_MAX_CUT_M`]): the funnel already keeps the agent radius from obstacle corners, and
//!   the curve must not take that clearance away. Corners that touch an indoor or waypoint segment
//!   (doors, stairs, bridges) are walked exactly.
//! * **A lateral offset.** On long straight stretches the bot walks a little beside the line, the
//!   same side and distance every time (a stable per-bot habit, so a street full of bots is not a
//!   queue on one line). The offset fades in and out so it never bends the path at a corner, and a
//!   stretch only gets it when the mesh has room beside the line (`free`).
//!
//! The 2D (x, y) lengths are what the bot walks; heights are interpolated along the segments and
//! are only the fallback for where the terrain is not known (indoors, instances).

/// Longest distance before a corner at which the rounding starts.
pub const FILLET_MAX_M: f32 = 2.0;
/// How far inside the corner point the rounded curve may pass.
pub const FILLET_MAX_CUT_M: f32 = 0.3;
/// Turns smaller than this (radians) are not rounded.
const MIN_TURN_RAD: f32 = 0.05;
/// A straight stretch must be at least this long (metres) to take a lateral offset.
pub const LATERAL_MIN_RUN_M: f32 = 16.0;
/// Distance over which the offset fades in and out.
const LATERAL_TAPER_M: f32 = 8.0;
/// The mesh must reach this much beyond the offset line.
const LATERAL_MARGIN_M: f32 = 0.6;
/// Points closer than this (metres, x/y) are one.
const MERGE_M: f32 = 0.05;

/// Where on the curve the bot is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum At {
    /// On segment `s`, `u` metres from its start (between the end of the previous rounding and the
    /// start of the next).
    Seg { s: usize, u: f32 },
    /// In the rounding of corner `i` (between segments `i - 1` and `i`), `tau` in 0..1.
    Corner { i: usize, tau: f32 },
    /// Past the last point.
    End,
}

/// A route being walked. See the module docs.
#[derive(Debug, Clone)]
pub struct Route {
    pts: Vec<[f32; 3]>,
    /// Per segment: it touches a waypoint (indoors, stairs, a bridge).
    indoor: Vec<bool>,
    /// Per segment: the signed lateral offset (left of the walking direction positive), 0 = none.
    lat: Vec<f32>,
    /// Per point: how far before the corner the rounding starts (0 = none; the ends have none).
    fillet: Vec<f32>,
    at: At,
    total: f32,
    travelled: f32,
}

fn len2d(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The derivative of [`smooth`].
fn smooth_d(x: f32) -> f32 {
    if (0.0..=1.0).contains(&x) {
        6.0 * x * (1.0 - x)
    } else {
        0.0
    }
}

impl Route {
    /// Builds the route over `points` (`indoor[k]` says segment `k`, from point `k` to `k + 1`,
    /// is indoors). `lateral` is the bot's preferred offset in metres (sign = side); `free(x, y,
    /// z)` says whether the mesh has room at that point. `None` when the points do not make a
    /// route (fewer than two distinct ones).
    pub fn new(
        points: &[[f32; 3]],
        indoor: &[bool],
        lateral: f32,
        free: &dyn Fn(f32, f32, f32) -> bool,
    ) -> Option<Route> {
        let mut pts: Vec<[f32; 3]> = Vec::with_capacity(points.len());
        let mut ind: Vec<bool> = Vec::with_capacity(points.len());
        let mut acc = false;
        for (k, p) in points.iter().enumerate() {
            if !p.iter().all(|v| v.is_finite()) {
                continue;
            }
            let Some(&last) = pts.last() else {
                pts.push(*p);
                continue;
            };
            acc |= indoor.get(k.saturating_sub(1)).copied().unwrap_or(false);
            if len2d(last, *p) < MERGE_M {
                // (the goal replaces a point just before it)
                if k + 1 == points.len() && pts.len() > 1 {
                    if let Some(l) = pts.last_mut() {
                        *l = *p;
                    }
                }
                continue;
            }
            pts.push(*p);
            ind.push(acc);
            acc = false;
        }
        if pts.len() < 2 {
            return None;
        }
        let n = pts.len();
        let mut route = Route {
            fillet: vec![0.0; n],
            lat: vec![0.0; n - 1],
            total: 0.0,
            travelled: 0.0,
            at: At::Seg { s: 0, u: 0.0 },
            pts,
            indoor: ind,
        };
        route.total = (0..n - 1).map(|s| route.seg(s).0).sum();
        route.round_corners();
        route.offset_lines(lateral, free);
        Some(route)
    }

    fn seg(&self, s: usize) -> (f32, [f32; 2]) {
        let (a, b) = (self.pts[s], self.pts[s + 1]);
        let len = len2d(a, b).max(1e-6);
        (len, [(b[0] - a[0]) / len, (b[1] - a[1]) / len])
    }

    fn round_corners(&mut self) {
        for i in 1..self.pts.len() - 1 {
            if self.indoor[i - 1] || self.indoor[i] {
                continue;
            }
            let ((l0, d0), (l1, d1)) = (self.seg(i - 1), self.seg(i));
            let cos = (d0[0] * d1[0] + d0[1] * d1[1]).clamp(-1.0, 1.0);
            if cos.acos() < MIN_TURN_RAD {
                continue;
            }
            // the rounding passes `t * sin(turn / 2) / 2` inside the corner point
            let sin_half = ((1.0 - cos) * 0.5).sqrt();
            let t = FILLET_MAX_M
                .min(l0 * 0.5)
                .min(l1 * 0.5)
                .min(FILLET_MAX_CUT_M / (0.5 * sin_half).max(1e-3));
            self.fillet[i] = if t >= 0.05 { t } else { 0.0 };
        }
    }

    fn offset_lines(&mut self, lateral: f32, free: &dyn Fn(f32, f32, f32) -> bool) {
        if lateral.abs() < 0.2 {
            return;
        }
        for s in 0..self.pts.len() - 1 {
            let (a, b) = self.range(s);
            if self.indoor[s] || b - a < LATERAL_MIN_RUN_M {
                continue;
            }
            let (len, d) = self.seg(s);
            let normal = [-d[1], d[0]];
            let (p, q) = (self.pts[s], self.pts[s + 1]);
            for cand in [lateral, lateral * 0.5] {
                let reach = cand + cand.signum() * LATERAL_MARGIN_M;
                let ok = [0.25f32, 0.5, 0.75].iter().all(|f| {
                    let u = a + (b - a) * f;
                    let z = p[2] + (q[2] - p[2]) * (u / len);
                    free(
                        p[0] + d[0] * u + normal[0] * reach,
                        p[1] + d[1] * u + normal[1] * reach,
                        z,
                    )
                });
                if ok {
                    self.lat[s] = cand;
                    break;
                }
            }
        }
    }

    /// The part of segment `s` that is walked straight: from the end of the previous rounding to
    /// the start of the next one.
    fn range(&self, s: usize) -> (f32, f32) {
        let len = self.seg(s).0;
        (self.fillet[s], len - self.fillet[s + 1])
    }

    /// The fade of the lateral offset on segment `s` at `u`, and its derivative.
    fn lateral_weight(&self, s: usize, u: f32) -> (f32, f32) {
        if self.lat[s] == 0.0 {
            return (0.0, 0.0);
        }
        let (a, b) = self.range(s);
        let taper = LATERAL_TAPER_M.min((b - a) * 0.5).max(1e-3);
        let (x1, x2) = ((u - a) / taper, (b - u) / taper);
        let w = smooth(x1) * smooth(x2);
        let dw = (smooth_d(x1) * smooth(x2) - smooth(x1) * smooth_d(x2)) / taper;
        (w, dw)
    }

    /// Moves `ds` metres along the curve and returns the part of `ds` that was not used because
    /// the end was reached.
    pub fn advance(&mut self, ds: f32) -> f32 {
        let mut left = ds.max(0.0);
        let last_seg = self.pts.len() - 2;
        // (every pass either uses distance or moves on to the next piece)
        for _ in 0..4 * self.pts.len() + 64 {
            if left <= 0.0 {
                break;
            }
            match self.at {
                At::End => break,
                At::Seg { s, u } => {
                    let avail = (self.range(s).1 - u).max(0.0);
                    if left < avail {
                        self.at = At::Seg { s, u: u + left };
                        self.travelled += left;
                        return 0.0;
                    }
                    left -= avail;
                    self.travelled += avail;
                    self.at = if s >= last_seg {
                        At::End
                    } else if self.fillet[s + 1] > 0.0 {
                        At::Corner { i: s + 1, tau: 0.0 }
                    } else {
                        At::Seg { s: s + 1, u: 0.0 }
                    };
                }
                At::Corner { i, tau } => {
                    let t = self.fillet[i];
                    let (d0, d1) = (self.seg(i - 1).1, self.seg(i).1);
                    // speed of the curve per unit of tau
                    let rate = |tau: f32| {
                        let v = [
                            (1.0 - tau) * d0[0] + tau * d1[0],
                            (1.0 - tau) * d0[1] + tau * d1[1],
                        ];
                        (2.0 * t * (v[0] * v[0] + v[1] * v[1]).sqrt()).max(1e-3)
                    };
                    let step = left.min(t * 0.25).max(1e-4);
                    let mid = (tau + 0.5 * step / rate(tau)).min(1.0);
                    let dtau = step / rate(mid);
                    if tau + dtau >= 1.0 {
                        let used = ((1.0 - tau) * rate(mid)).min(left);
                        left -= used;
                        self.travelled += used;
                        self.at = At::Seg { s: i, u: t };
                    } else {
                        left -= step;
                        self.travelled += step;
                        self.at = At::Corner { i, tau: tau + dtau };
                    }
                }
            }
        }
        left
    }

    /// Whether the end of the route has been reached.
    pub fn is_done(&self) -> bool {
        self.at == At::End
    }

    /// The position on the curve: x, y and the height interpolated along the route.
    pub fn position(&self) -> [f32; 3] {
        match self.at {
            At::End => self.pts[self.pts.len() - 1],
            At::Seg { s, u } => {
                let (len, d) = self.seg(s);
                let (p, q) = (self.pts[s], self.pts[s + 1]);
                let (w, _) = self.lateral_weight(s, u);
                let off = self.lat[s] * w;
                [
                    p[0] + d[0] * u - d[1] * off,
                    p[1] + d[1] * u + d[0] * off,
                    p[2] + (q[2] - p[2]) * (u / len).clamp(0.0, 1.0),
                ]
            }
            At::Corner { i, tau } => {
                let t = self.fillet[i];
                let (d0, d1) = (self.seg(i - 1).1, self.seg(i).1);
                let c = self.pts[i];
                let a = [c[0] - d0[0] * t, c[1] - d0[1] * t];
                let b = [c[0] + d1[0] * t, c[1] + d1[1] * t];
                let (k0, k1, k2) = ((1.0 - tau).powi(2), 2.0 * tau * (1.0 - tau), tau * tau);
                [
                    k0 * a[0] + k1 * c[0] + k2 * b[0],
                    k0 * a[1] + k1 * c[1] + k2 * b[1],
                    c[2],
                ]
            }
        }
    }

    /// The unit direction of travel at the current position (x, y).
    pub fn heading(&self) -> (f32, f32) {
        let unit = |x: f32, y: f32| {
            let l = (x * x + y * y).sqrt();
            if l > 1e-6 {
                (x / l, y / l)
            } else {
                (0.0, 0.0)
            }
        };
        match self.at {
            At::End => {
                let s = self.pts.len() - 2;
                let d = self.seg(s).1;
                (d[0], d[1])
            }
            At::Seg { s, u } => {
                let d = self.seg(s).1;
                let (_, dw) = self.lateral_weight(s, u);
                let k = self.lat[s] * dw;
                unit(d[0] - d[1] * k, d[1] + d[0] * k)
            }
            At::Corner { i, tau } => {
                let (d0, d1) = (self.seg(i - 1).1, self.seg(i).1);
                unit(
                    (1.0 - tau) * d0[0] + tau * d1[0],
                    (1.0 - tau) * d0[1] + tau * d1[1],
                )
            }
        }
    }

    /// Metres left on the straight piece the bot walks, before the next corner (or the end of
    /// the route); 0 in the rounding of a corner. A bot that wants a straight run to jump on
    /// asks for it.
    pub fn straight_left(&self) -> f32 {
        match self.at {
            At::Seg { s, u } => (self.range(s).1 - u).max(0.0),
            At::Corner { .. } | At::End => 0.0,
        }
    }

    /// The current piece touches a waypoint (indoors, stairs, a bridge).
    pub fn indoor_here(&self) -> bool {
        match self.at {
            At::Seg { s, .. } => self.indoor[s],
            At::Corner { .. } => false,
            At::End => self.indoor.last().copied().unwrap_or(false),
        }
    }

    /// Length (metres) of the segment the bot is on.
    pub fn segment_len_here(&self) -> f32 {
        match self.at {
            At::Seg { s, .. } => self.seg(s).0,
            At::Corner { i, .. } => self.seg(i).0,
            At::End => 0.0,
        }
    }

    /// The last point of the route.
    pub fn end_point(&self) -> [f32; 3] {
        self.pts[self.pts.len() - 1]
    }

    /// Length of the whole route in metres (x/y).
    pub fn total_len(&self) -> f32 {
        self.total
    }

    /// Distance still to walk, roughly.
    pub fn left_len(&self) -> f32 {
        (self.total - self.travelled).max(0.0)
    }

    /// Number of corner points.
    pub fn len(&self) -> usize {
        self.pts.len()
    }

    /// Never empty (a route has at least two points).
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Appends the rest of the route as map points: the bot's position, then the corners ahead.
    pub fn remaining_points(&self, out: &mut Vec<[f32; 2]>) {
        let p = self.position();
        out.push([p[0], p[1]]);
        let next = match self.at {
            At::Seg { s, .. } => s + 1,
            At::Corner { i, .. } => i + 1,
            At::End => self.pts.len(),
        };
        out.extend(self.pts.iter().skip(next).map(|p| [p[0], p[1]]));
    }
}

/// Reduces `points` to at most `max` (at least 2), keeping the corners that matter most
/// (Ramer-Douglas-Peucker with a growing tolerance), the first and the last point always.
pub fn decimate(points: &mut Vec<[f32; 2]>, max: usize) {
    let max = max.max(2);
    if points.len() <= max {
        return;
    }
    let mut eps = 0.25f32;
    for _ in 0..32 {
        let keep = rdp(points, eps);
        if keep.iter().filter(|k| **k).count() <= max {
            let mut i = 0;
            points.retain(|_| {
                i += 1;
                keep[i - 1]
            });
            return;
        }
        eps *= 1.6;
    }
    // (cannot happen: the tolerance outgrows any route; kept so the result is bounded anyway)
    let n = points.len();
    let picked: Vec<[f32; 2]> = (0..max).map(|k| points[k * (n - 1) / (max - 1)]).collect();
    *points = picked;
}

/// Marks the points Ramer-Douglas-Peucker keeps for tolerance `eps`.
fn rdp(points: &[[f32; 2]], eps: f32) -> Vec<bool> {
    let n = points.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((lo, hi)) = stack.pop() {
        if hi <= lo + 1 {
            continue;
        }
        let (a, b) = (points[lo], points[hi]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        let mut far = (0.0f32, lo);
        for (k, p) in points.iter().enumerate().take(hi).skip(lo + 1) {
            let d = if len < 1e-6 {
                ((p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)).sqrt()
            } else {
                ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / len
            };
            if d > far.0 {
                far = (d, k);
            }
        }
        if far.0 > eps {
            keep[far.1] = true;
            stack.push((lo, far.1));
            stack.push((far.1, hi));
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always(_: f32, _: f32, _: f32) -> bool {
        true
    }

    fn never(_: f32, _: f32, _: f32) -> bool {
        false
    }

    /// Distance from `p` to the polyline through `pts` (x/y).
    fn dist_to_line(p: [f32; 3], pts: &[[f32; 3]]) -> f32 {
        pts.windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let l2 = dx * dx + dy * dy;
                let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0);
                ((p[0] - a[0] - dx * t).powi(2) + (p[1] - a[1] - dy * t).powi(2)).sqrt()
            })
            .fold(f32::MAX, f32::min)
    }

    fn walk(route: &mut Route, step: f32) -> Vec<([f32; 3], (f32, f32))> {
        let mut seen = vec![(route.position(), route.heading())];
        for _ in 0..100_000 {
            if route.advance(step) > 0.0 || route.is_done() {
                break;
            }
            seen.push((route.position(), route.heading()));
        }
        seen
    }

    #[test]
    fn a_straight_route_is_walked_to_its_end() {
        let pts = [[0.0, 0.0, 10.0], [30.0, 0.0, 10.0], [30.0, 40.0, 20.0]];
        let mut r = Route::new(&pts, &[false, false], 0.0, &never).unwrap();
        assert!((r.total_len() - 70.0).abs() < 1e-3);
        let seen = walk(&mut r, 0.25);
        assert!(r.is_done());
        let last = r.position();
        assert_eq!(last, [30.0, 40.0, 20.0]);
        // the walked length is the route length less what the rounding cuts off
        assert!((r.travelled - 70.0).abs() < 0.5, "{}", r.travelled);
        assert!(seen.len() > 250);
        // the height follows the segments: halfway up the second one it is 15
        let mid = seen
            .iter()
            .find(|(p, _)| (p[1] - 20.0).abs() < 0.2 && p[0] > 29.0)
            .unwrap();
        assert!((mid.0[2] - 15.0).abs() < 0.5, "{:?}", mid.0);
    }

    #[test]
    fn a_right_angle_is_rounded_but_stays_within_the_cut_limit() {
        let pts = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [20.0, 20.0, 0.0]];
        let mut r = Route::new(&pts, &[false, false], 0.0, &never).unwrap();
        // 90 degrees: the rounding starts 0.3 / (0.5 * sin 45) = 0.85 m before the corner
        assert!((r.fillet[1] - 0.8485).abs() < 0.01, "{}", r.fillet[1]);
        let seen = walk(&mut r, 0.1);
        let mut worst = 0.0f32;
        let mut max_turn = 0.0f32;
        let mut prev: Option<(f32, f32)> = None;
        for (p, h) in &seen {
            worst = worst.max(dist_to_line(*p, &pts));
            if let Some(q) = prev {
                let turn = (q.0 * h.0 + q.1 * h.1).clamp(-1.0, 1.0).acos();
                max_turn = max_turn.max(turn);
            }
            prev = Some(*h);
            // no corner overshoot: the curve stays inside the corner's quadrant
            assert!(p[0] <= 20.0 + 1e-3 && p[1] >= -1e-3, "{p:?}");
        }
        assert!(worst <= FILLET_MAX_CUT_M + 0.02, "cut {worst}");
        assert!(worst > 0.1, "it really does cut the corner: {worst}");
        // 90 degrees over about 1.3 m walked in 0.1 m steps: no step turns more than 10 degrees
        assert!(max_turn < 0.18, "turn per step {max_turn}");
        // it ends on the last point, facing along it
        assert_eq!(r.position(), [20.0, 20.0, 0.0]);
    }

    #[test]
    fn gentle_turns_round_wider_and_tiny_ones_not_at_all() {
        let pts = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [40.0, 4.0, 0.0]];
        let r = Route::new(&pts, &[false, false], 0.0, &never).unwrap();
        // 11 degrees: cut limited to 2 m by FILLET_MAX_M, not by the cut limit
        assert!((r.fillet[1] - FILLET_MAX_M).abs() < 1e-4, "{}", r.fillet[1]);
        let slight = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [40.0, 1.5, 0.0]];
        let r = Route::new(&slight, &[false, false], 0.0, &never).unwrap();
        assert!(r.fillet[1] > 0.0);
        let tiny = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [40.0, 0.5, 0.0]];
        let r = Route::new(&tiny, &[false, false], 0.0, &never).unwrap();
        assert_eq!(r.fillet[1], 0.0, "1.4 degrees is a straight line");
        let straight = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [40.0, 0.0, 0.0]];
        let r = Route::new(&straight, &[false, false], 0.0, &never).unwrap();
        assert_eq!(r.fillet[1], 0.0);
    }

    #[test]
    fn short_segments_share_their_length_between_neighbouring_corners() {
        // a zig-zag of 1 m steps: no rounding may take more than half of a segment
        let pts = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
        ];
        let mut r = Route::new(&pts, &[false; 4], 0.0, &never).unwrap();
        for i in 1..4 {
            assert!(r.fillet[i] <= 0.5 + 1e-4, "{:?}", r.fillet);
        }
        let seen = walk(&mut r, 0.05);
        assert!(r.is_done());
        for (p, _) in &seen {
            assert!(dist_to_line(*p, &pts) < 0.3, "{p:?}");
        }
    }

    #[test]
    fn indoor_segments_and_their_corners_are_walked_exactly() {
        let pts = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]];
        let r = Route::new(&pts, &[false, true], 0.0, &never).unwrap();
        assert_eq!(r.fillet, vec![0.0; 3]);
        let mut r = r;
        let seen = walk(&mut r, 0.2);
        for (p, _) in &seen {
            assert!(dist_to_line(*p, &pts) < 1e-3, "{p:?}");
        }
        let mut r = Route::new(&pts, &[false, true], 0.0, &never).unwrap();
        r.advance(12.0);
        assert!(r.indoor_here());
    }

    #[test]
    fn a_lateral_offset_fades_at_both_ends_and_needs_room() {
        let pts = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]];
        let mut r = Route::new(&pts, &[false], 1.0, &always).unwrap();
        assert_eq!(r.lat[0], 1.0);
        let seen = walk(&mut r, 0.5);
        // starts and ends on the line
        assert!(seen[0].0[1].abs() < 1e-4);
        assert!((r.position()[1]).abs() < 1e-4);
        // in the middle it is a full metre to the left (+y when walking +x)
        let mid = seen.iter().find(|(p, _)| p[0] >= 50.0).unwrap().0;
        assert!((mid[1] - 1.0).abs() < 1e-3, "{mid:?}");
        // the heading never leaves the segment by more than a few degrees
        for (_, h) in &seen {
            assert!(h.1.abs() < 0.2, "{h:?}");
        }
        // the heading is a unit vector
        for (_, h) in &seen {
            assert!(((h.0 * h.0 + h.1 * h.1) - 1.0).abs() < 1e-3);
        }
        // no room: no offset
        let r = Route::new(&pts, &[false], 1.0, &never).unwrap();
        assert_eq!(r.lat[0], 0.0);
        // room for half of it only
        let half = |_x: f32, y: f32, _z: f32| y.abs() <= 1.2;
        let r = Route::new(&pts, &[false], 1.0, &half).unwrap();
        assert_eq!(r.lat[0], 0.5);
        // short stretches and indoor ones never get one
        let short = [[0.0, 0.0, 0.0], [15.0, 0.0, 0.0]];
        assert_eq!(
            Route::new(&short, &[false], 1.0, &always).unwrap().lat[0],
            0.0
        );
        let r = Route::new(&pts, &[true], 1.0, &always).unwrap();
        assert_eq!(r.lat[0], 0.0);
    }

    #[test]
    fn the_offset_does_not_bend_a_corner() {
        // two long legs with a right-hand corner: both ends of each leg fade to the line
        let pts = [[0.0, 0.0, 0.0], [60.0, 0.0, 0.0], [60.0, 60.0, 0.0]];
        let mut r = Route::new(&pts, &[false, false], -1.0, &always).unwrap();
        let seen = walk(&mut r, 0.1);
        let mut worst = 0.0f32;
        let mut near_corner = 0.0f32;
        for (p, _) in &seen {
            worst = worst.max(dist_to_line(*p, &pts));
            if len2d(*p, [60.0, 0.0, 0.0]) < 1.5 {
                near_corner = near_corner.max(dist_to_line(*p, &pts));
            }
        }
        assert!(worst <= 1.0 + 1e-3, "{worst}");
        assert!(near_corner <= 0.5, "{near_corner}");
    }

    #[test]
    fn degenerate_input_makes_no_route() {
        assert!(Route::new(&[], &[], 0.0, &never).is_none());
        assert!(Route::new(&[[1.0, 1.0, 1.0]], &[], 0.0, &never).is_none());
        let same = [[1.0, 1.0, 1.0], [1.0, 1.0, 5.0]];
        assert!(Route::new(&same, &[false], 0.0, &never).is_none());
        let nan = [[0.0, 0.0, 0.0], [f32::NAN, 1.0, 0.0], [10.0, 0.0, 0.0]];
        let r = Route::new(&nan, &[false, false], 0.0, &never).unwrap();
        assert_eq!(r.len(), 2);
        // a goal that lies on the last corner replaces it
        let tail = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.01, 0.0, 3.0]];
        let r = Route::new(&tail, &[false, false], 0.0, &never).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r.end_point(), [10.01, 0.0, 3.0]);
    }

    #[test]
    fn decimation_keeps_the_ends_and_the_sharp_corners() {
        // a long zig-zag of 100 corners plus a dense straight run
        let mut pts: Vec<[f32; 2]> = (0..200).map(|i| [i as f32, 0.0]).collect();
        pts.extend((0..50).map(|i| [200.0 + (i % 2) as f32 * 30.0, i as f32 * 5.0]));
        let first = pts[0];
        let last = *pts.last().unwrap();
        decimate(&mut pts, 32);
        assert!(pts.len() <= 32);
        assert_eq!(pts[0], first);
        assert_eq!(*pts.last().unwrap(), last);
        // short lists are untouched
        let mut few = vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]];
        decimate(&mut few, 32);
        assert_eq!(few.len(), 3);
        // a straight line collapses to its ends
        let mut line: Vec<[f32; 2]> = (0..100).map(|i| [i as f32, i as f32]).collect();
        decimate(&mut line, 8);
        assert_eq!(line.len(), 2);
    }

    #[test]
    fn remaining_points_start_at_the_bot() {
        let pts = [[0.0, 0.0, 0.0], [20.0, 0.0, 0.0], [20.0, 20.0, 0.0]];
        let mut r = Route::new(&pts, &[false, false], 0.0, &never).unwrap();
        let mut out = Vec::new();
        r.remaining_points(&mut out);
        assert_eq!(out, vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0]]);
        r.advance(25.0);
        out.clear();
        r.remaining_points(&mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[1], [20.0, 20.0]);
        assert!(r.left_len() < 20.0);
    }
}
