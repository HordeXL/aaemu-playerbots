//! Where an ambush waits: a pure planner over the trade routes.
//!
//! A carrier walks from a specialty workbench to the NPC that buys its pack, so an ambush belongs
//! on such a route. The planner takes every workbench spawn near the bot, every pack it offers and
//! the best buyer of each (the trade run's own [`best_buyer`]; a carrier may take any of them),
//! walks along the straight line between workbench and buyer and keeps the points where a hostile
//! carrier can be attacked at all:
//!
//! * the workbench stands in the victims' own zones or in no one's (that is where the carriers
//!   come from), and the point is not in a zone the victims' alliance owns (the server protects a
//!   character there, see [`rules`](super::rules));
//! * the point is on land the bot can stand on.
//!
//! Points in a conflict zone (a war, if it is on, counts for more) and the first point outside
//! the victims' zones (where every carrier of the route has to pass) score best, then the price
//! of the pack, less the way for the bot. The bot draws among the best few.

use aa_world::{Vec3, ZoneInfo};
use smallvec::SmallVec;

use crate::bot::goals::trade_run::plan::{best_buyer, RouteData};

/// The spacing of the points tried along a route (metres).
const STEP_M: f32 = 100.0;
/// A route is not tried this close to either end (metres): a post at a workbench or at a buyer
/// is crowded by the town's guards and by everyone else.
const END_MARGIN_M: f32 = 150.0;
/// Copper of pack price that make up for one point of score.
const COPPER_PER_POINT: f32 = 4_000.0;
/// Points a conflict zone adds ...
const CONFLICT_BONUS: f32 = 6.0;
/// ... and a zone that is at war right now.
const WAR_BONUS: f32 = 6.0;
/// Points for the first point outside the victims' own zones.
const EXIT_BONUS: f32 = 4.0;
/// Points lost per kilometre of way for the bot.
const KM_PENALTY: f32 = 1.5;
/// Posts closer together than this are one post (metres).
const MERGE_M: f32 = 150.0;
/// Most posts kept.
const MAX_POSTS: usize = 16;
/// The bot draws among this many of the best posts.
const PICK_AMONG: usize = 6;

/// An ambush post.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Post {
    /// Where the bot waits.
    pub pos: [f32; 2],
    /// A unit vector along the route towards the buyer (the bot patrols along it).
    pub dir: [f32; 2],
    /// The conflict zone group the post is in; 0 outside the conflict zones.
    pub group: u16,
    /// What the best pack of the route pays, in copper.
    pub price: u32,
    /// The score (higher is better).
    pub score: f32,
}

/// A workbench, the buyer of its best pack and what the pack pays.
type Route = ([f32; 2], [f32; 2], u32);

/// What the planner needs to know about the land.
pub trait PostGeo {
    /// The zone at (x, y); `None` outside every zone and without the baked zone grid.
    fn zone(&self, x: f32, y: f32) -> Option<ZoneInfo>;
    /// Whether the bot can stand at (x, y).
    fn walkable(&self, x: f32, y: f32) -> bool;
    /// Whether the zone group takes part in the zone conflicts.
    fn is_conflict(&self, group: u16) -> bool;
}

/// What a planning request wants.
pub struct Query<'a> {
    /// Where the bot is.
    pub from: [f32; 2],
    /// Workbenches within this many metres of `from` are looked at.
    pub radius_m: f32,
    /// Posts farther from `from` than this are dropped (`f32::INFINITY` when a GM `/move` makes
    /// distances irrelevant).
    pub max_trip_m: f32,
    /// The alliance (mother faction) of the carriers to ambush.
    pub victims: u32,
    /// Whether a zone group is at war right now.
    pub at_war: &'a dyn Fn(u16) -> bool,
    /// Whether a post close to (x, y) may be used (not one the bot gave up lately).
    pub post_ok: &'a dyn Fn([f32; 2]) -> bool,
}

fn d2(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Every acceptable post for `q`, best first (at most [`MAX_POSTS`]).
pub fn candidates<D: RouteData + ?Sized, G: PostGeo + ?Sized>(
    data: &D,
    geo: &G,
    q: &Query<'_>,
) -> Vec<Post> {
    let mut routes: SmallVec<[Route; 16]> = SmallVec::new();
    let center = Vec3::new(q.from[0], q.from[1], 0.0);
    data.benches_within(center, q.radius_m, &mut |tpl, pos| {
        let bench = [pos.x, pos.y];
        // the carriers come from the victims' own land or from no one's
        let owner = geo.zone(bench[0], bench[1]).map_or(0, |z| z.faction);
        if owner != 0 && owner != q.victims {
            return;
        }
        // every pack the bench offers goes to its own buyer: a carrier may take any of them
        let mut buyers: SmallVec<[([f32; 2], u32); 4]> = SmallVec::new();
        for &craft in data.pack_crafts_at(tpl) {
            let Some(c) = data.craft(craft) else { continue };
            if !c.is_pack || c.product.0 == 0 {
                continue;
            }
            let Some((_, buyer, price)) = best_buyer(data, c.product.0, bench, &|_| true) else {
                continue;
            };
            match buyers.iter_mut().find(|b| d2(b.0, buyer) < 1.0) {
                Some(b) => b.1 = b.1.max(price),
                None => buyers.push((buyer, price)),
            }
        }
        for (buyer, price) in buyers {
            if d2(bench, buyer) > 2.0 * END_MARGIN_M {
                routes.push((bench, buyer, price));
            }
        }
    });

    let mut out: Vec<Post> = Vec::new();
    for (bench, buyer, price) in routes {
        let len = d2(bench, buyer);
        let dir = [(buyer[0] - bench[0]) / len, (buyer[1] - bench[1]) / len];
        let mut left_home = false;
        let mut at = END_MARGIN_M;
        while at <= len - END_MARGIN_M {
            let p = [bench[0] + dir[0] * at, bench[1] + dir[1] * at];
            at += STEP_M;
            let zone = geo.zone(p[0], p[1]);
            let owner = zone.map_or(0, |z| z.faction);
            if owner == q.victims {
                continue; // the victims' land: protected there
            }
            // (the first point past the victims' land is where every carrier of the route comes by)
            let exit = !left_home;
            left_home = true;
            if !geo.walkable(p[0], p[1]) || !(q.post_ok)(p) {
                continue;
            }
            let trip = d2(q.from, p);
            if trip > q.max_trip_m {
                continue;
            }
            let group = zone
                .map(|z| z.group)
                .filter(|&g| geo.is_conflict(g))
                .unwrap_or(0);
            let mut score = price as f32 / COPPER_PER_POINT - KM_PENALTY * trip / 1000.0;
            if group != 0 {
                score += CONFLICT_BONUS;
                if (q.at_war)(group) {
                    score += WAR_BONUS;
                }
            }
            if exit {
                score += EXIT_BONUS;
            }
            out.push(Post {
                pos: p,
                dir,
                group,
                price,
                score,
            });
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    // one post per neighbourhood
    let mut kept: Vec<Post> = Vec::new();
    for p in out {
        if kept.iter().all(|k| d2(k.pos, p.pos) >= MERGE_M) {
            kept.push(p);
            if kept.len() == MAX_POSTS {
                break;
            }
        }
    }
    kept
}

/// Draws one of the best posts (`cands` sorted best first).
pub fn pick(cands: &[Post], rng: &mut fastrand::Rng) -> Option<Post> {
    let n = cands.len().min(PICK_AMONG);
    if n == 0 {
        None
    } else {
        Some(cands[rng.usize(..n)])
    }
}

impl PostGeo for aa_world::World {
    fn zone(&self, x: f32, y: f32) -> Option<ZoneInfo> {
        self.zone_info_at(x, y)
    }

    fn walkable(&self, x: f32, y: f32) -> bool {
        crate::bot::goals::walkable(self, x, y)
    }

    fn is_conflict(&self, group: u16) -> bool {
        self.is_conflict_group(group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::goals::trade_run::plan::tests::Fake;

    /// x < 500: Nuian land (148); 500..4000: no one's, group 20 is a conflict zone from 1800 on;
    /// x >= 6000: Haranyan land (149). Everything is walkable except x in 1200..1300.
    struct Land;

    impl PostGeo for Land {
        fn zone(&self, x: f32, _y: f32) -> Option<ZoneInfo> {
            Some(if x < 500.0 {
                ZoneInfo {
                    group: 1,
                    faction: 148,
                }
            } else if x >= 6000.0 {
                ZoneInfo {
                    group: 2,
                    faction: 149,
                }
            } else if x >= 1800.0 {
                ZoneInfo {
                    group: 20,
                    faction: 0,
                }
            } else {
                ZoneInfo {
                    group: 9,
                    faction: 0,
                }
            })
        }

        fn walkable(&self, x: f32, _y: f32) -> bool {
            !(1200.0..1300.0).contains(&x)
        }

        fn is_conflict(&self, group: u16) -> bool {
            group == 20
        }
    }

    /// Bench 100 at (1000, 0) is in no one's land, bench 200 at (0, 3000) in Nuian land; the
    /// buyers are at (1500, 0), (0, 9000) and (0, 3200).
    fn query<'a>(victims: u32, at_war: &'a dyn Fn(u16) -> bool) -> Query<'a> {
        Query {
            from: [0.0, 0.0],
            radius_m: 10_000.0,
            max_trip_m: f32::INFINITY,
            victims,
            at_war,
            post_ok: &|_| true,
        }
    }

    #[test]
    fn posts_lie_on_routes_outside_the_victims_land() {
        let mut data = Fake::sample();
        data.prices.insert((11, 500), 80_000);
        data.prices.insert((12, 500), 40_000);
        let none = |_: u16| false;
        // Haranyan victims: the Nuian bench 200 is not theirs; bench 100 lies in no one's land
        let posts = candidates(&data, &Land, &query(149, &none));
        assert!(!posts.is_empty());
        for p in &posts {
            let on_route_100 = p.pos[1].abs() < 1.0 && (1150.0..=1350.0).contains(&p.pos[0]);
            assert!(
                p.pos[0] < 6000.0 && !(1200.0..1300.0).contains(&p.pos[0]),
                "not in the victims' land or on water: {p:?}"
            );
            assert!(on_route_100 || p.pos[0] <= 1500.0, "{p:?}");
        }
        // Nuian victims: nothing of theirs may stand in Nuian land
        let posts = candidates(&data, &Land, &query(148, &none));
        assert!(posts.iter().all(|p| p.pos[0] >= 500.0), "{posts:?}");
    }

    #[test]
    fn every_pack_of_a_bench_has_its_own_route() {
        let mut data = Fake::sample();
        // bench 100 offers pack 11 (to the buyer at (1500, 0)) and pack 12 (to another one)
        data.buyers.insert(12, vec![501]);
        data.prices.insert((11, 500), 80_000);
        data.prices.insert((12, 501), 40_000);
        let none = |_: u16| false;
        let posts = candidates(&data, &Land, &query(149, &none));
        let east = posts.iter().any(|p| p.dir[0] > 0.9 && p.price == 80_000);
        let north = posts.iter().any(|p| p.dir[1] > 0.5 && p.price == 40_000);
        assert!(east && north, "{posts:?}");
    }

    #[test]
    fn a_post_in_a_conflict_zone_at_war_beats_one_outside() {
        let mut data = Fake::sample();
        // a long route through the conflict zone: bench 100 -> a buyer at (4000, 0)
        data.spawns.insert(500, vec![Vec3::new(4000.0, 0.0, 0.0)]);
        data.prices.insert((11, 500), 50_000);
        data.prices.insert((12, 500), 10_000);
        let calm = |_: u16| false;
        let war = |g: u16| g == 20;
        let calm_best = candidates(&data, &Land, &query(149, &calm))[0];
        let war_best = candidates(&data, &Land, &query(149, &war))[0];
        assert_eq!(calm_best.group, 20, "the conflict zone is best anyway");
        assert!(war_best.score > calm_best.score);
        assert_eq!(war_best.price, 50_000, "the best-paid pack of the bench");
        assert!(war_best.pos[0] >= 1800.0);
        // the direction points at the buyer
        assert!((war_best.dir[0] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_way_and_the_blocked_list_count() {
        let mut data = Fake::sample();
        data.spawns.insert(500, vec![Vec3::new(4000.0, 0.0, 0.0)]);
        data.prices.insert((11, 500), 50_000);
        let none = |_: u16| false;
        // a bot far from the bench with a short leash finds nothing
        let mut q = query(149, &none);
        q.from = [0.0, 9_000.0];
        q.max_trip_m = 1_000.0;
        assert!(candidates(&data, &Land, &q).is_empty());
        // posts the bot gave up on are not offered again
        let blocked = |p: [f32; 2]| !(1800.0..2600.0).contains(&p[0]);
        let mut q = query(149, &none);
        q.post_ok = &blocked;
        let posts = candidates(&data, &Land, &q);
        assert!(!posts.is_empty());
        assert!(posts.iter().all(|p| !(1800.0..2600.0).contains(&p.pos[0])));
    }

    #[test]
    fn posts_are_spread_and_sorted() {
        let mut data = Fake::sample();
        data.spawns.insert(500, vec![Vec3::new(4000.0, 0.0, 0.0)]);
        data.prices.insert((11, 500), 50_000);
        let none = |_: u16| false;
        let posts = candidates(&data, &Land, &query(149, &none));
        assert!(posts.len() <= MAX_POSTS);
        for w in posts.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
        for (i, a) in posts.iter().enumerate() {
            for b in &posts[i + 1..] {
                assert!(d2(a.pos, b.pos) >= MERGE_M);
            }
        }
        let mut rng = fastrand::Rng::with_seed(3);
        for _ in 0..50 {
            let p = pick(&posts, &mut rng).unwrap();
            assert!(posts[..PICK_AMONG.min(posts.len())].contains(&p));
        }
        assert_eq!(pick(&[], &mut rng), None);
    }
}
