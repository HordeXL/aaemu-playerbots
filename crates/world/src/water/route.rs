//! The water route planner: A* over the 16 m cells of a [`WaterMap`], then line-of-sight smoothing.
//!
//! The search only enters *open* cells (sea with [`RouteCfg::clear`] cells of water around them),
//! so a route keeps a hull's width from the shore; cells that are open but close to the shore cost
//! more, and cells near an [`Disc`] to avoid (a monster's territory) are closed. A start or goal
//! point that lies in the shallows is moved to the nearest open cell within [`SNAP_CELLS`]; the
//! route then still begins and ends at the exact points when a straight leg over sea reaches them.
//!
//! The search is bounded: at most [`RouteCfg::max_nodes`] cells are ever reached (the scratch map
//! and the open list stay under that), and it gives up with [`RouteError::TooComplex`] beyond it.
//! The scratch is a hash map of the cells reached, so a route across a 2000 x 2000 cell sea costs a
//! few hundred cells of memory, not a grid.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rustc_hash::{FxHashMap, FxHashSet};

use super::{WaterMap, CELL_M};

/// How far (cells) a start or goal point may be moved to reach an open cell.
pub const SNAP_CELLS: i32 = 6;
/// Distance between the samples of a line-of-sight test.
const LOS_STEP_M: f32 = 6.0;
/// Within this distance of the route's ends a leg may run over any sea cell, not only open ones.
const END_SLACK_M: f32 = 48.0;

/// A circle the route must stay out of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Disc {
    /// Centre x.
    pub x: f32,
    /// Centre y.
    pub y: f32,
    /// Radius in metres.
    pub r: f32,
}

impl Disc {
    /// Whether `(x, y)` is inside (or on the edge of) the disc.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (dx, dy) = (x - self.x, y - self.y);
        dx * dx + dy * dy <= self.r * self.r
    }
}

/// Parameters of a search.
#[derive(Debug, Clone, Copy)]
pub struct RouteCfg<'a> {
    /// Cells of water a route cell needs around it (1 = a 16 m margin to the shore).
    pub clear: i32,
    /// Extra cost factor of a cell that is open but has no room for `clear + 1`: 0.5 makes it cost
    /// 1.5 times as much.
    pub shore_penalty: f32,
    /// Most cells the search may reach.
    pub max_nodes: u32,
    /// Places the route must stay out of.
    pub avoid: &'a [Disc],
    /// How far (cells) a start or goal point may be moved to reach an open cell
    /// ([`SNAP_CELLS`]); a hull that needs a wide margin needs more.
    pub snap_cells: i32,
    /// Within this distance (m) of the route's ends a leg may run over any sea cell, not only
    /// open ones (48 m).
    pub end_slack_m: f32,
}

impl Default for RouteCfg<'_> {
    fn default() -> Self {
        RouteCfg {
            clear: 1,
            shore_penalty: 0.6,
            max_nodes: 40_000,
            avoid: &[],
            snap_cells: SNAP_CELLS,
            end_slack_m: END_SLACK_M,
        }
    }
}

/// A planned route.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// Waypoints from the start to the goal (both included), smoothed.
    pub points: Vec<(f32, f32)>,
    /// Length of the polyline in metres.
    pub length: f32,
    /// Cells the search reached (for tests and tuning).
    pub reached: u32,
}

/// Why no route was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// No open cell near the start.
    StartBlocked,
    /// No open cell near the goal.
    GoalBlocked,
    /// The open water around the start does not lead to the goal.
    NoRoute,
    /// The search reached [`RouteCfg::max_nodes`] cells without finding the goal.
    TooComplex,
}

type Cell = (i32, i32);

/// The open cell nearest to `(x, y)` within [`RouteCfg::snap_cells`] (the cell itself when it is
/// open).
fn snap(map: &WaterMap, x: f32, y: f32, cfg: &RouteCfg<'_>, closed: &Closed) -> Option<Cell> {
    let (c0, r0) = map.cell_of(x, y)?;
    let mut best: Option<(f32, Cell)> = None;
    for k in 0..=cfg.snap_cells {
        for dr in -k..=k {
            for dc in -k..=k {
                if dr.abs().max(dc.abs()) != k {
                    continue;
                }
                let (c, r) = (c0 + dc, r0 + dr);
                if !usable(map, c, r, cfg, closed) {
                    continue;
                }
                let (cx, cy) = map.center_of(c, r);
                let d = (cx - x) * (cx - x) + (cy - y) * (cy - y);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, (c, r)));
                }
            }
        }
        // a hit in ring k cannot be beaten by ring k + 2 or more
        if best.is_some() && k >= 1 {
            break;
        }
    }
    best.map(|b| b.1)
}

/// The cells the discs to avoid close: a cell is closed when any part of it can touch a disc.
/// Made once per search, so that a test of a cell is a lookup however many discs there are.
type Closed = FxHashSet<Cell>;

fn closed_cells(map: &WaterMap, avoid: &[Disc]) -> Closed {
    let mut closed = Closed::default();
    for d in avoid {
        let reach = d.r + CELL_M * 0.75;
        let Some((c0, r0)) = map.cell_of(d.x, d.y) else {
            continue;
        };
        let span = (reach / CELL_M).ceil() as i32 + 1;
        for r in r0 - span..=r0 + span {
            for c in c0 - span..=c0 + span {
                let (x, y) = map.center_of(c, r);
                let (dx, dy) = (x - d.x, y - d.y);
                if dx * dx + dy * dy <= reach * reach {
                    closed.insert((c, r));
                }
            }
        }
    }
    closed
}

/// Whether the search may stand in cell `(c, r)`.
fn usable(map: &WaterMap, c: i32, r: i32, cfg: &RouteCfg<'_>, closed: &Closed) -> bool {
    map.open_cell(c, r, cfg.clear) && (closed.is_empty() || !closed.contains(&(c, r)))
}

/// Octile distance between two cells in metres: the exact cost of the cheapest 8-connected path.
fn octile(a: Cell, b: Cell) -> f32 {
    let (dx, dy) = ((a.0 - b.0).abs() as f32, (a.1 - b.1).abs() as f32);
    let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
    (hi + (std::f32::consts::SQRT_2 - 1.0) * lo) * CELL_M
}

/// Plans a route over the sea from `from` to `to` (world x, y). See the module docs.
pub fn route(
    map: &WaterMap,
    from: (f32, f32),
    to: (f32, f32),
    cfg: &RouteCfg<'_>,
) -> Result<Route, RouteError> {
    let closed = closed_cells(map, cfg.avoid);
    let start = snap(map, from.0, from.1, cfg, &closed).ok_or(RouteError::StartBlocked)?;
    let goal = snap(map, to.0, to.1, cfg, &closed).ok_or(RouteError::GoalBlocked)?;
    let cols = map.cols() as i64;
    let id = |c: Cell| (i64::from(c.1) * cols + i64::from(c.0)) as u32;
    let cell_of = |n: u32| ((i64::from(n) % cols) as i32, (i64::from(n) / cols) as i32);

    // g cost and the node it was reached from, per cell reached
    let mut reached: FxHashMap<u32, (f32, u32)> = FxHashMap::default();
    reached.reserve(1024);
    let mut open: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let start_id = id(start);
    reached.insert(start_id, (0.0, start_id));
    open.push(Reverse((octile(start, goal).to_bits(), start_id)));
    let goal_id = id(goal);
    let mut found = start_id == goal_id;

    while let Some(Reverse((f_bits, n))) = open.pop() {
        if n == goal_id {
            found = true;
            break;
        }
        let (g, _) = reached[&n];
        let (c, r) = cell_of(n);
        // a stale entry: the cell was reached cheaper since and is queued again with that cost
        if f_bits > (g + octile((c, r), goal)).to_bits() {
            continue;
        }
        for dr in -1i32..=1 {
            for dc in -1i32..=1 {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let next = (c + dc, r + dr);
                if !usable(map, next.0, next.1, cfg, &closed) {
                    continue;
                }
                let diagonal = dr != 0 && dc != 0;
                // no cutting corners: both cells beside the step must be usable too
                if diagonal
                    && !(usable(map, c + dc, r, cfg, &closed)
                        && usable(map, c, r + dr, cfg, &closed))
                {
                    continue;
                }
                let step = if diagonal {
                    CELL_M * std::f32::consts::SQRT_2
                } else {
                    CELL_M
                };
                let near_shore = !map.open_cell(next.0, next.1, cfg.clear + 1);
                let cost = step
                    * if near_shore {
                        1.0 + cfg.shore_penalty
                    } else {
                        1.0
                    };
                let ng = g + cost;
                let nid = id(next);
                match reached.get(&nid) {
                    Some(&(old, _)) if old <= ng => continue,
                    None if reached.len() as u32 >= cfg.max_nodes => {
                        return Err(RouteError::TooComplex);
                    }
                    _ => {}
                }
                reached.insert(nid, (ng, n));
                open.push(Reverse(((ng + octile(next, goal)).to_bits(), nid)));
            }
        }
    }
    if !found {
        return Err(RouteError::NoRoute);
    }

    // walk back from the goal
    let mut cells = vec![goal_id];
    let mut at = goal_id;
    while at != start_id {
        at = reached[&at].1;
        cells.push(at);
    }
    cells.reverse();
    let centers: Vec<(f32, f32)> = cells
        .iter()
        .map(|&n| {
            let (c, r) = cell_of(n);
            map.center_of(c, r)
        })
        .collect();
    // the exact start and goal join the route when a straight leg over sea reaches the cell the
    // search used for them (a goal inland of the shore is not a point to sail to)
    let ends = (centers[0], centers[centers.len() - 1]);
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(centers.len() + 2);
    if map.sea_at(from.0, from.1) && line_clear(map, from, centers[0], ends, cfg) {
        pts.push(from);
    }
    pts.extend(centers.iter().copied());
    let last = centers[centers.len() - 1];
    if map.sea_at(to.0, to.1) && line_clear(map, last, to, ends, cfg) {
        pts.push(to);
    }
    let points = smooth(map, &pts, cfg);
    let length = points
        .windows(2)
        .map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt())
        .sum();
    Ok(Route {
        points,
        length,
        reached: reached.len() as u32,
    })
}

/// Greedy line-of-sight smoothing: from each anchor, jump to the farthest later point that is
/// visible over open water.
fn smooth(map: &WaterMap, pts: &[(f32, f32)], cfg: &RouteCfg<'_>) -> Vec<(f32, f32)> {
    let ends = (pts[0], pts[pts.len() - 1]);
    let mut out = vec![pts[0]];
    let mut i = 0;
    while i + 1 < pts.len() {
        // the next point is always reachable (it is a neighbour cell or an end point)
        let mut j = i + 1;
        for k in (i + 2..pts.len()).rev() {
            if line_clear(map, pts[i], pts[k], ends, cfg) {
                j = k;
                break;
            }
        }
        out.push(pts[j]);
        i = j;
    }
    out
}

/// Whether the straight leg `a` to `b` stays over open water (over any sea within
/// [`RouteCfg::end_slack_m`] of the route's `ends`) and out of the discs to avoid.
pub fn line_clear(
    map: &WaterMap,
    a: (f32, f32),
    b: (f32, f32),
    ends: ((f32, f32), (f32, f32)),
    cfg: &RouteCfg<'_>,
) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let steps = (len / LOS_STEP_M).ceil().max(1.0) as u32;
    // (only the discs the leg's bounding box touches can matter)
    let (lo, hi) = ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)));
    let near: Vec<&Disc> = cfg
        .avoid
        .iter()
        .filter(|d| {
            d.x + d.r >= lo.0 && d.x - d.r <= hi.0 && d.y + d.r >= lo.1 && d.y - d.r <= hi.1
        })
        .collect();
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let (x, y) = (a.0 + dx * t, a.1 + dy * t);
        if near.iter().any(|d| d.contains(x, y)) {
            return false;
        }
        let near_end = [ends.0, ends.1].iter().any(|e| {
            let (ex, ey) = (x - e.0, y - e.1);
            ex * ex + ey * ey <= cfg.end_slack_m * cfg.end_slack_m
        });
        let ok = if near_end {
            map.sea_at(x, y)
        } else {
            map.open_at(x, y, cfg.clear)
        };
        if !ok {
            return false;
        }
    }
    true
}

/// A turn of at least this many radians (about 149 degrees) is a hairpin for [`round_corners`].
const HAIRPIN_TURN: f32 = 2.6;

/// The half circle that turns a hull round at the vertex `b` of a hairpin: along the leg `u` to
/// `radius` before `b`, then half a circle of `radius` to the side `side` (+1 = to port) that ends
/// `2 * radius` beside the way out, heading back, and on to `c`. The circle's far point is `b`, so
/// the turn stays in the water the route was planned in. `None` when the leg in is shorter than
/// `1.2 * radius`, when the way on from the circle's end to `c` would bend by more than about 50
/// degrees, or when the circle or that way is not `ok` water on either side.
fn u_turn(
    b: (f32, f32),
    c: (f32, f32),
    u: (f32, f32),
    len_in: f32,
    radius: f32,
    side: f32,
    ok: &dyn Fn(f32, f32) -> bool,
) -> Option<Vec<(f32, f32)>> {
    if len_in < 1.2 * radius {
        return None;
    }
    let p1 = (b.0 - u.0 * radius, b.1 - u.1 * radius);
    for side in [side, -side] {
        let lat = (-u.1 * side, u.0 * side);
        let centre = (p1.0 + lat.0 * radius, p1.1 + lat.1 * radius);
        let a1 = (p1.1 - centre.1).atan2(p1.0 - centre.0);
        let n = ((radius * std::f32::consts::PI / 20.0).ceil() as usize).max(4);
        let arc: Vec<(f32, f32)> = (0..=n)
            .map(|k| {
                let a = a1 + std::f32::consts::PI * side * k as f32 / n as f32;
                (centre.0 + radius * a.cos(), centre.1 + radius * a.sin())
            })
            .collect();
        let end = arc[n];
        let (dx, dy) = (c.0 - end.0, c.1 - end.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        // the way on must head back, within about 50 degrees of the reverse of the way out
        if -(dx * u.0 + dy * u.1) / len < 0.64 {
            continue;
        }
        let steps = (len / 16.0).ceil() as usize;
        let way_ok = (1..=steps).all(|k| {
            let f = k as f32 / steps as f32;
            ok(end.0 + dx * f, end.1 + dy * f)
        });
        if way_ok && arc.iter().all(|p| ok(p.0, p.1)) {
            return Some(arc);
        }
    }
    None
}

/// Rounds the corners of a polyline with arcs of `radius` metres, for a hull that cannot turn on
/// the spot. At every vertex that turns by more than a few degrees the two legs are cut back by
/// `radius * tan(turn / 2)` (at most 45 percent of the shorter leg, so that neighbouring corners
/// do not meet) and the arc between the cut points replaces the vertex. An arc whose points are not
/// all `ok` (open enough water for the hull) is tried again with a smaller radius, three times, and
/// then the corner is left sharp. A hairpin (the route turns back on itself, as the loop of a trip
/// does at its far end) cannot be filleted, its cuts grow without bound: it becomes a half circle
/// of `radius` bulging past the vertex, and the way back runs `2 * radius` beside the way out
/// (see [`u_turn`]). The end points are kept.
pub fn round_corners(
    points: &[(f32, f32)],
    radius: f32,
    ok: &dyn Fn(f32, f32) -> bool,
) -> Vec<(f32, f32)> {
    if points.len() < 3 || radius < 1.0 {
        return points.to_vec();
    }
    let dir = |a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt();
        (dx / len.max(1e-3), dy / len.max(1e-3), len)
    };
    let mut out = vec![points[0]];
    for i in 1..points.len() - 1 {
        let (a, b, c) = (points[i - 1], points[i], points[i + 1]);
        let (ux, uy, len_in) = dir(a, b);
        let (wx, wy, len_out) = dir(b, c);
        let cos = (ux * wx + uy * wy).clamp(-1.0, 1.0);
        let turn = cos.acos();
        let cross = ux * wy - uy * wx;
        let side = if cross >= 0.0 { 1.0f32 } else { -1.0 };
        if turn >= HAIRPIN_TURN {
            if let Some(bulb) = u_turn(b, c, (ux, uy), len_in, radius, side, ok) {
                out.extend(bulb);
                continue;
            }
        }
        if turn < 0.15 || turn > 3.0 {
            out.push(b);
            continue;
        }
        let half = (turn / 2.0).tan();
        let limit = 0.45 * len_in.min(len_out);
        let mut r = radius.min(limit / half);
        let mut placed = false;
        for _ in 0..4 {
            let t = r * half;
            let p1 = (b.0 - ux * t, b.1 - uy * t);
            // the arc's centre lies on the inner side of the turn
            let centre = (p1.0 - uy * side * r, p1.1 + ux * side * r);
            let a1 = (p1.1 - centre.1).atan2(p1.0 - centre.0);
            let sweep = turn * side;
            let n = ((r * turn / 20.0).ceil() as usize).max(2);
            let arc: Vec<(f32, f32)> = (0..=n)
                .map(|k| {
                    let a = a1 + sweep * k as f32 / n as f32;
                    (centre.0 + r * a.cos(), centre.1 + r * a.sin())
                })
                .collect();
            if arc.iter().all(|p| ok(p.0, p.1)) {
                out.extend(arc);
                placed = true;
                break;
            }
            r *= 0.7;
            if r < 12.0 {
                break;
            }
        }
        if !placed {
            out.push(b);
        }
    }
    out.push(points[points.len() - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 60 x 60 cells of open sea with `land` cells (col, row) removed.
    fn sea(land: &[(u32, u32)]) -> WaterMap {
        WaterMap::from_cells((0.0, 0.0), 60, 60, |c, r| !land.contains(&(c, r)))
    }

    /// A wall of land cells at column `col` from row `lo` to `hi`.
    fn wall(col: u32, lo: u32, hi: u32) -> Vec<(u32, u32)> {
        (lo..=hi).map(|r| (col, r)).collect()
    }

    fn at(c: f32, r: f32) -> (f32, f32) {
        (c * CELL_M, r * CELL_M)
    }

    #[test]
    fn open_water_is_a_straight_line() {
        let m = sea(&[]);
        let r = route(&m, at(5.5, 5.5), at(50.5, 5.5), &RouteCfg::default()).unwrap();
        assert_eq!(r.points, vec![at(5.5, 5.5), at(50.5, 5.5)]);
        assert!((r.length - 45.0 * CELL_M).abs() < 0.01);
    }

    #[test]
    fn a_route_goes_around_an_island() {
        // a wall of land from row 2 to row 40 at column 30; the way round is at its open end
        let m = sea(&wall(30, 2, 40));
        let r = route(&m, at(10.5, 20.5), at(50.5, 20.5), &RouteCfg::default()).unwrap();
        assert!(r.points.len() >= 3, "{:?}", r.points);
        assert!(
            r.length > 40.0 * CELL_M * 1.2 && r.length < 80.0 * CELL_M,
            "{}",
            r.length
        );
        // every leg is clear of the wall (checked on the mask with the planner's own clearance)
        let cfg = RouteCfg::default();
        for w in r.points.windows(2) {
            assert!(line_clear(
                &m,
                w[0],
                w[1],
                (r.points[0], *r.points.last().unwrap()),
                &cfg
            ));
        }
        // the route does not cut through the wall
        for w in r.points.windows(2) {
            for s in 0..=40 {
                let t = s as f32 / 40.0;
                let (x, y) = (
                    w[0].0 + (w[1].0 - w[0].0) * t,
                    w[0].1 + (w[1].1 - w[0].1) * t,
                );
                assert!(m.sea_at(x, y));
            }
        }
    }

    #[test]
    fn no_route_when_the_sea_is_split() {
        let m = sea(&wall(30, 0, 59));
        assert_eq!(
            route(&m, at(10.5, 20.5), at(50.5, 20.5), &RouteCfg::default()),
            Err(RouteError::NoRoute)
        );
    }

    #[test]
    fn blocked_ends_are_reported() {
        // a block of land 11 cells wide: a point inside it has no open water within reach
        let block: Vec<(u32, u32)> = (25..=35)
            .flat_map(|c| (0..60).map(move |r| (c, r)))
            .collect();
        let m = sea(&block);
        assert_eq!(
            route(&m, at(10.5, 20.5), at(30.5, 20.5), &RouteCfg::default()),
            Err(RouteError::GoalBlocked)
        );
        assert_eq!(
            route(&m, at(30.5, 20.5), at(10.5, 20.5), &RouteCfg::default()),
            Err(RouteError::StartBlocked)
        );
        // outside the map
        assert_eq!(
            route(&m, at(-5.0, 20.5), at(10.5, 20.5), &RouteCfg::default()),
            Err(RouteError::StartBlocked)
        );
    }

    #[test]
    fn a_start_in_the_shallows_is_moved_to_open_water() {
        // land at column 10: cell 9 is sea but not open (no room for the clearance)
        let m = sea(&wall(10, 0, 59));
        let start = at(9.5, 20.5);
        let r = route(&m, start, at(40.5, 20.5), &RouteCfg::default());
        // the start is on the shore side of the wall and the goal is not: no way
        assert_eq!(r, Err(RouteError::NoRoute));
        let r = route(&m, start, at(3.5, 20.5), &RouteCfg::default()).unwrap();
        assert_eq!(r.points[0], start, "the route begins at the real start");
        assert_eq!(*r.points.last().unwrap(), at(3.5, 20.5));
    }

    #[test]
    fn avoid_discs_close_the_water_inside_them() {
        let m = sea(&[]);
        let disc = Disc {
            x: at(30.0, 20.5).0,
            y: at(30.0, 20.5).1,
            r: 200.0,
        };
        let cfg = RouteCfg {
            avoid: &[disc],
            ..RouteCfg::default()
        };
        let r = route(&m, at(10.5, 20.5), at(50.5, 20.5), &cfg).unwrap();
        assert!(r.points.len() >= 3, "{:?}", r.points);
        for w in r.points.windows(2) {
            for s in 0..=80 {
                let t = s as f32 / 80.0;
                let (x, y) = (
                    w[0].0 + (w[1].0 - w[0].0) * t,
                    w[0].1 + (w[1].1 - w[0].1) * t,
                );
                let deep = Disc {
                    r: disc.r - 1.0,
                    ..disc
                };
                assert!(!deep.contains(x, y), "({x}, {y}) is inside the disc");
            }
        }
        // a goal inside the disc is blocked
        let r = route(&m, at(10.5, 20.5), at(30.0, 20.5), &cfg);
        assert_eq!(r, Err(RouteError::GoalBlocked));
    }

    #[test]
    fn the_search_is_bounded() {
        let m = sea(&wall(30, 0, 59));
        let cfg = RouteCfg {
            max_nodes: 50,
            ..RouteCfg::default()
        };
        // a goal that is far away and cannot be reached explodes the search: it stops at the cap
        assert_eq!(
            route(&m, at(10.5, 20.5), at(50.5, 20.5), &cfg),
            Err(RouteError::TooComplex)
        );
        // a short route fits
        assert!(route(&m, at(10.5, 20.5), at(14.5, 20.5), &cfg).is_ok());
    }

    #[test]
    fn corners_are_rounded_with_arcs_of_the_radius() {
        // north 600 m, then east 600 m: a right-angle turn (to starboard, a negative cross)
        let pts = [(0.0, 0.0), (0.0, 600.0), (600.0, 600.0)];
        let r = round_corners(&pts, 140.0, &|_, _| true);
        assert_eq!(r[0], pts[0]);
        assert_eq!(*r.last().unwrap(), pts[2]);
        assert!(r.len() > 6, "{}", r.len());
        // the arc starts radius * tan(45 degrees) = 140 m before the corner and ends 140 m after
        assert!(
            (r[1].0).abs() < 0.01 && (r[1].1 - 460.0).abs() < 0.01,
            "{:?}",
            r[1]
        );
        let end = r[r.len() - 2];
        assert!(
            (end.0 - 140.0).abs() < 0.5 && (end.1 - 600.0).abs() < 0.5,
            "{end:?}"
        );
        // every arc point is 140 m from the centre (140, 460) and the middle cuts the corner
        for p in &r[1..r.len() - 1] {
            let d = ((p.0 - 140.0).powi(2) + (p.1 - 460.0).powi(2)).sqrt();
            assert!((d - 140.0).abs() < 0.5, "{p:?} {d}");
        }
        let mid = r[r.len() / 2];
        assert!(mid.0 > 20.0 && mid.1 < 580.0, "{mid:?}");
    }

    #[test]
    fn a_hairpin_becomes_a_half_circle_with_the_way_back_beside_the_way_out() {
        // out 600 m north, back to a point 40 m beside the start: a turn of nearly 180 degrees
        let pts = [(0.0, 0.0), (0.0, 600.0), (20.0, 40.0)];
        let r = round_corners(&pts, 70.0, &|_, _| true);
        assert_eq!(r[0], pts[0]);
        assert_eq!(*r.last().unwrap(), pts[2]);
        // the circle starts 70 m before the vertex and its far point is the vertex
        assert!(
            (r[1].0).abs() < 0.01 && (r[1].1 - 530.0).abs() < 0.01,
            "{:?}",
            r[1]
        );
        let far = r.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        assert!((far - 600.0).abs() < 2.0, "{far}");
        // the circle's points are 70 m from its centre, on the side the route turns to (here the
        // way back is to starboard of the way out, a negative cross)
        let centre = (70.0, 530.0);
        for p in &r[1..r.len() - 2] {
            let d = ((p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2)).sqrt();
            assert!((d - 70.0).abs() < 0.5, "{p:?} {d}");
        }
        let end = r[r.len() - 2];
        assert!(
            (end.0 - 140.0).abs() < 0.5 && (end.1 - 530.0).abs() < 0.5,
            "{end:?}"
        );
        // no corner of the result is tighter than the radius (the circumradius of every triple)
        for w in r.windows(3) {
            let (a, b, c) = (w[0], w[1], w[2]);
            let cross = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
            let sides = dist(a, b) * dist(b, c) * dist(a, c);
            if cross.abs() > 1e-3 {
                assert!(sides / (2.0 * cross.abs()) > 60.0, "{a:?} {b:?} {c:?}");
            }
        }
        // a hairpin that does not fit stays sharp
        assert_eq!(round_corners(&pts, 70.0, &|_, y| y < 520.0), pts.to_vec());
        // a short way in does not get a half circle either: the corner is cut inside the legs
        let short = [(0.0, 0.0), (0.0, 60.0), (20.0, 10.0)];
        let r = round_corners(&short, 70.0, &|_, _| true);
        assert!(r.iter().all(|p| p.1 < 60.0 + 1e-3), "{r:?}");
    }

    fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
        ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
    }

    #[test]
    fn short_legs_get_a_smaller_arc_and_blocked_arcs_stay_sharp() {
        let pts = [(0.0, 0.0), (0.0, 100.0), (100.0, 100.0)];
        let r = round_corners(&pts, 300.0, &|_, _| true);
        // the cut is limited to 45 percent of the legs: 45 m, an arc radius of 45 m
        assert!((r[1].1 - 55.0).abs() < 0.01, "{:?}", r[1]);
        // an arc over land is not made: the corner stays
        let r = round_corners(&pts, 300.0, &|_, _| false);
        assert_eq!(r, pts.to_vec());
        // nearly straight and degenerate routes are left alone
        let straight = [(0.0, 0.0), (0.0, 100.0), (1.0, 200.0)];
        assert_eq!(
            round_corners(&straight, 100.0, &|_, _| true),
            straight.to_vec()
        );
        assert_eq!(
            round_corners(&pts[..2], 100.0, &|_, _| true),
            pts[..2].to_vec()
        );
    }

    #[test]
    fn a_wider_snap_and_slack_reach_open_water_far_from_the_start() {
        // land at columns 0..=2 and a start in the shallows by it: with a margin of eight cells
        // the open water begins at column 11, further than the default six cells of snapping
        let land: Vec<(u32, u32)> = (0..60).flat_map(|r| (0..3).map(move |c| (c, r))).collect();
        let m = sea(&land);
        let start = at(3.5, 30.5);
        let goal = at(50.5, 30.5);
        let narrow = RouteCfg {
            clear: 8,
            ..RouteCfg::default()
        };
        assert_eq!(
            route(&m, start, goal, &narrow),
            Err(RouteError::StartBlocked)
        );
        let snapped = RouteCfg {
            clear: 8,
            snap_cells: 12,
            ..RouteCfg::default()
        };
        let r = route(&m, start, goal, &snapped).unwrap();
        // (the hull's own position joins the route only when a leg of sea reaches the open water)
        assert_ne!(r.points[0], start);
        let slack = RouteCfg {
            end_slack_m: 160.0,
            ..snapped
        };
        let r = route(&m, start, goal, &slack).unwrap();
        assert_eq!(r.points[0], start);
    }

    #[test]
    fn the_same_cell_is_a_route_of_two_points() {
        let m = sea(&[]);
        let r = route(&m, at(10.2, 10.2), at(10.8, 10.8), &RouteCfg::default()).unwrap();
        assert_eq!(r.points.len(), 2);
    }

    #[test]
    fn shore_cost_keeps_routes_off_the_shallows() {
        // a long coast at column 0..=2 of land: a route along it keeps away when it can
        let land: Vec<(u32, u32)> = (0..60).flat_map(|r| (0..3).map(move |c| (c, r))).collect();
        let m = sea(&land);
        let cfg = RouteCfg::default();
        let r = route(&m, at(6.5, 5.5), at(6.5, 50.5), &cfg).unwrap();
        for p in &r.points {
            assert!(m.open_at(p.0, p.1, 1), "{p:?}");
        }
    }
}
