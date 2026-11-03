//! Natural movement: bots walk navmesh routes instead of straight lines.
//!
//! The movement controller ([`super::motion::Motion`]) keeps its straight-line walker and hands
//! every trip of a bot that has a [`NavLink`] to a [`NavState`], which
//!
//! 1. plans the trip on the mesh of the world the bot is in (`aa_world::nav`): short trips on the
//!    bot's own task, long ones on the [`Planner`] pool (one or two threads, a bounded queue) with
//!    the answer delivered as `BotCmd::PathReady`;
//! 2. walks the route ([`follow::Route`]) with rounded corners, a stable lateral habit and a pace
//!    that slows over the last metres;
//! 3. falls back to the straight line when the mesh cannot help (the goal is unreachable or off
//!    the mesh, the planner is busy or late, the bot is stuck), and tells the numbers to
//!    [`NavMetrics`].
//!
//! `[behavior] navmesh = false` keeps the straight-line walker for every trip (trips and stuck
//! events are still counted, so the two can be compared).
//!
//! | File | What |
//! | --- | --- |
//! | `follow.rs` | the geometry of a route: rounding, lateral offset, decimation for the map |
//! | `trip.rs` | [`NavState`]: planning, continuations, replans, the walk |
//! | `planner.rs` | [`plan`] (one search), [`Planner`] (the pool), [`NavWorld`] |
//! | `metrics.rs` | [`NavMetrics`]: the counters and latency histograms behind `HubStats::nav` |

pub mod follow;
pub mod metrics;
pub mod planner;
pub mod trip;

#[cfg(test)]
pub(crate) mod testmesh;
#[cfg(test)]
mod walk_tests;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::config::Config;
use crate::hub::{BotSlot, Hub};

pub use metrics::{Fallback, NavMetrics, TripEnd};
pub use planner::{plan, Job, NavWorld, PlanSpec, Planner};
pub use trip::{NavLink, NavState, NavStep, StepIn};

/// How often the hub logs the navigation summary.
const LOG_EVERY: Duration = Duration::from_secs(60);

/// The process-wide navigation service the hub owns: the counters, and the planner pool, which
/// starts with the first bot that plans.
pub struct NavService {
    metrics: Arc<NavMetrics>,
    planner: OnceLock<Arc<Planner>>,
    enabled: std::sync::atomic::AtomicBool,
    last_log: Mutex<Instant>,
}

impl Default for NavService {
    fn default() -> Self {
        NavService {
            metrics: Arc::new(NavMetrics::default()),
            planner: OnceLock::new(),
            enabled: std::sync::atomic::AtomicBool::new(false),
            last_log: Mutex::new(Instant::now()),
        }
    }
}

impl NavService {
    /// The counters.
    pub fn metrics(&self) -> &Arc<NavMetrics> {
        &self.metrics
    }

    /// The pool, started on the first call.
    pub fn planner(&self) -> Arc<Planner> {
        self.planner
            .get_or_init(|| Arc::new(Planner::start_default(self.metrics.clone())))
            .clone()
    }

    /// Records whether path finding is on (a bot with a link says so when it starts).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// The numbers for the web UI.
    pub fn stats(&self) -> pb_web::NavStats {
        self.metrics
            .snapshot(self.enabled.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Logs the summary once a minute while there is anything to report. Call about once a
    /// second.
    pub fn tick(&self) {
        let mut last = self.last_log.lock();
        if last.elapsed() < LOG_EVERY {
            return;
        }
        *last = Instant::now();
        drop(last);
        let m = &self.metrics;
        let used = m.trips.load(std::sync::atomic::Ordering::Relaxed)
            + m.queries_inline.load(std::sync::atomic::Ordering::Relaxed);
        if used > 0 {
            tracing::info!("{}", m.summary());
        }
    }
}

/// The link a bot's motion controller plans with: the world, the hub's planner pool and counters.
/// Path finding is on when `behavior.navmesh` says so and the main world has a mesh.
pub fn link_for_bot(
    hub: &Hub,
    world: &Arc<aa_world::World>,
    cfg: &Config,
    reply: Arc<BotSlot>,
    seed: u32,
) -> NavLink {
    let enabled = cfg.behavior.navmesh && world.nav().is_some();
    hub.nav.set_enabled(enabled);
    NavLink {
        enabled,
        world: world.clone(),
        planner: enabled.then(|| hub.nav.planner()),
        reply,
        metrics: hub.nav.metrics().clone(),
        main: cfg.data.world.as_str().into(),
        seed,
    }
}
