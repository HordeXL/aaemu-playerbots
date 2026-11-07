//! Where an exploring bot goes next: a pure choice over the world's landmarks and the bot's
//! journal. The checks that need the world (danger, the navigation mesh, portals, war) come from
//! an [`Env`], so the choice can be tested without one.
//!
//! The candidates are the landmarks within `max_trip_m` and at least [`MIN_TRIP_M`] away. Those in
//! zones the bot has never stood in count most, then the places in known zones it has not seen
//! (hills and the coast before recall points and zone middles). Places it has seen count a
//! little, so a bot that has seen everything around still goes for a look now and then. Nearer
//! ones count more. The best [`TOP`] are drawn at random by weight and checked in that order, at
//! most [`MAX_CHECKS`] of them (the danger check reads the spawns along the way).
//!
//! A zone is a zone as players know it, the zone group of the keys ([`Landmarks::region`]): a
//! bot that stood in one key of "Solzreed Peninsula" has been to Solzreed Peninsula.
//!
//! A zone the bot was sent to ([`Query::zone`]) narrows the candidates to its landmarks, at any
//! distance; the checks still apply.

use aa_world::landmarks::{Landmark, LandmarkKind, Landmarks};
use aa_world::World;

use crate::bot::goals::quest::progress;
use crate::journal::Seen;

/// The shortest trip the bot makes on its own (metres): nearer places it sees anyway.
pub const MIN_TRIP_M: f32 = 150.0;
/// The shortest trip to a zone it was sent to (it may already stand next to the landmark).
pub const ASKED_MIN_M: f32 = 10.0;
/// Candidates drawn from, the best by weight.
pub const TOP: usize = 12;
/// Candidates checked against the [`Env`] at most.
pub const MAX_CHECKS: usize = 6;
/// The distance at which a candidate counts half (metres).
pub const HALF_AT_M: f32 = 2_000.0;
/// How much more a landmark in a zone the bot has never been to counts.
pub const NEW_ZONE_GAIN: f32 = 3.0;
/// How much a place the bot has seen counts, against one it has not.
pub const SEEN_FACTOR: f32 = 0.1;
/// The radius around the destination where monsters of any grade (bosses too) `level_gap` above
/// the bot make it unsafe (metres)...
pub const DANGER_RADIUS_M: f32 = 150.0;
/// ...and the wider one for normal and elite monsters: the area the bot looks around in.
pub const AREA_RADIUS_M: f32 = 300.0;
/// The straight line to the destination is sampled this often (metres)...
pub const ROUTE_STEP_M: f32 = 250.0;
/// ...at most this many times...
pub const ROUTE_SAMPLES: usize = 24;
/// ...within this radius (metres)...
pub const ROUTE_RADIUS_M: f32 = 60.0;
/// ...for normal and elite monsters this many levels more above the bot than `level_gap`.
pub const ROUTE_EXTRA_GAP: u8 = 4;
/// The highest monster grade the area and route checks count (bosses stand in their lairs).
const MAX_GRADE: u8 = 2;

/// Why the bot goes there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// A zone it has never stood in.
    NewZone,
    /// A landmark it has not seen in a zone it knows.
    NewPlace,
    /// A place it has seen before.
    Again,
    /// The zone or point it was sent to.
    Asked,
}

impl Why {
    /// Name for logs.
    pub const fn name(self) -> &'static str {
        match self {
            Why::NewZone => "new zone",
            Why::NewPlace => "new place",
            Why::Again => "seen before",
            Why::Asked => "asked",
        }
    }
}

/// The destination of a trip.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// The landmark's kind, `None` for a point the bot was sent to.
    pub kind: Option<LandmarkKind>,
    /// The zone it lies in.
    pub zone_key: Option<u16>,
    /// Where.
    pub pos: [f32; 3],
    /// Why.
    pub why: Why,
    /// How far it was when chosen (metres, straight).
    pub dist: f32,
    /// A recall point's name, empty for the others.
    pub name: Box<str>,
}

/// Why no destination was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoTrip {
    /// Nothing within reach (no landmarks, or all too near, too far or avoided).
    NothingNear,
    /// The zone the bot was sent to has no landmarks.
    UnknownZone,
    /// Every candidate checked was at war, out of reach or too dangerous.
    AllUnsafe,
}

impl NoTrip {
    /// Text for the log and the goal's failure.
    pub const fn text(self) -> &'static str {
        match self {
            NoTrip::NothingNear => "nowhere new to go",
            NoTrip::UnknownZone => "the zone has no landmarks",
            NoTrip::AllUnsafe => "every place nearby is unsafe or out of reach",
        }
    }
}

/// What the choice works from.
#[derive(Clone, Copy, Debug)]
pub struct Query<'a> {
    /// The bot's position.
    pub me: [f32; 3],
    /// The longest trip it makes on its own (metres, straight).
    pub max_trip_m: f32,
    /// The time, for the journal's avoid list (Unix seconds).
    pub now_unix: u64,
    /// The bot's journal: where it has been.
    pub seen: &'a Seen,
    /// The zone it was sent to.
    pub zone: Option<u16>,
}

/// The checks the choice asks about a candidate.
pub trait Env {
    /// The place lies in a zone at war the bot keeps out of.
    fn at_war(&mut self, x: f32, y: f32) -> bool;
    /// The bot cannot get there: another part of the navigation mesh and no portal route.
    fn unreachable(&mut self, from: [f32; 2], to: [f32; 2]) -> bool;
    /// Monsters well above the bot's level at the place or along the way.
    fn dangerous(&mut self, from: [f32; 2], to: [f32; 2]) -> bool;
}

/// How much a kind of landmark is worth seeing.
pub const fn kind_weight(kind: LandmarkKind) -> f32 {
    match kind {
        LandmarkKind::Hill => 1.6,
        LandmarkKind::Coast => 1.3,
        LandmarkKind::Recall => 1.0,
        LandmarkKind::Centre => 0.8,
    }
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The regions (zones as players know them) of the zone keys the bot has been to.
fn seen_regions(lm: &Landmarks, seen: &Seen) -> Vec<u32> {
    let mut r: Vec<u32> = seen.zones.iter().map(|&k| lm.region(k)).collect();
    r.sort_unstable();
    r.dedup();
    r
}

/// The weight of landmark `l` at distance `d`, and why the bot would go; `None` when it is not a
/// candidate. `asked`: the region of [`Query::zone`]; `known`: [`seen_regions`].
fn weigh(
    l: &Landmark,
    q: &Query<'_>,
    d: f32,
    asked: Option<u32>,
    known: &[u32],
) -> Option<(f32, Why)> {
    let (x, y) = (l.pos[0], l.pos[1]);
    if q.seen.avoided(x, y, q.now_unix) {
        return None;
    }
    let seen_place = q.seen.has_place(x, y);
    let (w, why) = match asked {
        Some(z) => {
            if l.region != z || d < ASKED_MIN_M {
                return None;
            }
            let w = if seen_place { SEEN_FACTOR } else { 1.0 };
            (kind_weight(l.kind) * w, Why::Asked)
        }
        None => {
            if !(MIN_TRIP_M..=q.max_trip_m).contains(&d) {
                return None;
            }
            if known.binary_search(&l.region).is_err() {
                (kind_weight(l.kind) * NEW_ZONE_GAIN, Why::NewZone)
            } else if !seen_place {
                (kind_weight(l.kind), Why::NewPlace)
            } else {
                (kind_weight(l.kind) * SEEN_FACTOR, Why::Again)
            }
        }
    };
    Some((w / (1.0 + d / HALF_AT_M), why))
}

/// How many landmarks within reach the bot has not seen (in new zones or new places), counted up
/// to `cap`. The explore score reads it; 0 means there is nothing new to go to.
pub fn novelty(lm: &Landmarks, q: &Query<'_>, cap: u32) -> u32 {
    let known = seen_regions(lm, q.seen);
    let mut n = 0;
    for l in lm.all() {
        let d = dist2(q.me, l.pos);
        if let Some((_, Why::NewZone | Why::NewPlace)) = weigh(l, q, d, None, &known) {
            n += 1;
            if n >= cap {
                break;
            }
        }
    }
    n
}

/// Chooses where to go. See the module docs.
pub fn choose(
    lm: &Landmarks,
    q: &Query<'_>,
    env: &mut dyn Env,
    rng: &mut fastrand::Rng,
) -> Result<Pick, NoTrip> {
    let asked = q.zone.map(|z| lm.region(z));
    if asked.is_some_and(|r| !lm.all().iter().any(|l| l.region == r)) {
        return Err(NoTrip::UnknownZone);
    }
    let known = seen_regions(lm, q.seen);
    let pool = lm.all();
    // (weight, distance, why, index)
    let mut cands: Vec<(f32, f32, Why, usize)> = pool
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let d = dist2(q.me, l.pos);
            weigh(l, q, d, asked, &known).map(|(w, why)| (w, d, why, i))
        })
        .collect();
    if cands.is_empty() {
        return Err(NoTrip::NothingNear);
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    cands.truncate(TOP);
    let from = [q.me[0], q.me[1]];
    for _ in 0..MAX_CHECKS.min(cands.len()) {
        let total: f32 = cands.iter().map(|c| c.0).sum();
        let mut r = rng.f32() * total;
        let mut at = cands.len() - 1;
        for (i, c) in cands.iter().enumerate() {
            if r < c.0 {
                at = i;
                break;
            }
            r -= c.0;
        }
        let (_, d, why, i) = cands.swap_remove(at);
        let l = &pool[i];
        let to = [l.pos[0], l.pos[1]];
        if env.at_war(to[0], to[1]) || env.unreachable(from, to) || env.dangerous(from, to) {
            continue;
        }
        return Ok(Pick {
            kind: Some(l.kind),
            zone_key: Some(l.zone_key),
            pos: l.pos,
            why,
            dist: d,
            name: l.name.clone(),
        });
    }
    Err(NoTrip::AllUnsafe)
}

/// The world's side of the checks: the danger of the spawns (the quest goal's checks, plus the
/// configured level gap around the destination and a tighter one along the way) and the
/// navigation mesh. The goal adds the portal routes and the war zones; a test can use it as it
/// is.
#[derive(Clone, Copy)]
pub struct WorldChecks<'w> {
    pub world: &'w World,
    /// The bot's faction.
    pub faction: u32,
    /// The bot's level.
    pub level: u8,
    /// Monsters this many levels above the bot make a destination unsafe (`[explore]
    /// level_gap`).
    pub level_gap: u8,
}

impl WorldChecks<'_> {
    /// Monsters well above the bot at `to` or on the way from `from`: hostile aggressive ones
    /// `level_gap` above it around the destination (any grade within [`DANGER_RADIUS_M`], normal
    /// and elite within [`AREA_RADIUS_M`]), [`ROUTE_EXTRA_GAP`] more along the straight line, or
    /// what the quest goal calls deadly.
    pub fn dangerous(&self, from: [f32; 2], to: [f32; 2]) -> bool {
        let (w, f, lvl) = (self.world, self.faction, self.level);
        let gap = self.level_gap;
        let above = |worst: u8, gap: u8| worst >= lvl.saturating_add(gap);
        if above(
            progress::worst_near(w, f, to[0], to[1], DANGER_RADIUS_M, u8::MAX),
            gap,
        ) || above(
            progress::worst_near(w, f, to[0], to[1], AREA_RADIUS_M, MAX_GRADE),
            gap,
        ) {
            return true;
        }
        let d = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();
        let n = ((d / ROUTE_STEP_M) as usize).min(ROUTE_SAMPLES);
        let route = gap.saturating_add(ROUTE_EXTRA_GAP);
        (1..n).any(|i| {
            let k = i as f32 / n as f32;
            let (x, y) = (
                from[0] + (to[0] - from[0]) * k,
                from[1] + (to[1] - from[1]) * k,
            );
            above(
                progress::worst_near(w, f, x, y, ROUTE_RADIUS_M, MAX_GRADE),
                route,
            )
        }) || progress::deadly(w, f, lvl, (from[0], from[1]), (to[0], to[1]))
    }

    /// `to` lies on another part of the navigation mesh than `from` (no walking there).
    pub fn cut_off(&self, from: [f32; 2], to: [f32; 2]) -> bool {
        progress::cut_off(self.world, (from[0], from[1]), (to[0], to[1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::baked::ZoneGrid;

    /// Two zones side by side (keys 1 and 2), 64 x 32 sectors of 64 m; flat land with a hill in
    /// zone 2.
    fn landmarks() -> Landmarks {
        let (w, h) = (64u32, 32u32);
        let keys = (0..w * h).map(|i| if i % w < 32 { 1 } else { 2 }).collect();
        let grid = ZoneGrid {
            width: w,
            height: h,
            sector: 64.0,
            keys,
        };
        let ground = |x: f32, y: f32| {
            let d = ((x - 3_800.0).powi(2) + (y - 300.0).powi(2)).sqrt();
            Some(110.0 + (80.0 - d / 5.0).max(0.0))
        };
        Landmarks::build(Some(&grid), &ground, &|_| Some(0), &[])
    }

    #[derive(Default)]
    struct FakeEnv {
        war: bool,
        cut: bool,
        danger_above_x: Option<f32>,
        asked: u32,
    }

    impl Env for FakeEnv {
        fn at_war(&mut self, _x: f32, _y: f32) -> bool {
            self.asked += 1;
            self.war
        }
        fn unreachable(&mut self, _from: [f32; 2], _to: [f32; 2]) -> bool {
            self.cut
        }
        fn dangerous(&mut self, _from: [f32; 2], to: [f32; 2]) -> bool {
            self.danger_above_x.is_some_and(|x| to[0] > x)
        }
    }

    fn query(seen: &Seen, me: [f32; 3]) -> Query<'_> {
        Query {
            me,
            max_trip_m: 6_000.0,
            now_unix: 1_000_000,
            seen,
            zone: None,
        }
    }

    #[test]
    fn a_bot_heads_for_the_zone_it_has_never_been_to() {
        let lm = landmarks();
        let mut seen = Seen::default();
        seen.add_zone(1);
        let me = [600.0, 600.0, 110.0];
        let mut rng = fastrand::Rng::with_seed(7);
        let mut new_zone = 0;
        for _ in 0..50 {
            let p = choose(&lm, &query(&seen, me), &mut FakeEnv::default(), &mut rng).unwrap();
            assert!(p.dist >= MIN_TRIP_M && p.dist <= 6_000.0);
            if p.why == Why::NewZone {
                assert_eq!(p.zone_key, Some(2));
                new_zone += 1;
            }
        }
        assert!(new_zone > 30, "{new_zone} of 50 went to the new zone");
        assert!(novelty(&lm, &query(&seen, me), 8) > 0);
        // once it has seen both zones and every place, nothing is new (but a trip is still found)
        seen.add_zone(2);
        for l in lm.all() {
            seen.add_place(l.pos[0], l.pos[1]);
        }
        assert_eq!(novelty(&lm, &query(&seen, me), 8), 0);
        let p = choose(&lm, &query(&seen, me), &mut FakeEnv::default(), &mut rng).unwrap();
        assert_eq!(p.why, Why::Again);
    }

    #[test]
    fn unsafe_and_unreachable_places_are_left_out() {
        let lm = landmarks();
        let mut seen = Seen::default();
        seen.add_zone(1);
        let me = [600.0, 600.0, 110.0];
        let mut rng = fastrand::Rng::with_seed(3);
        // zone 2 (x > 2048) is dangerous: the bot stays in zone 1
        let mut env = FakeEnv {
            danger_above_x: Some(2_048.0),
            ..FakeEnv::default()
        };
        for _ in 0..20 {
            match choose(&lm, &query(&seen, me), &mut env, &mut rng) {
                Ok(p) => assert_eq!(p.zone_key, Some(1), "{p:?}"),
                Err(e) => assert_eq!(e, NoTrip::AllUnsafe),
            }
        }
        let mut war = FakeEnv {
            war: true,
            ..FakeEnv::default()
        };
        assert_eq!(
            choose(&lm, &query(&seen, me), &mut war, &mut rng),
            Err(NoTrip::AllUnsafe)
        );
        assert!(war.asked as usize <= MAX_CHECKS, "bounded checks");
        let mut cut = FakeEnv {
            cut: true,
            ..FakeEnv::default()
        };
        assert!(choose(&lm, &query(&seen, me), &mut cut, &mut rng).is_err());
        // an avoided place is not chosen
        let mut seen = Seen::default();
        for l in lm.all() {
            seen.avoid(l.pos[0], l.pos[1], 2_000_000, 1_000_000);
        }
        assert!(seen.avoid.len() <= 8 /* seen::MAX_AVOID */);
        assert!(lm.all().len() <= 8 /* seen::MAX_AVOID */);
        assert_eq!(
            choose(&lm, &query(&seen, me), &mut FakeEnv::default(), &mut rng),
            Err(NoTrip::NothingNear)
        );
    }

    #[test]
    fn a_zone_it_is_sent_to_narrows_the_choice() {
        let lm = landmarks();
        let mut seen = Seen::default();
        seen.add_zone(1);
        seen.add_zone(2);
        let me = [600.0, 600.0, 110.0];
        let mut rng = fastrand::Rng::with_seed(9);
        let q = Query {
            zone: Some(2),
            max_trip_m: 200.0,
            ..query(&seen, me)
        };
        let p = choose(&lm, &q, &mut FakeEnv::default(), &mut rng).unwrap();
        assert_eq!((p.zone_key, p.why), (Some(2), Why::Asked));
        assert!(
            p.dist > 200.0,
            "the operator's zone may be farther than a trip of its own"
        );
        let q = Query {
            zone: Some(77),
            ..query(&seen, me)
        };
        assert_eq!(
            choose(&lm, &q, &mut FakeEnv::default(), &mut rng),
            Err(NoTrip::UnknownZone)
        );
        assert!(kind_weight(LandmarkKind::Hill) > kind_weight(LandmarkKind::Centre));
        assert_eq!(Why::NewZone.name(), "new zone");
        assert!(!NoTrip::AllUnsafe.text().is_empty());
    }
}
