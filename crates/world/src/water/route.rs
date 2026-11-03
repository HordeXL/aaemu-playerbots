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

use rustc_hash::FxHashMap;

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
}

impl Default for RouteCfg<'_> {
    fn default() -> Self {
        RouteCfg {
            clear: 1,
            shore_penalty: 0.6,
            max_nodes: 40_000,
            avoid: &[],
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

/// The open cell nearest to `(x, y)` within [`SNAP_CELLS`] (the cell itself when it is open).
fn snap(map: &WaterMap, x: f32, y: f32, cfg: &RouteCfg<'_>) -> Option<Cell> {
    let (c0, r0) = map.cell_of(x, y)?;
    let mut best: Option<(f32, Cell)> = None;
    for k in 0..=SNAP_CELLS {
        for dr in -k..=k {
            for dc in -k..=k {
                if dr.abs().max(dc.abs()) != k {
                    continue;
                }
                let (c, r) = (c0 + dc, r0 + dr);
                if !usable(map, c, r, cfg) {
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

/// Whether the search may stand in cell `(c, r)`.
fn usable(map: &WaterMap, c: i32, r: i32, cfg: &RouteCfg<'_>) -> bool {
    if !map.open_cell(c, r, cfg.clear) {
        return false;
    }
    if cfg.avoid.is_empty() {
        return true;
    }
    let (x, y) = map.center_of(c, r);
    // a cell is closed when any part of it can touch a disc
    !cfg.avoid.iter().any(|d| {
        let reach = d.r + CELL_M * 0.75;
        let (dx, dy) = (x - d.x, y - d.y);
        dx * dx + dy * dy <= reach * reach
    })
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
    let start = snap(map, from.0, from.1, cfg).ok_or(RouteError::StartBlocked)?;
    let goal = snap(map, to.0, to.1, cfg).ok_or(RouteError::GoalBlocked)?;
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
                if !usable(map, next.0, next.1, cfg) {
                    continue;
                }
                let diagonal = dr != 0 && dc != 0;
                // no cutting corners: both cells beside the step must be usable too
                if diagonal && !(usable(map, c + dc, r, cfg) && usable(map, c, r + dr, cfg)) {
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
/// [`END_SLACK_M`] of the route's `ends`) and out of the discs to avoid.
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
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let (x, y) = (a.0 + dx * t, a.1 + dy * t);
        if cfg.avoid.iter().any(|d| d.contains(x, y)) {
            return false;
        }
        let near_end = [ends.0, ends.1].iter().any(|e| {
            let (ex, ey) = (x - e.0, y - e.1);
            ex * ex + ey * ey <= END_SLACK_M * END_SLACK_M
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
