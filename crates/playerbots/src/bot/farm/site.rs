//! Where to plant: the land of the bot's own houses and the public farms.
//!
//! The server accepts a seed anywhere on dry ground within 30 m of the character, but only two
//! kinds of place cost no labor and keep the crop safe: the garden of a house of the character
//! (a square of side `2 * garden_radius` around the house, owner only) and a public farm (the
//! sub zones 966, 967, 968, 974 and 998, `farm_areas.json` in the baked data; a field takes ten
//! crops of a character, the other kinds five, and only the owner may harvest for 24 hours). The
//! bots plant on those two and nowhere else.
//!
//! A [`Site`] is one such place; [`sites`] lists the ones within reach, nearest first, and
//! [`pick_spot`] draws a free spot on one of them.

use aa_world::{farm_limit, FarmArea, FarmKind};
use smallvec::SmallVec;

use super::{FarmMem, OWN_LAND};
use crate::bot::state::{dist2d, HouseBrief};

/// How far from a polygon's edge a spot lies at least (metres).
pub const EDGE_MARGIN_M: f32 = 1.5;
/// The inner and outer ring of a garden a spot is drawn from, as shares of the garden radius: the
/// house stands in the middle and the garden's border is the edge of the owned square.
pub const GARDEN_RING: (f32, f32) = (0.4, 0.9);
/// How many spots [`pick_spot`] draws before it gives up.
pub const TRIES: u32 = 40;

/// A place to plant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Site {
    /// [`OWN_LAND`], or the [`FarmKind`] of the public farm as `u8`.
    pub land: u8,
    /// The house id of a garden, the index into the farm areas of a farm.
    pub index: u32,
    /// The middle of the place.
    pub centre: [f32; 2],
    /// The garden radius of a garden, 0 for a farm.
    pub radius: f32,
}

impl Site {
    /// The place's farm kind, `None` for a garden.
    pub fn kind(&self) -> Option<FarmKind> {
        FarmKind::from_group(u32::from(self.land))
    }

    /// Short text for the log and the web UI.
    pub fn describe(&self) -> String {
        match self.kind() {
            None => format!("garden of house {}", self.index),
            Some(k) => format!("{} area {}", k.name(), self.index),
        }
    }
}

/// How many more plots `mem` may put on `land` with at most `max_plots` plots in all.
pub fn room(mem: Option<&FarmMem>, land: u8, max_plots: u32) -> u32 {
    let (all, here) = mem.map_or((0, 0), |m| (m.plots.len(), m.count_on(land)));
    let all_left = (max_plots as usize).saturating_sub(all);
    let here_left = match FarmKind::from_group(u32::from(land)) {
        None => usize::MAX,
        Some(k) => (farm_limit(k) as usize).saturating_sub(here),
    };
    all_left.min(here_left).min(u32::MAX as usize) as u32
}

/// The gardens of the bot's houses (`garden_radius_of` gives the radius of a house design).
pub fn gardens(
    houses: &[HouseBrief],
    garden_radius_of: impl Fn(u32) -> f32,
) -> SmallVec<[Site; 2]> {
    houses
        .iter()
        .filter(|h| h.id != 0)
        .map(|h| Site {
            land: OWN_LAND,
            index: h.id,
            centre: [h.pos[0], h.pos[1]],
            radius: garden_radius_of(h.template),
        })
        .collect()
}

/// The public farms whose kind `kinds` (a [`FarmKind::bit`] mask, as in
/// [`Crop::kinds`](aa_world::Crop::kinds)) allows.
pub fn farms(areas: &[FarmArea], kinds: u8) -> SmallVec<[Site; 4]> {
    areas
        .iter()
        .enumerate()
        .filter(|(_, a)| kinds & a.kind.bit() != 0)
        .map(|(i, a)| Site {
            land: a.kind as u8,
            index: i as u32,
            centre: a.centre,
            radius: 0.0,
        })
        .collect()
}

/// Sorts `sites` by distance from `from` and drops the ones farther than `max_m`.
pub fn nearest(mut sites: Vec<Site>, from: [f32; 3], max_m: f32) -> Vec<Site> {
    let d = |s: &Site| dist2d(from, [s.centre[0], s.centre[1], 0.0]);
    sites.retain(|s| d(s) <= max_m);
    sites.sort_by(|a, b| d(a).total_cmp(&d(b)));
    sites
}

/// How much room a crop of `radius_mm` keeps around itself (the server refuses a seed closer to
/// another doodad than the sum of both radii; a margin keeps the crops from touching).
pub fn spacing_m(radius_mm: u16) -> f32 {
    2.0 * (f32::from(radius_mm) / 1000.0).max(0.4) + 0.8
}

/// A free spot on `site`: a random point of the place that `fits` accepts, tried [`TRIES`] times.
/// `areas` are the world's farm areas (for a farm site). `fits(x, y)` says whether the point is
/// walkable and not near another plot, a refused spot or another bot's claim.
pub fn pick_spot(
    site: &Site,
    areas: &[FarmArea],
    rng: &mut fastrand::Rng,
    fits: &dyn Fn(f32, f32) -> bool,
) -> Option<[f32; 2]> {
    match site.kind() {
        None => {
            let (lo, hi) = (site.radius * GARDEN_RING.0, site.radius * GARDEN_RING.1);
            for _ in 0..TRIES {
                let ang = rng.f32() * std::f32::consts::TAU;
                let r = lo + rng.f32() * (hi - lo).max(0.0);
                let (x, y) = (
                    site.centre[0] + ang.cos() * r,
                    site.centre[1] + ang.sin() * r,
                );
                if fits(x, y) {
                    return Some([x, y]);
                }
            }
            None
        }
        Some(_) => {
            let a = areas.get(site.index as usize)?;
            let b = a.bounds;
            for _ in 0..TRIES * 2 {
                let x = b[0] + rng.f32() * (b[2] - b[0]);
                let y = b[1] + rng.f32() * (b[3] - b[1]);
                if a.depth(x, y) >= EDGE_MARGIN_M && fits(x, y) {
                    return Some([x, y]);
                }
            }
            None
        }
    }
}

/// Whether (x, y) is inside the place (a garden's owned square, a farm's polygon).
pub fn contains(site: &Site, areas: &[FarmArea], x: f32, y: f32) -> bool {
    match site.kind() {
        None => {
            (x - site.centre[0]).abs() < site.radius && (y - site.centre[1]).abs() < site.radius
        }
        Some(_) => areas
            .get(site.index as usize)
            .is_some_and(|a| a.contains(x, y)),
    }
}

/// The land type at (x, y) for a plot found again: a farm area it lies in, else
/// [`OWN_LAND`].
pub fn land_at(areas: &[FarmArea], x: f32, y: f32) -> u8 {
    areas
        .iter()
        .find(|a| a.contains(x, y))
        .map_or(OWN_LAND, |a| a.kind as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(kind: FarmKind, x: f32, y: f32, side: f32) -> FarmArea {
        let pts: Box<[[f32; 2]]> =
            vec![[x, y], [x + side, y], [x + side, y + side], [x, y + side]].into();
        FarmArea {
            kind,
            sub_zone: 966,
            zone_key: 1,
            bounds: [x, y, x + side, y + side],
            centre: [x + side / 2.0, y + side / 2.0],
            points: pts,
        }
    }

    fn house(id: u32, x: f32, y: f32) -> HouseBrief {
        HouseBrief {
            tl: 1,
            id,
            template: 7,
            pos: [x, y, 0.0],
            build_left: 0,
        }
    }

    #[test]
    fn gardens_follow_the_houses_and_their_designs() {
        let g = gardens(&[house(4, 100.0, 200.0), house(0, 1.0, 1.0)], |t| {
            assert_eq!(t, 7);
            4.0
        });
        assert_eq!(g.len(), 1, "a house without an id is not a house yet");
        assert_eq!(g[0].land, OWN_LAND);
        assert_eq!(
            (g[0].index, g[0].centre, g[0].radius),
            (4, [100.0, 200.0], 4.0)
        );
        assert!(g[0].kind().is_none());
    }

    #[test]
    fn farms_are_the_areas_a_crop_allows() {
        let areas = [
            square(FarmKind::Farm, 0.0, 0.0, 50.0),
            square(FarmKind::Nursery, 100.0, 0.0, 50.0),
            square(FarmKind::Farm, 200.0, 0.0, 50.0),
        ];
        let field_only = farms(&areas, FarmKind::Farm.bit());
        assert_eq!(
            field_only.iter().map(|s| s.index).collect::<Vec<_>>(),
            [0, 2]
        );
        let both = farms(&areas, FarmKind::Farm.bit() | FarmKind::Nursery.bit());
        assert_eq!(both.len(), 3);
        assert_eq!(both[1].kind(), Some(FarmKind::Nursery));
        assert!(farms(&areas, 0).is_empty());
    }

    #[test]
    fn sites_are_sorted_by_distance_and_cut_at_the_trip_limit() {
        let mk = |index, x| Site {
            land: OWN_LAND,
            index,
            centre: [x, 0.0],
            radius: 4.0,
        };
        let out = nearest(
            vec![mk(1, 900.0), mk(2, 100.0), mk(3, 5000.0), mk(4, 400.0)],
            [0.0; 3],
            1000.0,
        );
        assert_eq!(out.iter().map(|s| s.index).collect::<Vec<_>>(), [2, 4, 1]);
    }

    #[test]
    fn room_counts_the_bots_limit_and_the_kinds_limit() {
        let mut mem = FarmMem::default();
        assert_eq!(room(None, OWN_LAND, 5), 5);
        assert_eq!(
            room(Some(&mem), FarmKind::Farm as u8, 50),
            10,
            "a field takes ten"
        );
        assert_eq!(room(Some(&mem), FarmKind::Nursery as u8, 50), 5);
        for i in 0..3 {
            mem.plots.push(crate::bot::farm::Plot {
                obj: i,
                doodad: 1,
                seed: 2,
                pos: [0.0; 3],
                phase: 0,
                ripe: false,
                check_ms: 0,
                planted_ms: 0,
                honest: false,
                land: FarmKind::Nursery as u8,
            });
        }
        assert_eq!(room(Some(&mem), FarmKind::Nursery as u8, 50), 2);
        assert_eq!(
            room(Some(&mem), OWN_LAND, 5),
            2,
            "three of five plots are used"
        );
        assert_eq!(room(Some(&mem), OWN_LAND, 2), 0);
    }

    #[test]
    fn spacing_follows_the_crop_radius() {
        assert!(
            (spacing_m(0) - 1.6).abs() < 1e-4,
            "a tiny crop keeps the minimum"
        );
        assert!((spacing_m(1500) - 3.8).abs() < 1e-4);
    }

    #[test]
    fn garden_spots_lie_in_the_ring_and_farm_spots_inside_the_margin() {
        let mut rng = fastrand::Rng::with_seed(9);
        let garden = Site {
            land: OWN_LAND,
            index: 1,
            centre: [100.0, 100.0],
            radius: 8.0,
        };
        for _ in 0..200 {
            let p = pick_spot(&garden, &[], &mut rng, &|_, _| true).unwrap();
            let r = dist2d([p[0], p[1], 0.0], [100.0, 100.0, 0.0]);
            assert!((3.2..=7.2 + 1e-3).contains(&r), "{r}");
            assert!(contains(&garden, &[], p[0], p[1]));
        }
        let areas = [square(FarmKind::Farm, 0.0, 0.0, 30.0)];
        let farm = farms(&areas, FarmKind::Farm.bit())[0];
        for _ in 0..200 {
            let p = pick_spot(&farm, &areas, &mut rng, &|_, _| true).unwrap();
            assert!(areas[0].depth(p[0], p[1]) >= EDGE_MARGIN_M);
            assert!(contains(&farm, &areas, p[0], p[1]));
        }
        // nothing fits: no spot, after a bounded number of tries
        let calls = std::cell::Cell::new(0);
        assert!(pick_spot(&garden, &[], &mut rng, &|_, _| {
            calls.set(calls.get() + 1);
            false
        })
        .is_none());
        assert_eq!(calls.get(), TRIES);
        // a farm index that is not in the list has no spot
        let ghost = Site { index: 9, ..farm };
        assert!(pick_spot(&ghost, &areas, &mut rng, &|_, _| true).is_none());
    }

    #[test]
    fn land_is_found_from_the_position() {
        let areas = [square(FarmKind::Nursery, 0.0, 0.0, 30.0)];
        assert_eq!(land_at(&areas, 10.0, 10.0), FarmKind::Nursery as u8);
        assert_eq!(land_at(&areas, 100.0, 10.0), OWN_LAND);
    }
}
