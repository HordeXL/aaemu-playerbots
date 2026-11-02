//! Rate limits of the chat: a sliding window per bot (and per conversation) and the process-wide
//! gate in the hub that keeps a swarm of bots from spamming the server.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use std::sync::atomic::AtomicU64;

use parking_lot::{Mutex, RwLock};
use rustc_hash::FxHashSet;

/// At most `N` events in any window of `span_ms`, on a bot clock that starts at 1 (0 marks a free
/// slot). Fixed size, no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateWindow<const N: usize> {
    stamps: [u64; N],
}

impl<const N: usize> Default for RateWindow<N> {
    fn default() -> Self {
        RateWindow { stamps: [0; N] }
    }
}

impl<const N: usize> RateWindow<N> {
    /// Events recorded within the last `span_ms`.
    pub fn count(&self, now_ms: u64, span_ms: u64) -> usize {
        self.stamps
            .iter()
            .filter(|s| **s != 0 && now_ms.saturating_sub(**s) < span_ms)
            .count()
    }

    /// Whether another event fits right now (nothing is recorded).
    pub fn has_room(&self, now_ms: u64, span_ms: u64) -> bool {
        self.count(now_ms, span_ms) < N
    }

    /// Records an event at `now_ms` if it fits and says whether it did.
    pub fn try_take(&mut self, now_ms: u64, span_ms: u64) -> bool {
        if !self.has_room(now_ms, span_ms) {
            return false;
        }
        // Overwrite a free or expired slot, else the oldest.
        let slot = self
            .stamps
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| **s)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.stamps[slot] = now_ms.max(1);
        true
    }
}

/// How long a claimed line stays claimed.
const CLAIM_TTL: Duration = Duration::from_secs(15);
/// Claims remembered at most.
const MAX_CLAIMS: usize = 32;
/// Cap on the per-minute queues (a limit above this is clamped).
const MAX_PER_MIN: usize = 600;

/// Counters of the chat add-ons, shown in the web UI next to the LLM budget.
#[derive(Debug, Default)]
pub struct ChatStats {
    /// Replies written by the LLM and sent.
    pub llm_replies: AtomicU64,
    /// Replies from the canned table and sent.
    pub canned_replies: AtomicU64,
    /// Unsolicited bot-to-bot lines sent.
    pub chatter: AtomicU64,
    /// Goals the LLM director chose and the brain started.
    pub director_picks: AtomicU64,
}

/// Process-wide limits: lines per minute across all bots, and who answers a line of a player.
#[derive(Debug, Default)]
pub struct ChatGate {
    chatter: Mutex<VecDeque<Instant>>,
    replies: Mutex<VecDeque<Instant>>,
    claims: Mutex<VecDeque<(u64, Instant)>>,
    /// Counters.
    pub stats: ChatStats,
}

impl ChatGate {
    /// Takes one of the `per_min` bot-to-bot lines of the current minute.
    pub fn take_chatter(&self, now: Instant, per_min: u32) -> bool {
        take(&self.chatter, now, per_min)
    }

    /// Takes one of the `per_min` replies to players of the current minute.
    pub fn take_reply(&self, now: Instant, per_min: u32) -> bool {
        take(&self.replies, now, per_min)
    }

    /// Whether a bot may answer the line with this `key`: true for the first bot that asks within
    /// [`CLAIM_TTL`], false for the others (one answer per line, not one per bot in earshot).
    pub fn claim_line(&self, key: u64, now: Instant) -> bool {
        let mut claims = self.claims.lock();
        claims.retain(|(_, at)| now.saturating_duration_since(*at) < CLAIM_TTL);
        if claims.iter().any(|(k, _)| *k == key) {
            return false;
        }
        if claims.len() >= MAX_CLAIMS {
            claims.pop_front();
        }
        claims.push_back((key, now));
        true
    }

    /// Notes that a bot answers the line with this `key` whatever others did (it was addressed by
    /// name), so the bots that merely overheard it stay quiet.
    pub fn mark_claimed(&self, key: u64, now: Instant) {
        let _ = self.claim_line(key, now);
    }
}

fn take(queue: &Mutex<VecDeque<Instant>>, now: Instant, per_min: u32) -> bool {
    let limit = (per_min as usize).min(MAX_PER_MIN);
    if limit == 0 {
        return false;
    }
    let mut q = queue.lock();
    while q
        .front()
        .is_some_and(|t| now.saturating_duration_since(*t) >= Duration::from_secs(60))
    {
        q.pop_front();
    }
    if q.len() >= limit {
        return false;
    }
    q.push_back(now);
    true
}

/// The names of our own bots, to tell their chat from a player's when the sender is out of view.
#[derive(Debug, Default)]
pub struct BotNames {
    names: RwLock<FxHashSet<String>>,
}

impl BotNames {
    /// Remembers `name` as one of ours.
    pub fn add(&self, name: &str) {
        let lower = name.to_lowercase();
        if lower.is_empty() || self.names.read().contains(&lower) {
            return;
        }
        self.names.write().insert(lower);
    }

    /// Whether `name` is one of ours.
    pub fn contains(&self, name: &str) -> bool {
        self.names.read().contains(&name.to_lowercase())
    }
}

/// What the hub holds for the chat: the process-wide limits and the names of our bots.
#[derive(Debug, Default)]
pub struct ChatHub {
    /// Limits and counters.
    pub gate: ChatGate,
    /// Names of our bots.
    pub names: BotNames,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_allows_n_events_per_span() {
        let mut w = RateWindow::<3>::default();
        assert!(w.try_take(1000, 10_000));
        assert!(w.try_take(2000, 10_000));
        assert!(w.try_take(3000, 10_000));
        assert!(!w.try_take(4000, 10_000), "the fourth is one too many");
        assert_eq!(w.count(4000, 10_000), 3);
        // the first one expires after the span
        assert!(!w.has_room(10_999, 10_000));
        assert!(w.try_take(11_001, 10_000));
        assert_eq!(w.count(11_001, 10_000), 3);
    }

    #[test]
    fn a_window_never_records_a_refused_event() {
        let mut w = RateWindow::<1>::default();
        assert!(w.try_take(5, 100));
        assert!(!w.try_take(6, 100));
        assert!(w.try_take(105, 100), "only the first event counted");
    }

    #[test]
    fn the_gate_limits_lines_per_minute_and_a_limit_of_zero_means_none() {
        let gate = ChatGate::default();
        let t0 = Instant::now();
        assert!(!gate.take_chatter(t0, 0));
        for _ in 0..3 {
            assert!(gate.take_chatter(t0, 3));
        }
        assert!(!gate.take_chatter(t0 + Duration::from_secs(30), 3));
        assert!(gate.take_chatter(t0 + Duration::from_secs(61), 3));
        // replies and chatter are separate budgets
        assert!(gate.take_reply(t0, 1));
        assert!(!gate.take_reply(t0, 1));
    }

    #[test]
    fn only_the_first_bot_claims_a_line() {
        let gate = ChatGate::default();
        let t0 = Instant::now();
        assert!(gate.claim_line(7, t0));
        assert!(!gate.claim_line(7, t0 + Duration::from_secs(1)));
        assert!(gate.claim_line(8, t0));
        assert!(
            gate.claim_line(7, t0 + Duration::from_secs(16)),
            "claims expire"
        );
        for k in 100..140 {
            gate.mark_claimed(k, t0);
        }
        assert!(gate.claims.lock().len() <= MAX_CLAIMS);
    }

    #[test]
    fn bot_names_are_case_insensitive() {
        let n = BotNames::default();
        n.add("Aria");
        assert!(n.contains("aria") && n.contains("ARIA"));
        assert!(!n.contains("Bob"));
        n.add("");
        assert!(!n.contains(""));
    }
}
