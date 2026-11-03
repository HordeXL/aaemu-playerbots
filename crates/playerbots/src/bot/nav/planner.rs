//! Path searches: the one function both the bot task (short trips) and the planner threads (long
//! ones) call, and the planner pool itself.
//!
//! The pool is a bounded queue and one or two plain threads, so hundreds of bots that start long
//! trips at the same moment cannot take the CPU away from the bot tasks: a request that does not fit
//! in the queue is refused at once and that trip walks the straight line. A finished search is
//! delivered to the bot as `BotCmd::PathReady` through its command channel.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use aa_world::geom::Vec3;
use aa_world::nav::{NavMesh, NodeRef, PathQuery, PathResult};
use parking_lot::{Condvar, Mutex};

use super::metrics::NavMetrics;
use crate::bot::cmd::BotCmd;
use crate::hub::BotSlot;

/// A start or goal this far (metres) from the mesh is snapped onto it on the second try: an NPC on
/// a porch, a bot standing on a spot the mesh does not cover.
pub const SNAP_RETRY_M: f32 = 24.0;
/// Expansion cap of a search the bot task runs itself. Short trips need hundreds of expansions; a
/// goal behind a wall can need many more, and that must not hold up the task.
pub const INLINE_MAX_EXPANSIONS: u32 = 12_000;
/// Expansion cap of a pooled search (the library default), doubled for every continuation.
pub const POOL_MAX_EXPANSIONS: u32 = 60_000;
/// Most requests the pool holds.
pub const QUEUE_CAPACITY: usize = 512;

/// What the planner needs to know about the world.
pub trait NavWorld: Send + Sync + 'static {
    /// The mesh of world `name` (the main world's, or an instance world's).
    fn mesh(&self, name: &str) -> Option<Arc<NavMesh>>;
    /// Terrain height in the main world.
    fn ground(&self, x: f32, y: f32) -> Option<f32>;
}

impl NavWorld for aa_world::World {
    fn mesh(&self, name: &str) -> Option<Arc<NavMesh>> {
        self.nav_of(name)
    }

    fn ground(&self, x: f32, y: f32) -> Option<f32> {
        self.ground_z(x, y)
    }
}

/// One search.
#[derive(Debug, Clone)]
pub struct PlanSpec {
    pub from: [f32; 3],
    pub to: [f32; 3],
    /// Expansion cap.
    pub max_expansions: u32,
    /// Extra cost of entering nodes (the spot a bot got stuck on).
    pub penalties: Vec<(NodeRef, f32)>,
    /// Path corners on triangles take the terrain height (the main world; an instance has none).
    pub terrain: bool,
}

/// Runs the search; `height` is the terrain (only used when `spec.terrain`). A start or goal off
/// the mesh is snapped from further away on a second try.
pub fn plan(
    mesh: &NavMesh,
    spec: &PlanSpec,
    height: &dyn Fn(f32, f32) -> Option<f32>,
) -> PathResult {
    let mut q = PathQuery::new(
        Vec3::new(spec.from[0], spec.from[1], spec.from[2]),
        Vec3::new(spec.to[0], spec.to[1], spec.to[2]),
    );
    q.max_expansions = spec.max_expansions;
    q.penalties = &spec.penalties;
    if spec.terrain {
        q.height = Some(height);
    }
    let r = mesh.find_path(&q);
    if matches!(r, PathResult::NoMesh) {
        q.snap_dist = SNAP_RETRY_M;
        return mesh.find_path(&q);
    }
    r
}

/// A request to the pool.
pub struct Job {
    /// Echoed in the answer; the bot drops answers for tickets it no longer waits for.
    pub ticket: u32,
    pub mesh: Arc<NavMesh>,
    pub world: Arc<dyn NavWorld>,
    pub spec: PlanSpec,
    /// Where the answer goes.
    pub reply: Arc<BotSlot>,
    pub submitted: Instant,
}

struct Shared {
    queue: Mutex<VecDeque<Job>>,
    wake: Condvar,
    stop: AtomicBool,
    capacity: usize,
    metrics: Arc<NavMetrics>,
}

/// The planner pool. Dropping it stops the threads (requests still queued are dropped).
pub struct Planner {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Planner {
    /// Starts `threads` planner threads (at least one) with room for `capacity` queued requests.
    pub fn start(threads: usize, capacity: usize, metrics: Arc<NavMetrics>) -> Planner {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            capacity,
            metrics,
        });
        let threads = (0..threads.max(1))
            .filter_map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("pb-nav-{i}"))
                    .spawn(move || worker(&shared))
                    .map_err(|e| tracing::warn!("starting a planner thread: {e}"))
                    .ok()
            })
            .collect();
        Planner { shared, threads }
    }

    /// The default pool: two threads on a machine with four or more cores, one otherwise.
    pub fn start_default(metrics: Arc<NavMetrics>) -> Planner {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Planner::start(if cores >= 4 { 2 } else { 1 }, QUEUE_CAPACITY, metrics)
    }

    /// Queues a request; the job comes back when the queue is full.
    pub fn submit(&self, job: Job) -> Result<(), Box<Job>> {
        let mut q = self.shared.queue.lock();
        if q.len() >= self.shared.capacity || self.shared.stop.load(Ordering::Relaxed) {
            return Err(Box::new(job));
        }
        q.push_back(job);
        self.shared.metrics.queued(q.len() as u32);
        drop(q);
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Requests waiting for a thread.
    pub fn depth(&self) -> usize {
        self.shared.queue.lock().len()
    }

    /// Number of planner threads that run.
    pub fn threads(&self) -> usize {
        self.threads.len()
    }
}

impl Drop for Planner {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker(shared: &Shared) {
    loop {
        let job = {
            let mut q = shared.queue.lock();
            loop {
                if shared.stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(j) = q.pop_front() {
                    shared.metrics.queued(q.len() as u32);
                    break j;
                }
                shared.wake.wait(&mut q);
            }
        };
        run(shared, job);
    }
}

fn run(shared: &Shared, job: Job) {
    let started = Instant::now();
    let world = &job.world;
    let height = |x: f32, y: f32| world.ground(x, y);
    let result = catch_unwind(AssertUnwindSafe(|| plan(&job.mesh, &job.spec, &height)))
        .unwrap_or_else(|_| {
            shared
                .metrics
                .planner_panics
                .fetch_add(1, Ordering::Relaxed);
            PathResult::NoMesh
        });
    let m = &shared.metrics;
    m.record_query(&result, true, started.elapsed().as_micros() as u64);
    m.wait.record(job.submitted.elapsed().as_micros() as u64);
    let cmd = BotCmd::PathReady {
        ticket: job.ticket,
        result: Box::new(result),
    };
    if job.reply.send(cmd).is_err() {
        m.delivery_failed.fetch_add(1, Ordering::Relaxed);
    }
}
