//! Process-wide counters of the navigation motion: how many searches ran where and how they
//! ended, how long they took, how many trips arrived. All atomics, so the bot tasks and the
//! planner threads add to them without a lock; [`NavMetrics::snapshot`] turns them into the
//! numbers the web UI shows (`pb_web::NavStats`).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use aa_world::nav::PathResult;
use pb_web::NavStats;

/// Sub-buckets per octave of the latency histogram.
const SUB_BITS: u32 = 2;
/// Values below this (microseconds) have a bucket each.
const LINEAR: u64 = 1 << (SUB_BITS + 2);
/// Buckets: [`LINEAR`] exact ones, then four per octave up to 2^32 microseconds (71 minutes).
const BUCKETS: usize = LINEAR as usize + (32 - 4) * (1 << SUB_BITS);

/// A latency histogram: four buckets per octave (about 20 % resolution), 120 counters.
#[derive(Debug)]
pub struct Hist {
    counts: [AtomicU64; BUCKETS],
}

impl Default for Hist {
    fn default() -> Self {
        Hist {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

fn bucket_of(us: u64) -> usize {
    let us = us.min(u64::from(u32::MAX));
    if us < LINEAR {
        return us as usize;
    }
    let e = 63 - us.leading_zeros(); // at least 4
    let sub = ((us >> (e - SUB_BITS)) & ((1 << SUB_BITS) - 1)) as usize;
    LINEAR as usize + (e as usize - 4) * (1 << SUB_BITS) + sub
}

/// The smallest value that lands in bucket `i`.
fn bucket_low(i: usize) -> u64 {
    if (i as u64) < LINEAR {
        return i as u64;
    }
    let k = i - LINEAR as usize;
    let e = (k >> SUB_BITS) as u32 + 4;
    let sub = (k & ((1 << SUB_BITS) - 1)) as u64;
    ((1 << SUB_BITS) + sub) << (e - SUB_BITS)
}

impl Hist {
    /// Adds one measurement (microseconds).
    pub fn record(&self, us: u64) {
        self.counts[bucket_of(us)].fetch_add(1, Relaxed);
    }

    /// Number of measurements.
    pub fn count(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Relaxed)).sum()
    }

    /// The value below which a fraction `q` of the measurements lie (the middle of its bucket),
    /// 0 without measurements.
    pub fn quantile(&self, q: f64) -> u64 {
        let counts: Vec<u64> = self.counts.iter().map(|c| c.load(Relaxed)).collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return 0;
        }
        let want = ((total as f64) * q.clamp(0.0, 1.0)).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, c) in counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                let (lo, hi) = (bucket_low(i), bucket_low(i + 1));
                return if hi > lo + 1 { (lo + hi) / 2 } else { lo };
            }
        }
        0
    }
}

/// How a trip ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripEnd {
    /// The bot got there.
    Arrived,
    /// The motion controller gave up (stuck, time limit).
    Failed,
    /// The caller stopped it, replaced it, or the server moved the bot.
    Cancelled,
}

/// Why a trip walked a straight line instead of a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Navigation is switched off, or there is no mesh for the world the bot is in.
    NoMeshForWorld,
    /// The search proved the goal unreachable.
    Unreachable,
    /// The start or the goal is not on the mesh.
    OffMesh,
    /// The planner queue was full.
    QueueFull,
    /// The planner did not answer in time.
    Timeout,
    /// A partial route made no progress, or the replan budget was used up.
    NoProgress,
    /// The route ended short of the goal and nothing continued it.
    RouteEnded,
    /// The bot got stuck on the route and replanning did not help.
    Stuck,
}

/// The counters. One per process, shared through the hub.
#[derive(Debug, Default)]
pub struct NavMetrics {
    pub queries_inline: AtomicU64,
    pub queries_pooled: AtomicU64,
    pub found: AtomicU64,
    pub partial: AtomicU64,
    pub unreachable: AtomicU64,
    pub no_mesh: AtomicU64,
    pub replans: AtomicU64,
    pub stuck_replans: AtomicU64,
    pub stuck_events: AtomicU64,
    pub legacy_fallbacks: AtomicU64,
    pub plan_timeouts: AtomicU64,
    pub queue_full: AtomicU64,
    pub stale_results: AtomicU64,
    pub delivery_failed: AtomicU64,
    pub planner_panics: AtomicU64,
    pub queue_depth: AtomicU32,
    pub queue_max: AtomicU32,
    pub trips: AtomicU64,
    pub trips_arrived: AtomicU64,
    pub trips_failed: AtomicU64,
    pub trips_cancelled: AtomicU64,
    /// Millimetres walked / straight distance of arrived trips.
    pub walked_mm: AtomicU64,
    pub straight_mm: AtomicU64,
    /// How long a search took.
    pub compute: Hist,
    /// Pooled requests: from the submit to the end of the search.
    pub wait: Hist,
}

impl NavMetrics {
    /// A finished search.
    pub fn record_query(&self, result: &PathResult, pooled: bool, compute_us: u64) {
        let field = if pooled {
            &self.queries_pooled
        } else {
            &self.queries_inline
        };
        field.fetch_add(1, Relaxed);
        self.compute.record(compute_us);
        match result {
            PathResult::Found(_) => self.found.fetch_add(1, Relaxed),
            PathResult::Partial(_) => self.partial.fetch_add(1, Relaxed),
            PathResult::Unreachable => self.unreachable.fetch_add(1, Relaxed),
            PathResult::NoMesh => self.no_mesh.fetch_add(1, Relaxed),
        };
    }

    /// A request entered the planner queue, which now holds `depth` requests.
    pub fn queued(&self, depth: u32) {
        self.queue_depth.store(depth, Relaxed);
        self.queue_max.fetch_max(depth, Relaxed);
    }

    /// A trip fell back to the straight line.
    pub fn fell_back(&self, why: Fallback) {
        self.legacy_fallbacks.fetch_add(1, Relaxed);
        match why {
            Fallback::QueueFull => self.queue_full.fetch_add(1, Relaxed),
            Fallback::Timeout => self.plan_timeouts.fetch_add(1, Relaxed),
            _ => 0,
        };
    }

    /// A trip of 15 m or more ended.
    pub fn trip_ended(&self, how: TripEnd, walked_m: f32, straight_m: f32) {
        match how {
            TripEnd::Arrived => {
                self.trips_arrived.fetch_add(1, Relaxed);
                self.walked_mm
                    .fetch_add((walked_m * 1000.0) as u64, Relaxed);
                self.straight_mm
                    .fetch_add((straight_m * 1000.0) as u64, Relaxed);
            }
            TripEnd::Failed => {
                self.trips_failed.fetch_add(1, Relaxed);
            }
            TripEnd::Cancelled => {
                self.trips_cancelled.fetch_add(1, Relaxed);
            }
        }
    }

    /// The numbers for the web UI.
    pub fn snapshot(&self, enabled: bool) -> NavStats {
        let walked = self.walked_mm.load(Relaxed) as f64;
        let straight = self.straight_mm.load(Relaxed) as f64;
        NavStats {
            enabled,
            queries_inline: self.queries_inline.load(Relaxed),
            queries_pooled: self.queries_pooled.load(Relaxed),
            found: self.found.load(Relaxed),
            partial: self.partial.load(Relaxed),
            unreachable: self.unreachable.load(Relaxed),
            no_mesh: self.no_mesh.load(Relaxed),
            replans: self.replans.load(Relaxed),
            stuck_replans: self.stuck_replans.load(Relaxed),
            stuck_events: self.stuck_events.load(Relaxed),
            legacy_fallbacks: self.legacy_fallbacks.load(Relaxed),
            plan_timeouts: self.plan_timeouts.load(Relaxed),
            queue_full: self.queue_full.load(Relaxed),
            stale_results: self.stale_results.load(Relaxed),
            compute_p50_us: self.compute.quantile(0.5),
            compute_p99_us: self.compute.quantile(0.99),
            wait_p50_us: self.wait.quantile(0.5),
            wait_p99_us: self.wait.quantile(0.99),
            queue_depth: self.queue_depth.load(Relaxed),
            queue_max: self.queue_max.load(Relaxed),
            trips: self.trips.load(Relaxed),
            trips_arrived: self.trips_arrived.load(Relaxed),
            trips_failed: self.trips_failed.load(Relaxed),
            trips_cancelled: self.trips_cancelled.load(Relaxed),
            detour_ratio: if straight > 0.0 {
                (walked / straight) as f32
            } else {
                0.0
            },
        }
    }

    /// One log line with the figures that matter.
    pub fn summary(&self) -> String {
        let s = self.snapshot(true);
        format!(
            "nav: {} inline + {} pooled searches (found {}, partial {}, unreachable {}, off mesh {}), \
             compute p50 {} us p99 {} us, pooled wait p50 {} us p99 {} us, queue {} (max {}), \
             replans {} + {} stuck, stuck events {}, straight-line fallbacks {} (timeouts {}, queue full {}), \
             trips {} (arrived {}, failed {}, cancelled {}), detour {:.2}",
            s.queries_inline,
            s.queries_pooled,
            s.found,
            s.partial,
            s.unreachable,
            s.no_mesh,
            s.compute_p50_us,
            s.compute_p99_us,
            s.wait_p50_us,
            s.wait_p99_us,
            s.queue_depth,
            s.queue_max,
            s.replans,
            s.stuck_replans,
            s.stuck_events,
            s.legacy_fallbacks,
            s.plan_timeouts,
            s.queue_full,
            s.trips,
            s.trips_arrived,
            s.trips_failed,
            s.trips_cancelled,
            s.detour_ratio,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotone_and_contain_their_values() {
        let mut prev = 0;
        for us in (0..40_000u64).chain([1 << 20, 1 << 30, u64::from(u32::MAX), u64::MAX]) {
            let i = bucket_of(us);
            assert!(i < BUCKETS, "{us} -> {i}");
            assert!(i >= prev || us > u64::from(u32::MAX), "{us}");
            prev = i;
            let capped = us.min(u64::from(u32::MAX));
            assert!(bucket_low(i) <= capped, "{us} below its bucket {i}");
            assert!(bucket_low(i + 1) > capped || i + 1 == BUCKETS, "{us}");
        }
    }

    #[test]
    fn quantiles_of_a_known_distribution() {
        let h = Hist::default();
        assert_eq!(h.quantile(0.5), 0);
        for us in 1..=100u64 {
            h.record(us * 10); // 10 .. 1000 us
        }
        assert_eq!(h.count(), 100);
        let (p50, p99) = (h.quantile(0.5), h.quantile(0.99));
        assert!((420..=600).contains(&p50), "p50 {p50}");
        assert!((900..=1100).contains(&p99), "p99 {p99}");
        // one slow outlier moves p99 only past the 99th percentile
        let h = Hist::default();
        for _ in 0..99 {
            h.record(100);
        }
        h.record(300_000);
        assert!(h.quantile(0.99) < 200);
        assert!(h.quantile(1.0) > 200_000);
    }

    #[test]
    fn counters_roll_up_into_the_snapshot() {
        let m = NavMetrics::default();
        m.record_query(&PathResult::Unreachable, false, 33);
        m.record_query(&PathResult::NoMesh, true, 50);
        m.queued(3);
        m.queued(1);
        m.fell_back(Fallback::Timeout);
        m.fell_back(Fallback::QueueFull);
        m.trips.fetch_add(3, Relaxed);
        m.trip_ended(TripEnd::Arrived, 110.0, 100.0);
        m.trip_ended(TripEnd::Arrived, 210.0, 200.0);
        m.trip_ended(TripEnd::Failed, 5.0, 100.0);
        let s = m.snapshot(true);
        assert_eq!((s.queries_inline, s.queries_pooled), (1, 1));
        assert_eq!((s.unreachable, s.no_mesh), (1, 1));
        assert_eq!((s.queue_depth, s.queue_max), (1, 3));
        assert_eq!(
            (s.legacy_fallbacks, s.plan_timeouts, s.queue_full),
            (2, 1, 1)
        );
        assert_eq!((s.trips, s.trips_arrived, s.trips_failed), (3, 2, 1));
        assert!(
            (s.detour_ratio - 320.0 / 300.0).abs() < 1e-4,
            "{}",
            s.detour_ratio
        );
        assert!(m.summary().contains("2 straight-line") || m.summary().contains("fallbacks 2"));
    }
}
