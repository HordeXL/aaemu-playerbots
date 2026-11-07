//! Where to plant: the land of the bot's houses, the public farms and wild patches.
//!
//! The server accepts a seed anywhere on dry ground within 30 m of the character, but only two
//! kinds of place cost no labor and keep the crop safe: the garden of a house of the character
//! (a square of side `2 * garden_radius` around the house, owner only) and a public farm (the
//! sub zones 966, 967, 968, 974 and 998, `farm_areas.json` in the baked data; a field takes ten
//! crops of a character, the other kinds five, and only the owner may harvest for 24 hours).
//!
//! Anywhere else is a wild ("illegal") farm: the seed costs the labor of its use skill (10 points
//! for most seeds), and anybody may harvest the crop; a stranger who does within 24 hours of the
//! planting leaves footprints, a crime the owner can report (the server's `crop_harvest` and
//! `theft_evidence`). Outlaw bots with `[farm] wild` plant a few crops on one small patch
//! ([`WILD_PATCH_M`]) at a secluded spot ([`wild_patch`]): dry, flat, out of the farms, housing
//! areas and war zones, away from NPCs, quest givers, towns, monsters and players.
//!
//! A [`Site`] is one such place; the farm goal lists the ones within reach, nearest first, and
//! [`pick_spot`] draws a free spot on one of them.

use aa_world::{farm_limit, FarmArea, FarmKind};
use smallvec::SmallVec;

use super::{FarmMem, OWN_LAND, WILD_LAND};
use crate::bot::state::{dist2d, HouseBrief};

/// How far from a polygon's edge a spot lies at least (metres).
pub const EDGE_MARGIN_M: f32 = 1.5;
/// The inner and outer ring of a garden a spot is drawn from, as shares of the garden radius: the
/// house stands in the middle and the garden's border is the edge of the owned square.
pub const GARDEN_RING: (f32, f32) = (0.4, 0.9);
/// How many spots [`pick_spot`] draws before it gives up.
pub const TRIES: u32 = 40;
/// The radius of a wild patch (metres): a handful of crops side by side.
pub const WILD_PATCH_M: f32 = 4.0;
/// A wild patch is looked for this far from the bot, at least and at most (metres).
pub const WILD_RING_M: (f32, f32) = (60.0, 600.0);
/// Candidate patches drawn.
pub const WILD_TRIES: u32 = 32;
/// No townsfolk (an NPC friendly to the bot or with a service: a guard, a merchant, a villager)
/// spawns this close to a wild patch: a village, a camp, a road post.
pub const WILD_LONELY_M: f32 = 100.0;
/// No quest giver this close.
pub const WILD_QUEST_M: f32 = 150.0;
/// No hostile aggressive monster this close.
pub const WILD_MONSTER_M: f32 = 50.0;
/// No recall or respawn point (a town square, a crossroads) this close.
pub const WILD_TOWN_M: f32 = 200.0;
/// No real player in view this close.
pub const WILD_PLAYER_M: f32 = 120.0;
/// A patch where crops were stolen keeps the next one this far away.
pub const WILD_BAD_M: f32 = 150.0;
/// The ground of a patch rises or falls at most this much across it (metres).
pub const WILD_FLAT_M: f32 = 1.2;
/// The radius in which the townsfolk are counted to rank the candidates (fewer is more
/// secluded).
pub const WILD_QUIET_M: f32 = 250.0;

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
    /// The place's farm kind, `None` for a garden and a wild patch.
    pub fn kind(&self) -> Option<FarmKind> {
        FarmKind::from_group(u32::from(self.land))
    }

    /// Whether it is a wild patch.
    pub fn is_wild(&self) -> bool {
        self.land == WILD_LAND
    }

    /// A wild patch around `centre`.
    pub fn wild(centre: [f32; 2]) -> Site {
        Site {
            land: WILD_LAND,
            index: 0,
            centre,
            radius: WILD_PATCH_M,
        }
    }

    /// Short text for the log and the web UI.
    pub fn describe(&self) -> String {
        match self.kind() {
            None if self.is_wild() => format!(
                "wild patch at ({:.0}, {:.0})",
                self.centre[0], self.centre[1]
            ),
            None => format!("garden of house {}", self.index),
            Some(k) => format!("{} area {}", k.name(), self.index),
        }
    }
}

/// How many more wild plots `mem` may put down: at most `wild_plots` of them, and `max_plots`
/// plots in all.
pub fn wild_room(mem: Option<&FarmMem>, max_plots: u32, wild_plots: u32) -> u32 {
    let wild = mem.map_or(0, |m| m.count_on(WILD_LAND)) as u32;
    room(mem, WILD_LAND, max_plots).min(wild_plots.saturating_sub(wild))
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
            // a garden keeps the middle for the house; a wild patch has none
            let ring = if site.is_wild() {
                (0.0, 1.0)
            } else {
                GARDEN_RING
            };
            let (lo, hi) = (site.radius * ring.0, site.radius * ring.1);
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

/// Whether (x, y) is inside the place (a garden's owned square, a farm's polygon, a wild
/// patch's circle).
pub fn contains(site: &Site, areas: &[FarmArea], x: f32, y: f32) -> bool {
    match site.kind() {
        None if site.is_wild() => {
            dist2d([x, y, 0.0], [site.centre[0], site.centre[1], 0.0]) <= site.radius + 0.5
        }
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

/// The land type at (x, y) for a crop of the bot found again: the farm area it lies in, else
/// [`OWN_LAND`] in one of the bot's `gardens` or a housing area (`housing`), else
/// [`WILD_LAND`].
pub fn land_of(
    areas: &[FarmArea],
    gardens: &[Site],
    housing: &dyn Fn(f32, f32) -> bool,
    x: f32,
    y: f32,
) -> u8 {
    match land_at(areas, x, y) {
        OWN_LAND if !gardens.iter().any(|g| contains(g, areas, x, y)) && !housing(x, y) => {
            WILD_LAND
        }
        land => land,
    }
}

/// What [`wild_patch`] asks about a candidate spot.
pub trait WildEnv {
    /// The height of dry, walkable ground at (x, y); `None` for water or no ground.
    fn ground(&self, x: f32, y: f32) -> Option<f32>;
    /// The spot is protected or off limits: a public farm, a housing area, a zone at war.
    fn protected(&self, x: f32, y: f32) -> bool;
    /// Townsfolk spawns (NPCs friendly to the bot or with a service) within `radius`, counted up
    /// to `cap`.
    fn npcs(&self, x: f32, y: f32, radius: f32, cap: u32) -> u32;
    /// A quest giver spawns within `radius`.
    fn quest_giver(&self, x: f32, y: f32, radius: f32) -> bool;
    /// A hostile aggressive monster spawns within `radius`.
    fn monster(&self, x: f32, y: f32, radius: f32) -> bool;
    /// A recall or respawn point lies within `radius`.
    fn town(&self, x: f32, y: f32, radius: f32) -> bool;
    /// A real player the bot sees stands within `radius`.
    fn player(&self, x: f32, y: f32, radius: f32) -> bool;
    /// The spot is near a patch where the bot's crops were stolen.
    fn bad(&self, x: f32, y: f32) -> bool;
}

/// Whether a wild patch around (x, y) would do: dry and flat ground across it, not protected,
/// no NPC, quest giver, town, monster or player near, not near a patch that was robbed. The
/// cheap tests come first.
pub fn wild_ok(env: &dyn WildEnv, x: f32, y: f32) -> bool {
    if env.bad(x, y) || env.protected(x, y) {
        return false;
    }
    let r = WILD_PATCH_M;
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for (dx, dy) in [(0.0, 0.0), (r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)] {
        let Some(z) = env.ground(x + dx, y + dy) else {
            return false;
        };
        lo = lo.min(z);
        hi = hi.max(z);
    }
    hi - lo <= WILD_FLAT_M
        && !env.player(x, y, WILD_PLAYER_M)
        && env.npcs(x, y, WILD_LONELY_M, 1) == 0
        && !env.town(x, y, WILD_TOWN_M)
        && !env.monster(x, y, WILD_MONSTER_M)
        && !env.quest_giver(x, y, WILD_QUEST_M)
}

/// A secluded spot for a wild patch within [`WILD_RING_M`] of `from`: of [`WILD_TRIES`] random
/// candidates that pass [`wild_ok`], the one with the fewest townsfolk within [`WILD_QUIET_M`]
/// (the nearer on a tie). `None` when none passes.
pub fn wild_patch(env: &dyn WildEnv, from: [f32; 3], rng: &mut fastrand::Rng) -> Option<Site> {
    let mut best: Option<(u32, f32, [f32; 2])> = None;
    for _ in 0..WILD_TRIES {
        let ang = rng.f32() * std::f32::consts::TAU;
        let d = WILD_RING_M.0 + rng.f32() * (WILD_RING_M.1 - WILD_RING_M.0);
        let (x, y) = (from[0] + ang.cos() * d, from[1] + ang.sin() * d);
        if !wild_ok(env, x, y) {
            continue;
        }
        let quiet = env.npcs(x, y, WILD_QUIET_M, 20);
        if best.is_none_or(|b| (quiet, d) < (b.0, b.1)) {
            best = Some((quiet, d, [x, y]));
        }
    }
    best.map(|b| Site::wild(b.2))
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
        // a crop found again: a farm, a garden, a housing area, else the wild
        let gardens = gardens(&[house(4, 200.0, 10.0)], |_| 8.0);
        let housing = |x: f32, _y: f32| x > 400.0;
        let at = |x: f32| land_of(&areas, &gardens, &housing, x, 10.0);
        assert_eq!(at(10.0), FarmKind::Nursery as u8);
        assert_eq!(at(203.0), OWN_LAND);
        assert_eq!(at(450.0), OWN_LAND);
        assert_eq!(at(100.0), WILD_LAND);
    }

    /// Flat dry land at 110 m; water west of x = 0; a village (NPCs, a quest giver and a recall
    /// point) at (500, 0); a monster camp at (0, 500); a robbed patch at (0, -400).
    struct Land {
        player_at: Option<[f32; 2]>,
    }

    fn near(x: f32, y: f32, at: [f32; 2], r: f32) -> bool {
        (x - at[0]).powi(2) + (y - at[1]).powi(2) <= r * r
    }

    impl WildEnv for Land {
        fn ground(&self, x: f32, _y: f32) -> Option<f32> {
            if x < -300.0 {
                None
            } else if x > 900.0 {
                // a steep slope
                Some(110.0 + (x - 900.0))
            } else {
                Some(110.0)
            }
        }
        fn protected(&self, x: f32, y: f32) -> bool {
            near(x, y, [0.0, 0.0], 50.0)
        }
        fn npcs(&self, x: f32, y: f32, radius: f32, cap: u32) -> u32 {
            if near(x, y, [500.0, 0.0], radius + 30.0) {
                cap.min(9)
            } else {
                0
            }
        }
        fn quest_giver(&self, x: f32, y: f32, radius: f32) -> bool {
            near(x, y, [500.0, 0.0], radius)
        }
        fn monster(&self, x: f32, y: f32, radius: f32) -> bool {
            near(x, y, [0.0, 500.0], radius + 40.0)
        }
        fn town(&self, x: f32, y: f32, radius: f32) -> bool {
            near(x, y, [500.0, 0.0], radius)
        }
        fn player(&self, x: f32, y: f32, radius: f32) -> bool {
            self.player_at.is_some_and(|p| near(x, y, p, radius))
        }
        fn bad(&self, x: f32, y: f32) -> bool {
            near(x, y, [0.0, -400.0], WILD_BAD_M)
        }
    }

    #[test]
    fn a_wild_patch_is_dry_flat_and_out_of_the_way() {
        let land = Land { player_at: None };
        assert!(wild_ok(&land, 200.0, 200.0));
        assert!(!wild_ok(&land, -400.0, 0.0), "water");
        assert!(!wild_ok(&land, 1_000.0, 0.0), "a slope");
        assert!(!wild_ok(&land, 10.0, 10.0), "a farm or housing area");
        assert!(!wild_ok(&land, 420.0, 0.0), "the village");
        assert!(!wild_ok(&land, 0.0, 480.0), "the monsters");
        assert!(!wild_ok(&land, 0.0, -350.0), "the robbed patch");
        let watched = Land {
            player_at: Some([200.0, 200.0]),
        };
        assert!(!wild_ok(&watched, 220.0, 200.0), "a player looks on");
        let mut rng = fastrand::Rng::with_seed(4);
        for _ in 0..30 {
            let from = [100.0, 100.0, 110.0];
            let site = wild_patch(&land, from, &mut rng).expect("a spot");
            assert!(site.is_wild() && site.kind().is_none());
            let [x, y] = site.centre;
            assert!(wild_ok(&land, x, y));
            let d = dist2d(from, [x, y, 0.0]);
            assert!(
                (WILD_RING_M.0 - 1e-3..=WILD_RING_M.1 + 1e-3).contains(&d),
                "{d}"
            );
            assert!(site.describe().starts_with("wild patch"));
            // spots lie in the patch's circle
            let p = pick_spot(&site, &[], &mut rng, &|_, _| true).unwrap();
            assert!(contains(&site, &[], p[0], p[1]));
            assert!(dist2d([p[0], p[1], 0.0], [x, y, 0.0]) <= WILD_PATCH_M + 1e-3);
        }
        // boxed in: water everywhere, no spot
        struct Sea;
        impl WildEnv for Sea {
            fn ground(&self, _x: f32, _y: f32) -> Option<f32> {
                None
            }
            fn protected(&self, _x: f32, _y: f32) -> bool {
                false
            }
            fn npcs(&self, _x: f32, _y: f32, _r: f32, _cap: u32) -> u32 {
                0
            }
            fn quest_giver(&self, _x: f32, _y: f32, _r: f32) -> bool {
                false
            }
            fn monster(&self, _x: f32, _y: f32, _r: f32) -> bool {
                false
            }
            fn town(&self, _x: f32, _y: f32, _r: f32) -> bool {
                false
            }
            fn player(&self, _x: f32, _y: f32, _r: f32) -> bool {
                false
            }
            fn bad(&self, _x: f32, _y: f32) -> bool {
                false
            }
        }
        assert!(wild_patch(&Sea, [0.0; 3], &mut rng).is_none());
    }

    #[test]
    fn wild_room_keeps_to_the_wild_cap_and_the_plot_cap() {
        let mut mem = FarmMem::default();
        assert_eq!(wild_room(None, 5, 3), 3);
        assert_eq!(wild_room(None, 2, 3), 2);
        for i in 0..2 {
            mem.plots.push(crate::bot::farm::Plot {
                obj: i,
                doodad: 1,
                seed: 2,
                pos: [i as f32 * 3.0, 0.0, 0.0],
                phase: 0,
                ripe: false,
                check_ms: 0,
                planted_ms: 0,
                honest: false,
                land: WILD_LAND,
            });
        }
        assert_eq!(wild_room(Some(&mem), 5, 3), 1);
        assert_eq!(wild_room(Some(&mem), 5, 2), 0);
        assert_eq!(
            room(Some(&mem), OWN_LAND, 5),
            3,
            "wild plots count to max_plots"
        );
    }
}
