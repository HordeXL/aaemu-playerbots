//! The supervisor: starts the bot tasks (one per roster entry, spread over time), starts and stops
//! bots on request of the web UI, and shuts everything down gracefully.
//!
//! Bots are started `bots.ramp_ms` apart so the login server is not hit by hundreds of logins at
//! once. A despawned bot logs out and stays in the roster (and as an offline entry in the UI), so
//! a later `Spawn` brings the same character back.
//!
//! The bookkeeping lives in [`Book`], which knows nothing about tokio: a task that was told to
//! stop stays in the book as *stopping* until it has finished. A stopping bot is free for the
//! next `Spawn` (it is started again as soon as its old task is gone, so two tasks never share a
//! slot), is never picked by a second `Despawn`, and never makes the roster grow.
//!
//! With `[presence] enabled` the supervisor starts nobody at launch: the presence schedules (see
//! [`crate::presence`]) ask for the bots by id ([`SupCmd::StartIds`], [`SupCmd::StopIds`]), the
//! supervisor queues them for the ramp, stops only those that are safe to stop and tells presence
//! what it did. The bots the operator stops by hand are reported to presence as well, so it does
//! not start them again at once.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use futures::FutureExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::archetype::Archetypes;
use crate::bot::{initial_snapshot, run_bot, Shared};
use crate::config::{Config, MAX_BOTS};
use crate::hub::{unix_ms, SupCmd};
use crate::population::select::{pick_stops, Candidate, Selector, StopRule};
use crate::roster::{RosterSpec, SharedRoster};

/// How long shutdown waits for the bots to log out before it aborts their tasks.
pub const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// How often finished bot tasks are cleaned up.
const REAP_EVERY: Duration = Duration::from_secs(5);

/// A running bot task.
struct Running {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// Makes sure the roster has at least `count` bots; returns how many were added. New bots get a
/// race and gender `creatable` accepts among `cfg.bots.races`.
pub fn ensure_roster(
    roster: &SharedRoster,
    cfg: &Config,
    archetypes: &Archetypes,
    creatable: &dyn Fn(u8, u8) -> bool,
    count: usize,
) -> anyhow::Result<usize> {
    ensure_roster_races(
        roster,
        cfg,
        archetypes,
        creatable,
        count,
        &cfg.bots.race_ids(),
    )
}

/// [`ensure_roster`] with the new bots' races limited to `races` (a filtered spawn).
pub fn ensure_roster_races(
    roster: &SharedRoster,
    cfg: &Config,
    archetypes: &Archetypes,
    creatable: &dyn Fn(u8, u8) -> bool,
    count: usize,
    races: &[u8],
) -> anyhow::Result<usize> {
    let spec = RosterSpec {
        account_prefix: &cfg.server.account_prefix,
        races,
        creatable,
        archetypes: archetypes.all(),
    };
    let mut rng = fastrand::Rng::new();
    roster
        .update(|r| match r.ensure(count, &spec, &mut rng) {
            Ok(added) => (Ok(added), added > 0),
            Err(e) => (Err(e), false),
        })
        .context("growing the roster")
}

/// Registers an offline hub slot for every roster entry, so the web UI lists all bots from the
/// start. Returns how many entries were registered.
pub fn register_slots(shared: &Shared) -> usize {
    let entries: Vec<_> = shared.roster.read(|r| r.entries().to_vec());
    let mut n = 0;
    for e in &entries {
        let Some(arch) = shared.archetypes.get(&e.archetype) else {
            tracing::warn!(bot = e.id, archetype = %e.archetype, "unknown archetype in the roster");
            continue;
        };
        let slot = shared.hub.register(initial_snapshot(e, arch));
        if let Some(c) = e.char_id {
            slot.char_id.store(c, std::sync::atomic::Ordering::Relaxed);
            shared.hub.mark_bot_char(c);
        }
        n += 1;
    }
    n
}

// ---- the bookkeeping -------------------------------------------------------------------------

/// What [`Book`] needs to know about a bot task.
pub trait Task {
    /// Tells the task to log out and end.
    fn cancel(&self);
    /// Whether the task has ended.
    fn finished(&self) -> bool;
}

impl Task for Running {
    fn cancel(&self) {
        self.cancel.cancel();
    }

    fn finished(&self) -> bool {
        self.handle.is_finished()
    }
}

struct Entry<T> {
    task: T,
    /// Told to stop; the task may still be logging out.
    stopping: bool,
}

/// What [`Book::start`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// A new task was started.
    Started,
    /// The bot is running already.
    AlreadyRunning,
    /// The bot's old task is still logging out: try again later (it was queued again).
    Waiting,
}

/// Which bots run, which are stopping and which wait for their start. See the module docs.
pub struct Book<T> {
    running: BTreeMap<u32, Entry<T>>,
    pending: VecDeque<u32>,
}

impl<T: Task> Default for Book<T> {
    fn default() -> Self {
        Book {
            running: BTreeMap::new(),
            pending: VecDeque::new(),
        }
    }
}

impl<T: Task> Book<T> {
    /// Number of bot tasks (stopping ones included).
    pub fn tasks(&self) -> usize {
        self.running.len()
    }

    /// Number of bots waiting for their start.
    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    /// Bots that run and were not told to stop.
    pub fn active(&self) -> usize {
        self.running.values().filter(|e| !e.stopping).count()
    }

    /// Whether bot `id` has a task that was told to stop and is not gone yet.
    pub fn is_stopping(&self, id: u32) -> bool {
        self.running.get(&id).is_some_and(|e| e.stopping)
    }

    /// The ids to start: the lowest `n` ids of `all` that do not run (or are stopping) and do not
    /// wait for their start already.
    pub fn pick_idle(&self, all: &[u32], n: usize) -> Vec<u32> {
        all.iter()
            .copied()
            .filter(|id| !self.running.get(id).is_some_and(|e| !e.stopping))
            .filter(|id| !self.pending.contains(id))
            .take(n)
            .collect()
    }

    /// The ids to stop: the highest `n` bots that run and were not told to stop yet.
    pub fn pick_despawn(&self, n: usize) -> Vec<u32> {
        self.running
            .iter()
            .rev()
            .filter(|(_, e)| !e.stopping)
            .map(|(id, _)| *id)
            .take(n)
            .collect()
    }

    /// Queues `n` bots for a start. `ids` lists the roster; `grow(k)` is asked to add `k` roster
    /// entries (never more than the roster has room for, see [`MAX_BOTS`]) when fewer than `n`
    /// bots are free, and `ids` is read again afterwards. Returns how many were queued.
    pub fn spawn(
        &mut self,
        n: usize,
        ids: &mut dyn FnMut() -> Vec<u32>,
        grow: &mut dyn FnMut(usize),
    ) -> usize {
        let total = ids().len();
        self.spawn_matching(n, total, ids, grow).len()
    }

    /// [`Book::spawn`] for a subset of the roster: `ids` lists only the entries that may be
    /// started (the ones a filter matches), `total` is the size of the whole roster (which the
    /// [`MAX_BOTS`] limit applies to) and `grow(k)` must add `k` entries that match. Returns the
    /// ids that were queued.
    pub fn spawn_matching(
        &mut self,
        n: usize,
        total: usize,
        ids: &mut dyn FnMut() -> Vec<u32>,
        grow: &mut dyn FnMut(usize),
    ) -> Vec<u32> {
        let all = ids();
        let free = self.pick_idle(&all, n).len();
        let missing = n.saturating_sub(free);
        let room = (MAX_BOTS as usize).saturating_sub(total);
        let add = missing.min(room);
        let picked = if add > 0 {
            grow(add);
            self.pick_idle(&ids(), n)
        } else {
            self.pick_idle(&all, n)
        };
        self.pending.extend(picked.iter().copied());
        picked
    }

    /// The bots that run and were not told to stop, lowest id first.
    pub fn live_ids(&self) -> Vec<u32> {
        self.running
            .iter()
            .filter(|(_, e)| !e.stopping)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The bots waiting for their start, in queue order.
    pub fn pending_ids(&self) -> Vec<u32> {
        self.pending.iter().copied().collect()
    }

    /// Takes `ids` out of the start queue; returns how many were in it.
    pub fn unqueue(&mut self, ids: &[u32]) -> usize {
        let before = self.pending.len();
        self.pending.retain(|id| !ids.contains(id));
        before - self.pending.len()
    }

    /// Tells the running bots among `ids` to stop; returns how many were told.
    pub fn stop(&mut self, ids: &[u32]) -> usize {
        let mut n = 0;
        for id in ids {
            if let Some(e) = self.running.get_mut(id).filter(|e| !e.stopping) {
                e.stopping = true;
                e.task.cancel();
                n += 1;
            }
        }
        n
    }

    /// Tells the `n` highest running bots to stop; bots that never started leave the queue first.
    /// Returns how many bots were dropped from the queue and how many tasks were told to stop.
    pub fn despawn(&mut self, n: usize) -> (usize, usize) {
        let (unqueued, stopped) = self.despawn_ids(n);
        (unqueued.len(), stopped.len())
    }

    /// [`Book::despawn`], returning the ids dropped from the queue and the ids told to stop.
    pub fn despawn_ids(&mut self, n: usize) -> (Vec<u32>, Vec<u32>) {
        let mut unqueued = Vec::new();
        while unqueued.len() < n {
            let Some(id) = self.pending.pop_back() else {
                break;
            };
            unqueued.push(id);
        }
        let ids = self.pick_despawn(n - unqueued.len());
        for id in &ids {
            if let Some(e) = self.running.get_mut(id) {
                e.stopping = true;
                e.task.cancel();
            }
        }
        (unqueued, ids)
    }

    /// Queues those of `ids` that do not run (or are stopping) and do not wait for their start
    /// already, in the order given; returns them.
    pub fn queue_ids(&mut self, ids: &[u32]) -> Vec<u32> {
        let mut picked: Vec<u32> = Vec::with_capacity(ids.len());
        for &id in ids {
            if !picked.contains(&id) && !self.pick_idle(&[id], 1).is_empty() {
                picked.push(id);
            }
        }
        self.pending.extend(picked.iter().copied());
        picked
    }

    /// The next bot to start (queue order), or `None`.
    pub fn next_pending(&mut self) -> Option<u32> {
        self.pending.pop_front()
    }

    /// Starts bot `id` with `make` unless it runs. A bot whose old task is still logging out is
    /// put back at the end of the queue.
    pub fn start(&mut self, id: u32, make: impl FnOnce() -> T) -> Start {
        match self.running.get(&id) {
            Some(e) if e.stopping => {
                self.pending.push_back(id);
                Start::Waiting
            }
            Some(_) => Start::AlreadyRunning,
            None => {
                self.running.insert(
                    id,
                    Entry {
                        task: make(),
                        stopping: false,
                    },
                );
                Start::Started
            }
        }
    }

    /// Removes the tasks that have ended and hands them back (with their bot id), so the caller
    /// can look at how they ended.
    pub fn reap(&mut self) -> Vec<(u32, T)> {
        let done: Vec<u32> = self
            .running
            .iter()
            .filter(|(_, e)| e.task.finished())
            .map(|(id, _)| *id)
            .collect();
        done.into_iter()
            .filter_map(|id| self.running.remove(&id).map(|e| (id, e.task)))
            .collect()
    }

    /// Tells every task to stop and hands them all back.
    pub fn take_all(&mut self) -> Vec<(u32, T)> {
        for e in self.running.values() {
            e.task.cancel();
        }
        self.pending.clear();
        std::mem::take(&mut self.running)
            .into_iter()
            .map(|(id, e)| (id, e.task))
            .collect()
    }
}

/// What the panic of a bot task said.
fn panic_text(e: tokio::task::JoinError) -> String {
    match e.try_into_panic() {
        Ok(p) => p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic without a message".to_string()),
        Err(e) => e.to_string(),
    }
}

// ---- the supervisor --------------------------------------------------------------------------

/// Owns the bot tasks.
pub struct Supervisor {
    shared: Arc<Shared>,
    book: Book<Running>,
}

impl Supervisor {
    /// A supervisor with no bots running. Hands the presence schedules the roster and the
    /// configuration.
    pub fn new(shared: Arc<Shared>) -> Supervisor {
        shared
            .hub
            .presence
            .attach(&shared.cfg, shared.roster.clone());
        Supervisor {
            shared,
            book: Book::default(),
        }
    }

    /// Number of running bot tasks (stopping ones included).
    pub fn running(&self) -> usize {
        self.book.tasks()
    }

    fn start_bot(&mut self, id: u32, parent: &CancellationToken) {
        let shared = self.shared.clone();
        let started = self.book.start(id, || {
            let cancel = parent.child_token();
            let handle = tokio::spawn(run_bot(id, shared, cancel.clone()));
            Running { cancel, handle }
        });
        if started == Start::Waiting {
            tracing::debug!(bot = id, "still logging out; starting it again in a moment");
        }
    }

    /// Cleans up finished tasks; a task that panicked is logged and its bot shown as offline.
    fn reap(&mut self) {
        for (id, task) in self.book.reap() {
            // (finished: this does not wait)
            let Some(Err(e)) = task.handle.now_or_never() else {
                continue;
            };
            let why = if e.is_panic() {
                format!("crashed: {}", panic_text(e))
            } else {
                format!("task failed: {e}")
            };
            tracing::error!(bot = id, "bot task {why}");
            self.shared.hub.contest.release_bot(id);
            if let Some(slot) = self.shared.hub.slot(id) {
                slot.set_offline();
                slot.push_log(format!("bot task {why}"));
            }
        }
    }

    fn queue_spawn(&mut self, n: usize) {
        let shared = self.shared.clone();
        let mut ids = || -> Vec<u32> {
            shared
                .roster
                .read(|r| r.entries().iter().map(|e| e.id).collect())
        };
        let mut grow = |k: usize| {
            let target = shared.roster.read(|r| r.len()) + k;
            let w = shared.world.clone();
            let creatable = move |r: u8, g: u8| w.creatable(r, g);
            if let Err(e) = ensure_roster(
                &shared.roster,
                &shared.cfg,
                &shared.archetypes,
                &creatable,
                target,
            ) {
                tracing::warn!("cannot add bots: {e:#}");
            }
            register_slots(&shared);
        };
        let queued = self.book.spawn(n, &mut ids, &mut grow);
        tracing::info!(requested = n, queued, "starting bots");
    }

    fn despawn(&mut self, n: usize) {
        let (unqueued, stopping) = self.book.despawn_ids(n);
        tracing::info!(
            requested = n,
            unqueued = unqueued.len(),
            stopping = stopping.len(),
            "stopping bots"
        );
        // (the operator's: the presence schedules do not start them again at once)
        let presence = &self.shared.hub.presence;
        presence.operator_stopped(&unqueued);
        presence.operator_stopped(&stopping);
    }

    /// Queues the roster bots `ids` that do not run; `presence`: for the presence schedules.
    fn start_ids(&mut self, ids: &[u32], presence: bool) {
        let shared = self.shared.clone();
        let known: Vec<u32> = shared.roster.read(|r| {
            ids.iter()
                .copied()
                .filter(|id| r.get(*id).is_some())
                .collect()
        });
        let picked = self.book.queue_ids(&known);
        if presence {
            shared.hub.presence.own(&picked, unix_ms());
        }
        tracing::info!(
            requested = ids.len(),
            queued = picked.len(),
            presence,
            "starting bots by id"
        );
    }

    /// Stops those of the bots `ids` that are safe to stop now; queued ones leave the queue.
    /// `presence`: only the bots the presence schedules started.
    fn stop_ids(&mut self, ids: &[u32], presence: bool) {
        let shared = self.shared.clone();
        let ours = |id: u32| !presence || shared.hub.presence.is_owned(id);
        let queued: Vec<u32> = self
            .book
            .pending_ids()
            .into_iter()
            .filter(|id| ids.contains(id) && ours(*id))
            .collect();
        self.book.unqueue(&queued);
        let race_of = |id: u32| shared.roster.read(|r| r.get(id).map(|e| e.race));
        let snaps: Vec<(u32, u8, pb_web::BotSnapshot)> = self
            .book
            .live_ids()
            .into_iter()
            .filter(|id| ids.contains(id) && ours(*id))
            .filter_map(|id| Some((id, race_of(id)?, shared.hub.slot(id)?.snapshot())))
            .collect();
        let cands: Vec<Candidate<'_>> = snaps
            .iter()
            // (never under a real player's party)
            .filter(|(_, _, s)| {
                !s.party
                    .as_ref()
                    .is_some_and(|p| p.members.iter().any(|m| !m.bot && m.online))
            })
            .map(|(id, race, s)| Candidate {
                id: *id,
                race: *race,
                alliance: s.alliance,
                continent: s.continent,
                activity: s.activity,
                goal: &s.goal,
                online_secs: s.online_secs,
                owned: false,
            })
            .collect();
        let stop = pick_stops(&cands, &Selector::ALL, StopRule::default(), cands.len());
        self.book.stop(&stop);
        if presence {
            shared.hub.presence.disown(&queued);
            shared.hub.presence.disown(&stop);
        }
        tracing::info!(
            requested = ids.len(),
            unqueued = queued.len(),
            stopping = stop.len(),
            busy = snaps.len() - stop.len(),
            presence,
            "stopping bots by id"
        );
    }

    /// Starts `n` bots of the kind `sel` names; the roster grows with matching races when it has
    /// too few. `owned`: the population director started them.
    fn queue_spawn_where(&mut self, n: usize, sel: Selector, owned: bool) {
        let shared = self.shared.clone();
        let allowed = sel.races(&shared.cfg.bots.race_ids());
        let total = shared.roster.read(|r| r.len());
        let mut ids = || -> Vec<u32> {
            shared.roster.read(|r| {
                r.entries()
                    .iter()
                    .filter(|e| sel.matches_race(e.race))
                    .map(|e| e.id)
                    .collect()
            })
        };
        let mut added = 0usize;
        let mut grow = |k: usize| {
            if allowed.is_empty() {
                tracing::warn!(
                    "no race in bots.races matches {sel:?}; no new bot can be created for it"
                );
                return;
            }
            let target = shared.roster.read(|r| r.len()) + k;
            let w = shared.world.clone();
            let creatable = move |r: u8, g: u8| w.creatable(r, g);
            match ensure_roster_races(
                &shared.roster,
                &shared.cfg,
                &shared.archetypes,
                &creatable,
                target,
                &allowed,
            ) {
                Ok(a) => added += a,
                Err(e) => tracing::warn!("cannot add bots: {e:#}"),
            }
            register_slots(&shared);
        };
        let picked = self.book.spawn_matching(n, total, &mut ids, &mut grow);
        if owned {
            shared.hub.pop.own(&picked, unix_ms());
        }
        tracing::info!(
            requested = n,
            queued = picked.len(),
            created = added,
            owned,
            "starting bots ({sel:?})"
        );
        if owned {
            self.report(
                &sel,
                "spawn",
                picked.len() as u32,
                format!("{} queued, {added} new character(s)", picked.len()),
            );
        }
    }

    /// Stops up to `n` bots of the kind `sel` names that are safe to stop (see
    /// [`pick_stops`]); bots that never started leave the queue first.
    fn despawn_where(&mut self, n: usize, sel: Selector, rule: StopRule) {
        let shared = self.shared.clone();
        let race_of = |id: u32| shared.roster.read(|r| r.get(id).map(|e| e.race));
        let owned = |id: u32| shared.hub.pop.is_owned(id);
        // queued bots have not logged in: they are the cheapest to drop
        let queued: Vec<u32> = self
            .book
            .pending_ids()
            .into_iter()
            .filter(|id| !rule.owned_only || owned(*id))
            .filter(|id| race_of(*id).is_some_and(|r| sel.matches_race(r)))
            .take(n)
            .collect();
        let unqueued = self.book.unqueue(&queued);
        let left = n.saturating_sub(unqueued);
        let mut stopped = Vec::new();
        if left > 0 {
            let snaps: Vec<(u32, u8, pb_web::BotSnapshot)> = self
                .book
                .live_ids()
                .into_iter()
                .filter_map(|id| {
                    let race = race_of(id)?;
                    Some((id, race, shared.hub.slot(id)?.snapshot()))
                })
                .collect();
            let cands: Vec<Candidate<'_>> = snaps
                .iter()
                .map(|(id, race, s)| Candidate {
                    id: *id,
                    race: *race,
                    alliance: s.alliance,
                    continent: s.continent,
                    activity: s.activity,
                    goal: &s.goal,
                    online_secs: s.online_secs,
                    owned: owned(*id),
                })
                .collect();
            let ids = pick_stops(&cands, &sel, rule, left);
            self.book.stop(&ids);
            stopped = ids;
        }
        let mut gone = queued;
        gone.extend(&stopped);
        shared.hub.pop.disown(&gone);
        if !rule.owned_only {
            // (the operator's: the presence schedules do not start them again at once)
            shared.hub.presence.operator_stopped(&gone);
        }
        tracing::info!(
            requested = n,
            unqueued,
            stopping = stopped.len(),
            "stopping bots ({sel:?}, {rule:?})"
        );
        if rule.owned_only {
            self.report(
                &sel,
                "despawn",
                gone.len() as u32,
                format!(
                    "{unqueued} taken off the queue, {} logging out",
                    stopped.len()
                ),
            );
        }
    }

    /// Writes what the supervisor did for the population director into its log.
    fn report(&self, sel: &Selector, kind: &str, count: u32, result: String) {
        self.shared.hub.pop.director.note(pb_web::ActionRecord {
            at_ms: unix_ms(),
            kind: "result".into(),
            alliance: sel.alliance.unwrap_or_default(),
            count,
            applied: true,
            reason: format!("the supervisor handled the {kind}"),
            result,
        });
    }

    /// Runs the supervisor: starts `initial` bots (ramped), serves `rx` and, when `cancel` fires,
    /// logs everything out.
    pub async fn run(
        mut self,
        initial: usize,
        mut rx: mpsc::Receiver<SupCmd>,
        cancel: CancellationToken,
    ) {
        if self.shared.hub.presence.enabled() {
            // (the schedules decide who is online; their first pass comes in a moment)
            tracing::info!(roster = initial, "presence: the schedules log the bots in");
        } else {
            self.queue_spawn(initial);
        }
        let ramp = Duration::from_millis(self.shared.cfg.bots.ramp_ms.max(1));
        let mut ramp_timer = tokio::time::interval(ramp);
        ramp_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reap_timer = tokio::time::interval(REAP_EVERY);
        let mut commands_open = true;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ramp_timer.tick(), if self.book.queued() > 0 => {
                    if let Some(id) = self.book.next_pending() {
                        self.reap();
                        self.start_bot(id, &cancel);
                    }
                }
                _ = reap_timer.tick() => self.reap(),
                cmd = rx.recv(), if commands_open => match cmd {
                    Some(SupCmd::Spawn(n)) => self.queue_spawn(n as usize),
                    Some(SupCmd::Despawn(n)) => self.despawn(n as usize),
                    Some(SupCmd::SpawnWhere { n, sel, owned }) => {
                        self.queue_spawn_where(n as usize, sel, owned)
                    }
                    Some(SupCmd::DespawnWhere { n, sel, rule }) => {
                        self.despawn_where(n as usize, sel, rule)
                    }
                    Some(SupCmd::StartIds { ids, presence }) => self.start_ids(&ids, presence),
                    Some(SupCmd::StopIds { ids, presence }) => self.stop_ids(&ids, presence),
                    None => commands_open = false,
                },
            }
        }
        self.shutdown().await;
    }

    /// Stops every bot and waits for the logouts.
    async fn shutdown(&mut self) {
        let tasks = self.book.take_all();
        tracing::info!(bots = tasks.len(), "shutting down");
        let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
        for (id, r) in tasks {
            let mut handle = r.handle;
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) if e.is_panic() => {
                    tracing::error!(bot = id, "bot task crashed: {}", panic_text(e));
                }
                Ok(Err(_)) => {}
                Err(_) => {
                    tracing::warn!(bot = id, "did not log out in time; aborting");
                    handle.abort();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// A bot task that ends when the test says so (or, once told to stop, when `finish` is called).
    #[derive(Clone)]
    struct Fake {
        cancelled: Rc<Cell<bool>>,
        done: Rc<Cell<bool>>,
        generation: u32,
    }

    impl Fake {
        fn new(generation: u32) -> Fake {
            Fake {
                cancelled: Rc::new(Cell::new(false)),
                done: Rc::new(Cell::new(false)),
                generation,
            }
        }
    }

    impl Task for Fake {
        fn cancel(&self) {
            self.cancelled.set(true);
        }

        fn finished(&self) -> bool {
            self.done.get()
        }
    }

    /// A book with the bots `ids` running, remembering each started task.
    struct Rig {
        book: Book<Fake>,
        tasks: Rc<RefCell<Vec<(u32, Fake)>>>,
        roster: Rc<RefCell<Vec<u32>>>,
        grown: Rc<Cell<usize>>,
    }

    impl Rig {
        fn new(roster_len: u32, running: &[u32]) -> Rig {
            let mut rig = Rig {
                book: Book::default(),
                tasks: Rc::default(),
                roster: Rc::new(RefCell::new((1..=roster_len).collect())),
                grown: Rc::default(),
            };
            for &id in running {
                assert_eq!(rig.start(id), Start::Started);
            }
            rig
        }

        fn start(&mut self, id: u32) -> Start {
            let tasks = self.tasks.clone();
            let generation = tasks.borrow().iter().filter(|(i, _)| *i == id).count() as u32 + 1;
            self.book.start(id, move || {
                let f = Fake::new(generation);
                tasks.borrow_mut().push((id, f.clone()));
                f
            })
        }

        fn spawn(&mut self, n: usize) -> usize {
            let (roster, grown) = (self.roster.clone(), self.grown.clone());
            let r2 = self.roster.clone();
            self.book.spawn(n, &mut || r2.borrow().clone(), &mut |k| {
                grown.set(grown.get() + k);
                let mut r = roster.borrow_mut();
                let next = r.last().copied().unwrap_or(0);
                r.extend(next + 1..=next + k as u32);
            })
        }

        /// Starts everything in the queue the way the ramp timer does.
        fn ramp(&mut self) -> Vec<(u32, Start)> {
            let mut out = Vec::new();
            for _ in 0..self.book.queued() {
                let Some(id) = self.book.next_pending() else {
                    break;
                };
                self.reap();
                out.push((id, self.start(id)));
            }
            out
        }

        fn reap(&mut self) -> Vec<u32> {
            self.book.reap().into_iter().map(|(id, _)| id).collect()
        }

        /// The task of bot `id` finishes (it has logged out).
        fn finish(&self, id: u32) {
            for (i, f) in self.tasks.borrow().iter() {
                if *i == id && f.cancelled.get() {
                    f.done.set(true);
                }
            }
        }

        fn current(&self, id: u32) -> Fake {
            self.tasks
                .borrow()
                .iter()
                .rev()
                .find(|(i, _)| *i == id)
                .map(|(_, f)| f.clone())
                .expect("a task")
        }
    }

    #[test]
    fn idle_bots_start_lowest_first_and_skip_running_and_pending() {
        let mut rig = Rig::new(6, &[2, 3]);
        rig.book.pending.push_back(4);
        let all: Vec<u32> = (1..=6).collect();
        assert_eq!(rig.book.pick_idle(&all, 2), vec![1, 5]);
        assert_eq!(rig.book.pick_idle(&all, 10), vec![1, 5, 6]);
        assert!(rig.book.pick_idle(&all, 0).is_empty());
    }

    #[test]
    fn despawn_takes_the_highest_ids() {
        let rig = Rig::new(12, &[1, 5, 9, 12]);
        assert_eq!(rig.book.pick_despawn(2), vec![12, 9]);
        assert_eq!(rig.book.pick_despawn(10), vec![12, 9, 5, 1]);
        assert!(rig.book.pick_despawn(0).is_empty());
    }

    #[test]
    fn a_stopping_bot_is_free_for_the_next_spawn_and_does_not_grow_the_roster() {
        let mut rig = Rig::new(6, &[1, 2, 3, 4, 5, 6]);
        // despawn 3: 6, 5 and 4 are told to stop but are still logging out
        assert_eq!(rig.book.despawn(3), (0, 3));
        assert_eq!(rig.book.active(), 3);
        assert_eq!(rig.book.tasks(), 6);
        assert!([6, 5, 4].iter().all(|id| rig.book.is_stopping(*id)));
        assert!(rig.current(6).cancelled.get());
        assert!(!rig.current(1).cancelled.get());
        // spawn 3 right away: the same three bots are queued, no roster entry is added
        assert_eq!(rig.spawn(3), 3);
        assert_eq!(rig.grown.get(), 0);
        assert_eq!(rig.roster.borrow().len(), 6);
        assert_eq!(rig.book.pending, VecDeque::from([4, 5, 6]));
        // their old tasks are still alive: nothing starts yet, the ids stay queued
        let first = rig.ramp();
        assert!(first.iter().all(|(_, s)| *s == Start::Waiting), "{first:?}");
        assert_eq!(rig.book.queued(), 3);
        assert_eq!(rig.tasks.borrow().len(), 6, "no second task for a busy bot");
        // the old tasks end; the next ramp starts the same bots again
        for id in [4, 5, 6] {
            rig.finish(id);
        }
        let second = rig.ramp();
        assert_eq!(second.len(), 3);
        assert!(
            second.iter().all(|(_, s)| *s == Start::Started),
            "{second:?}"
        );
        assert_eq!(rig.book.queued(), 0);
        assert_eq!(rig.book.active(), 6);
        assert_eq!(rig.tasks.borrow().len(), 9);
        assert_eq!(rig.current(5).generation, 2);
        assert_eq!(rig.roster.borrow().len(), 6, "still the same six bots");
    }

    #[test]
    fn a_second_despawn_does_not_cancel_the_same_bots_again() {
        let mut rig = Rig::new(6, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(rig.book.despawn(2), (0, 2));
        // 6 and 5 are stopping: the next two go to 4 and 3, not to 6 and 5 again
        assert_eq!(rig.book.pick_despawn(2), vec![4, 3]);
        assert_eq!(rig.book.despawn(2), (0, 2));
        assert_eq!(rig.book.active(), 2);
        assert!([4, 3].iter().all(|id| rig.book.is_stopping(*id)));
        // asking for more than is left stops what is left, once
        assert_eq!(rig.book.despawn(10), (0, 2));
        assert_eq!(rig.book.despawn(10), (0, 0));
        assert_eq!(rig.book.active(), 0);
    }

    #[test]
    fn despawn_takes_bots_that_never_started_out_of_the_queue_first() {
        let mut rig = Rig::new(5, &[1, 2]);
        assert_eq!(rig.spawn(3), 3);
        assert_eq!(rig.book.pending, VecDeque::from([3, 4, 5]));
        assert_eq!(rig.book.despawn(4), (3, 1));
        assert_eq!(rig.book.queued(), 0);
        assert!(rig.book.is_stopping(2));
        assert!(!rig.book.is_stopping(1));
    }

    #[test]
    fn spawn_adds_roster_entries_only_for_bots_that_do_not_exist() {
        let mut rig = Rig::new(3, &[1, 2, 3]);
        assert_eq!(rig.spawn(2), 2);
        assert_eq!(rig.grown.get(), 2);
        assert_eq!(rig.book.pending, VecDeque::from([4, 5]));
        // asking again while they wait adds two more: nobody is free
        assert_eq!(rig.spawn(2), 2);
        assert_eq!(rig.grown.get(), 4);
        // an offline bot of the roster is used before the roster grows
        let mut rig = Rig::new(5, &[1, 2, 3]);
        assert_eq!(rig.spawn(2), 2);
        assert_eq!(rig.grown.get(), 0);
        assert_eq!(rig.book.pending, VecDeque::from([4, 5]));
    }

    #[test]
    fn the_roster_never_grows_past_the_bot_limit() {
        let limit = MAX_BOTS;
        let mut rig = Rig::new(limit - 2, &[]);
        // a huge request adds the two entries that fit and queues every bot once
        let queued = rig.spawn(u32::MAX as usize);
        assert_eq!(rig.grown.get(), 2);
        assert_eq!(rig.roster.borrow().len(), limit as usize);
        assert_eq!(queued, limit as usize);
        // at the limit nothing more is added
        assert_eq!(rig.spawn(100), 0);
        assert_eq!(rig.grown.get(), 2);
        assert_eq!(rig.roster.borrow().len(), limit as usize);
    }

    /// A rig whose roster entries with an even id are "east" bots.
    fn east(rig: &Rig) -> impl FnMut() -> Vec<u32> {
        let roster = rig.roster.clone();
        move || {
            roster
                .borrow()
                .iter()
                .copied()
                .filter(|i| i % 2 == 0)
                .collect()
        }
    }

    #[test]
    fn a_filtered_spawn_starts_only_matching_bots_and_grows_the_roster_with_matches() {
        let mut rig = Rig::new(6, &[2]);
        let total = rig.roster.borrow().len();
        let grown = rig.grown.clone();
        let roster = rig.roster.clone();
        // two east bots are free (4 and 6): no growth
        let picked = rig
            .book
            .spawn_matching(2, total, &mut east(&rig), &mut |k| {
                grown.set(grown.get() + k)
            });
        assert_eq!(picked, vec![4, 6]);
        assert_eq!(rig.grown.get(), 0);
        assert_eq!(rig.book.pending_ids(), vec![4, 6]);
        // three more: none is free any more, the roster grows by three east entries
        let picked = rig
            .book
            .spawn_matching(3, total, &mut east(&rig), &mut |k| {
                grown.set(grown.get() + k);
                let mut r = roster.borrow_mut();
                for _ in 0..k {
                    let next = r.last().copied().unwrap_or(0) + 1;
                    // (the next even id)
                    r.push(next + next % 2);
                }
            });
        assert_eq!(rig.grown.get(), 3);
        assert_eq!(picked.len(), 3);
        assert!(picked.iter().all(|i| i % 2 == 0), "{picked:?}");
        assert!(!picked.contains(&2) && !picked.contains(&4) && !picked.contains(&6));
        // the bot limit counts the whole roster, not the matching part
        let mut full = Rig::new(MAX_BOTS - 1, &[]);
        let total = full.roster.borrow().len();
        let mut added = 0usize;
        let ids = east(&full);
        let mut ids = ids;
        let picked = full
            .book
            .spawn_matching(usize::MAX / 2, total, &mut ids, &mut |k| added += k);
        assert_eq!(added, 1, "one entry fits");
        assert_eq!(picked.len() as u32, (MAX_BOTS - 1) / 2);
    }

    #[test]
    fn queued_and_running_bots_can_be_taken_back_by_id() {
        let mut rig = Rig::new(8, &[1, 2, 3]);
        assert_eq!(rig.spawn(3), 3);
        assert_eq!(rig.book.pending_ids(), vec![4, 5, 6]);
        assert_eq!(rig.book.live_ids(), vec![1, 2, 3]);
        // ids that are not queued are ignored
        assert_eq!(rig.book.unqueue(&[5, 7, 99]), 1);
        assert_eq!(rig.book.pending_ids(), vec![4, 6]);
        // only running bots that were not told to stop yet are stopped, once
        assert_eq!(rig.book.stop(&[2, 3, 4, 99]), 2);
        assert!(rig.book.is_stopping(2) && rig.book.is_stopping(3) && !rig.book.is_stopping(1));
        assert!(rig.current(2).cancelled.get() && !rig.current(1).cancelled.get());
        assert_eq!(rig.book.live_ids(), vec![1]);
        assert_eq!(rig.book.stop(&[2, 3]), 0);
    }

    #[test]
    fn bots_are_queued_by_id_once_and_despawns_name_their_ids() {
        let mut rig = Rig::new(8, &[1, 2]);
        // 2 runs, 5 is named twice, 9 is not running either (the caller checks the roster)
        assert_eq!(rig.book.queue_ids(&[2, 5, 3, 5, 9]), vec![5, 3, 9]);
        assert_eq!(rig.book.pending_ids(), vec![5, 3, 9]);
        assert!(rig.book.queue_ids(&[5, 3]).is_empty(), "already queued");
        // a stopping bot may be queued again (it starts once its old task is gone)
        rig.book.stop(&[1]);
        assert_eq!(rig.book.queue_ids(&[1]), vec![1]);
        // a despawn names what it took off the queue (newest first) and what it stopped
        let (unqueued, stopped) = rig.book.despawn_ids(5);
        assert_eq!(unqueued, vec![1, 9, 3, 5]);
        assert_eq!(stopped, vec![2]);
        assert_eq!(rig.book.despawn(3), (0, 0));
    }

    #[test]
    fn a_running_bot_is_not_started_twice() {
        let mut rig = Rig::new(3, &[2]);
        assert_eq!(rig.start(2), Start::AlreadyRunning);
        assert_eq!(rig.tasks.borrow().len(), 1);
    }

    #[test]
    fn finished_tasks_are_handed_back_by_reap_and_everything_else_stays() {
        let mut rig = Rig::new(4, &[1, 2, 3]);
        rig.book.despawn(1);
        rig.finish(3);
        assert_eq!(rig.reap(), vec![3]);
        assert_eq!(rig.book.tasks(), 2);
        assert!(!rig.book.is_stopping(3));
        // a task that ended on its own (not told to stop) is reaped too
        rig.current(1).done.set(true);
        assert_eq!(rig.reap(), vec![1]);
        assert_eq!(rig.book.tasks(), 1);
    }

    #[test]
    fn shutdown_cancels_every_task_and_forgets_the_queue() {
        let mut rig = Rig::new(5, &[1, 2, 3]);
        rig.spawn(2);
        let all = rig.book.take_all();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|(_, f)| f.cancelled.get()));
        assert_eq!(rig.book.tasks(), 0);
        assert_eq!(rig.book.queued(), 0);
    }

    #[tokio::test]
    async fn a_panicking_task_is_reported_with_its_message() {
        let h = tokio::spawn(async {
            panic!("boom in a bot");
        });
        let mut h = h;
        // wait for it to end
        while !h.is_finished() {
            tokio::task::yield_now().await;
        }
        let err = (&mut h)
            .now_or_never()
            .expect("finished")
            .expect_err("panicked");
        assert!(err.is_panic());
        assert_eq!(panic_text(err), "boom in a bot");

        let h = tokio::spawn(async {
            let n = 3;
            panic!("formatted {n}");
        });
        let err = h.await.expect_err("panicked");
        assert_eq!(panic_text(err), "formatted 3");
    }

    #[test]
    fn a_filtered_spawn_grows_the_roster_with_the_races_it_names() {
        let cfg = Config::parse(
            "[bots]
races = [\"nuian\", \"elf\", \"ferre\"]
",
        )
        .unwrap();
        let archetypes = Archetypes::builtin().unwrap();
        let dir = crate::testutil::temp_dir("sup-roster-races");
        let roster = SharedRoster::new(crate::roster::Roster::new(), dir.join("roster.json"));
        let any = |_: u8, _: u8| true;
        let haranya = Selector::side(pb_web::Alliance::Haranya).races(&cfg.bots.race_ids());
        assert_eq!(haranya, vec![6]);
        let added = ensure_roster_races(&roster, &cfg, &archetypes, &any, 12, &haranya).unwrap();
        assert_eq!(added, 12);
        roster.read(|r| assert!(r.entries().iter().all(|e| e.race == 6)));
        // no race to create: an error, not a loop
        assert!(ensure_roster_races(&roster, &cfg, &archetypes, &any, 20, &[]).is_err());
    }

    #[test]
    fn the_roster_grows_with_creatable_races_only() {
        let cfg = Config::parse("[bots]\nraces = [\"nuian\", \"elf\"]\n").unwrap();
        let archetypes = Archetypes::builtin().unwrap();
        let dir = crate::testutil::temp_dir("sup-roster");
        let roster = SharedRoster::new(crate::roster::Roster::new(), dir.join("roster.json"));
        // only female elves can be created in this fake server
        let creatable = |race: u8, gender: u8| race == 4 && gender == 2;
        let added = ensure_roster(&roster, &cfg, &archetypes, &creatable, 25).unwrap();
        assert_eq!(added, 25);
        roster.read(|r| {
            assert_eq!(r.len(), 25);
            assert!(r.entries().iter().all(|e| e.race == 4 && e.gender == 2));
            assert_eq!(r.entries()[0].account, "pb1");
        });
        // asking again adds nothing; a growing count adds the difference
        assert_eq!(
            ensure_roster(&roster, &cfg, &archetypes, &creatable, 25).unwrap(),
            0
        );
        assert_eq!(
            ensure_roster(&roster, &cfg, &archetypes, &creatable, 27).unwrap(),
            2
        );
        // nothing creatable is an error, not a loop
        let none = |_: u8, _: u8| false;
        assert!(ensure_roster(&roster, &cfg, &archetypes, &none, 40).is_err());
    }
}
