//! The population director: logs bots in and out by what the census sees.
//!
//! (Not to be confused with `bot::director`, the per-bot LLM goal picker.)
//!
//! Once a second the hub hands the director what it knows: how many real players each side has in
//! view, how many bots each side has, how many of those the director itself started, and the
//! health of the bot process. [`plan`] is a pure function of those numbers and the [`Policy`]; the
//! rules are
//!
//! * **Mirror.** A side aims for `mirror_pct` characters (players plus bots) per 100 players on the
//!   other side: when the east fills up with players, the west gets bots. At least `min_per_side`,
//!   at most `per_side_cap`.
//! * **Total.** It never starts a bot that would take the bots in the world above `max_online`.
//! * **Pace.** One action at a time, `min_action_gap_secs` apart (a hard limit from the
//!   configuration), `step` bots at most per action, `max_spawn_per_min` bots per minute (also hard).
//! * **Hysteresis.** A side starts getting bots as soon as it is short of one, but loses bots only
//!   when it has more than `hysteresis` above its target, and then only bots the director
//!   started, that have been up for [`MIN_STAY_SECS`], and that are safe to stop (see
//!   [`super::select::stop_rank`]); the whole surplus goes at once, so a player at the edge of a
//!   bot's view does not make bots come and go.
//! * **Back-off.** While the process is over a limit of the policy (memory, packets per second,
//!   failed sessions, open server issues) it starts no bots; stopping some is still allowed.
//! * **Dry run.** With `dry_run` it writes down what it would do and does nothing. Every action,
//!   proposal and refusal is in the log the web UI shows, and in the process log (`pb_pop`).
//!
//! The kill switch is `Policy::active`: off, the director does nothing at all.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use pb_web::{
    ActionRecord, Alliance, BoostRequest, BoostWish, DirectorView, HealthView, Policy, PolicyPatch,
    SideRow,
};

use super::select::{Selector, StopRule};
use crate::config::{PopulationCfg, MAX_BOTS};

/// A bot the director started stays up at least this long before the director stops it.
pub const MIN_STAY_SECS: u32 = 180;
/// Log lines kept.
const MAX_LOG: usize = 200;
/// Bots just started count towards their side for this long, until they show as online.
const LOGIN_GRACE_MS: u64 = 120_000;
/// A dry run says the same thing at most this often.
const REPEAT_MS: u64 = 5 * 60_000;
/// The automatic wishes are looked at this often.
const AUTO_EVERY_MS: u64 = 60_000;
/// A wish made by the director lasts this long.
const AUTO_WISH_MS: u64 = 30 * 60_000;

/// The limits that the configuration sets and the policy cannot lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bots the director starts per minute.
    pub max_spawn_per_min: u32,
    /// Seconds between two of its actions.
    pub min_action_gap_secs: u32,
}

impl Limits {
    /// The limits of a configuration.
    pub fn of(cfg: &PopulationCfg) -> Limits {
        Limits {
            max_spawn_per_min: cfg.max_spawn_per_min,
            min_action_gap_secs: cfg.min_action_gap_secs.min(u64::from(u32::MAX)) as u32,
        }
    }
}

/// What [`plan`] works from.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub policy: &'a Policy,
    pub limits: Limits,
    /// Players seen per side `[Nuia, Haranya]`.
    pub players: [u32; 2],
    /// Bots in the world or logging in, per side (the ones the director just started included).
    pub bots: [u32; 2],
    /// Of those, the ones the director started and may stop.
    pub owned: [u32; 2],
    /// Bots in the world or logging in, all together.
    pub total_online: u32,
    /// Why starting bots is on hold (see [`holding_reason`]).
    pub holding: Option<String>,
    /// Milliseconds since the last action; `u64::MAX` when there was none.
    pub since_action_ms: u64,
    /// Bots started in the last minute.
    pub spawned_last_min: u32,
}

/// What the director does about one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Start `n` bots of side `side` (0 Nuia, 1 Haranya).
    Spawn { side: usize, n: u32 },
    /// Stop `n` bots of side `side`.
    Despawn { side: usize, n: u32 },
}

/// The outcome of [`plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// How many bots each side should have.
    pub targets: [u32; 2],
    /// The action to take now, if any.
    pub act: Option<Act>,
    /// Why, in words (for the action), or why not (when a side is short and nothing happens).
    pub why: String,
}

/// How many bots each side should have.
pub fn targets(p: &Policy, players: [u32; 2]) -> [u32; 2] {
    let mut t = [0u32; 2];
    for (s, slot) in t.iter_mut().enumerate() {
        let other = players[1 - s];
        let mirror = (u64::from(p.mirror_pct) * u64::from(other)).div_ceil(100) as u32;
        let want = mirror.saturating_sub(players[s]).max(p.min_per_side);
        *slot = want.min(p.per_side_cap);
    }
    t
}

fn side_name(side: usize) -> &'static str {
    Alliance::from_side(side).as_str()
}

/// Why the process is too loaded to take on more bots, if it is.
pub fn holding_reason(p: &Policy, h: &HealthView) -> Option<String> {
    if p.backoff_rss_mb > 0 && h.rss_mb >= p.backoff_rss_mb {
        return Some(format!(
            "memory is at {} MB (limit {})",
            h.rss_mb, p.backoff_rss_mb
        ));
    }
    if p.backoff_packets_in > 0 && h.packets_in_per_sec >= p.backoff_packets_in as f32 {
        return Some(format!(
            "{:.0} packets per second arrive (limit {})",
            h.packets_in_per_sec, p.backoff_packets_in
        ));
    }
    if p.backoff_login_failures > 0 && h.login_failures >= p.backoff_login_failures {
        return Some(format!(
            "{} sessions failed in five minutes (limit {})",
            h.login_failures, p.backoff_login_failures
        ));
    }
    if p.backoff_open_issues > 0 && h.open_issues >= p.backoff_open_issues {
        return Some(format!(
            "{} server issues are open (limit {})",
            h.open_issues, p.backoff_open_issues
        ));
    }
    None
}

/// What the director does now. Pure; see the module docs for the rules.
pub fn plan(i: &Inputs<'_>) -> Plan {
    let p = i.policy;
    let t = targets(p, i.players);
    let gap_ms = u64::from(i.limits.min_action_gap_secs) * 1000;
    let ready = i.since_action_ms >= gap_ms;
    let wait_secs = gap_ms.saturating_sub(i.since_action_ms).div_ceil(1000);
    let line = |s: usize| {
        format!(
            "{} wants {} bots ({} players on the {} side, {} on its own); it has {}",
            side_name(s),
            t[s],
            i.players[1 - s],
            side_name(1 - s),
            i.players[s],
            i.bots[s],
        )
    };

    // a side that is short of bots: the shortest first
    let short = (0..2)
        .filter(|s| i.bots[*s] < t[*s])
        .max_by_key(|s| (t[*s] - i.bots[*s], 1 - *s));
    if let Some(s) = short {
        let deficit = t[s] - i.bots[s];
        let why = line(s);
        if let Some(reason) = &i.holding {
            return Plan {
                targets: t,
                act: None,
                why: format!("{why}; on hold: {reason}"),
            };
        }
        if !ready {
            return Plan {
                targets: t,
                act: None,
                why: format!("{why}; next action in {wait_secs} s"),
            };
        }
        let room = p.max_online.min(MAX_BOTS).saturating_sub(i.total_online);
        if room == 0 {
            return Plan {
                targets: t,
                act: None,
                why: format!("{why}; at the limit of {} bots online", p.max_online),
            };
        }
        let per_min = i
            .limits
            .max_spawn_per_min
            .saturating_sub(i.spawned_last_min);
        if per_min == 0 {
            return Plan {
                targets: t,
                act: None,
                why: format!(
                    "{why}; at the limit of {} bots per minute",
                    i.limits.max_spawn_per_min
                ),
            };
        }
        let n = deficit.min(p.step.max(1)).min(room).min(per_min);
        return Plan {
            targets: t,
            act: Some(Act::Spawn { side: s, n }),
            why,
        };
    }

    // a side with more bots than it needs: the biggest surplus first, but only past the dead band
    if p.despawn {
        let long = (0..2)
            .filter(|s| i.bots[*s] > t[*s] + p.hysteresis && i.owned[*s] > 0)
            .max_by_key(|s| (i.bots[*s] - t[*s], 1 - *s));
        if let Some(s) = long {
            let surplus = i.bots[s] - t[s];
            let why = format!(
                "{} has {} bots and needs {} ({} players on the {} side, {} on its own)",
                side_name(s),
                i.bots[s],
                t[s],
                i.players[1 - s],
                side_name(1 - s),
                i.players[s]
            );
            if !ready {
                return Plan {
                    targets: t,
                    act: None,
                    why: format!("{why}; next action in {wait_secs} s"),
                };
            }
            let n = surplus.min(p.step.max(1)).min(i.owned[s]);
            return Plan {
                targets: t,
                act: Some(Act::Despawn { side: s, n }),
                why,
            };
        }
    }

    Plan {
        targets: t,
        act: None,
        why: String::new(),
    }
}

/// What the supervisor is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Start `n` bots that match `sel`; the director owns them.
    Spawn { sel: Selector, n: u32 },
    /// Stop up to `n` bots that match `sel`, under `rule`.
    Despawn {
        sel: Selector,
        n: u32,
        rule: StopRule,
    },
}

/// What [`Director::step`] needs from the hub each second.
#[derive(Debug, Clone)]
pub struct StepInputs {
    pub now_ms: u64,
    pub players: [u32; 2],
    pub bots: [u32; 2],
    pub owned: [u32; 2],
    pub total_online: u32,
    pub health: HealthView,
}

#[derive(Debug)]
struct State {
    policy: Policy,
    log: VecDeque<ActionRecord>,
    last_action_ms: Option<u64>,
    /// `(ms, bots)` of the bots started in the last minute.
    spawns: VecDeque<(u64, u32)>,
    /// What a dry run said last: `(kind, side, n)` and when.
    last_proposal: Option<((&'static str, Alliance, u32), u64)>,
    /// Per side: at least this many bots until `.1` (the bots just started are still logging in).
    floor: [(u32, u64); 2],
    sides: [SideRow; 2],
    holding: Option<String>,
    next_action_secs: u32,
    spawned_last_min: u32,
    auto_wishes: Vec<(u32, u64)>,
    last_auto_ms: u64,
}

/// The director: the policy, its log and the pacing. See the module docs.
#[derive(Debug)]
pub struct Director {
    state: Mutex<State>,
    limits: Limits,
    /// `[population] enabled`: the policy cannot be switched on otherwise.
    available: bool,
    file: Option<PathBuf>,
}

impl Director {
    /// A director for `cfg`; the policy file, if there is one, overrides the configuration's start
    /// values.
    pub fn new(cfg: &PopulationCfg) -> Director {
        let mut policy = cfg.policy();
        if let Some(path) = &cfg.policy_file {
            match load_policy(path) {
                Ok(Some(p)) => policy = p,
                Ok(None) => {}
                Err(e) => tracing::warn!(target: "pb_pop", "policy file {}: {e}", path.display()),
            }
        }
        if !cfg.enabled {
            policy.active = false;
        }
        Director {
            state: Mutex::new(State {
                policy,
                log: VecDeque::new(),
                last_action_ms: None,
                spawns: VecDeque::new(),
                last_proposal: None,
                floor: [(0, 0); 2],
                sides: [
                    SideRow {
                        alliance: Alliance::Nuia,
                        ..SideRow::default()
                    },
                    SideRow {
                        alliance: Alliance::Haranya,
                        ..SideRow::default()
                    },
                ],
                holding: None,
                next_action_secs: 0,
                spawned_last_min: 0,
                auto_wishes: Vec::new(),
                last_auto_ms: 0,
            }),
            limits: Limits::of(cfg),
            available: cfg.enabled,
            file: cfg.policy_file.clone(),
        }
    }

    /// Whether the policy may be switched on.
    pub fn available(&self) -> bool {
        self.available
    }

    /// The limits from the configuration.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// A copy of the policy.
    pub fn policy(&self) -> Policy {
        self.state.lock().policy.clone()
    }

    /// Applies a change the operator made. The numbers were checked by the web layer; this checks
    /// what only the director knows (it may be switched on only when the configuration allows it),
    /// keeps the file, and writes the change down.
    pub fn apply(&self, patch: PolicyPatch, now_ms: u64) -> Result<(), String> {
        if patch.active == Some(true) && !self.available {
            return Err(
                "the director is switched off in the configuration; set [population] enabled = true"
                    .to_owned(),
            );
        }
        let mut st = self.state.lock();
        let before = st.policy.clone();
        let p = &mut st.policy;
        macro_rules! set {
            ($($f:ident),* $(,)?) => { $( if let Some(v) = patch.$f { p.$f = v; } )* };
        }
        set!(
            active,
            dry_run,
            max_online,
            min_per_side,
            mirror_pct,
            per_side_cap,
            step,
            hysteresis,
            despawn,
            backoff_rss_mb,
            backoff_packets_in,
            backoff_login_failures,
            backoff_open_issues,
            auto_boost_min,
            auto_boost,
        );
        p.max_online = p.max_online.min(MAX_BOTS);
        if let Some(b) = patch.boosts {
            p.boosts = boosts_from(&b, now_ms);
        }
        if let Some(g) = patch.goal_bias {
            p.goal_bias = g;
        }
        if st.policy == before {
            return Ok(());
        }
        let what = describe_change(&before, &st.policy);
        let policy = st.policy.clone();
        push_log(
            &mut st.log,
            ActionRecord {
                at_ms: now_ms,
                kind: "policy".into(),
                alliance: Alliance::Unknown,
                count: 0,
                applied: true,
                reason: what.clone(),
                result: String::new(),
            },
        );
        drop(st);
        tracing::info!(target: "pb_pop", "policy changed: {what}");
        if let Some(path) = &self.file {
            if let Err(e) = save_policy(path, &policy) {
                tracing::warn!(target: "pb_pop", "saving the policy to {}: {e:#}", path.display());
            }
        }
        Ok(())
    }

    /// The items to grow: the operator's wishes that have not run out, then the automatic ones.
    pub fn wishes(&self, now_ms: u64) -> Vec<u32> {
        let st = self.state.lock();
        let mut v: Vec<u32> = st
            .policy
            .boosts
            .iter()
            .filter(|b| b.until_ms == 0 || b.until_ms > now_ms)
            .map(|b| b.template)
            .collect();
        for (t, until) in &st.auto_wishes {
            if *until > now_ms && !v.contains(t) {
                v.push(*t);
            }
        }
        v
    }

    /// Sets the automatic wishes from the scarcest items (a minute apart at most), and writes down
    /// the ones that are new. `scarce` is `(template, score)`, most scarce first.
    pub fn update_auto(&self, now_ms: u64, scarce: impl FnOnce(usize, f32) -> Vec<(u32, f32)>) {
        let mut st = self.state.lock();
        if now_ms.saturating_sub(st.last_auto_ms) < AUTO_EVERY_MS {
            return;
        }
        st.last_auto_ms = now_ms;
        st.auto_wishes.retain(|(_, until)| *until > now_ms);
        let (n, min) = (st.policy.auto_boost as usize, st.policy.auto_boost_min);
        if !st.policy.active || n == 0 {
            st.auto_wishes.clear();
            return;
        }
        let manual: Vec<u32> = st.policy.boosts.iter().map(|b| b.template).collect();
        let found = scarce(n, min);
        let mut fresh = Vec::new();
        for (t, score) in found {
            if manual.contains(&t) {
                continue;
            }
            if let Some(w) = st.auto_wishes.iter_mut().find(|w| w.0 == t) {
                w.1 = now_ms + AUTO_WISH_MS;
            } else if st.auto_wishes.len() < n {
                st.auto_wishes.push((t, now_ms + AUTO_WISH_MS));
                fresh.push((t, score));
            }
        }
        for (t, score) in fresh {
            push_log(
                &mut st.log,
                ActionRecord {
                    at_ms: now_ms,
                    kind: "wish".into(),
                    alliance: Alliance::Unknown,
                    count: 1,
                    applied: true,
                    reason: format!(
                        "item {t} scores {score:.2} for scarcity; it is on the wish list"
                    ),
                    result: String::new(),
                },
            );
            tracing::info!(target: "pb_pop", "item {t} is scarce ({score:.2}); wished");
        }
    }

    /// One look at the world. Returns nothing to do, or hands the order to `send` (which returns
    /// what came of it, or why it was refused) unless the policy is a dry run.
    pub fn step(&self, i: &StepInputs, send: &mut dyn FnMut(Order) -> Result<String, String>) {
        let mut st = self.state.lock();
        // the bots started in the last minute
        while st
            .spawns
            .front()
            .is_some_and(|(t, _)| i.now_ms.saturating_sub(*t) >= 60_000)
        {
            st.spawns.pop_front();
        }
        let spawned_last_min: u32 = st.spawns.iter().map(|(_, n)| *n).sum();
        st.spawned_last_min = spawned_last_min;
        // bots just started are counted until they show as online
        let mut bots = i.bots;
        for (s, b) in bots.iter_mut().enumerate() {
            let (floor, until) = st.floor[s];
            if i.now_ms < until {
                *b = (*b).max(floor);
            }
        }
        let total_online = i.total_online.max(bots.iter().sum());
        let holding = holding_reason(&st.policy, &i.health);
        st.holding = holding.clone();
        let since = st
            .last_action_ms
            .map_or(u64::MAX, |t| i.now_ms.saturating_sub(t));
        let policy = st.policy.clone();
        let plan = plan(&Inputs {
            policy: &policy,
            limits: self.limits,
            players: i.players,
            bots,
            owned: i.owned,
            total_online,
            holding,
            since_action_ms: since,
            spawned_last_min,
        });
        st.sides = std::array::from_fn(|s| SideRow {
            alliance: Alliance::from_side(s),
            players: i.players[s],
            bots: bots[s],
            owned: i.owned[s],
            target: plan.targets[s],
        });
        st.next_action_secs = self
            .limits
            .min_action_gap_secs
            .saturating_sub((since / 1000).min(u64::from(u32::MAX)) as u32);
        if !policy.active {
            return;
        }
        let Some(act) = plan.act else {
            // a refusal worth a line: a side is short and something stops the director
            if !plan.why.is_empty() && is_blocked(&plan.why) {
                let key = ("hold", Alliance::Unknown, 0);
                if repeats(&st.last_proposal, key, i.now_ms) {
                    return;
                }
                st.last_proposal = Some((key, i.now_ms));
                push_log(
                    &mut st.log,
                    ActionRecord {
                        at_ms: i.now_ms,
                        kind: "hold".into(),
                        alliance: Alliance::Unknown,
                        count: 0,
                        applied: false,
                        reason: plan.why.clone(),
                        result: String::new(),
                    },
                );
                tracing::info!(target: "pb_pop", "holding: {}", plan.why);
            }
            return;
        };
        let (kind, side, n, order) = match act {
            Act::Spawn { side, n } => (
                "spawn",
                side,
                n,
                Order::Spawn {
                    sel: Selector::side(Alliance::from_side(side)),
                    n,
                },
            ),
            Act::Despawn { side, n } => (
                "despawn",
                side,
                n,
                Order::Despawn {
                    sel: Selector::side(Alliance::from_side(side)),
                    n,
                    rule: StopRule {
                        owned_only: true,
                        min_age_secs: MIN_STAY_SECS,
                    },
                },
            ),
        };
        let alliance = Alliance::from_side(side);
        if policy.dry_run {
            let key = (kind, alliance, n);
            if repeats(&st.last_proposal, key, i.now_ms) {
                return;
            }
            st.last_proposal = Some((key, i.now_ms));
            push_log(
                &mut st.log,
                ActionRecord {
                    at_ms: i.now_ms,
                    kind: kind.into(),
                    alliance,
                    count: n,
                    applied: false,
                    reason: plan.why.clone(),
                    result: "dry run: nothing was done".into(),
                },
            );
            tracing::info!(target: "pb_pop", "dry run: would {kind} {n} {} bots: {}", alliance.as_str(), plan.why);
            return;
        }
        // live: the lock is held across the call (the supervisor's queue is a try_send)
        let result = send(order);
        st.last_action_ms = Some(i.now_ms);
        st.last_proposal = None;
        let (applied, text) = match result {
            Ok(t) => (true, t),
            Err(e) => (false, e),
        };
        if applied && kind == "spawn" {
            st.spawns.push_back((i.now_ms, n));
            st.floor[side] = (bots[side] + n, i.now_ms + LOGIN_GRACE_MS);
        }
        tracing::info!(
            target: "pb_pop",
            "{kind} {n} {} bots: {} -> {text}",
            alliance.as_str(),
            plan.why
        );
        push_log(
            &mut st.log,
            ActionRecord {
                at_ms: i.now_ms,
                kind: kind.into(),
                alliance,
                count: n,
                applied,
                reason: plan.why,
                result: text,
            },
        );
    }

    /// Writes a line into the log (the supervisor reports what it did).
    pub fn note(&self, rec: ActionRecord) {
        push_log(&mut self.state.lock().log, rec);
    }

    /// The Director tab. `health` and `owned_total` come from the hub.
    pub fn view(&self, health: HealthView, owned_total: u32) -> DirectorView {
        let st = self.state.lock();
        let mode = if !st.policy.active {
            "off"
        } else if st.holding.is_some() {
            "holding"
        } else if st.policy.dry_run {
            "dry_run"
        } else {
            "live"
        };
        DirectorView {
            available: self.available,
            policy: st.policy.clone(),
            mode: mode.to_owned(),
            sides: st.sides.to_vec(),
            health: HealthView {
                holding: st.holding.clone(),
                ..health
            },
            owned: owned_total,
            next_action_secs: st.next_action_secs,
            spawned_last_min: st.spawned_last_min,
            max_spawn_per_min: self.limits.max_spawn_per_min,
            min_action_gap_secs: self.limits.min_action_gap_secs,
            auto_wishes: st.auto_wishes.iter().map(|(t, _)| *t).collect(),
            log: st.log.iter().cloned().collect(),
        }
    }
}

/// Whether the plan's text is the kind that says "a side is short but something holds it back".
fn is_blocked(why: &str) -> bool {
    why.contains("on hold") || why.contains("at the limit")
}

/// Whether a proposal was made already (not long ago).
fn repeats(
    last: &Option<((&'static str, Alliance, u32), u64)>,
    key: (&'static str, Alliance, u32),
    now_ms: u64,
) -> bool {
    last.as_ref()
        .is_some_and(|(k, t)| *k == key && now_ms.saturating_sub(*t) < REPEAT_MS)
}

fn push_log(log: &mut VecDeque<ActionRecord>, rec: ActionRecord) {
    if log.len() >= MAX_LOG {
        log.pop_back();
    }
    log.push_front(rec);
}

/// The wishes an operator's request makes.
pub fn boosts_from(req: &[BoostRequest], now_ms: u64) -> Vec<BoostWish> {
    let mut out: Vec<BoostWish> = Vec::new();
    for r in req {
        if out.iter().any(|w| w.template == r.template) {
            continue;
        }
        out.push(BoostWish {
            template: r.template,
            until_ms: if r.hours == 0 {
                0
            } else {
                now_ms + u64::from(r.hours) * 3_600_000
            },
            auto: false,
        });
    }
    out.truncate(Policy::MAX_BOOSTS);
    out
}

/// A line that says what changed between two policies.
fn describe_change(a: &Policy, b: &Policy) -> String {
    let mut parts: Vec<String> = Vec::new();
    macro_rules! diff {
        ($($f:ident),* $(,)?) => { $( if a.$f != b.$f { parts.push(format!("{} {} -> {}", stringify!($f), a.$f, b.$f)); } )* };
    }
    diff!(
        active,
        dry_run,
        max_online,
        min_per_side,
        mirror_pct,
        per_side_cap,
        step,
        hysteresis,
        despawn,
        backoff_rss_mb,
        backoff_packets_in,
        backoff_login_failures,
        backoff_open_issues,
        auto_boost_min,
        auto_boost,
    );
    if a.boosts != b.boosts {
        parts.push(format!("wishes: {} item(s)", b.boosts.len()));
    }
    if a.goal_bias != b.goal_bias {
        parts.push(format!("goal bias: {} goal(s)", b.goal_bias.len()));
    }
    format!("policy changed by the operator: {}", parts.join(", "))
}

/// Reads a saved policy; `None` when there is no file.
pub fn load_policy(path: &Path) -> anyhow::Result<Option<Policy>> {
    use anyhow::Context;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("reading the policy file"),
    };
    let mut p: Policy = serde_json::from_str(&text).context("parsing the policy file")?;
    p.max_online = p.max_online.min(MAX_BOTS);
    p.boosts.truncate(Policy::MAX_BOOSTS);
    Ok(Some(p))
}

/// Writes the policy to `path` atomically.
pub fn save_policy(path: &Path, policy: &Policy) -> anyhow::Result<()> {
    crate::roster::write_atomic(path, &serde_json::to_string_pretty(policy)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_spawn_per_min: 10,
        min_action_gap_secs: 60,
    };

    fn policy() -> Policy {
        Policy {
            active: true,
            dry_run: false,
            max_online: 40,
            min_per_side: 0,
            mirror_pct: 100,
            per_side_cap: 20,
            step: 3,
            hysteresis: 2,
            despawn: true,
            ..Policy::default()
        }
    }

    fn inputs(p: &Policy) -> Inputs<'_> {
        Inputs {
            policy: p,
            limits: LIMITS,
            players: [0, 0],
            bots: [0, 0],
            owned: [0, 0],
            total_online: 0,
            holding: None,
            since_action_ms: u64::MAX,
            spawned_last_min: 0,
        }
    }

    #[test]
    fn a_side_mirrors_the_players_of_the_other_side() {
        let p = policy();
        // east (Haranya) has 7 players, west none: the west wants 7 characters
        assert_eq!(targets(&p, [0, 7]), [7, 0]);
        // west has 2 players itself: it needs 5 bots to match
        assert_eq!(targets(&p, [2, 7]), [5, 0]);
        // west has more players than the east: the east wants the difference, the west nothing
        assert_eq!(targets(&p, [9, 3]), [0, 6]);
        assert_eq!(targets(&p, [0, 0]), [0, 0]);
        // 50 percent, rounded up; 150 percent
        let half = Policy {
            mirror_pct: 50,
            ..p.clone()
        };
        assert_eq!(targets(&half, [0, 7]), [4, 0]);
        let more = Policy {
            mirror_pct: 150,
            ..p.clone()
        };
        assert_eq!(targets(&more, [0, 4]), [6, 0]);
        // a floor, and a cap that wins over the floor
        let floor = Policy {
            min_per_side: 3,
            ..p.clone()
        };
        assert_eq!(targets(&floor, [0, 0]), [3, 3]);
        let cap = Policy {
            per_side_cap: 4,
            min_per_side: 6,
            ..p.clone()
        };
        assert_eq!(targets(&cap, [0, 30]), [4, 4]);
        // 0 percent: only the floor
        let none = Policy { mirror_pct: 0, ..p };
        assert_eq!(targets(&none, [0, 30]), [0, 0]);
    }

    #[test]
    fn a_short_side_gets_bots_up_to_the_step() {
        let p = policy();
        let plan = plan(&Inputs {
            players: [0, 7],
            bots: [1, 0],
            total_online: 1,
            ..inputs(&p)
        });
        assert_eq!(plan.targets, [7, 0]);
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 3 }));
        assert!(plan.why.contains("nuia wants 7 bots"), "{}", plan.why);
        assert!(
            plan.why.contains("7 players on the haranya side"),
            "{}",
            plan.why
        );
        // the last few
        let plan = super::plan(&Inputs {
            players: [0, 7],
            bots: [6, 0],
            total_online: 6,
            ..inputs(&p)
        });
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 1 }));
        // nothing is short: nothing happens, and nothing is said
        let plan = super::plan(&Inputs {
            players: [0, 7],
            bots: [7, 0],
            total_online: 7,
            ..inputs(&p)
        });
        assert_eq!(plan.act, None);
        assert!(plan.why.is_empty());
    }

    #[test]
    fn the_side_with_the_bigger_deficit_goes_first() {
        let p = policy();
        let plan = plan(&Inputs {
            players: [3, 9],
            bots: [0, 0],
            ..inputs(&p)
        });
        // west wants 6, east wants 0
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 3 }));
        let floor = Policy {
            min_per_side: 2,
            ..p.clone()
        };
        let plan = super::plan(&Inputs {
            players: [5, 5],
            bots: [0, 0],
            ..inputs(&floor)
        });
        // equal deficits: Nuia first
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 2 }));
        let plan = super::plan(&Inputs {
            players: [5, 5],
            bots: [2, 0],
            ..inputs(&floor)
        });
        assert_eq!(plan.act, Some(Act::Spawn { side: 1, n: 2 }));
    }

    #[test]
    fn the_total_the_pace_and_the_per_minute_limit_cap_a_spawn() {
        let p = policy();
        let base = Inputs {
            players: [0, 20],
            ..inputs(&p)
        };
        // 40 online of 40: no room
        let plan = plan(&Inputs {
            total_online: 40,
            bots: [0, 40],
            ..base.clone()
        });
        assert_eq!(plan.act, None);
        assert!(plan.why.contains("limit of 40 bots online"), "{}", plan.why);
        // 38 online: room for two
        let plan = super::plan(&Inputs {
            total_online: 38,
            bots: [0, 38],
            ..base.clone()
        });
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 2 }));
        // eight started in the last minute of ten: two more
        let plan = super::plan(&Inputs {
            spawned_last_min: 8,
            ..base.clone()
        });
        assert_eq!(plan.act, Some(Act::Spawn { side: 0, n: 2 }));
        let plan = super::plan(&Inputs {
            spawned_last_min: 10,
            ..base.clone()
        });
        assert_eq!(plan.act, None);
        assert!(plan.why.contains("10 bots per minute"), "{}", plan.why);
        // 20 s after the last action of a 60 s gap
        let plan = super::plan(&Inputs {
            since_action_ms: 20_000,
            ..base.clone()
        });
        assert_eq!(plan.act, None);
        assert!(plan.why.contains("next action in 40 s"), "{}", plan.why);
        let plan = super::plan(&Inputs {
            since_action_ms: 60_000,
            ..base.clone()
        });
        assert!(plan.act.is_some());
        // a limit of 0 per minute starts nothing
        let plan = super::plan(&Inputs {
            limits: Limits {
                max_spawn_per_min: 0,
                ..LIMITS
            },
            ..base
        });
        assert_eq!(plan.act, None);
    }

    #[test]
    fn nothing_is_started_while_the_process_is_over_a_limit() {
        let p = policy();
        let plan = plan(&Inputs {
            players: [0, 7],
            holding: Some("memory is at 4000 MB (limit 3072)".into()),
            ..inputs(&p)
        });
        assert_eq!(plan.act, None);
        assert!(plan.why.contains("on hold: memory"), "{}", plan.why);
        // the hold does not stop a despawn
        let plan = super::plan(&Inputs {
            players: [0, 0],
            bots: [10, 0],
            owned: [10, 0],
            total_online: 10,
            holding: Some("busy".into()),
            ..inputs(&p)
        });
        assert_eq!(plan.act, Some(Act::Despawn { side: 0, n: 3 }));
    }

    #[test]
    fn the_health_limits_name_their_reason() {
        let p = policy();
        let ok = HealthView {
            rss_mb: 500,
            packets_in_per_sec: 100.0,
            login_failures: 1,
            open_issues: 3,
            ..HealthView::default()
        };
        assert_eq!(holding_reason(&p, &ok), None);
        for (h, word) in [
            (
                HealthView {
                    rss_mb: 3072,
                    ..ok.clone()
                },
                "memory",
            ),
            (
                HealthView {
                    packets_in_per_sec: 31_000.0,
                    ..ok.clone()
                },
                "packets",
            ),
            (
                HealthView {
                    login_failures: 8,
                    ..ok.clone()
                },
                "sessions failed",
            ),
            (
                HealthView {
                    open_issues: 50,
                    ..ok.clone()
                },
                "issues",
            ),
        ] {
            let why = holding_reason(&p, &h).unwrap();
            assert!(why.contains(word), "{why}");
        }
        // a limit of 0 is ignored
        let lax = Policy {
            backoff_rss_mb: 0,
            backoff_packets_in: 0,
            backoff_login_failures: 0,
            backoff_open_issues: 0,
            ..p
        };
        let awful = HealthView {
            rss_mb: u32::MAX,
            packets_in_per_sec: 1e9,
            login_failures: u32::MAX,
            open_issues: u32::MAX,
            ..HealthView::default()
        };
        assert_eq!(holding_reason(&lax, &awful), None);
    }

    #[test]
    fn a_surplus_is_stopped_only_past_the_dead_band_and_only_what_the_director_started() {
        let p = policy();
        let at = |bots: u32, owned: u32, players: [u32; 2]| {
            plan(&Inputs {
                players,
                bots: [bots, 0],
                owned: [owned, 0],
                total_online: bots,
                ..inputs(&p)
            })
        };
        // the east emptied: the west needs 0 bots. Two over is inside the band (hysteresis 2)
        assert_eq!(at(2, 2, [0, 0]).act, None);
        // three over: stop them, a step at most
        assert_eq!(at(3, 3, [0, 0]).act, Some(Act::Despawn { side: 0, n: 3 }));
        assert_eq!(at(8, 8, [0, 0]).act, Some(Act::Despawn { side: 0, n: 3 }));
        // only what the director started: 2 of 8
        assert_eq!(at(8, 2, [0, 0]).act, Some(Act::Despawn { side: 0, n: 2 }));
        // none of them its own: nothing to stop
        assert_eq!(at(8, 0, [0, 0]).act, None);
        // some players remain: the surplus is over the target, not over zero
        assert_eq!(at(6, 6, [0, 3]).act, Some(Act::Despawn { side: 0, n: 3 }));
        assert_eq!(at(5, 5, [0, 3]).act, None);
        // switched off
        let no = Policy {
            despawn: false,
            ..p.clone()
        };
        let plan = plan(&Inputs {
            bots: [8, 0],
            owned: [8, 0],
            total_online: 8,
            ..inputs(&no)
        });
        assert_eq!(plan.act, None);
        // a short side is served before a long one is trimmed
        let both = super::plan(&Inputs {
            players: [0, 4],
            bots: [0, 9],
            owned: [0, 9],
            total_online: 9,
            ..inputs(&p)
        });
        assert_eq!(both.act, Some(Act::Spawn { side: 0, n: 3 }));
    }

    #[test]
    fn a_player_at_the_edge_of_view_does_not_make_bots_come_and_go() {
        // simulate a player who flickers in and out of view every other second
        let p = policy();
        let mut bots = 0u32;
        let mut spawned = 0u32;
        let mut stopped = 0u32;
        let mut since = u64::MAX;
        for t in 0..600u64 {
            let players = [0, if t % 2 == 0 { 1 } else { 0 }];
            let plan = plan(&Inputs {
                players,
                bots: [bots, 0],
                owned: [bots, 0],
                total_online: bots,
                since_action_ms: since,
                ..inputs(&p)
            });
            match plan.act {
                Some(Act::Spawn { n, .. }) => {
                    bots += n;
                    spawned += n;
                    since = 0;
                }
                Some(Act::Despawn { n, .. }) => {
                    bots -= n;
                    stopped += n;
                    since = 0;
                }
                None => since = since.saturating_add(1000),
            }
        }
        assert_eq!(spawned, 1, "one bot arrived for the flickering player");
        assert_eq!(
            stopped, 0,
            "and it never left again (one over the target is in the band)"
        );
    }

    fn cfg(enabled: bool) -> PopulationCfg {
        PopulationCfg {
            enabled,
            dry_run: false,
            ..PopulationCfg::default()
        }
    }

    fn step_in(now_ms: u64, players: [u32; 2], bots: [u32; 2], owned: [u32; 2]) -> StepInputs {
        StepInputs {
            now_ms,
            players,
            bots,
            owned,
            total_online: bots.iter().sum(),
            health: HealthView::default(),
        }
    }

    /// Records the orders and answers each with a text.
    fn run(d: &Director, i: &StepInputs) -> Vec<Order> {
        let mut orders = Vec::new();
        d.step(i, &mut |o| {
            orders.push(o);
            Ok("queued".to_owned())
        });
        orders
    }

    #[test]
    fn a_live_step_sends_the_order_logs_it_and_paces_itself() {
        let d = Director::new(&cfg(true));
        let o = run(&d, &step_in(1_000_000, [0, 7], [1, 0], [0, 0]));
        assert_eq!(
            o,
            vec![Order::Spawn {
                sel: Selector::side(Alliance::Nuia),
                n: 3
            }]
        );
        let v = d.view(HealthView::default(), 0);
        assert_eq!(v.mode, "live");
        assert_eq!(v.log[0].kind, "spawn");
        assert!(v.log[0].applied);
        assert_eq!((v.log[0].alliance, v.log[0].count), (Alliance::Nuia, 3));
        assert_eq!(v.log[0].result, "queued");
        assert_eq!(v.spawned_last_min, 0, "counted from the next step");
        // the next second: inside the gap, and the three are counted as on their way
        let o = run(&d, &step_in(1_001_000, [0, 7], [1, 0], [0, 0]));
        assert!(o.is_empty());
        let v = d.view(HealthView::default(), 0);
        assert_eq!(v.spawned_last_min, 3);
        assert_eq!(v.sides[0].bots, 4, "1 online + 3 logging in");
        assert_eq!(v.sides[0].target, 7);
        assert_eq!(v.next_action_secs, 59);
        // a minute later the gap is over and they are online: 4 are there, 3 more are wanted
        let o = run(&d, &step_in(1_061_000, [0, 7], [4, 0], [3, 0]));
        assert_eq!(
            o,
            vec![Order::Spawn {
                sel: Selector::side(Alliance::Nuia),
                n: 3
            }]
        );
        // the per-minute limit is hard: ten a minute
        let v = d.view(HealthView::default(), 0);
        assert!(v.max_spawn_per_min == 10 && v.min_action_gap_secs == 60);
    }

    #[test]
    fn a_dry_run_only_writes_things_down_and_does_not_repeat_itself() {
        let d = Director::new(&PopulationCfg {
            dry_run: true,
            ..cfg(true)
        });
        let o = run(&d, &step_in(1_000_000, [0, 7], [0, 0], [0, 0]));
        assert!(o.is_empty(), "a dry run sends nothing");
        let v = d.view(HealthView::default(), 0);
        assert_eq!(v.mode, "dry_run");
        assert_eq!(v.log.len(), 1);
        assert!(!v.log[0].applied);
        assert!(v.log[0].result.contains("dry run"));
        assert_eq!(v.log[0].count, 3);
        // the same proposal a minute later is not written again
        run(&d, &step_in(1_060_000, [0, 7], [0, 0], [0, 0]));
        assert_eq!(d.view(HealthView::default(), 0).log.len(), 1);
        // a different one is
        run(&d, &step_in(1_120_000, [0, 2], [0, 0], [0, 0]));
        assert_eq!(d.view(HealthView::default(), 0).log.len(), 2);
        // and after five minutes the first again
        run(&d, &step_in(1_500_000, [0, 2], [0, 0], [0, 0]));
        assert_eq!(d.view(HealthView::default(), 0).log.len(), 3);
        assert_eq!(d.view(HealthView::default(), 0).spawned_last_min, 0);
    }

    #[test]
    fn the_kill_switch_stops_everything_and_the_config_can_forbid_switching_on() {
        let d = Director::new(&cfg(true));
        d.apply(
            PolicyPatch {
                active: Some(false),
                ..PolicyPatch::default()
            },
            5,
        )
        .unwrap();
        assert!(run(&d, &step_in(1_000_000, [0, 7], [0, 0], [0, 0])).is_empty());
        let v = d.view(HealthView::default(), 0);
        assert_eq!(v.mode, "off");
        assert_eq!(v.sides[0].target, 7, "the targets are still shown");
        assert!(v.log.iter().all(|r| r.kind == "policy"));
        // back on
        d.apply(
            PolicyPatch {
                active: Some(true),
                ..PolicyPatch::default()
            },
            6,
        )
        .unwrap();
        assert_eq!(
            run(&d, &step_in(1_001_000, [0, 7], [0, 0], [0, 0])).len(),
            1
        );

        // not available: it starts off and cannot be switched on
        let d = Director::new(&cfg(false));
        let v = d.view(HealthView::default(), 0);
        assert!(!v.available && !v.policy.active);
        let err = d
            .apply(
                PolicyPatch {
                    active: Some(true),
                    ..PolicyPatch::default()
                },
                1,
            )
            .unwrap_err();
        assert!(err.contains("[population] enabled"), "{err}");
        assert!(!d.policy().active);
        assert!(run(&d, &step_in(1_000_000, [0, 7], [0, 0], [0, 0])).is_empty());
        // switching off is always allowed
        d.apply(
            PolicyPatch {
                active: Some(false),
                ..PolicyPatch::default()
            },
            1,
        )
        .unwrap();
    }

    #[test]
    fn a_back_off_is_logged_once_and_blocks_spawns() {
        let d = Director::new(&cfg(true));
        let mut i = step_in(1_000_000, [0, 7], [0, 0], [0, 0]);
        i.health.rss_mb = 4000;
        assert!(run(&d, &i).is_empty());
        let v = d.view(i.health.clone(), 0);
        assert_eq!(v.mode, "holding");
        assert!(v.health.holding.as_deref().unwrap().contains("memory"));
        assert_eq!(v.log[0].kind, "hold");
        assert!(!v.log[0].applied);
        run(
            &d,
            &StepInputs {
                now_ms: 1_010_000,
                ..i.clone()
            },
        );
        assert_eq!(d.view(i.health.clone(), 0).log.len(), 1, "said once");
        // the load goes away and the spawn happens
        let calm = step_in(1_020_000, [0, 7], [0, 0], [0, 0]);
        assert_eq!(run(&d, &calm).len(), 1);
        assert_eq!(d.view(HealthView::default(), 0).mode, "live");
    }

    #[test]
    fn a_despawn_asks_for_owned_bots_that_have_been_up_a_while() {
        let d = Director::new(&cfg(true));
        let o = run(&d, &step_in(1_000_000, [0, 0], [6, 0], [6, 0]));
        assert_eq!(
            o,
            vec![Order::Despawn {
                sel: Selector::side(Alliance::Nuia),
                n: 3,
                rule: StopRule {
                    owned_only: true,
                    min_age_secs: MIN_STAY_SECS
                }
            }]
        );
        assert_eq!(d.view(HealthView::default(), 0).log[0].kind, "despawn");
    }

    #[test]
    fn a_refused_order_is_logged_as_not_applied_and_leaves_the_floor_alone() {
        let d = Director::new(&cfg(true));
        let mut orders = 0;
        d.step(&step_in(1_000_000, [0, 7], [0, 0], [0, 0]), &mut |_| {
            orders += 1;
            Err("the supervisor is busy, try again".to_owned())
        });
        assert_eq!(orders, 1);
        let v = d.view(HealthView::default(), 0);
        assert!(!v.log[0].applied);
        assert!(v.log[0].result.contains("busy"));
        // it counts as an action (the gap applies) but the bots are not counted as on their way
        run(&d, &step_in(1_001_000, [0, 7], [0, 0], [0, 0]));
        assert_eq!(d.view(HealthView::default(), 0).sides[0].bots, 0);
        assert_eq!(d.view(HealthView::default(), 0).spawned_last_min, 0);
    }

    #[test]
    fn policy_changes_are_applied_logged_and_bounded() {
        let d = Director::new(&cfg(true));
        d.apply(
            PolicyPatch {
                max_online: Some(9_999_999),
                mirror_pct: Some(80),
                despawn: Some(false),
                ..PolicyPatch::default()
            },
            10,
        )
        .unwrap();
        let p = d.policy();
        assert_eq!(p.max_online, MAX_BOTS, "never above the process limit");
        assert_eq!(p.mirror_pct, 80);
        assert!(!p.despawn);
        let v = d.view(HealthView::default(), 0);
        assert_eq!(v.log.len(), 1);
        assert!(
            v.log[0].reason.contains("mirror_pct 100 -> 80"),
            "{}",
            v.log[0].reason
        );
        assert!(v.log[0].reason.contains("despawn true -> false"));
        // an empty patch, or one that changes nothing, writes nothing
        d.apply(PolicyPatch::default(), 11).unwrap();
        d.apply(
            PolicyPatch {
                mirror_pct: Some(80),
                ..PolicyPatch::default()
            },
            12,
        )
        .unwrap();
        assert_eq!(d.view(HealthView::default(), 0).log.len(), 1);
        // the log is bounded
        for k in 0..(MAX_LOG as u32 + 20) {
            d.apply(
                PolicyPatch {
                    step: Some(k % 50 + 1),
                    hysteresis: Some(k % 7),
                    ..PolicyPatch::default()
                },
                100 + u64::from(k),
            )
            .unwrap();
        }
        assert!(d.view(HealthView::default(), 0).log.len() <= MAX_LOG);
    }

    #[test]
    fn wishes_expire_and_automatic_ones_follow_the_scarcest_items() {
        let d = Director::new(&cfg(true));
        let hour = 3_600_000u64;
        d.apply(
            PolicyPatch {
                boosts: Some(vec![
                    BoostRequest {
                        template: 3412,
                        hours: 2,
                    },
                    BoostRequest {
                        template: 3411,
                        hours: 0,
                    },
                    BoostRequest {
                        template: 3412,
                        hours: 9,
                    },
                ]),
                auto_boost: Some(2),
                ..PolicyPatch::default()
            },
            hour,
        )
        .unwrap();
        let p = d.policy();
        assert_eq!(p.boosts.len(), 2, "the same item twice is one wish");
        assert_eq!(p.boosts[0].until_ms, 3 * hour);
        assert_eq!(p.boosts[1].until_ms, 0);
        assert_eq!(d.wishes(hour), vec![3412, 3411]);
        assert_eq!(d.wishes(3 * hour + 1), vec![3411], "the first ran out");

        let mut asked = 0;
        d.update_auto(hour, |n, min| {
            asked += 1;
            assert_eq!(n, 2);
            assert!((min - 0.7).abs() < 1e-6);
            vec![(3411, 0.9), (7000, 0.8), (7001, 0.75)]
        });
        assert_eq!(asked, 1);
        // the operator's own wish is not repeated; the cap is 2 so only 7000 and 7001 fit... but
        // 3411 is manual, so two automatic ones
        assert_eq!(d.wishes(hour), vec![3412, 3411, 7000, 7001]);
        assert_eq!(
            d.view(HealthView::default(), 0).auto_wishes,
            vec![7000, 7001]
        );
        let log = d.view(HealthView::default(), 0).log;
        assert_eq!(log.iter().filter(|r| r.kind == "wish").count(), 2);
        // a minute is the least between two looks
        d.update_auto(hour + 1_000, |_, _| panic!("too soon"));
        // later, an automatic wish runs out when the item is not scarce any more
        d.update_auto(hour + 31 * 60_000 + AUTO_EVERY_MS, |_, _| vec![]);
        assert_eq!(
            d.wishes(hour + 31 * 60_000 + AUTO_EVERY_MS),
            vec![3412, 3411],
            "only the operator's wishes are left"
        );
        // nothing automatic while the director is off or the count is 0
        d.apply(
            PolicyPatch {
                auto_boost: Some(0),
                ..PolicyPatch::default()
            },
            hour + 40 * 60_000,
        )
        .unwrap();
        d.update_auto(hour + 3 * 60 * 60_000, |_, _| {
            panic!("{}", "auto_boost is 0, but it must not ask... ")
        });
    }

    #[test]
    fn the_policy_is_kept_in_a_file_between_runs() {
        let dir = crate::testutil::temp_dir("director-policy");
        let file = dir.join("population.policy.json");
        let cfg = PopulationCfg {
            policy_file: Some(file.clone()),
            ..cfg(true)
        };
        let d = Director::new(&cfg);
        assert!(!file.exists(), "nothing is written before a change");
        d.apply(
            PolicyPatch {
                max_online: Some(77),
                dry_run: Some(true),
                ..PolicyPatch::default()
            },
            1,
        )
        .unwrap();
        assert!(file.exists());
        let again = Director::new(&cfg);
        let p = again.policy();
        assert_eq!(p.max_online, 77);
        assert!(p.dry_run);
        // a file with a policy that is switched on is not trusted when the config is off
        let off = Director::new(&PopulationCfg {
            enabled: false,
            policy_file: Some(file.clone()),
            ..PopulationCfg::default()
        });
        assert_eq!(off.policy().max_online, 77);
        assert!(!off.policy().active);
        // a broken file leaves the configured policy and does not stop the start
        std::fs::write(&file, "{ not json").unwrap();
        let broken = Director::new(&cfg);
        assert_eq!(broken.policy().max_online, cfg.max_online);
        // a huge number in a hand-edited file is cut to the process limit
        let p = Policy {
            max_online: u32::MAX,
            ..Policy::default()
        };
        save_policy(&file, &p).unwrap();
        assert_eq!(load_policy(&file).unwrap().unwrap().max_online, MAX_BOTS);
        assert!(load_policy(&dir.join("none.json")).unwrap().is_none());
    }
}
