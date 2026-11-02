//! Candidate spots for a house inside a housing-area polygon.
//!
//! The server (`systems/housing/placement.rs::validate_house`) checks a placement against a
//! footprint of `cells x cells` squares of 4 m whose origin is snapped to the 4 m grid, and every
//! square must lie inside one housing polygon (all four corners). This module repeats that rule
//! so the bot only walks to spots the server has a chance to accept, and spaces the candidates
//! so one rejected spot does not make its neighbours fail for the same reason.

use aa_world::baked::HousingAreaPoly;
use aa_world::housing::contains;

/// Side of a placement cell in metres (`FOOTPRINT_CELL` of the server).
pub const CELL: f32 = 4.0;
/// A garden radius that needs this many cells or more has no footprint (the server skips the
/// footprint checks for it).
pub const MAX_CELLS: i32 = 12;

/// The number of cells a design's footprint is wide: `int((garden_radius * 2 + 3) / 4)`, 0 when
/// that is [`MAX_CELLS`] or more (`FUN_39897170` of the client, `footprint_cells` of the server).
pub fn cells_for_radius(garden_radius: f32) -> i32 {
    if !garden_radius.is_finite() {
        return 0;
    }
    let n = ((garden_radius * 2.0 + 3.0) * 0.25) as i32;
    if n >= MAX_CELLS {
        0
    } else {
        n.max(0)
    }
}

/// The corner of the footprint grid for a house placed at (x, y): the server snaps it to the 4 m
/// grid (`footprint_origin`).
pub fn footprint_origin(x: f32, y: f32, cells: i32) -> (f32, f32) {
    let half = cells as f32 * CELL * 0.5;
    let snap = |v: f32| ((v - half + CELL * 0.5) * 0.25) as i32 as f32 * CELL;
    (snap(x), snap(y))
}

/// Whether a house with a footprint of `cells` placed at (x, y) lies inside `poly` the way the
/// server wants it: the centre inside, and every 4 m square of the snapped footprint with all
/// four corners inside. `cells == 0` (no footprint) only needs the centre.
pub fn footprint_fits(poly: &[[f32; 2]], x: f32, y: f32, cells: i32) -> bool {
    if !contains(poly, x, y) {
        return false;
    }
    if cells <= 0 {
        return true;
    }
    let (x0, y0) = footprint_origin(x, y, cells);
    for ix in 0..cells {
        for iy in 0..cells {
            let cx = x0 + ix as f32 * CELL;
            let cy = y0 + iy as f32 * CELL;
            let corners = [
                (cx, cy),
                (cx + CELL, cy),
                (cx, cy + CELL),
                (cx + CELL, cy + CELL),
            ];
            if !corners.iter().all(|&(px, py)| contains(poly, px, py)) {
                return false;
            }
        }
    }
    true
}

/// Side of the footprint in metres (at least one cell).
pub fn side(cells: i32) -> f32 {
    cells.max(1) as f32 * CELL
}

/// Distance from `p` to the polygon: 0 inside, else to the nearest edge.
pub fn dist_to_polygon(poly: &[[f32; 2]], p: [f32; 2]) -> f32 {
    if contains(poly, p[0], p[1]) {
        return 0.0;
    }
    let n = poly.len();
    let mut best = f32::MAX;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        best = best.min(dist_to_segment(p, a, b));
    }
    best
}

fn dist_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (a[0] + abx * t, a[1] + aby * t);
    ((p[0] - qx).powi(2) + (p[1] - qy).powi(2)).sqrt()
}

/// The housing areas worth a look: polygons the design's category is allowed in (`allows`), at
/// least `min_extent` metres wide and tall, within `max_dist` metres of `from`; nearest first,
/// at most `limit`. Returns the polygon ids.
pub fn rank_areas(
    polys: &[HousingAreaPoly],
    allows: impl Fn(u32) -> bool,
    from: [f32; 2],
    max_dist: f32,
    min_extent: f32,
    limit: usize,
) -> Vec<u32> {
    let mut ranked: Vec<(f32, u32)> = polys
        .iter()
        .filter(|p| p.points.len() >= 3 && allows(p.id))
        .filter(|p| {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for q in &p.points {
                lo = [lo[0].min(q[0]), lo[1].min(q[1])];
                hi = [hi[0].max(q[0]), hi[1].max(q[1])];
            }
            hi[0] - lo[0] >= min_extent && hi[1] - lo[1] >= min_extent
        })
        .map(|p| (dist_to_polygon(&p.points, from), p.id))
        .filter(|(d, _)| *d <= max_dist)
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, id)| id).collect()
}

/// How the spots of one area are sampled.
#[derive(Debug, Clone, Copy)]
pub struct SampleOpts {
    /// Footprint cells of the design (see [`cells_for_radius`]).
    pub cells: i32,
    /// Free space between two candidates beyond the footprint, in metres.
    pub gap: f32,
    /// Only the part of the polygon within this many metres of the anchor is sampled.
    pub window: f32,
    /// Most candidates returned.
    pub max: usize,
    /// Candidates closer than this to a point of `avoid` are dropped.
    pub avoid_r: f32,
}

impl Default for SampleOpts {
    fn default() -> Self {
        SampleOpts {
            cells: 4,
            gap: 4.0,
            window: 120.0,
            max: 12,
            avoid_r: 6.0,
        }
    }
}

/// Candidate positions for a house inside `poly`: a grid spaced footprint + gap, around the
/// point of the polygon nearest to `from`, every candidate passing [`footprint_fits`] and keeping
/// `avoid_r` from the points in `avoid`; nearest to `from` first (with a little jitter in the
/// order so two bots in the same area do not always pick the same spot).
pub fn sample_spots(
    poly: &[[f32; 2]],
    from: [f32; 2],
    o: &SampleOpts,
    avoid: &[[f32; 2]],
    rng: &mut fastrand::Rng,
) -> Vec<[f32; 2]> {
    if poly.len() < 3 {
        return Vec::new();
    }
    let step = side(o.cells) + o.gap.max(0.0);
    let anchor = if contains(poly, from[0], from[1]) {
        from
    } else {
        nearest_polygon_point(poly, from)
    };
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for q in poly {
        lo = [lo[0].min(q[0]), lo[1].min(q[1])];
        hi = [hi[0].max(q[0]), hi[1].max(q[1])];
    }
    let lo = [
        lo[0].max(anchor[0] - o.window),
        lo[1].max(anchor[1] - o.window),
    ];
    let hi = [
        hi[0].min(anchor[0] + o.window),
        hi[1].min(anchor[1] + o.window),
    ];
    let (jx, jy) = (rng.f32() * step, rng.f32() * step);
    let mut found: Vec<(f32, [f32; 2])> = Vec::new();
    let mut x = lo[0] + jx;
    while x <= hi[0] {
        let mut y = lo[1] + jy;
        while y <= hi[1] {
            let near_avoid = avoid
                .iter()
                .any(|a| (a[0] - x).hypot(a[1] - y) < o.avoid_r + side(o.cells) * 0.5);
            if !near_avoid && footprint_fits(poly, x, y, o.cells) {
                let d = (x - from[0]).hypot(y - from[1]);
                found.push((d * (0.8 + rng.f32() * 0.4), [x, y]));
            }
            y += step;
        }
        x += step;
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.truncate(o.max);
    found.into_iter().map(|(_, p)| p).collect()
}

/// The point of the polygon outline nearest to `p`.
fn nearest_polygon_point(poly: &[[f32; 2]], p: [f32; 2]) -> [f32; 2] {
    let n = poly.len();
    let mut best = (f32::MAX, poly[0]);
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
        let len2 = abx * abx + aby * aby;
        let t = if len2 <= f32::EPSILON {
            0.0
        } else {
            (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0)
        };
        let q = [a[0] + abx * t, a[1] + aby * t];
        let d = (p[0] - q[0]).hypot(p[1] - q[1]);
        if d < best.0 {
            best = (d, q);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(lo: f32, hi: f32) -> Vec<[f32; 2]> {
        vec![[lo, lo], [hi, lo], [hi, hi], [lo, hi]]
    }

    fn poly(id: u32, points: Vec<[f32; 2]>) -> HousingAreaPoly {
        HousingAreaPoly {
            id,
            zone_key: 0,
            points,
        }
    }

    #[test]
    fn footprint_sizes_follow_the_servers_rule() {
        assert_eq!(cells_for_radius(4.0), 2); // scarecrow garden: 11 / 4
        assert_eq!(cells_for_radius(7.5), 4); // cottage: 18 / 4
        assert_eq!(cells_for_radius(0.0), 0); // 3 / 4
        assert_eq!(cells_for_radius(20.0), 10); // 43 / 4 = 10: still a footprint
        assert_eq!(cells_for_radius(22.5), 0); // 48 / 4 = 12: none
        assert_eq!(cells_for_radius(f32::NAN), 0);
    }

    #[test]
    fn footprint_origin_snaps_to_the_four_metre_grid() {
        // 2 cells: half = 4, snap(v) = trunc((v - 4 + 2) / 4) * 4
        assert_eq!(footprint_origin(100.0, 100.0, 2), (96.0, 96.0));
        assert_eq!(footprint_origin(101.9, 98.0, 2), (96.0, 96.0));
        assert_eq!(footprint_origin(102.0, 102.0, 2), (100.0, 100.0));
        // 4 cells: half = 8
        assert_eq!(footprint_origin(100.0, 100.0, 4), (92.0, 92.0));
        // negative coordinates truncate towards zero like the server's `as i32`
        assert_eq!(footprint_origin(1.0, 1.0, 2), (0.0, 0.0));
    }

    #[test]
    fn a_footprint_must_lie_inside_the_polygon() {
        let sq = square(0.0, 100.0);
        assert!(footprint_fits(&sq, 50.0, 50.0, 4));
        // centre inside but the 16 m footprint sticks out of the polygon
        assert!(!footprint_fits(&sq, 1.0, 50.0, 4)); // snapped origin is -4
        assert!(footprint_fits(&sq, 1.0, 50.0, 0)); // no footprint: only the centre counts
        assert!(!footprint_fits(&sq, 101.0, 50.0, 0));
        // an L-shaped area: the inner corner is outside
        let l = vec![
            [0.0, 0.0],
            [100.0, 0.0],
            [100.0, 40.0],
            [40.0, 40.0],
            [40.0, 100.0],
            [0.0, 100.0],
        ];
        assert!(footprint_fits(&l, 20.0, 80.0, 4));
        assert!(!footprint_fits(&l, 70.0, 70.0, 4));
        assert!(!footprint_fits(&l, 38.0, 38.0, 4)); // straddles the notch
    }

    #[test]
    fn samples_fit_are_spaced_and_nearest_first() {
        let sq = square(0.0, 200.0);
        let mut rng = fastrand::Rng::with_seed(3);
        let o = SampleOpts {
            cells: 2,
            gap: 4.0,
            window: 60.0,
            max: 50,
            avoid_r: 0.0,
        };
        let from = [100.0, 100.0];
        let spots = sample_spots(&sq, from, &o, &[], &mut rng);
        assert!(spots.len() > 10, "{}", spots.len());
        for s in &spots {
            assert!(footprint_fits(&sq, s[0], s[1], 2), "{s:?}");
            // inside the sampling window around the anchor
            assert!((s[0] - from[0]).abs() <= 60.0 && (s[1] - from[1]).abs() <= 60.0);
        }
        // no two candidates closer than footprint + gap (on both axes' grid step)
        let step = side(2) + 4.0;
        for (i, a) in spots.iter().enumerate() {
            for b in &spots[i + 1..] {
                let dx = (a[0] - b[0]).abs();
                let dy = (a[1] - b[1]).abs();
                assert!(dx >= step - 0.01 || dy >= step - 0.01, "{a:?} {b:?}");
            }
        }
        // roughly nearest first: the first is closer than the median
        let d = |p: &[f32; 2]| (p[0] - from[0]).hypot(p[1] - from[1]);
        assert!(d(&spots[0]) <= d(&spots[spots.len() / 2]) * 1.5 + 1.0);
    }

    #[test]
    fn avoided_points_and_small_polygons_yield_nothing() {
        let sq = square(0.0, 60.0);
        let mut rng = fastrand::Rng::with_seed(9);
        let o = SampleOpts {
            cells: 2,
            gap: 4.0,
            window: 100.0,
            max: 100,
            avoid_r: 5.0,
        };
        let all = sample_spots(&sq, [30.0, 30.0], &o, &[], &mut rng);
        assert!(!all.is_empty());
        let avoid = [all[0]];
        let mut rng = fastrand::Rng::with_seed(9);
        let fewer = sample_spots(&sq, [30.0, 30.0], &o, &avoid, &mut rng);
        assert!(!fewer.contains(&avoid[0]));
        assert!(fewer
            .iter()
            .all(|s| (s[0] - avoid[0][0]).hypot(s[1] - avoid[0][1]) >= 5.0 + 4.0));
        // a plot smaller than the footprint, and degenerate polygons
        let tiny = square(0.0, 6.0);
        let big = SampleOpts { cells: 4, ..o };
        assert!(sample_spots(&tiny, [3.0, 3.0], &big, &[], &mut rng).is_empty());
        assert!(sample_spots(&[[0.0, 0.0], [1.0, 1.0]], [0.0, 0.0], &o, &[], &mut rng).is_empty());
    }

    #[test]
    fn a_start_outside_the_area_samples_near_the_closest_edge() {
        let sq = square(1000.0, 1200.0);
        let mut rng = fastrand::Rng::with_seed(1);
        let o = SampleOpts {
            cells: 2,
            window: 40.0,
            max: 20,
            ..SampleOpts::default()
        };
        let spots = sample_spots(&sq, [500.0, 1100.0], &o, &[], &mut rng);
        assert!(!spots.is_empty());
        assert!(spots.iter().all(|s| s[0] < 1060.0));
        // nothing is sampled when the window does not reach the polygon at all
        let none = sample_spots(
            &sq,
            [0.0, 0.0],
            &SampleOpts { window: 10.0, ..o },
            &[],
            &mut rng,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn distances_to_polygons() {
        let sq = square(0.0, 10.0);
        assert_eq!(dist_to_polygon(&sq, [5.0, 5.0]), 0.0);
        assert!((dist_to_polygon(&sq, [15.0, 5.0]) - 5.0).abs() < 1e-4);
        assert!((dist_to_polygon(&sq, [13.0, 14.0]) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn areas_are_ranked_by_distance_and_filtered() {
        let polys = vec![
            poly(1, square(0.0, 100.0)),
            poly(2, square(500.0, 700.0)),
            poly(3, square(300.0, 305.0)),         // too small
            poly(4, square(200.0, 300.0)),         // not allowed
            poly(5, vec![[0.0, 0.0], [1.0, 1.0]]), // not a polygon
            poly(6, square(5000.0, 5200.0)),       // too far
        ];
        let allows = |id: u32| id != 4;
        let from = [150.0, 50.0];
        let ranked = rank_areas(&polys, allows, from, 1000.0, 10.0, 8);
        assert_eq!(ranked, vec![1, 2]);
        // limit and distance
        assert_eq!(rank_areas(&polys, allows, from, 1000.0, 10.0, 1), vec![1]);
        assert_eq!(
            rank_areas(&polys, allows, from, f32::INFINITY, 10.0, 8),
            vec![1, 2, 6]
        );
        // standing inside area 2 puts it first
        assert_eq!(
            rank_areas(&polys, allows, [600.0, 600.0], 1000.0, 10.0, 8)[0],
            2
        );
    }
}
