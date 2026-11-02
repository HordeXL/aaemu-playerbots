//! The supervisor: starts the bot tasks (one per roster entry, spread over time), starts and stops
//! bots on request of the web UI, and shuts everything down gracefully.
//!
//! Bots are started `bots.ramp_ms` apart so the login server is not hit by hundreds of logins at
//! once. A despawned bot logs out and stays in the roster (and as an offline entry in the UI), so
//! a later `Spawn` brings the same character back.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::archetype::Archetypes;
use crate::bot::{initial_snapshot, run_bot, Shared};
use crate::config::Config;
use crate::hub::SupCmd;
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
    let races = cfg.bots.race_ids();
    let spec = RosterSpec {
        account_prefix: &cfg.server.account_prefix,
        races: &races,
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

/// The ids to start: the lowest `n` ids of `all` that are not running.
pub fn pick_idle(
    all: &[u32],
    running: &BTreeMap<u32, impl Sized>,
    pending: &VecDeque<u32>,
    n: usize,
) -> Vec<u32> {
    all.iter()
        .copied()
        .filter(|id| !running.contains_key(id) && !pending.contains(id))
        .take(n)
        .collect()
}

/// The ids to stop: the highest `n` running ids.
pub fn pick_despawn(running: &BTreeMap<u32, impl Sized>, n: usize) -> Vec<u32> {
    running.keys().rev().copied().take(n).collect()
}

/// Owns the bot tasks.
pub struct Supervisor {
    shared: Arc<Shared>,
    running: BTreeMap<u32, Running>,
    pending: VecDeque<u32>,
}

impl Supervisor {
    /// A supervisor with no bots running.
    pub fn new(shared: Arc<Shared>) -> Supervisor {
        Supervisor {
            shared,
            running: BTreeMap::new(),
            pending: VecDeque::new(),
        }
    }

    /// Number of running bot tasks.
    pub fn running(&self) -> usize {
        self.running.len()
    }

    fn start_bot(&mut self, id: u32, parent: &CancellationToken) {
        if self.running.contains_key(&id) {
            return;
        }
        let cancel = parent.child_token();
        let handle = tokio::spawn(run_bot(id, self.shared.clone(), cancel.clone()));
        self.running.insert(id, Running { cancel, handle });
    }

    fn reap(&mut self) {
        self.running.retain(|_, r| !r.handle.is_finished());
    }

    fn queue_spawn(&mut self, n: usize) {
        let all: Vec<u32> = self
            .shared
            .roster
            .read(|r| r.entries().iter().map(|e| e.id).collect());
        let missing = n.saturating_sub(pick_idle(&all, &self.running, &self.pending, n).len());
        if missing > 0 {
            let target = all.len() + missing;
            let w = self.shared.world.clone();
            let creatable = move |r: u8, g: u8| w.creatable(r, g);
            if let Err(e) = ensure_roster(
                &self.shared.roster,
                &self.shared.cfg,
                &self.shared.archetypes,
                &creatable,
                target,
            ) {
                tracing::warn!("cannot add bots: {e:#}");
            }
            register_slots(&self.shared);
        }
        let all: Vec<u32> = self
            .shared
            .roster
            .read(|r| r.entries().iter().map(|e| e.id).collect());
        let ids = pick_idle(&all, &self.running, &self.pending, n);
        tracing::info!(requested = n, queued = ids.len(), "starting bots");
        self.pending.extend(ids);
    }

    fn despawn(&mut self, n: usize) {
        // Pending bots that never started go first.
        let mut left = n;
        while left > 0 && self.pending.pop_back().is_some() {
            left -= 1;
        }
        let ids = pick_despawn(&self.running, left);
        tracing::info!(requested = n, stopping = ids.len(), "stopping bots");
        for id in ids {
            if let Some(r) = self.running.get(&id) {
                r.cancel.cancel();
            }
        }
    }

    /// Runs the supervisor: starts `initial` bots (ramped), serves `rx` and, when `cancel` fires,
    /// logs everything out.
    pub async fn run(
        mut self,
        initial: usize,
        mut rx: mpsc::Receiver<SupCmd>,
        cancel: CancellationToken,
    ) {
        self.queue_spawn(initial);
        let ramp = Duration::from_millis(self.shared.cfg.bots.ramp_ms.max(1));
        let mut ramp_timer = tokio::time::interval(ramp);
        ramp_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reap_timer = tokio::time::interval(REAP_EVERY);
        let mut commands_open = true;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ramp_timer.tick(), if !self.pending.is_empty() => {
                    if let Some(id) = self.pending.pop_front() {
                        self.start_bot(id, &cancel);
                    }
                }
                _ = reap_timer.tick() => self.reap(),
                cmd = rx.recv(), if commands_open => match cmd {
                    Some(SupCmd::Spawn(n)) => self.queue_spawn(n as usize),
                    Some(SupCmd::Despawn(n)) => self.despawn(n as usize),
                    None => commands_open = false,
                },
            }
        }
        self.shutdown().await;
    }

    /// Stops every bot and waits for the logouts.
    async fn shutdown(&mut self) {
        let n = self.running.len();
        tracing::info!(bots = n, "shutting down");
        for r in self.running.values() {
            r.cancel.cancel();
        }
        let deadline = tokio::time::Instant::now() + SHUTDOWN_WAIT;
        for (id, r) in std::mem::take(&mut self.running) {
            let mut handle = r.handle;
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(_) => {}
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

    fn running(ids: &[u32]) -> BTreeMap<u32, ()> {
        ids.iter().map(|i| (*i, ())).collect()
    }

    #[test]
    fn idle_bots_start_lowest_first_and_skip_running_and_pending() {
        let all = [1, 2, 3, 4, 5, 6];
        let run = running(&[2, 3]);
        let pending: VecDeque<u32> = [4].into_iter().collect();
        assert_eq!(pick_idle(&all, &run, &pending, 2), vec![1, 5]);
        assert_eq!(pick_idle(&all, &run, &pending, 10), vec![1, 5, 6]);
        assert!(pick_idle(&all, &run, &pending, 0).is_empty());
    }

    #[test]
    fn despawn_takes_the_highest_ids() {
        let run = running(&[1, 5, 9, 12]);
        assert_eq!(pick_despawn(&run, 2), vec![12, 9]);
        assert_eq!(pick_despawn(&run, 10), vec![12, 9, 5, 1]);
        assert!(pick_despawn(&run, 0).is_empty());
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
