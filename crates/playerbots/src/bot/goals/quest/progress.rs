//! Progression: where a quest bot goes when nothing it can do is near.
//!
//! The quest goal looks for givers within 300 m and then 1500 m. A bot that has done everything in
//! that circle used to stay there for good (the goal failed, grinding and wandering filled in), so
//! a bot at the end of a zone's quests never reached the next zone when the next story quest was
//! more than 1500 m away (the Harani and Firran hand-off from Villanelle to the Silent Forest is
//! 2 km). When the goal has found nothing for [`EMPTY_SCANS`] scans in a row it plans a trip with
//! the functions below, in this order:
//!
//! 1. [`story_target`]: the giver of the first quest of the bot's race story line that it
//!    qualifies for (level, prerequisites, race gates, not resting), beyond the search circle;
//! 2. [`zone_target`]: the densest cluster of quest givers of the zone that holds the most quests
//!    suited to the bot's level, discounted by distance;
//! 3. [`grind_target`]: a spot with monsters of its level near the zone whose quests it will be
//!    able to take within a few levels.
//!
//! The functions are pure: the caller passes the quest table ([`QuestAtlas`]), what a point is
//! worth to this bot (`eligible`/`weight`) and what is too dangerous or recently failed
//! (`risky`/`blocked`). [`Progress`] is the bot's memory of it: the trip under way, how often scans
//! came back empty, destinations that failed (they rest for 10 minutes, then 20, then 40) and the
//! last few trips. It lives in the quest [`Memory`](super::Memory) so a trip survives the end of a
//! quest run (a rest, a duel, a time limit).

use aa_world::{AtlasPoint, QuestAtlas, Relation, Vec3, World};
use smallvec::SmallVec;

use super::select::FAR_RADIUS;

/// Empty scans in a row (5 s apart) before a trip is planned.
pub const EMPTY_SCANS: u8 = 3;
/// Targets nearer than this are the quest picker's business, not a trip's.
pub const NEAR_M: f32 = FAR_RADIUS;
/// The bot has arrived this close to the target of a trip.
pub const ARRIVE_M: f32 = 30.0;
/// Average speed assumed for the time limit of a trip (metres per second): well below running
/// pace, because a route is longer than the straight line and bots fight and rest on the way.
pub const SPEED_MPS: f32 = 3.0;
/// Time added to the walking time of a trip.
pub const TRIP_SLACK_MS: u64 = 180_000;
/// Shortest time limit of a trip.
pub const TRIP_MIN_MS: u64 = 300_000;
/// Longest time limit of a trip.
pub const TRIP_MAX_MS: u64 = 2_700_000;
/// How long a destination whose trip failed is left alone: this, twice, four times.
pub const FAILED_MS: u64 = 600_000;
/// Failures of one destination that raise its rest (at most two doublings).
const MAX_DOUBLINGS: u8 = 2;
/// Destinations within this many metres of a failed one are left alone with it.
pub const FAILED_RADIUS_M: f32 = 400.0;
/// Failed destinations remembered.
const MAX_FAILED: usize = 4;
/// After a search that found no destination the next one waits this long.
pub const NO_DEST_MS: u64 = 120_000;
/// After a trip to a grinding spot no further grinding trip is planned for this long.
pub const SETTLE_MS: u64 = 600_000;
/// A scan this soon after arriving that finds nothing means the destination was empty.
pub const BARREN_MS: u64 = 90_000;
/// Deaths during one trip after which it is given up.
pub const MAX_DEATHS: u8 = 2;
/// Hostile aggressive monsters this many levels above the bot make a destination risky.
pub const DEST_DANGER_GAP: u8 = 10;
/// ... and a point of the straight line to it.
pub const ROUTE_DANGER_GAP: u8 = 14;
/// Radius around a destination that is looked at for danger.
const DEST_DANGER_RADIUS_M: f32 = 120.0;
/// Spacing and radius of the points sampled along the straight line.
const ROUTE_STEP_M: f32 = 300.0;
const ROUTE_RADIUS_M: f32 = 60.0;
/// Monsters of a higher grade (bosses) are not counted along the line.
const ROUTE_MAX_GRADE: u8 = 2;
/// Most points sampled along a line.
const ROUTE_SAMPLES: usize = 40;
/// A zone needs this much weight (one quest suited to the level counts 1) to be worth a trip.
pub const MIN_ZONE_WEIGHT: f32 = 1.5;
/// Quests within this distance of each other make a cluster, the hub of a zone is its densest.
pub const CLUSTER_M: f32 = 400.0;
/// A zone this far away is worth half of one next door.
pub const DIST_SCALE_M: f32 = 4_000.0;
/// Zones evaluated for danger before giving up.
const MAX_RISK_CHECKS: usize = 4;
/// Quests this many levels below the bot are trivial: the experience is not worth a trip.
pub const TRIVIAL_GAP: u8 = 6;
/// Trips remembered.
const MAX_HISTORY: usize = 4;
/// Monsters are looked for this far around the zone a grinding trip aims at.
const GRIND_RADIUS_M: f32 = 600.0;
/// Quests this many levels above the bot (and one more) make a zone one to grind for.
const GRIND_AHEAD: std::ops::RangeInclusive<u8> = 2..=4;

/// What a trip is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The giver of this story quest.
    Story(u32),
    /// The quests of this zone key.
    Zone(u16),
    /// Monsters of the bot's level near this zone key.
    Grind(u16),
}

/// Where to go and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dest {
    /// Destination x.
    pub x: f32,
    /// Destination y.
    pub y: f32,
    /// What it is for.
    pub why: Why,
    /// Straight-line distance from the bot when it was chosen.
    pub dist: f32,
    /// What the choice rested on: 100 for a story quest, the weight of the zone's quests, 0 for
    /// grinding.
    pub weight: f32,
}

/// A trip under way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trip {
    /// Where it goes.
    pub target: Dest,
    /// Where it started.
    pub from: (f32, f32),
    /// Bot clock at the start.
    pub started_ms: u64,
    /// Bot clock after which the trip is given up.
    pub deadline_ms: u64,
    /// Deaths on the way.
    pub deaths: u8,
}

/// How a trip ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The bot stood at the destination.
    Arrived,
    /// A quest came within the picker's reach on the way: the trip's job was done.
    InRange,
    /// The time limit ran out.
    TimedOut,
    /// The way was blocked.
    Blocked,
    /// The bot died too often on the way.
    Died,
}

impl Outcome {
    /// Whether the trip did what it was for.
    pub fn good(self) -> bool {
        matches!(self, Outcome::Arrived | Outcome::InRange)
    }

    /// For logs.
    pub fn text(self) -> &'static str {
        match self {
            Outcome::Arrived => "arrived",
            Outcome::InRange => "quests in range",
            Outcome::TimedOut => "timed out",
            Outcome::Blocked => "blocked",
            Outcome::Died => "died too often",
        }
    }
}

/// One finished trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Record {
    /// Start (x, y), whole metres.
    pub from: (i32, i32),
    /// Destination (x, y), whole metres.
    pub to: (i32, i32),
    /// Straight-line distance at the start, metres.
    pub dist_m: u32,
    /// What it was for.
    pub why: Why,
    /// How it ended.
    pub outcome: Outcome,
    /// Bot clock at the end.
    pub at_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct Failed {
    x: f32,
    y: f32,
    count: u8,
    until_ms: u64,
}

/// The bot's memory of its trips. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct Progress {
    /// Empty scans in a row.
    pub empty_scans: u8,
    /// The trip under way.
    pub trip: Option<Trip>,
    failed: SmallVec<[Failed; MAX_FAILED]>,
    no_dest_until_ms: u64,
    settle_until_ms: u64,
    /// Where and when the last trip arrived, until a scan found something there.
    arrival: Option<(f32, f32, u64)>,
    /// The last trips, newest last.
    pub history: SmallVec<[Record; MAX_HISTORY]>,
    /// Trips started.
    pub started: u16,
    /// Trips that did what they were for.
    pub done: u16,
}

impl Progress {
    /// Whether a destination at (x, y) is resting after a failure.
    pub fn blocked(&self, x: f32, y: f32, now_ms: u64) -> bool {
        self.failed.iter().any(|f| {
            now_ms < f.until_ms && ((f.x - x).powi(2) + (f.y - y).powi(2)).sqrt() <= FAILED_RADIUS_M
        })
    }

    /// Whether a search for a destination is pointless until later.
    pub fn searched_in_vain(&self, now_ms: u64) -> bool {
        now_ms < self.no_dest_until_ms
    }

    /// A search found no destination.
    pub fn found_nothing(&mut self, now_ms: u64) {
        self.no_dest_until_ms = now_ms + NO_DEST_MS;
    }

    /// Whether grinding trips are held back after the last one arrived.
    pub fn settled(&self, now_ms: u64) -> bool {
        now_ms < self.settle_until_ms
    }

    /// Starts a trip from `from` to `target`. Returns the trip.
    pub fn begin(&mut self, target: Dest, from: (f32, f32), now_ms: u64) -> Trip {
        let limit = ((target.dist / SPEED_MPS) as u64) * 1000 + TRIP_SLACK_MS;
        let trip = Trip {
            target,
            from,
            started_ms: now_ms,
            deadline_ms: now_ms + limit.clamp(TRIP_MIN_MS, TRIP_MAX_MS),
            deaths: 0,
        };
        self.trip = Some(trip);
        self.empty_scans = 0;
        self.arrival = None;
        self.started = self.started.saturating_add(1);
        trip
    }

    /// The trip under way ended. A trip that did not do its job puts its destination to rest.
    pub fn end(&mut self, outcome: Outcome, now_ms: u64) -> Option<Trip> {
        let trip = self.trip.take()?;
        let t = trip.target;
        if outcome.good() {
            self.done = self.done.saturating_add(1);
            if outcome == Outcome::Arrived {
                self.arrival = Some((t.x, t.y, now_ms));
                if matches!(t.why, Why::Grind(_)) {
                    self.settle_until_ms = now_ms + SETTLE_MS;
                }
            }
        } else {
            self.rest_destination(t.x, t.y, now_ms);
        }
        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(Record {
            from: (trip.from.0 as i32, trip.from.1 as i32),
            to: (t.x as i32, t.y as i32),
            dist_m: t.dist as u32,
            why: t.why,
            outcome,
            at_ms: now_ms,
        });
        Some(trip)
    }

    /// The bot died on its trip; `true` when that was once too often (the trip is over and its
    /// destination rests).
    pub fn died(&mut self, now_ms: u64) -> bool {
        let Some(trip) = self.trip.as_mut() else {
            return false;
        };
        trip.deaths = trip.deaths.saturating_add(1);
        if trip.deaths >= MAX_DEATHS {
            self.end(Outcome::Died, now_ms);
            return true;
        }
        false
    }

    /// A scan at the place a trip arrived found no quest: the destination is empty for this bot
    /// and rests. Returns whether that was the case (the scan came soon enough after arriving).
    pub fn barren(&mut self, now_ms: u64) -> bool {
        let Some((x, y, at)) = self.arrival.take() else {
            return false;
        };
        if now_ms.saturating_sub(at) > BARREN_MS {
            return false;
        }
        self.rest_destination(x, y, now_ms);
        true
    }

    /// A scan found a quest: whatever the last trip brought, it was not in vain.
    pub fn fruitful(&mut self) {
        self.arrival = None;
        self.empty_scans = 0;
    }

    fn rest_destination(&mut self, x: f32, y: f32, now_ms: u64) {
        let near = |f: &Failed| ((f.x - x).powi(2) + (f.y - y).powi(2)).sqrt() <= FAILED_RADIUS_M;
        let count = self
            .failed
            .iter()
            .find(|f| near(f))
            .map_or(0, |f| f.count)
            .saturating_add(1);
        self.failed.retain(|f| !near(f));
        if self.failed.len() >= MAX_FAILED {
            self.failed.remove(0);
        }
        let shift = (count - 1).min(MAX_DOUBLINGS);
        self.failed.push(Failed {
            x,
            y,
            count,
            until_ms: now_ms + (FAILED_MS << shift),
        });
    }
}

fn dist(a: (f32, f32), x: f32, y: f32) -> f32 {
    ((a.0 - x).powi(2) + (a.1 - y).powi(2)).sqrt()
}

/// The giver of the first quest of the race story line `chain` that `eligible` accepts and that
/// is at least [`NEAR_M`] from `me` (nearer ones are the picker's), and not `blocked` or `risky`.
/// A quest has up to three giver spawns; the nearest acceptable one wins.
pub fn story_target(
    atlas: &QuestAtlas,
    chain: &[u32],
    me: (f32, f32),
    eligible: &dyn Fn(&AtlasPoint) -> bool,
    blocked: &dyn Fn(f32, f32) -> bool,
    risky: &dyn Fn((f32, f32), (f32, f32)) -> bool,
) -> Option<Dest> {
    for &quest in chain {
        let mut spots: SmallVec<[(f32, &AtlasPoint); 3]> = atlas
            .spots(quest)
            .map(|p| (dist(me, p.x, p.y), p))
            .filter(|(d, p)| *d >= NEAR_M && eligible(p) && !blocked(p.x, p.y))
            .collect();
        spots.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (d, p) in spots {
            if risky(me, (p.x, p.y)) {
                continue;
            }
            return Some(Dest {
                x: p.x,
                y: p.y,
                why: Why::Story(quest),
                dist: d,
                weight: 100.0,
            });
        }
    }
    None
}

/// The hub of the best zone: for each zone the weights of its quests (`weight(point)`, 0 for one
/// the bot cannot or should not do; only points at least [`NEAR_M`] away and not `blocked` count,
/// a quest counts once however many givers it has), discounted by the distance of the zone's
/// nearest point, and the zone with the highest result of at least [`MIN_ZONE_WEIGHT`] wins. The
/// hub is the point with the most weight within [`CLUSTER_M`] around it. A zone whose hub is
/// `risky` loses to the next one.
pub fn zone_target(
    atlas: &QuestAtlas,
    me: (f32, f32),
    weight: &dyn Fn(&AtlasPoint) -> f32,
    blocked: &dyn Fn(f32, f32) -> bool,
    risky: &dyn Fn((f32, f32), (f32, f32)) -> bool,
) -> Option<Dest> {
    let mut found: Vec<(f32, Dest)> = Vec::new();
    for zone in atlas.zones() {
        // (weight, point, distance), one entry per quest (the nearest of its givers)
        let mut cands: Vec<(f32, &AtlasPoint, f32)> = Vec::new();
        for p in atlas.zone_points(zone) {
            let d = dist(me, p.x, p.y);
            if d < NEAR_M || blocked(p.x, p.y) {
                continue;
            }
            let w = weight(p);
            if w <= 0.0 {
                continue;
            }
            match cands.iter_mut().find(|c| c.1.quest == p.quest) {
                Some(c) if d < c.2 => *c = (w, p, d),
                Some(_) => {}
                None => cands.push((w, p, d)),
            }
        }
        let total: f32 = cands.iter().map(|c| c.0).sum();
        if total < MIN_ZONE_WEIGHT {
            continue;
        }
        let nearest = cands.iter().map(|c| c.2).fold(f32::MAX, f32::min);
        // the hub: the point with the most weight around it, the nearer one on a tie
        let mut hub: Option<(f32, f32, &AtlasPoint)> = None;
        for &(_, p, d) in &cands {
            let around: f32 = cands
                .iter()
                .filter(|c| dist((p.x, p.y), c.1.x, c.1.y) <= CLUSTER_M)
                .map(|c| c.0)
                .sum();
            if hub.is_none_or(|h| around > h.0 + 1e-3 || (around >= h.0 - 1e-3 && d < h.1)) {
                hub = Some((around, d, p));
            }
        }
        let Some((_, d, p)) = hub else { continue };
        found.push((
            total / (1.0 + nearest / DIST_SCALE_M),
            Dest {
                x: p.x,
                y: p.y,
                why: Why::Zone(zone.zone),
                dist: d,
                weight: total,
            },
        ));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    found
        .into_iter()
        .take(MAX_RISK_CHECKS)
        .find(|(_, t)| !risky(me, (t.x, t.y)))
        .map(|(_, t)| t)
}

/// A spot with monsters of the bot's level near the nearest zone that has at least three quests a
/// few levels above it: grinding there leads towards them. `spot(hub)` finds the monsters (see
/// [`grind_spot`]).
pub fn grind_target(
    atlas: &QuestAtlas,
    me: (f32, f32),
    level: u8,
    blocked: &dyn Fn(f32, f32) -> bool,
    risky: &dyn Fn((f32, f32), (f32, f32)) -> bool,
    spot: &dyn Fn((f32, f32)) -> Option<(f32, f32)>,
) -> Option<Dest> {
    let lo = level.saturating_add(*GRIND_AHEAD.start());
    let hi = level.saturating_add(*GRIND_AHEAD.end());
    let mut zones: Vec<(f32, u16, [f32; 2])> = atlas
        .zones()
        .iter()
        .filter(|z| {
            let mut ahead: SmallVec<[u32; 8]> = atlas
                .zone_points(z)
                .iter()
                .filter(|p| (lo..=hi).contains(&p.level))
                .map(|p| p.quest)
                .collect();
            ahead.sort_unstable();
            ahead.dedup();
            ahead.len() >= 3
        })
        .map(|z| (dist(me, z.hub[0], z.hub[1]), z.zone, z.hub))
        .collect();
    zones.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, zone, hub) in zones.into_iter().take(MAX_RISK_CHECKS) {
        let Some((x, y)) = spot((hub[0], hub[1])) else {
            continue;
        };
        let d = dist(me, x, y);
        if blocked(x, y) || risky(me, (x, y)) {
            continue;
        }
        return Some(Dest {
            x,
            y,
            why: Why::Grind(zone),
            dist: d,
            weight: 0.0,
        });
    }
    None
}

/// The spawn of a fair monster nearest to `around`: hostile to `faction`, a normal grade, not a
/// quest giver, of a level the bot can fight ([`crate::bot::goals::level_ok`]).
pub fn grind_spot(
    world: &World,
    faction: u32,
    level: u8,
    around: (f32, f32),
) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32, f32)> = None;
    world.npc_spawns_within(
        Vec3::new(around.0, around.1, 0.0),
        GRIND_RADIUS_M,
        |t, p| {
            let Some(n) = world.npc(t) else { return };
            if n.grade > 1
                || n.is_quest_giver
                || !crate::bot::goals::level_ok(level, n.level)
                || world.relation(faction, n.faction_id) != Relation::Hostile
            {
                return;
            }
            let d = dist(around, p.x, p.y);
            if best.is_none_or(|b| d < b.2) {
                best = Some((p.x, p.y, d));
            }
        },
    );
    best.map(|b| (b.0, b.1))
}

/// The level of the strongest hostile aggressive monster of a grade up to `max_grade` that spawns
/// within `radius` of (x, y).
pub fn worst_near(world: &World, faction: u32, x: f32, y: f32, radius: f32, max_grade: u8) -> u8 {
    let mut worst = 0;
    world.npc_spawns_within(Vec3::new(x, y, 0.0), radius, |t, _| {
        if let Some(n) = world.npc(t) {
            if n.aggressive
                && n.grade <= max_grade
                && n.level > worst
                && world.relation(faction, n.faction_id) == Relation::Hostile
            {
                worst = n.level;
            }
        }
    });
    worst
}

/// Whether going from `from` to `to` looks deadly for a bot of `level`: hostile aggressive
/// monsters of any grade [`DEST_DANGER_GAP`] levels above it around the destination, or normal
/// and elite ones [`ROUTE_DANGER_GAP`] levels above it along the straight line. A route follows
/// roads and bends and the line is sampled sparsely, so the second test is a coarse guess that
/// only the worst cases trip; bosses (grade 3 and up) stand in their lairs and are left out of it.
pub fn deadly(world: &World, faction: u32, level: u8, from: (f32, f32), to: (f32, f32)) -> bool {
    if worst_near(world, faction, to.0, to.1, DEST_DANGER_RADIUS_M, u8::MAX)
        >= level.saturating_add(DEST_DANGER_GAP)
    {
        return true;
    }
    let d = dist(from, to.0, to.1);
    let n = ((d / ROUTE_STEP_M) as usize).min(ROUTE_SAMPLES);
    (1..n).any(|i| {
        let k = i as f32 / n as f32;
        let (x, y) = (from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k);
        worst_near(world, faction, x, y, ROUTE_RADIUS_M, ROUTE_MAX_GRADE)
            >= level.saturating_add(ROUTE_DANGER_GAP)
    })
}

/// Metres a position may be off the navigation mesh to be placed on it.
const SNAP_M: f32 = 24.0;

/// Whether the navigation mesh rules out walking from `from` to `to` (a place across the sea, on
/// an island or behind a wall: the component tables answer without a search). `false` when the
/// answer is not known: no mesh, or one of the places is not near it.
pub fn cut_off(world: &World, from: (f32, f32), to: (f32, f32)) -> bool {
    let Some(mesh) = world.nav() else {
        return false;
    };
    let at = |p: (f32, f32)| {
        let z = world.ground_z(p.0, p.1).unwrap_or(0.0);
        mesh.locate(Vec3::new(p.0, p.1, z), SNAP_M)
    };
    match (at(from), at(to)) {
        (Some(a), Some(b)) => mesh.may_reach(a, b) == Some(false),
        _ => false,
    }
}

/// How much a quest of `quest_level` is worth to a bot of `own` level when it picks a zone: one
/// for a quest at its level, less for easy ones, almost nothing for trivial ones.
pub fn level_weight(own: u8, quest_level: u8) -> f32 {
    if quest_level.saturating_add(TRIVIAL_GAP) <= own {
        0.15
    } else if quest_level.saturating_add(3) <= own {
        0.6
    } else {
        1.0
    }
}

/// Whether a quest of `quest_level` is trivial for a bot of `own` level (and not part of a story
/// line, which is never trivial).
pub fn trivial(own: u8, quest_level: u8, story: bool) -> bool {
    !story && quest_level.saturating_add(TRIVIAL_GAP) <= own
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::quest_atlas::{Chain, Entry};

    fn entry(quest: u32, level: u8, story: bool, x: f32, y: f32, zone: u16) -> Entry {
        Entry {
            quest,
            level,
            story,
            spots: vec![(x, y, zone, 7, false)],
        }
    }

    fn all_ok(_: &AtlasPoint) -> bool {
        true
    }
    fn none_blocked(_: f32, _: f32) -> bool {
        false
    }
    fn safe(_: (f32, f32), _: (f32, f32)) -> bool {
        false
    }

    fn atlas(entries: Vec<Entry>) -> QuestAtlas {
        QuestAtlas::from_entries(
            entries,
            vec![Chain {
                race: 1,
                category: 3,
                quests: Box::new([1, 2, 3]),
            }],
        )
    }

    #[test]
    fn the_story_target_is_the_first_acceptable_quest_of_the_chain() {
        let a = atlas(vec![
            entry(1, 5, true, 100.0, 0.0, 1),
            entry(2, 10, true, 3000.0, 0.0, 2),
            entry(3, 12, true, 2500.0, 0.0, 2),
        ]);
        let chain = a.chain(1).unwrap().quests.clone();
        // quest 1 is done: the target is quest 2, not the nearer quest 3
        let not_1 = |p: &AtlasPoint| p.quest != 1;
        let t = story_target(&a, &chain, (0.0, 0.0), &not_1, &none_blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Story(2));
        assert_eq!((t.x, t.dist), (3000.0, 3000.0));
        // quest 1 is acceptable but within the picker's reach: it is skipped
        let t = story_target(&a, &chain, (0.0, 0.0), &all_ok, &none_blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Story(2));
        // quest 2 is not acceptable yet: quest 3 follows
        let not_12 = |p: &AtlasPoint| p.quest == 3;
        let t = story_target(&a, &chain, (0.0, 0.0), &not_12, &none_blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Story(3));
        // nothing acceptable, nothing to go to
        let nothing = |_: &AtlasPoint| false;
        assert!(story_target(&a, &chain, (0.0, 0.0), &nothing, &none_blocked, &safe).is_none());
    }

    #[test]
    fn blocked_and_risky_story_targets_are_passed_over() {
        let a = atlas(vec![
            entry(2, 10, true, 3000.0, 0.0, 2),
            entry(3, 12, true, 2500.0, 0.0, 2),
        ]);
        let chain = a.chain(1).unwrap().quests.clone();
        let blocked = |x: f32, _: f32| x == 3000.0;
        let t = story_target(&a, &chain, (0.0, 0.0), &all_ok, &blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Story(3));
        let risky = |_: (f32, f32), to: (f32, f32)| to.0 == 3000.0;
        let t = story_target(&a, &chain, (0.0, 0.0), &all_ok, &none_blocked, &risky).unwrap();
        assert_eq!(t.why, Why::Story(3));
    }

    #[test]
    fn the_nearest_giver_of_a_quest_wins() {
        let mut e = entry(2, 10, true, 4000.0, 0.0, 2);
        e.spots.push((2200.0, 0.0, 2, 8, false));
        let a = atlas(vec![e]);
        let chain = a.chain(1).unwrap().quests.clone();
        let t = story_target(&a, &chain, (0.0, 0.0), &all_ok, &none_blocked, &safe).unwrap();
        assert_eq!(t.x, 2200.0);
    }

    fn weigh(p: &AtlasPoint) -> f32 {
        level_weight(10, p.level)
    }

    #[test]
    fn the_zone_with_the_most_suited_quests_wins_and_distance_discounts_it() {
        // zone 5: five quests 3 km away; zone 6: three quests 10 km away; zone 7: two at 2 km
        let mut e = Vec::new();
        for i in 0..5 {
            e.push(entry(10 + i, 10, false, 3000.0 + i as f32 * 20.0, 0.0, 5));
        }
        for i in 0..3 {
            e.push(entry(20 + i, 10, false, 10000.0 + i as f32 * 20.0, 0.0, 6));
        }
        for i in 0..2 {
            e.push(entry(30 + i, 10, false, 0.0, 2000.0 + i as f32 * 20.0, 7));
        }
        let a = atlas(e);
        let t = zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Zone(5));
        assert_eq!(t.weight, 5.0);
        // zone 5's quests are done: the nearer, smaller zone beats the far one
        let weigh_no5 = |p: &AtlasPoint| if p.zone == 5 { 0.0 } else { weigh(p) };
        let t = zone_target(&a, (0.0, 0.0), &weigh_no5, &none_blocked, &safe).unwrap();
        assert_eq!(t.why, Why::Zone(7));
    }

    #[test]
    fn nothing_near_counts_and_trivial_quests_hardly_count() {
        // everything within the picker's reach: no trip
        let a = atlas(vec![
            entry(10, 10, false, 100.0, 0.0, 5),
            entry(11, 10, false, 200.0, 0.0, 5),
        ]);
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).is_none());
        // far, but trivial for level 10: not worth the walk
        let a = atlas(vec![
            entry(10, 2, false, 5000.0, 0.0, 5),
            entry(11, 3, false, 5020.0, 0.0, 5),
            entry(12, 4, false, 5040.0, 0.0, 5),
        ]);
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).is_none());
        // one quest at the bot's level is below the minimum, two make a trip
        let a = atlas(vec![entry(10, 10, false, 5000.0, 0.0, 5)]);
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).is_none());
        let a = atlas(vec![
            entry(10, 10, false, 5000.0, 0.0, 5),
            entry(11, 10, false, 5020.0, 0.0, 5),
        ]);
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).is_some());
    }

    #[test]
    fn the_hub_is_the_densest_cluster_and_blocked_places_do_not_count() {
        // a lone giver at 3 km, a village of four at 6 km in the same zone
        let mut e = vec![entry(10, 10, false, 3000.0, 0.0, 5)];
        for i in 0..4 {
            e.push(entry(20 + i, 10, false, 6000.0 + i as f32 * 50.0, 0.0, 5));
        }
        let a = atlas(e);
        let t = zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).unwrap();
        assert!(t.x >= 6000.0, "hub at {}", t.x);
        assert_eq!(t.weight, 5.0);
        // the village is resting after a failed trip: the lone giver is below the minimum
        let blocked = |x: f32, _: f32| x >= 6000.0;
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &blocked, &safe).is_none());
    }

    #[test]
    fn a_risky_hub_loses_to_the_next_zone() {
        let mut e = Vec::new();
        for i in 0..4 {
            e.push(entry(10 + i, 10, false, 3000.0 + i as f32 * 20.0, 0.0, 5));
        }
        for i in 0..3 {
            e.push(entry(20 + i, 10, false, 0.0, 4000.0 + i as f32 * 20.0, 6));
        }
        let a = atlas(e);
        let risky = |_: (f32, f32), to: (f32, f32)| to.0 > 1000.0;
        let t = zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &risky).unwrap();
        assert_eq!(t.why, Why::Zone(6));
        let all_risky = |_: (f32, f32), _: (f32, f32)| true;
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &all_risky).is_none());
    }

    #[test]
    fn a_quest_with_several_givers_counts_once() {
        let mut e = entry(10, 10, false, 3000.0, 0.0, 5);
        e.spots.push((3050.0, 0.0, 5, 8, false));
        e.spots.push((3100.0, 0.0, 5, 9, false));
        let a = atlas(vec![e]);
        // 1.0 is below the minimum of 1.5 however many givers there are
        assert!(zone_target(&a, (0.0, 0.0), &weigh, &none_blocked, &safe).is_none());
    }

    #[test]
    fn grinding_goes_to_the_nearest_zone_with_quests_ahead() {
        let mut e = Vec::new();
        // zone 5 (far): three quests at level 13; zone 6 (nearer): two at level 13; zone 7: level 30
        for i in 0..3 {
            e.push(entry(10 + i, 13, false, 8000.0 + i as f32 * 10.0, 0.0, 5));
        }
        for i in 0..2 {
            e.push(entry(20 + i, 13, false, 0.0, 3000.0, 6));
        }
        for i in 0..4 {
            e.push(entry(30 + i, 30, false, 0.0, 2000.0 + i as f32, 7));
        }
        let a = atlas(e);
        let spot = |hub: (f32, f32)| Some((hub.0 + 50.0, hub.1));
        let t = grind_target(&a, (0.0, 0.0), 10, &none_blocked, &safe, &spot).unwrap();
        assert_eq!(t.why, Why::Grind(5));
        assert_eq!(t.x, 8010.0 + 50.0);
        // no monsters near the hub: no trip
        let nowhere = |_: (f32, f32)| None;
        assert!(grind_target(&a, (0.0, 0.0), 10, &none_blocked, &safe, &nowhere).is_none());
        // a bot of level 28 has quests ahead only in zone 7
        let t = grind_target(&a, (0.0, 0.0), 27, &none_blocked, &safe, &spot).unwrap();
        assert_eq!(t.why, Why::Grind(7));
    }

    #[test]
    fn level_weights_and_trivial_quests() {
        assert_eq!(level_weight(10, 11), 1.0);
        assert_eq!(level_weight(10, 8), 1.0);
        assert_eq!(level_weight(10, 7), 0.6);
        assert_eq!(level_weight(10, 4), 0.15);
        assert!(trivial(10, 4, false));
        assert!(!trivial(10, 5, false));
        assert!(!trivial(10, 1, true), "a story quest is never trivial");
        assert!(!trivial(5, 1, false));
    }

    fn target(x: f32, why: Why, dist: f32) -> Dest {
        Dest {
            x,
            y: 0.0,
            why,
            dist,
            weight: 3.0,
        }
    }

    #[test]
    fn a_trip_has_a_bounded_time_limit() {
        let mut p = Progress::default();
        let near = p.begin(target(1.0, Why::Zone(1), 300.0), (0.0, 0.0), 1_000);
        assert_eq!(near.deadline_ms, 1_000 + TRIP_MIN_MS);
        let mid = p.begin(target(1.0, Why::Zone(1), 6000.0), (0.0, 0.0), 0);
        assert_eq!(mid.deadline_ms, 2_000 * 1000 + TRIP_SLACK_MS);
        let far = p.begin(target(1.0, Why::Zone(1), 100_000.0), (0.0, 0.0), 0);
        assert_eq!(far.deadline_ms, TRIP_MAX_MS);
        assert_eq!(p.started, 3);
    }

    #[test]
    fn a_failed_destination_rests_and_the_rest_doubles() {
        let mut p = Progress::default();
        let t = target(5000.0, Why::Zone(4), 3000.0);
        p.begin(t, (0.0, 0.0), 0);
        assert!(p.end(Outcome::TimedOut, 1_000).is_some());
        assert!(p.blocked(5000.0, 0.0, 1_001));
        assert!(p.blocked(5300.0, 0.0, 1_001), "near the failed place too");
        assert!(!p.blocked(6000.0, 0.0, 1_001));
        assert!(!p.blocked(5000.0, 0.0, 1_000 + FAILED_MS));
        // the second failure rests twice as long, the third four times, then it stays at that
        for (n, factor) in [(2u64, 2u64), (3, 4), (4, 4)] {
            let now = 10_000_000 * n;
            p.begin(t, (0.0, 0.0), now);
            p.end(Outcome::Blocked, now);
            assert!(p.blocked(5000.0, 0.0, now + FAILED_MS * factor - 1));
            assert!(!p.blocked(5000.0, 0.0, now + FAILED_MS * factor));
        }
        assert_eq!(p.done, 0);
        assert_eq!(p.history.len(), MAX_HISTORY);
    }

    #[test]
    fn a_good_trip_leaves_its_destination_alone_and_is_counted() {
        let mut p = Progress::default();
        let t = target(5000.0, Why::Story(77), 3000.0);
        p.begin(t, (10.0, 20.0), 0);
        p.end(Outcome::InRange, 500);
        assert!(!p.blocked(5000.0, 0.0, 501));
        assert_eq!(p.done, 1);
        let r = p.history.last().unwrap();
        assert_eq!((r.from, r.to, r.dist_m), ((10, 20), (5000, 0), 3000));
        assert_eq!(r.why, Why::Story(77));
        assert!(r.outcome.good());
        // nothing under way: ending again does nothing
        assert!(p.end(Outcome::Arrived, 600).is_none());
    }

    #[test]
    fn an_empty_destination_rests_when_the_arrival_scan_finds_nothing() {
        let mut p = Progress::default();
        let t = target(5000.0, Why::Zone(4), 3000.0);
        p.begin(t, (0.0, 0.0), 0);
        p.end(Outcome::Arrived, 10_000);
        assert!(p.barren(12_000));
        assert!(p.blocked(5000.0, 0.0, 12_001));
        // a scan that came too late says nothing
        p.begin(t, (0.0, 0.0), 1_000_000);
        p.end(Outcome::Arrived, 1_010_000);
        assert!(!p.barren(1_010_000 + BARREN_MS + 1));
        // a scan that found a quest clears the suspicion
        p.begin(t, (0.0, 0.0), 2_000_000);
        p.end(Outcome::Arrived, 2_010_000);
        p.fruitful();
        assert!(!p.barren(2_011_000));
        assert_eq!(p.empty_scans, 0);
    }

    #[test]
    fn two_deaths_end_the_trip() {
        let mut p = Progress::default();
        assert!(!p.died(0), "no trip, nothing to give up");
        p.begin(target(5000.0, Why::Zone(4), 3000.0), (0.0, 0.0), 0);
        assert!(!p.died(1_000));
        assert!(p.trip.is_some());
        assert!(p.died(2_000));
        assert!(p.trip.is_none());
        assert!(p.blocked(5000.0, 0.0, 2_001));
        assert_eq!(p.history.last().unwrap().outcome, Outcome::Died);
    }

    #[test]
    fn grinding_trips_settle_the_bot_and_a_search_in_vain_waits() {
        let mut p = Progress::default();
        p.begin(target(5000.0, Why::Grind(4), 3000.0), (0.0, 0.0), 0);
        p.end(Outcome::Arrived, 1_000);
        assert!(p.settled(1_001));
        assert!(!p.settled(1_000 + SETTLE_MS));
        assert!(!p.searched_in_vain(0));
        p.found_nothing(5_000);
        assert!(p.searched_in_vain(5_000 + NO_DEST_MS - 1));
        assert!(!p.searched_in_vain(5_000 + NO_DEST_MS));
    }

    #[test]
    fn failed_destinations_are_bounded() {
        let mut p = Progress::default();
        for i in 0..10 {
            let t = target(i as f32 * 1000.0, Why::Zone(1), 2000.0);
            p.begin(t, (0.0, 0.0), 0);
            p.end(Outcome::Blocked, 0);
        }
        assert!(p.blocked(9000.0, 0.0, 1));
        assert!(!p.blocked(0.0, 0.0, 1), "the oldest was forgotten");
        assert!(p.history.len() <= MAX_HISTORY);
    }

    #[test]
    fn outcomes_read_well() {
        assert!(Outcome::Arrived.good() && Outcome::InRange.good());
        assert!(!Outcome::TimedOut.good() && !Outcome::Blocked.good() && !Outcome::Died.good());
        assert_eq!(Outcome::TimedOut.text(), "timed out");
    }
}
