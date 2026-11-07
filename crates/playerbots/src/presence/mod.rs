//! Presence: bots log in and out on schedules of their own, so the world looks played.
//!
//! Every bot has a daily rhythm from its profile ([`schedule`]: when in the day it plays from its
//! `chronotype`, how long and how often from its `dedication`, breaks, the odd skipped day; the
//! same for a bot and a day on every pass). [`Presence::tick`] runs once a second from `Hub::tick`
//! and does a pass every [`PASS_SECS`]: it reads what every bot is doing, asks [`plan::decide`]
//! which bots to log in and out, and hands the answer to the supervisor
//! ([`SupCmd::StartIds`], [`SupCmd::StopIds`]). Starts go through the supervisor's ramp; a bot is
//! only stopped when it is safe (no trade, duel, dungeon, market meeting, boat or party with a
//! real player), at most `max_stops_per_min` a minute. Now and then a bot says a short goodbye
//! first (`goodbye_chance`, only with `[social] enabled`) and logs out at the next pass.
//!
//! # Who decides about which bot
//!
//! * The **population director** owns the bots it started (`Population::is_owned`): presence never
//!   starts, stops or adopts them; they count towards the bounds like every bot online.
//! * A bot the **operator** started by hand is never stopped by presence. A bot the operator
//!   stopped by hand is *held*: presence does not start it again until its schedule ends the
//!   stretch it was in.
//! * Everything else in the roster is the **pool** the schedules draw from. With presence on, the
//!   supervisor starts nobody at launch (`bots.count` only sizes the roster); the first pass logs
//!   in the bots that are scheduled now.
//! * `min_online` and `max_online` bound all bots online together; presence meets them with bots of
//!   its own only.
//!
//! The supervisor tells presence which bots it queued for it ([`Presence::own`]), which it logged
//! out ([`Presence::disown`]) and which the operator stopped ([`Presence::operator_stopped`]). The
//! time is the machine's local time, or UTC plus `utc_offset_hours`.

pub mod plan;
pub mod schedule;

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use pb_web::{Activity, ChronotypeRow, PresenceChange, PresenceView};
use rustc_hash::{FxHashMap, FxHashSet};

use self::plan::{Bounds, Row};
use self::schedule::{Traits, DAY_MIN};
use crate::bot::cmd::BotCmd;
use crate::config::{Config, PresenceCfg};
use crate::hub::{unix_ms, Hub, SupCmd};
use crate::population::select::stop_rank;
use crate::roster::{Chronotype, Profile, SharedRoster};

/// Seconds between two passes.
pub const PASS_SECS: u64 = 20;
/// The first pass comes this long after the supervisor attached presence.
const FIRST_PASS_MS: u64 = 3_000;
/// A bot presence queued that has not shown up after this long is not its own any more.
pub const QUEUE_GRACE_MS: u64 = 5 * 60_000;
/// Rows of the "next changes" list.
const NEXT_ROWS: usize = 8;
/// The curve of the day is recomputed this often.
const CURVE_EVERY_MS: u64 = 10 * 60_000;
/// Log-ins and log-outs remembered for the counts of the last hour.
const MAX_EVENTS: usize = 4096;
/// A goodbye is said at most this long before the log-out; later the bot is just stopped.
const GOODBYE_STALE_MS: u64 = 3 * 60_000;

/// What a bot says when it logs out after a goodbye roll: `(late at night, line)`.
const GOODBYES: [(bool, &str); 8] = [
    (false, "gtg, cya"),
    (false, "ok I'm off, later all"),
    (false, "brb... nah, done for today. bye"),
    (false, "cya tomorrow"),
    (false, "off for now o/"),
    (true, "night all"),
    (true, "bed time, gn"),
    (true, "ok sleep. night"),
];

/// The log-in and log-out schedules. See the module docs. Inert until the supervisor attaches it
/// ([`Presence::attach`]).
#[derive(Default)]
pub struct Presence {
    inner: OnceLock<Inner>,
}

impl std::fmt::Debug for Presence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presence")
            .field("attached", &self.inner.get().is_some())
            .finish_non_exhaustive()
    }
}

struct Inner {
    cfg: PresenceCfg,
    /// `[personality] enabled` (off: every bot plays the neutral profile).
    personality: bool,
    /// `[social] enabled` (off: no goodbyes).
    social: bool,
    roster: Arc<SharedRoster>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The bots presence queued or logged in: id -> unix ms it was queued.
    owned: FxHashMap<u32, u64>,
    /// The bots the operator stopped by hand.
    held: FxHashSet<u32>,
    /// The bots that said goodbye: id -> unix ms.
    leaving: FxHashMap<u32, u64>,
    /// Unix ms of the next pass.
    next_pass_ms: u64,
    /// Unix ms of the log-ins and log-outs presence asked for, oldest first.
    starts: VecDeque<u64>,
    stops: VecDeque<u64>,
    rng: Option<fastrand::Rng>,
    /// What the last pass found, for the web UI.
    last: Option<Box<Snapshot>>,
    /// Bots scheduled per hour of the day `curve.0`, recomputed every [`CURVE_EVERY_MS`].
    curve: (i64, [u32; 24], u64),
}

/// What the last pass found.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    online: u32,
    own_online: u32,
    scheduled: u32,
    target: u32,
    pool: u32,
    waiting: u32,
    /// `(local minute, id, name, logs in)`, soonest first.
    next: Vec<(i64, u32, String, bool)>,
    /// Per chronotype: bots, online, scheduled.
    chrono: [(u32, u32, u32); 4],
}

/// A bot as its slot shows it.
#[derive(Debug, Clone, Copy)]
struct Live {
    online: bool,
    stop_rank: Option<u8>,
    online_secs: u32,
}

impl Presence {
    /// Hands presence the configuration and the roster (the supervisor does this once at start).
    pub fn attach(&self, cfg: &Config, roster: Arc<SharedRoster>) {
        let _ = self.inner.set(Inner {
            cfg: cfg.presence.clone(),
            personality: cfg.personality.enabled,
            social: cfg.social.enabled,
            roster,
            state: Mutex::new(State {
                next_pass_ms: unix_ms() + FIRST_PASS_MS,
                ..State::default()
            }),
        });
    }

    /// Whether the schedules decide who is online (`[presence] enabled`, attached).
    pub fn enabled(&self) -> bool {
        self.inner.get().is_some_and(|i| i.cfg.enabled)
    }

    /// The supervisor queued `ids` for presence.
    pub fn own(&self, ids: &[u32], now_ms: u64) {
        let Some(i) = self.inner.get() else { return };
        let mut st = i.state.lock();
        for id in ids {
            st.owned.insert(*id, now_ms);
            st.held.remove(id);
        }
    }

    /// The supervisor logged `ids` out (or took them off the queue) for presence.
    pub fn disown(&self, ids: &[u32]) {
        let Some(i) = self.inner.get() else { return };
        let mut st = i.state.lock();
        for id in ids {
            st.owned.remove(id);
            st.leaving.remove(id);
        }
    }

    /// The operator stopped `ids` by hand: they are not presence's any more and stay offline until
    /// their schedule lets them go.
    pub fn operator_stopped(&self, ids: &[u32]) {
        let Some(i) = self.inner.get() else { return };
        let mut st = i.state.lock();
        for id in ids {
            st.owned.remove(id);
            st.leaving.remove(id);
            st.held.insert(*id);
        }
    }

    /// Whether presence started bot `id`.
    pub fn is_owned(&self, id: u32) -> bool {
        self.inner
            .get()
            .is_some_and(|i| i.state.lock().owned.contains_key(&id))
    }

    /// Once a second, from `Hub::tick`: a pass every [`PASS_SECS`].
    pub fn tick(&self, hub: &Hub) {
        let Some(inner) = self.inner.get() else {
            return;
        };
        if !inner.cfg.enabled {
            return;
        }
        let now = unix_ms();
        {
            let mut st = inner.state.lock();
            if now < st.next_pass_ms {
                return;
            }
            st.next_pass_ms = now + PASS_SECS * 1000;
        }
        inner.pass(hub, now);
    }

    /// The Director view's presence panel; `None` before the supervisor attached presence.
    pub fn view(&self) -> Option<PresenceView> {
        let inner = self.inner.get()?;
        let cfg = &inner.cfg;
        let (offset_min, local_min) = local_minutes(cfg, unix_ms());
        let mut view = PresenceView {
            enabled: cfg.enabled,
            local_time: clock(local_min),
            utc_offset_hours: offset_min as f32 / 60.0,
            min_online: cfg.min_online,
            max_online: cfg.max_online,
            ..PresenceView::default()
        };
        if !cfg.enabled {
            return Some(view);
        }
        let mut st = inner.state.lock();
        let now = unix_ms();
        let hour_ago = now.saturating_sub(3_600_000);
        view.logins_last_hour = st.starts.iter().filter(|t| **t >= hour_ago).count() as u32;
        view.logouts_last_hour = st.stops.iter().filter(|t| **t >= hour_ago).count() as u32;
        view.held = st.held.len() as u32;
        view.leaving = st.leaving.len() as u32;
        view.curve = st.curve.1.to_vec();
        if let Some(s) = st.last.as_deref_mut() {
            view.online = s.online;
            view.own_online = s.own_online;
            view.scheduled = s.scheduled;
            view.target = s.target;
            view.pool = s.pool;
            view.waiting = s.waiting;
            view.next = s
                .next
                .iter()
                .filter(|n| n.0 > local_min - 1)
                .take(NEXT_ROWS)
                .map(|(at, id, name, login)| PresenceChange {
                    id: *id,
                    name: name.clone(),
                    in_secs: (at - local_min).clamp(0, i64::from(u32::MAX / 60)) as u32 * 60,
                    login: *login,
                })
                .collect();
            view.chronotypes = Chronotype::ALL
                .iter()
                .zip(s.chrono)
                .map(|(c, (bots, online, scheduled))| ChronotypeRow {
                    chronotype: c.name().to_owned(),
                    bots,
                    online,
                    scheduled,
                })
                .collect();
        }
        Some(view)
    }
}

impl Inner {
    /// One pass. See the module docs.
    fn pass(&self, hub: &Hub, now_ms: u64) {
        let (_, local_min) = local_minutes(&self.cfg, now_ms);
        let bots = self.bots();
        let live = live_bots(hub);
        let director: FxHashSet<u32> = hub.pop.owned_ids().into_iter().collect();
        let out = self.decide(now_ms, local_min, &bots, &live, &director);
        // the panel: the next changes with names
        let next = out
            .next
            .iter()
            .map(|&(at, id, login)| {
                let name = hub
                    .slot(id)
                    .map(|s| s.with(|g| g.snap.name.clone()))
                    .unwrap_or_default();
                (at, id, name, login)
            })
            .collect();
        self.state.lock().last = Some(Box::new(Snapshot {
            next,
            ..out.snapshot.clone()
        }));
        // the orders
        for (id, line) in &out.farewells {
            if let Some(slot) = hub.slot(*id) {
                let _ = slot.send(BotCmd::Say((*line).to_owned()));
            }
        }
        if !out.start.is_empty() {
            if let Err(e) = hub.order(SupCmd::StartIds {
                ids: out.start.clone().into_boxed_slice(),
                presence: true,
            }) {
                tracing::debug!("presence: {e}");
            }
        }
        if !out.stop.is_empty() {
            if let Err(e) = hub.order(SupCmd::StopIds {
                ids: out.stop.clone().into_boxed_slice(),
                presence: true,
            }) {
                tracing::debug!("presence: {e}");
            }
        }
        if !out.start.is_empty() || !out.stop.is_empty() || !out.farewells.is_empty() {
            tracing::info!(
                login = out.start.len(),
                logout = out.stop.len(),
                goodbyes = out.farewells.len(),
                busy = out.snapshot.waiting,
                target = out.snapshot.target,
                online = out.snapshot.online,
                local = %clock(local_min),
                "presence"
            );
        }
    }

    /// The roster with the traits the schedules read.
    fn bots(&self) -> Vec<(u32, Traits)> {
        let personality = self.personality;
        self.roster.read(|r| {
            r.entries()
                .iter()
                .map(|e| {
                    let p = if personality {
                        e.profile()
                    } else {
                        Profile::NEUTRAL
                    };
                    (
                        e.id,
                        Traits {
                            dedication: p.dedication,
                            chronotype: p.chronotype,
                        },
                    )
                })
                .collect()
        })
    }

    /// The decision of a pass from what the bots are doing (`live`, by id), which of them the
    /// director owns, and presence's own state; updates the state.
    fn decide(
        &self,
        now_ms: u64,
        local_min: i64,
        bots: &[(u32, Traits)],
        live: &FxHashMap<u32, Live>,
        director: &FxHashSet<u32>,
    ) -> PassOut {
        let cfg = &self.cfg;
        let (day, minute) = schedule::split(local_min);
        let mut st = self.state.lock();
        // queued bots that never showed up are not presence's any more
        st.owned.retain(|id, since| {
            live.get(id).is_some_and(|l| l.online) || now_ms < *since + QUEUE_GRACE_MS
        });
        st.leaving
            .retain(|_, at| now_ms < *at + 2 * GOODBYE_STALE_MS);
        let order_seed = (day as u64).wrapping_mul(0x9e37_79b9);
        let mut rows: Vec<Row> = Vec::with_capacity(bots.len());
        let mut out = PassOut::default();
        for &(id, t) in bots {
            let l = live.get(&id).copied().unwrap_or(Live::OFFLINE);
            let scheduled = schedule::online_at(id, t, local_min);
            let mut held = st.held.contains(&id);
            if held && !scheduled {
                // the stretch the operator cut short is over
                st.held.remove(&id);
                held = false;
            }
            let edge = schedule::next_edge(id, t, local_min);
            let row = Row {
                id,
                online: l.online,
                owned: st.owned.contains_key(&id),
                director: director.contains(&id),
                scheduled,
                held,
                stop_rank: l.stop_rank,
                online_secs: l.online_secs,
                next_login_min: match edge {
                    Some((at, true)) => at - local_min,
                    _ => i64::MAX,
                },
                order: mix(u64::from(id) ^ order_seed),
            };
            let c = Chronotype::ALL
                .iter()
                .position(|c| *c == t.chronotype)
                .unwrap_or(1);
            let chrono = &mut out.snapshot.chrono[c];
            chrono.0 += 1;
            chrono.1 += u32::from(l.online);
            chrono.2 += u32::from(scheduled && !row.director);
            if !row.director {
                if let Some((at, login)) = edge {
                    out.next.push((at, id, login));
                }
            }
            rows.push(row);
        }
        let minute_ago = now_ms.saturating_sub(60_000);
        let per_min = |q: &VecDeque<u64>| q.iter().filter(|t| **t >= minute_ago).count();
        let starts_left = (cfg.max_starts_per_min as usize).saturating_sub(per_min(&st.starts));
        let stops_left = (cfg.max_stops_per_min as usize).saturating_sub(per_min(&st.stops));
        let orders = plan::decide(
            &rows,
            Bounds {
                min_online: cfg.min_online,
                max_online: cfg.max_online,
            },
            starts_left,
            stops_left,
        );
        // the log-outs: a goodbye now and then, the log-out at the next pass
        let late = !(6 * 60..22 * 60).contains(&minute);
        let mut rng = st
            .rng
            .take()
            .unwrap_or_else(|| fastrand::Rng::with_seed(now_ms));
        for &id in &orders.stop {
            let online = live.get(&id).is_some_and(|l| l.online);
            let announced = st
                .leaving
                .get(&id)
                .is_some_and(|at| now_ms < at + GOODBYE_STALE_MS);
            if online
                && !announced
                && self.social
                && cfg.goodbye_chance > 0.0
                && rng.f32() < cfg.goodbye_chance
            {
                out.farewells.push((id, goodbye(&mut rng, late)));
                st.leaving.insert(id, now_ms);
                continue;
            }
            if online {
                push_bounded(&mut st.stops, now_ms);
            }
            out.stop.push(id);
        }
        st.rng = Some(rng);
        for _ in 0..orders.start.len() {
            push_bounded(&mut st.starts, now_ms);
        }
        out.start = orders.start;
        out.next.sort_unstable();
        out.next.truncate(NEXT_ROWS * 2);
        let s = &mut out.snapshot;
        s.online = rows.iter().filter(|r| r.online).count() as u32;
        s.own_online = rows.iter().filter(|r| r.owned && r.online).count() as u32;
        s.scheduled = rows.iter().filter(|r| r.scheduled && !r.director).count() as u32;
        s.pool = rows.iter().filter(|r| !r.director).count() as u32;
        s.target = orders.target;
        s.waiting = orders.waiting;
        if st.curve.0 != day || now_ms >= st.curve.2 {
            st.curve = (day, schedule::curve(bots, day), now_ms + CURVE_EVERY_MS);
        }
        out
    }
}

/// What a pass decided.
#[derive(Debug, Default)]
struct PassOut {
    start: Vec<u32>,
    stop: Vec<u32>,
    farewells: Vec<(u32, &'static str)>,
    /// `(local minute, id, logs in)`, soonest first.
    next: Vec<(i64, u32, bool)>,
    snapshot: Snapshot,
}

impl Live {
    const OFFLINE: Live = Live {
        online: false,
        stop_rank: None,
        online_secs: 0,
    };
}

/// What the slots show of every bot.
fn live_bots(hub: &Hub) -> FxHashMap<u32, Live> {
    hub.slots()
        .iter()
        .map(|s| {
            s.with(|g| {
                let b = &g.snap;
                // (a bot in the party of a real player is never logged out under it)
                let with_player = b
                    .party
                    .as_ref()
                    .is_some_and(|p| p.members.iter().any(|m| !m.bot && m.online));
                let rank = if with_player {
                    None
                } else {
                    stop_rank(b.activity, &b.goal)
                };
                (
                    b.id,
                    Live {
                        online: !matches!(b.activity, Activity::Offline),
                        stop_rank: rank,
                        online_secs: b.online_secs,
                    },
                )
            })
        })
        .collect()
}

// A goodbye line; the "good night" ones only `late`.
fn goodbye(rng: &mut fastrand::Rng, late: bool) -> &'static str {
    let n = GOODBYES.iter().filter(|g| !g.0 || late).count();
    let k = rng.usize(0..n.max(1));
    GOODBYES
        .iter()
        .filter(|g| !g.0 || late)
        .nth(k)
        .map_or("cya", |g| g.1)
}

fn push_bounded(q: &mut VecDeque<u64>, t: u64) {
    if q.len() >= MAX_EVENTS {
        q.pop_front();
    }
    q.push_back(t);
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The offset of the schedules' clock from UTC and the local time, both in minutes (the local
/// time as minutes since 1970-01-01 local).
pub fn local_minutes(cfg: &PresenceCfg, now_ms: u64) -> (i64, i64) {
    let offset_min = match cfg.utc_offset_hours {
        Some(h) => (f64::from(h) * 60.0).round() as i64,
        None => {
            use chrono::Offset;
            let secs = i64::try_from(now_ms / 1000).unwrap_or(0);
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|utc| {
                    let local = utc.with_timezone(&chrono::Local);
                    i64::from(local.offset().fix().local_minus_utc()) / 60
                })
                .unwrap_or(0)
        }
    };
    let utc_min = i64::try_from(now_ms / 60_000).unwrap_or(0);
    (offset_min, utc_min + offset_min)
}

/// `HH:MM` of a local minute.
fn clock(local_min: i64) -> String {
    let m = local_min.rem_euclid(DAY_MIN);
    format!("{:02}:{:02}", m / 60, m % 60)
}

#[cfg(test)]
mod tests;
