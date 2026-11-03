//! Launch and landing spots: a standing point on a beach and open water 12 to 28 m in front of it.
//!
//! [`find_shore`] looks for the sea cell next to the land that is nearest to a point, ring by ring,
//! and turns it into a [`ShoreSpot`]: it probes sixteen directions for the nearest walkable land,
//! stands a few metres inland of the waterline and checks the way into the water (a beach, then
//! water all the way out). The same spot serves a launch (stand there, face the water, summon) and
//! a landing (sail to the water point, swim to the stand point).

use super::route::Disc;
use super::{yaw_of, WaterMap, CELL_M};

/// Terrain at least this high can be walked on (the bots' `SWIM_THRESHOLD` plus a metre, and a
/// margin for the beach).
pub const WALK_Z: f32 = 100.5;
/// A standing point is lower than this: a steep coast is no place to walk down from.
pub const MAX_STAND_Z: f32 = 112.0;
/// Terrain below this is water for the way out to the water point.
const WATER_Z: f32 = 99.0;
/// The stand point is this far inland of the first walkable terrain met from the sea.
const INLAND_M: f32 = 3.0;
/// The water point is this far (at least, at most) from the stand point.
const WATER_DIST_M: (f32, f32) = (12.0, 28.0);
/// At least this much water between the waterline and the water point.
const MIN_WATER_RUN_M: f32 = 6.0;
/// Directions probed per coastal cell.
const DIRECTIONS: u32 = 16;
/// Distance of the probes from the cell centre and their step.
const PROBE_MAX_M: f32 = 64.0;
const PROBE_STEP_M: f32 = 4.0;

/// A place to launch from or to land at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShoreSpot {
    /// Where the bot stands on land.
    pub stand: (f32, f32),
    /// Open water 12 to 28 m out, on the line from the stand point to the sea.
    pub water: (f32, f32),
    /// The yaw (bot convention) that faces the water from the stand point.
    pub yaw: f32,
}

/// The spot nearest to `from` within `max_r` metres whose standing point and water point are
/// outside every disc of `avoid`; `ground` gives the terrain height. `None` when the coast near
/// `from` has none (no sea, no beach, cliffs, or a cell outside the map).
pub fn find_shore(
    map: &WaterMap,
    ground: &dyn Fn(f32, f32) -> Option<f32>,
    from: (f32, f32),
    max_r: f32,
    avoid: &[Disc],
) -> Option<ShoreSpot> {
    let (c0, r0) = map.cell_of(from.0, from.1)?;
    let kmax = (max_r / CELL_M).ceil() as i32;
    let mut best: Option<(f32, ShoreSpot)> = None;
    let mut found_at: Option<i32> = None;
    for k in 0..=kmax {
        if found_at.is_some_and(|f| k > f + 1) {
            break;
        }
        for dr in -k..=k {
            for dc in -k..=k {
                if dr.abs().max(dc.abs()) != k {
                    continue;
                }
                let (c, r) = (c0 + dc, r0 + dr);
                if !map.sea_cell(c, r) || !coastal(map, c, r) {
                    continue;
                }
                let Some(spot) = spot_at(map, ground, map.center_of(c, r), avoid) else {
                    continue;
                };
                let d = ((spot.stand.0 - from.0).powi(2) + (spot.stand.1 - from.1).powi(2)).sqrt();
                if d <= max_r && best.is_none_or(|b| d < b.0) {
                    best = Some((d, spot));
                    found_at.get_or_insert(k);
                }
            }
        }
    }
    best.map(|b| b.1)
}

/// A sea cell with a land (non-sea) cell on one of its four sides.
fn coastal(map: &WaterMap, c: i32, r: i32) -> bool {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .iter()
        .any(|&(dc, dr)| !map.sea_cell(c + dc, r + dr))
}

/// The spot next to the sea cell whose centre is `center`.
fn spot_at(
    map: &WaterMap,
    ground: &dyn Fn(f32, f32) -> Option<f32>,
    center: (f32, f32),
    avoid: &[Disc],
) -> Option<ShoreSpot> {
    // the direction in which walkable land is nearest
    let mut nearest: Option<(f32, (f32, f32))> = None;
    for i in 0..DIRECTIONS {
        let a = i as f32 * std::f32::consts::TAU / DIRECTIONS as f32;
        let dir = (a.cos(), a.sin());
        let land = |d: f32| {
            ground(center.0 + dir.0 * d, center.1 + dir.1 * d).is_some_and(|z| z >= WALK_Z)
        };
        let mut d = PROBE_STEP_M;
        while d <= PROBE_MAX_M {
            if land(d) {
                // the waterline between the last water sample and this one, to a few centimetres,
                // so that the direction that meets the land first is the one square to the coast
                let (mut lo, mut hi) = (d - PROBE_STEP_M, d);
                for _ in 0..7 {
                    let mid = (lo + hi) * 0.5;
                    if land(mid) {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                if nearest.is_none_or(|n| hi < n.0) {
                    nearest = Some((hi, dir));
                }
                break;
            }
            d += PROBE_STEP_M;
        }
    }
    let (d_land, dir) = nearest?;
    let stand = (
        center.0 + dir.0 * (d_land + INLAND_M),
        center.1 + dir.1 * (d_land + INLAND_M),
    );
    let sz = ground(stand.0, stand.1)?;
    let inland = (stand.0 + dir.0 * INLAND_M, stand.1 + dir.1 * INLAND_M);
    if !(WALK_Z..=MAX_STAND_Z).contains(&sz) || ground(inland.0, inland.1)? < WALK_Z {
        return None;
    }
    // out to sea: the way from the stand point to the water point
    let out = (-dir.0, -dir.1);
    let run = (d_land + INLAND_M).clamp(WATER_DIST_M.0, WATER_DIST_M.1);
    let water = (stand.0 + out.0 * run, stand.1 + out.1 * run);
    if !map.sea_at(water.0, water.1) || ground(water.0, water.1)? >= WATER_Z {
        return None;
    }
    let mut first_water: Option<f32> = None;
    let mut d = 0.0;
    while d <= run {
        let p = (stand.0 + out.0 * d, stand.1 + out.1 * d);
        let z = ground(p.0, p.1)?;
        if z < WATER_Z {
            first_water.get_or_insert(d);
        } else if first_water.is_some() {
            return None; // land again after the water
        }
        d += 2.0;
    }
    if run - first_water? < MIN_WATER_RUN_M {
        return None;
    }
    if avoid
        .iter()
        .any(|a| a.contains(stand.0, stand.1) || a.contains(water.0, water.1))
    {
        return None;
    }
    Some(ShoreSpot {
        stand,
        water,
        yaw: yaw_of(out.0, out.1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Land (110 m) west of x = 100, sea (50 m) east of it; the matching 16 m cell mask.
    fn straight_coast() -> (WaterMap, impl Fn(f32, f32) -> Option<f32>) {
        // cells with x >= 112 are sea (cell 7 spans 112..128; cell 6 touches the land at 100)
        let map = WaterMap::from_cells((0.0, 0.0), 100, 100, |c, _| c >= 7);
        (map, |x: f32, _y: f32| {
            Some(if x < 100.0 { 110.0 } else { 50.0 })
        })
    }

    #[test]
    fn a_straight_beach_gives_a_stand_point_and_water() {
        let (map, ground) = straight_coast();
        let s = find_shore(&map, &ground, (60.0, 400.0), 200.0, &[]).unwrap();
        // the stand point is inland of the waterline at x = 100, the water point out to sea
        assert!(s.stand.0 < 100.0 && s.stand.0 > 80.0, "{s:?}");
        assert!(s.water.0 > 110.0, "{s:?}");
        let run = ((s.water.0 - s.stand.0).powi(2) + (s.water.1 - s.stand.1).powi(2)).sqrt();
        assert!((12.0..=28.0).contains(&run), "{run}");
        assert!(ground(s.stand.0, s.stand.1).unwrap() >= WALK_Z);
        assert!(map.sea_at(s.water.0, s.water.1));
        // facing east: yaw -90 degrees in the bot convention
        assert!(
            (s.yaw + std::f32::consts::FRAC_PI_2).abs() < 0.3,
            "{}",
            s.yaw
        );
    }

    #[test]
    fn the_nearest_spot_wins() {
        let (map, ground) = straight_coast();
        let near = find_shore(&map, &ground, (95.0, 400.0), 300.0, &[]).unwrap();
        let far = find_shore(&map, &ground, (5.0, 400.0), 300.0, &[]).unwrap();
        assert!((near.stand.1 - 400.0).abs() < 60.0, "{near:?}");
        assert!(far.stand.0 > 70.0, "{far:?}");
    }

    #[test]
    fn nothing_in_reach_is_none() {
        let (map, ground) = straight_coast();
        assert!(find_shore(&map, &ground, (5.0, 400.0), 40.0, &[]).is_none());
        // outside the map
        assert!(find_shore(&map, &ground, (-50.0, 400.0), 400.0, &[]).is_none());
    }

    #[test]
    fn discs_to_avoid_exclude_spots() {
        let (map, ground) = straight_coast();
        let first = find_shore(&map, &ground, (60.0, 400.0), 300.0, &[]).unwrap();
        let disc = Disc {
            x: first.stand.0,
            y: first.stand.1,
            r: 60.0,
        };
        let second = find_shore(&map, &ground, (60.0, 400.0), 300.0, &[disc]).unwrap();
        assert!(!disc.contains(second.stand.0, second.stand.1));
        assert!(!disc.contains(second.water.0, second.water.1));
    }

    #[test]
    fn a_cliff_is_no_beach() {
        // the land is 140 m high: too steep to walk down from
        let map = WaterMap::from_cells((0.0, 0.0), 100, 100, |c, _| c >= 7);
        let ground = |x: f32, _y: f32| Some(if x < 100.0 { 140.0 } else { 50.0 });
        assert!(find_shore(&map, &ground, (60.0, 400.0), 300.0, &[]).is_none());
    }

    #[test]
    fn land_in_the_water_way_is_refused() {
        // a sandbar 8 m out: the water point behind it is not reachable by a straight swim
        let map = WaterMap::from_cells((0.0, 0.0), 100, 100, |c, _| c >= 7);
        let ground = |x: f32, _y: f32| {
            Some(if x < 100.0 || (108.0..112.0).contains(&x) {
                110.0
            } else {
                50.0
            })
        };
        let s = find_shore(&map, &ground, (60.0, 400.0), 300.0, &[]);
        assert!(s.is_none(), "{s:?}");
    }
}
