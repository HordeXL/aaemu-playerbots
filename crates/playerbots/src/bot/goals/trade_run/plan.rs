//! Route planning for trade runs: which pack to craft where, and which specialty NPC buys it.
//!
//! A *route* is a specialty workbench spawn (a doodad whose template offers pack crafts), one of
//! the pack crafts it offers, and the nearest spawn of an NPC that buys the product. Routes are
//! scored by the way the bot has to go (to the bench, then carrying the pack to the buyer) and the
//! planner picks among the best few so that many bots do not all queue at the same bench.
//!
//! The planner works on [`RouteData`], a narrow view of the static world, so the tests run on a
//! synthetic world; [`aa_world::World`] implements it.
//!
//! The world data has no sale prices (`specialty_bundle_items.profit` is not loaded), so routes are
//! not scored by profit; see the open issues in the trade-run report.

use aa_world::{Craft, Vec3, World};

/// Routes kept after sorting.
const MAX_CANDIDATES: usize = 64;
/// The planner draws among this many of the best routes.
const PICK_AMONG: usize = 4;
/// Carrying the pack counts this much per metre (the way back is free, the way there is not).
const CARRY_WEIGHT: f32 = 1.25;

/// The static facts the planner needs. Implemented by [`World`]; tests use a synthetic world.
pub trait RouteData {
    /// Calls `f(doodad template, position)` for every spawn of a doodad that offers pack crafts
    /// within `radius` metres (horizontal) of `center`.
    fn benches_within(&self, center: Vec3, radius: f32, f: &mut dyn FnMut(u32, Vec3));
    /// The pack crafts a bench template offers.
    fn pack_crafts_at(&self, bench: u32) -> &[u32];
    /// A craft.
    fn craft(&self, id: u32) -> Option<&Craft>;
    /// NPC templates that buy `item`.
    fn buyers(&self, item: u32) -> &[u32];
    /// Spawn positions of an NPC template.
    fn npc_spawns(&self, npc: u32) -> &[Vec3];
}

impl RouteData for World {
    fn benches_within(&self, center: Vec3, radius: f32, f: &mut dyn FnMut(u32, Vec3)) {
        self.doodad_spawns_within(center, radius, |template, pos| {
            if !self.pack_crafts_at(template).is_empty() {
                f(template, pos);
            }
        });
    }

    fn pack_crafts_at(&self, bench: u32) -> &[u32] {
        World::pack_crafts_at(self, bench)
    }

    fn craft(&self, id: u32) -> Option<&Craft> {
        World::craft(self, id)
    }

    fn buyers(&self, item: u32) -> &[u32] {
        self.specialty_buyers(item)
    }

    fn npc_spawns(&self, npc: u32) -> &[Vec3] {
        World::npc_spawns(self, npc)
    }
}

/// One planned trade run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// The craft to execute; 0 when the bot already carries the pack.
    pub craft: u32,
    /// The pack item (the craft's first product).
    pub item: u32,
    /// The bench doodad template; 0 when the bot already carries the pack.
    pub bench_tpl: u32,
    /// The buying NPC template.
    pub buyer_tpl: u32,
    /// Bench spawn (x, y).
    pub bench: [f32; 2],
    /// Buyer spawn (x, y).
    pub buyer: [f32; 2],
    /// Straight-line distance from the bot to the bench.
    pub to_bench_m: f32,
    /// Straight-line distance from the bench to the buyer.
    pub carry_m: f32,
}

impl Plan {
    /// What the route costs: the way to the bench plus the weighted way with the pack.
    pub fn score(&self) -> f32 {
        self.to_bench_m + CARRY_WEIGHT * self.carry_m
    }

    /// Total straight-line distance of the run.
    pub fn total_m(&self) -> f32 {
        self.to_bench_m + self.carry_m
    }
}

/// What a planning request wants.
pub struct Query<'a> {
    /// Where the bot is (x, y).
    pub from: [f32; 2],
    /// Longest total route (to the bench plus the carry) in metres; `f32::INFINITY` when the
    /// distances do not matter (GM `/move`).
    pub max_route_m: f32,
    /// Benches further than this from `from` are not looked at.
    pub radius_m: f32,
    /// Whether a craft may be chosen (materials and labor at hand, not quarantined).
    pub craft_ok: &'a dyn Fn(&Craft) -> bool,
    /// Whether a bench spawn (template, x, y) may be used.
    pub bench_ok: &'a dyn Fn(u32, [f32; 2]) -> bool,
    /// Whether a buyer NPC template may be used.
    pub buyer_ok: &'a dyn Fn(u32) -> bool,
}

fn d2(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The nearest spawn of any acceptable buyer of `item` to `from`: `(npc template, (x, y))`.
pub fn nearest_buyer<D: RouteData + ?Sized>(
    data: &D,
    item: u32,
    from: [f32; 2],
    buyer_ok: &dyn Fn(u32) -> bool,
) -> Option<(u32, [f32; 2])> {
    let mut best: Option<(u32, [f32; 2], f32)> = None;
    for &npc in data.buyers(item) {
        if !buyer_ok(npc) {
            continue;
        }
        for sp in data.npc_spawns(npc) {
            let p = [sp.x, sp.y];
            let d = d2(from, p);
            if best.is_none_or(|b| d < b.2) {
                best = Some((npc, p, d));
            }
        }
    }
    best.map(|b| (b.0, b.1))
}

/// Every acceptable route for `q`, best first (at most [`MAX_CANDIDATES`]).
pub fn candidates<D: RouteData + ?Sized>(data: &D, q: &Query<'_>) -> Vec<Plan> {
    let mut out: Vec<Plan> = Vec::new();
    let center = Vec3::new(q.from[0], q.from[1], 0.0);
    data.benches_within(center, q.radius_m, &mut |tpl, pos| {
        let bench = [pos.x, pos.y];
        if !(q.bench_ok)(tpl, bench) {
            return;
        }
        let to_bench_m = d2(q.from, bench);
        for &id in data.pack_crafts_at(tpl) {
            let Some(c) = data.craft(id) else { continue };
            if !c.is_pack || c.product.0 == 0 || !(q.craft_ok)(c) {
                continue;
            }
            let Some((buyer_tpl, buyer)) = nearest_buyer(data, c.product.0, bench, q.buyer_ok)
            else {
                continue;
            };
            let plan = Plan {
                craft: id,
                item: c.product.0,
                bench_tpl: tpl,
                buyer_tpl,
                bench,
                buyer,
                to_bench_m,
                carry_m: d2(bench, buyer),
            };
            if plan.total_m() <= q.max_route_m {
                out.push(plan);
            }
        }
    });
    out.sort_by(|a, b| {
        a.score()
            .total_cmp(&b.score())
            .then(a.craft.cmp(&b.craft))
            .then(a.bench_tpl.cmp(&b.bench_tpl))
    });
    out.truncate(MAX_CANDIDATES);
    out
}

/// Draws one of the best routes (`cands` sorted best first), preferring different crafts:
/// among the first [`PICK_AMONG`] distinct crafts, uniformly.
pub fn pick(cands: &[Plan], rng: &mut fastrand::Rng) -> Option<Plan> {
    let mut crafts: smallvec::SmallVec<[Plan; PICK_AMONG]> = smallvec::SmallVec::new();
    for p in cands {
        if crafts.iter().all(|c| c.craft != p.craft) {
            crafts.push(*p);
            if crafts.len() == PICK_AMONG {
                break;
            }
        }
    }
    if crafts.is_empty() {
        None
    } else {
        Some(crafts[rng.usize(..crafts.len())])
    }
}

/// A plan for a bot that already carries pack `item`: only the walk to the nearest buyer.
pub fn carry_only<D: RouteData + ?Sized>(
    data: &D,
    item: u32,
    from: [f32; 2],
    buyer_ok: &dyn Fn(u32) -> bool,
) -> Option<Plan> {
    let (buyer_tpl, buyer) = nearest_buyer(data, item, from, buyer_ok)?;
    Some(Plan {
        craft: 0,
        item,
        bench_tpl: 0,
        buyer_tpl,
        bench: from,
        buyer,
        to_bench_m: 0.0,
        carry_m: d2(from, buyer),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rustc_hash::FxHashMap;

    /// A tiny synthetic world.
    #[derive(Default)]
    pub struct Fake {
        pub benches: Vec<(u32, Vec3)>,
        pub bench_crafts: FxHashMap<u32, Vec<u32>>,
        pub crafts: FxHashMap<u32, Craft>,
        pub buyers: FxHashMap<u32, Vec<u32>>,
        pub spawns: FxHashMap<u32, Vec<Vec3>>,
    }

    pub fn craft(id: u32, item: u32, labor: u16) -> Craft {
        Craft {
            id,
            name: format!("craft {id}").into(),
            skill_id: 16766,
            labor,
            product: (item, 1),
            materials: vec![(4747, 1), (30901, 50)].into_boxed_slice(),
            is_pack: true,
            req_doodad: 0,
            benches: Box::new([]),
        }
    }

    impl Fake {
        /// Benches 100 (at 1000,0) and 200 (at 0,3000); crafts 1 (item 11) and 2 (item 12) at
        /// bench 100, craft 3 (item 13) at bench 200; buyers 500 (item 11 and 12) at (1500,0)
        /// and (0, 9000), 501 (item 13) at (0, 3200).
        pub fn sample() -> Fake {
            let mut f = Fake::default();
            f.benches.push((100, Vec3::new(1000.0, 0.0, 0.0)));
            f.benches.push((200, Vec3::new(0.0, 3000.0, 0.0)));
            f.bench_crafts.insert(100, vec![1, 2]);
            f.bench_crafts.insert(200, vec![3]);
            f.crafts.insert(1, craft(1, 11, 60));
            f.crafts.insert(2, craft(2, 12, 60));
            f.crafts.insert(3, craft(3, 13, 60));
            f.buyers.insert(11, vec![500]);
            f.buyers.insert(12, vec![500]);
            f.buyers.insert(13, vec![501]);
            f.spawns.insert(
                500,
                vec![Vec3::new(0.0, 9000.0, 0.0), Vec3::new(1500.0, 0.0, 0.0)],
            );
            f.spawns.insert(501, vec![Vec3::new(0.0, 3200.0, 0.0)]);
            f
        }
    }

    impl RouteData for Fake {
        fn benches_within(&self, center: Vec3, radius: f32, f: &mut dyn FnMut(u32, Vec3)) {
            for (t, p) in &self.benches {
                if center.dist2d(*p) <= radius {
                    f(*t, *p);
                }
            }
        }
        fn pack_crafts_at(&self, bench: u32) -> &[u32] {
            self.bench_crafts.get(&bench).map_or(&[], |v| &v[..])
        }
        fn craft(&self, id: u32) -> Option<&Craft> {
            self.crafts.get(&id)
        }
        fn buyers(&self, item: u32) -> &[u32] {
            self.buyers.get(&item).map_or(&[], |v| &v[..])
        }
        fn npc_spawns(&self, npc: u32) -> &[Vec3] {
            self.spawns.get(&npc).map_or(&[], |v| &v[..])
        }
    }

    fn open_query<'a>(
        from: [f32; 2],
        max_route_m: f32,
        radius_m: f32,
        craft_ok: &'a dyn Fn(&Craft) -> bool,
        bench_ok: &'a dyn Fn(u32, [f32; 2]) -> bool,
        buyer_ok: &'a dyn Fn(u32) -> bool,
    ) -> Query<'a> {
        Query {
            from,
            max_route_m,
            radius_m,
            craft_ok,
            bench_ok,
            buyer_ok,
        }
    }

    #[test]
    fn the_nearest_buyer_spawn_wins_and_routes_are_sorted() {
        let w = Fake::sample();
        let q = open_query(
            [0.0, 0.0],
            f32::INFINITY,
            50_000.0,
            &|_| true,
            &|_, _| true,
            &|_| true,
        );
        let c = candidates(&w, &q);
        assert_eq!(c.len(), 3);
        // craft 1 and 2: bench 1000 m away, buyer 500 m beyond it (the nearer of two spawns)
        assert_eq!((c[0].craft, c[0].bench_tpl, c[0].buyer_tpl), (1, 100, 500));
        assert_eq!(c[0].buyer, [1500.0, 0.0]);
        assert!((c[0].to_bench_m - 1000.0).abs() < 0.1);
        assert!((c[0].carry_m - 500.0).abs() < 0.1);
        assert_eq!(c[1].craft, 2);
        assert_eq!(c[2].craft, 3);
        assert!(c[0].score() <= c[1].score() && c[1].score() <= c[2].score());
        assert!((c[2].total_m() - 3200.0).abs() < 0.1);
    }

    #[test]
    fn the_walking_budget_and_the_radius_limit_the_routes() {
        let w = Fake::sample();
        let all = |_: &Craft| true;
        let any = |_: u32, _: [f32; 2]| true;
        let buy = |_: u32| true;
        let q = open_query([0.0, 0.0], 2000.0, 50_000.0, &all, &any, &buy);
        let c = candidates(&w, &q);
        assert_eq!(c.iter().map(|p| p.craft).collect::<Vec<_>>(), vec![1, 2]);
        // bench 200 is outside a 2 km radius
        let q = open_query([0.0, 0.0], f32::INFINITY, 2000.0, &all, &any, &buy);
        assert_eq!(candidates(&w, &q).len(), 2);
        // from the north bench the other one is out of the budget
        let q = open_query([0.0, 3000.0], 500.0, 50_000.0, &all, &any, &buy);
        let c = candidates(&w, &q);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].craft, c[0].bench_tpl, c[0].buyer_tpl), (3, 200, 501));
    }

    #[test]
    fn filters_remove_crafts_benches_and_buyers() {
        let w = Fake::sample();
        let no_two = |c: &Craft| c.id != 2;
        let any = |_: u32, _: [f32; 2]| true;
        let buy = |_: u32| true;
        let q = open_query([0.0, 0.0], f32::INFINITY, 50_000.0, &no_two, &any, &buy);
        assert_eq!(
            candidates(&w, &q)
                .iter()
                .map(|p| p.craft)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        let all = |_: &Craft| true;
        let no_bench_100 = |t: u32, _: [f32; 2]| t != 100;
        let q = open_query(
            [0.0, 0.0],
            f32::INFINITY,
            50_000.0,
            &all,
            &no_bench_100,
            &buy,
        );
        assert_eq!(
            candidates(&w, &q)
                .iter()
                .map(|p| p.craft)
                .collect::<Vec<_>>(),
            vec![3]
        );
        let no_501 = |n: u32| n != 501;
        let q = open_query([0.0, 0.0], f32::INFINITY, 50_000.0, &all, &any, &no_501);
        assert_eq!(candidates(&w, &q).len(), 2);
        // a craft that is no pack is never planned
        let mut w2 = Fake::sample();
        w2.crafts.get_mut(&1).unwrap().is_pack = false;
        let q = open_query([0.0, 0.0], f32::INFINITY, 50_000.0, &all, &any, &buy);
        assert_eq!(candidates(&w2, &q).len(), 2);
    }

    #[test]
    fn a_craft_without_a_buyer_is_dropped() {
        let mut w = Fake::sample();
        w.buyers.remove(&13);
        let q = open_query(
            [0.0, 0.0],
            f32::INFINITY,
            50_000.0,
            &|_| true,
            &|_, _| true,
            &|_| true,
        );
        assert_eq!(candidates(&w, &q).len(), 2);
    }

    #[test]
    fn pick_prefers_distinct_crafts_among_the_best() {
        let w = Fake::sample();
        let q = open_query(
            [0.0, 0.0],
            f32::INFINITY,
            50_000.0,
            &|_| true,
            &|_, _| true,
            &|_| true,
        );
        let c = candidates(&w, &q);
        let mut rng = fastrand::Rng::with_seed(9);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            seen.insert(pick(&c, &mut rng).unwrap().craft);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(pick(&[], &mut rng).is_none());
        // the same craft at two benches counts once
        let mut dup = c.clone();
        dup.insert(
            1,
            Plan {
                bench_tpl: 101,
                ..c[0]
            },
        );
        let mut counts = std::collections::BTreeMap::new();
        for _ in 0..3000 {
            *counts
                .entry(pick(&dup, &mut rng).unwrap().craft)
                .or_insert(0u32) += 1;
        }
        assert!(counts[&1] < 1300, "{counts:?}");
    }

    #[test]
    fn carry_only_goes_to_the_nearest_buyer() {
        let w = Fake::sample();
        let p = carry_only(&w, 11, [1400.0, 0.0], &|_| true).unwrap();
        assert_eq!((p.craft, p.bench_tpl, p.buyer_tpl), (0, 0, 500));
        assert_eq!(p.buyer, [1500.0, 0.0]);
        assert!((p.carry_m - 100.0).abs() < 0.1 && p.to_bench_m == 0.0);
        assert!(carry_only(&w, 11, [0.0, 0.0], &|n| n != 500).is_none());
        assert!(carry_only(&w, 99, [0.0, 0.0], &|_| true).is_none());
    }
}
