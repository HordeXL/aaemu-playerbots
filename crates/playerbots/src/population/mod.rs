//! Population: what the bots know about the world's players and goods, and the director that logs
//! bots in and out because of it.
//!
//! One [`Population`] hangs on the [`Hub`] (`hub.pop`) and is shared by all bots:
//!
//! * [`census`]: the real players the bots can see (character, side, continent, zone, position);
//! * [`economy`]: how scarce each item is, from the bots' price notes, trade chat and (optionally)
//!   the server's own auction list; read by goals through [`economy::ScarcityView`];
//! * [`serverapi`]: the optional poller of the game server's web API (exact players online, the
//!   auction list);
//! * [`director`]: the rules, the pacing and the log of the population director, and [`select`] /
//!   [`region`] / [`view`] for what it and the World panel need to tell sides, races and
//!   continents apart.
//!
//! The hub calls [`Population::tick`] once a second. A bot feeds the census and the economy from
//! its own once-a-second housekeeping and never asks the population anything, apart from
//! [`GoalBias`], a handful of atomics that the brain reads when it scores its goals.

pub mod census;
pub mod director;
pub mod economy;
pub mod region;
pub mod select;
pub mod serverapi;
pub mod view;

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use aa_world::World;
use parking_lot::Mutex;
use pb_web::{Activity, DirectorView, EconomyView, HealthView, PolicyPatch, WorldView};
use rustc_hash::FxHashMap;

use self::census::Census;
use self::director::{Director, Order, StepInputs};
use self::economy::{Economy, ScarcityView, VIEW_ROWS};
use self::select::stop_rank;
use self::serverapi::ServerState;
use self::view::{count_bots, world_view, BotRow, Presence, WorldInputs};
use crate::bot::cmd::GoalKind;
use crate::config::PopulationCfg;
use crate::hub::{unix_ms, Hub, SupCmd};

/// Failed or short sessions count towards the back-off for this long (milliseconds).
const FAILURE_WINDOW_MS: u64 = 5 * 60_000;
/// The most failed sessions remembered.
const MAX_FAILURES: usize = 256;
/// The census is cleaned up every this many ticks.
const PRUNE_EVERY: u32 = 5;
/// A bot the director started that has been offline this long is not the director's any more.
const DISOWN_AFTER_MS: u64 = 15 * 60_000;
/// With wishes on the list the farming goal is scored this much higher (unless the operator set a
/// bias for it).
pub const FARM_NUDGE: f32 = 1.5;

/// Multipliers on the goal scores of every bot, set from the policy. The brain multiplies the
/// scores of its goals by these when it chooses; 1.0 changes nothing. Atomics, so a bot reads them
/// without a lock.
#[derive(Debug)]
pub struct GoalBias {
    mult: [AtomicU32; GoalKind::COUNT],
}

impl Default for GoalBias {
    fn default() -> Self {
        GoalBias {
            mult: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
        }
    }
}

impl GoalBias {
    /// The multiplier of a goal.
    pub fn get(&self, kind: GoalKind) -> f32 {
        f32::from_bits(self.mult[kind.index()].load(Ordering::Relaxed))
    }

    /// Replaces all multipliers: 1.0 for every goal, then the ones in `bias` (goal name, factor;
    /// unknown names and numbers that are not finite are skipped), then, with `farm_nudge`,
    /// [`FARM_NUDGE`] for the farming goal unless `bias` names it.
    pub fn set(&self, bias: &BTreeMap<String, f32>, farm_nudge: bool) {
        let (lo, hi) = pb_web::Policy::BIAS_RANGE;
        let mut next = [1.0f32; GoalKind::COUNT];
        for (name, f) in bias {
            if let Some(kind) = GoalKind::parse(name).filter(|_| f.is_finite()) {
                next[kind.index()] = f.clamp(lo, hi);
            }
        }
        if farm_nudge && !bias.contains_key(GoalKind::Farm.name()) {
            next[GoalKind::Farm.index()] = FARM_NUDGE;
        }
        for (slot, v) in self.mult.iter().zip(next) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// Multiplies the scores in place. A goal that scores 0 (blocked, impossible) stays 0.
    pub fn apply(&self, scores: &mut [f32; GoalKind::COUNT]) {
        for (s, m) in scores.iter_mut().zip(&self.mult) {
            let f = f32::from_bits(m.load(Ordering::Relaxed));
            if f != 1.0 {
                *s *= f;
            }
        }
    }

    /// Whether every multiplier is 1.0.
    pub fn is_neutral(&self) -> bool {
        self.mult
            .iter()
            .all(|m| f32::from_bits(m.load(Ordering::Relaxed)) == 1.0)
    }
}

/// See the module docs.
pub struct Population {
    /// The real players the bots see.
    pub census: Census,
    /// How scarce items are.
    pub economy: Economy,
    /// What the game server's web API said.
    pub server: ServerState,
    /// The director.
    pub director: Director,
    /// The goal multipliers.
    pub bias: GoalBias,
    /// Start times (unix ms) of failed or short sessions, oldest first.
    failures: Mutex<VecDeque<u64>>,
    /// The bots the director started, with the time it queued them (unix ms).
    owned: Mutex<FxHashMap<u32, u64>>,
    world: Option<Arc<World>>,
    /// The races `bots.races` allows (new roster entries get one of them).
    races: Vec<u8>,
    ticks: AtomicU64,
}

impl std::fmt::Debug for Population {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Population")
            .field("players", &self.census.len())
            .field("items", &self.economy.len())
            .finish_non_exhaustive()
    }
}

impl Population {
    /// A population for `cfg` over `world`; `races` are the race ids bots may be created with.
    pub fn new(cfg: &PopulationCfg, races: Vec<u8>, world: Option<Arc<World>>) -> Population {
        Population {
            census: Census::default(),
            economy: Economy::default(),
            server: ServerState::new(!cfg.server_api.is_empty()),
            director: Director::new(cfg),
            bias: GoalBias::default(),
            failures: Mutex::new(VecDeque::new()),
            owned: Mutex::new(FxHashMap::default()),
            world,
            races,
            ticks: AtomicU64::new(0),
        }
    }

    /// A population that does nothing (the director is off), for a hub without configuration.
    pub fn inert() -> Population {
        Population::new(&PopulationCfg::default(), Vec::new(), None)
    }

    /// The races a bot may be created with.
    pub fn races(&self) -> &[u8] {
        &self.races
    }

    /// The world data, when the population was given it.
    pub fn world(&self) -> Option<&Arc<World>> {
        self.world.as_ref()
    }

    // ---- what the bots feed it -------------------------------------------------------------

    /// A bot's session ended without ever being healthy (the login failed, the connection was
    /// refused or dropped at once).
    pub fn note_failed_session(&self) {
        let now = unix_ms();
        let mut g = self.failures.lock();
        if g.len() >= MAX_FAILURES {
            g.pop_front();
        }
        g.push_back(now);
    }

    /// Failed sessions in the last five minutes.
    pub fn failed_sessions(&self, now_ms: u64) -> u32 {
        let mut g = self.failures.lock();
        while g
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= FAILURE_WINDOW_MS)
        {
            g.pop_front();
        }
        g.len() as u32
    }

    /// A chat line a bot heard: the economy counts it when it offers or asks for an item.
    pub fn hear(&self, hub: &Hub, world: &World, kind: i16, sender: &str, text: &str) {
        let names = crate::bot::market::ads::names::global(world);
        let from_bot = hub.chat.names.contains(sender);
        self.economy
            .hear(names, from_bot, kind, sender, text, unix_ms());
    }

    // ---- the bots the director owns --------------------------------------------------------

    /// Marks `ids` as started by the director.
    pub fn own(&self, ids: &[u32], now_ms: u64) {
        let mut g = self.owned.lock();
        for id in ids {
            g.insert(*id, now_ms);
        }
    }

    /// The director no longer owns `ids`.
    pub fn disown(&self, ids: &[u32]) {
        let mut g = self.owned.lock();
        for id in ids {
            g.remove(id);
        }
    }

    /// Whether the director started bot `id`.
    pub fn is_owned(&self, id: u32) -> bool {
        self.owned.lock().contains_key(&id)
    }

    /// The ids the director owns.
    pub fn owned_ids(&self) -> Vec<u32> {
        self.owned.lock().keys().copied().collect()
    }

    // ---- the once-a-second pass ------------------------------------------------------------

    /// Looks at the world: counts players and bots, lets the director act, refreshes the goal
    /// multipliers. Called by [`Hub::tick`].
    pub fn tick(&self, hub: &Hub) {
        let now = unix_ms();
        let n = self.ticks.fetch_add(1, Ordering::Relaxed);
        if n.is_multiple_of(u64::from(PRUNE_EVERY)) {
            self.census.prune(now);
        }
        let rows = bot_rows(hub);
        let counts = count_bots(&rows);
        let (owned, ready) = self.count_owned(&rows, now);
        let health = self.health(hub, counts.connecting, now);
        self.director.step(
            &StepInputs {
                now_ms: now,
                players: self.census.per_side(now),
                bots: counts.by_side,
                owned,
                ready,
                total_online: counts.total,
                health,
            },
            &mut |order| send_order(hub, order),
        );
        if let Some(world) = &self.world {
            self.director
                .update_auto(now, |n, min| self.economy.top_scarce(n, min, now, &**world));
        }
        let policy = self.director.policy();
        let live = policy.active && !policy.dry_run;
        let wished = live && !self.director.wishes(now).is_empty();
        if live {
            self.bias.set(&policy.goal_bias, wished);
        } else if !self.bias.is_neutral() {
            self.bias.set(&BTreeMap::new(), false);
        }
    }

    /// Owned bots per side that are in the world or logging in, and of those the ones the
    /// director may stop now (up for [`director::MIN_STAY_SECS`] and doing nothing that must not be
    /// interrupted); forgets the ones that went offline long ago.
    fn count_owned(&self, rows: &[BotRow], now_ms: u64) -> ([u32; 2], [u32; 2]) {
        let mut own = self.owned.lock();
        if own.is_empty() {
            return ([0, 0], [0, 0]);
        }
        let by_id: FxHashMap<u32, &BotRow> = rows.iter().map(|r| (r.id, r)).collect();
        let mut n = [0u32; 2];
        let mut ready = [0u32; 2];
        own.retain(|id, since| match by_id.get(id) {
            None => false,
            Some(r) if r.presence == Presence::Offline => {
                now_ms.saturating_sub(*since) < DISOWN_AFTER_MS
            }
            Some(r) => {
                if let Some(s) = r.alliance.side() {
                    n[s] += 1;
                    if r.calm && r.online_secs >= director::MIN_STAY_SECS {
                        ready[s] += 1;
                    }
                }
                true
            }
        });
        (n, ready)
    }

    fn health(&self, hub: &Hub, connecting: u32, now_ms: u64) -> HealthView {
        use pb_web::BotHub;
        let s = hub.stats();
        HealthView {
            rss_mb: (s.rss_bytes / (1024 * 1024)).min(u64::from(u32::MAX)) as u32,
            packets_in_per_sec: s.packets_in_per_sec,
            packets_out_per_sec: s.packets_out_per_sec,
            login_failures: self.failed_sessions(now_ms),
            open_issues: s.open_issues,
            connecting,
            holding: None,
        }
    }

    // ---- the views -------------------------------------------------------------------------

    /// The World panel.
    pub fn world_view(&self, hub: &Hub) -> WorldView {
        let now = unix_ms();
        let Some(world) = &self.world else {
            return WorldView {
                at_ms: now,
                ttl_secs: (census::TTL_MS / 1000) as u32,
                server: self.server.view(),
                ..WorldView::default()
            };
        };
        let rows = bot_rows(hub);
        let players = self.census.fresh(now);
        world_view(WorldInputs {
            now_ms: now,
            rows: &rows,
            players: &players,
            server: self.server.view(),
            geo: &**world,
        })
    }

    /// The Economy panel.
    pub fn economy_view(&self) -> EconomyView {
        let now = unix_ms();
        let Some(world) = &self.world else {
            return EconomyView {
                at_ms: now,
                ..EconomyView::default()
            };
        };
        let wished = self.director.wishes(now);
        self.economy.view(now, &**world, &wished, VIEW_ROWS)
    }

    /// The Director panel.
    pub fn director_view(&self, hub: &Hub) -> DirectorView {
        let now = unix_ms();
        let rows = bot_rows(hub);
        let counts = count_bots(&rows);
        let (owned, _) = self.count_owned(&rows, now);
        self.director.view(
            self.health(hub, counts.connecting, now),
            owned[0] + owned[1],
        )
    }

    /// Applies a policy change from the operator.
    pub fn set_policy(&self, patch: PolicyPatch) -> Result<(), String> {
        self.director.apply(patch, unix_ms())
    }
}

impl ScarcityView for Population {
    fn scarcity(&self, template: u32) -> f32 {
        self.world
            .as_ref()
            .map_or(0.0, |w| self.economy.scarcity(template, unix_ms(), &**w))
    }

    fn most_scarce(&self, n: usize, min: f32) -> Vec<(u32, f32)> {
        self.world.as_ref().map_or_else(Vec::new, |w| {
            self.economy.top_scarce(n, min, unix_ms(), &**w)
        })
    }

    fn wished(&self) -> Vec<u32> {
        self.director.wishes(unix_ms())
    }
}

/// What the director and the World panel read of every bot, from its slot.
pub fn bot_rows(hub: &Hub) -> Vec<BotRow> {
    hub.slots()
        .iter()
        .map(|s| {
            s.with(|g| {
                let b = &g.snap;
                BotRow {
                    id: b.id,
                    alliance: b.alliance,
                    continent: b.continent,
                    zone_key: b.zone_key,
                    x: b.x,
                    y: b.y,
                    presence: match b.activity {
                        Activity::Offline => Presence::Offline,
                        Activity::Connecting => Presence::Connecting,
                        _ => Presence::Online,
                    },
                    online_secs: b.online_secs,
                    calm: stop_rank(b.activity, &b.goal).is_some(),
                }
            })
        })
        .collect()
}

/// Hands the director's order to the supervisor.
fn send_order(hub: &Hub, order: Order) -> Result<String, String> {
    let cmd = match order {
        Order::Spawn { sel, n } => SupCmd::SpawnWhere {
            n,
            sel,
            owned: true,
        },
        Order::Despawn { sel, n, rule } => SupCmd::DespawnWhere { n, sel, rule },
    };
    hub.order(cmd)?;
    Ok("ordered".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goal_bias_scales_scores_and_never_wakes_a_blocked_goal() {
        let b = GoalBias::default();
        assert!(b.is_neutral());
        let mut s = [1.0f32; GoalKind::COUNT];
        s[GoalKind::Rest.index()] = 0.0;
        let before = s;
        b.apply(&mut s);
        assert_eq!(s, before);

        let mut m = BTreeMap::new();
        m.insert("grind".to_owned(), 2.0);
        m.insert("rest".to_owned(), 3.0);
        m.insert("fly".to_owned(), 3.0);
        m.insert("quest".to_owned(), f32::NAN);
        m.insert("duel".to_owned(), 99.0);
        b.set(&m, false);
        assert!(!b.is_neutral());
        assert_eq!(b.get(GoalKind::Grind), 2.0);
        assert_eq!(b.get(GoalKind::Duel), 3.0, "clamped to the range");
        assert_eq!(b.get(GoalKind::Quest), 1.0, "not a number: skipped");
        assert_eq!(b.get(GoalKind::Farm), 1.0);
        b.apply(&mut s);
        assert_eq!(s[GoalKind::Grind.index()], 2.0);
        assert_eq!(
            s[GoalKind::Rest.index()],
            0.0,
            "a blocked goal stays blocked"
        );

        // wishes nudge farming, unless the operator set it
        b.set(&BTreeMap::new(), true);
        assert_eq!(b.get(GoalKind::Farm), FARM_NUDGE);
        assert_eq!(b.get(GoalKind::Grind), 1.0, "the old biases are gone");
        let mut own = BTreeMap::new();
        own.insert("farm".to_owned(), 0.5);
        b.set(&own, true);
        assert_eq!(b.get(GoalKind::Farm), 0.5);
        b.set(&BTreeMap::new(), false);
        assert!(b.is_neutral());
    }

    #[test]
    fn failed_sessions_count_for_five_minutes_and_are_bounded() {
        let p = Population::inert();
        for _ in 0..3 {
            p.note_failed_session();
        }
        let now = unix_ms();
        assert_eq!(p.failed_sessions(now), 3);
        assert_eq!(p.failed_sessions(now + FAILURE_WINDOW_MS + 1_000), 0);
        for _ in 0..(MAX_FAILURES + 50) {
            p.note_failed_session();
        }
        assert_eq!(p.failed_sessions(unix_ms()), MAX_FAILURES as u32);
    }

    #[test]
    fn owned_bots_are_counted_per_side_and_forgotten_when_gone() {
        let p = Population::inert();
        let row = |id: u32, alliance: pb_web::Alliance, presence: Presence| BotRow {
            id,
            alliance,
            continent: pb_web::Continent::West,
            zone_key: 1,
            x: 0.0,
            y: 0.0,
            presence,
            online_secs: 600,
            calm: true,
        };
        let rows = [
            row(1, pb_web::Alliance::Nuia, Presence::Online),
            row(2, pb_web::Alliance::Nuia, Presence::Connecting),
            row(3, pb_web::Alliance::Haranya, Presence::Online),
            row(4, pb_web::Alliance::Haranya, Presence::Offline),
            row(5, pb_web::Alliance::Nuia, Presence::Online),
        ];
        assert_eq!(p.count_owned(&rows, 1_000), ([0, 0], [0, 0]));
        p.own(&[1, 2, 3, 4, 99], 1_000);
        // 99 has no slot any more; 4 is offline but was queued a moment ago (it may be waiting
        // for the ramp)
        assert_eq!(p.count_owned(&rows, 2_000), ([2, 1], [2, 1]));
        assert!(p.is_owned(4) && !p.is_owned(99));
        // long offline: not the director's any more
        assert_eq!(
            p.count_owned(&rows, 1_000 + DISOWN_AFTER_MS),
            ([2, 1], [2, 1])
        );
        assert!(!p.is_owned(4));
        p.disown(&[1]);
        assert_eq!(p.count_owned(&rows, 3_000), ([1, 1], [1, 1]));
        // a bot that fights, or is new, is not ready to be stopped
        let mut busy = [
            row(1, pb_web::Alliance::Nuia, Presence::Online),
            row(2, pb_web::Alliance::Nuia, Presence::Online),
            row(3, pb_web::Alliance::Haranya, Presence::Online),
        ];
        busy[0].calm = false;
        busy[1].online_secs = director::MIN_STAY_SECS - 1;
        p.own(&[1, 2, 3], 5_000);
        assert_eq!(p.count_owned(&busy, 6_000), ([2, 1], [0, 1]));
        let mut ids = p.owned_ids();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}
