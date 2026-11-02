//! Local cost guards: a rolling-minute request window, the daily token budget and the global
//! cool-down after an HTTP 429. All take the clock as a parameter so they can be tested.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Days since the Unix epoch in UTC; the daily budget resets when this changes.
pub(crate) fn utc_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

/// At most `limit` requests in any rolling `window`. Stricter than a refilling bucket: a burst
/// of `limit` is allowed, but never more than `limit` per window. `limit == 0` means unlimited.
pub(crate) struct RateWindow {
    limit: usize,
    window: Duration,
    stamps: Mutex<VecDeque<Instant>>,
}

impl RateWindow {
    pub fn per_minute(limit: u32) -> Self {
        Self::new(limit as usize, Duration::from_secs(60))
    }

    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            stamps: Mutex::new(VecDeque::with_capacity(limit.min(1024))),
        }
    }

    /// Records a request at `now` and returns `true`, or returns `false` when the window is full.
    pub fn try_take(&self, now: Instant) -> bool {
        if self.limit == 0 {
            return true;
        }
        let mut stamps = self.stamps.lock();
        while let Some(&oldest) = stamps.front() {
            if now.saturating_duration_since(oldest) >= self.window {
                stamps.pop_front();
            } else {
                break;
            }
        }
        if stamps.len() >= self.limit {
            return false;
        }
        stamps.push_back(now);
        true
    }
}

struct DayState {
    day: u64,
    used: u64,
}

/// Tokens per UTC day. `limit == 0` means unlimited.
pub(crate) struct DailyBudget {
    limit: u64,
    state: Mutex<DayState>,
}

impl DailyBudget {
    pub fn new(limit: u64, today: u64) -> Self {
        Self {
            limit,
            state: Mutex::new(DayState {
                day: today,
                used: 0,
            }),
        }
    }

    fn roll(state: &mut DayState, today: u64) {
        if today > state.day {
            state.day = today;
            state.used = 0;
        }
    }

    pub fn is_exhausted(&self, today: u64) -> bool {
        if self.limit == 0 {
            return false;
        }
        let mut state = self.state.lock();
        Self::roll(&mut state, today);
        state.used >= self.limit
    }

    pub fn charge(&self, today: u64, tokens: u64) {
        let mut state = self.state.lock();
        Self::roll(&mut state, today);
        state.used = state.used.saturating_add(tokens);
    }

    pub fn used(&self, today: u64) -> u64 {
        let mut state = self.state.lock();
        Self::roll(&mut state, today);
        state.used
    }

    pub fn left(&self, today: u64) -> u64 {
        if self.limit == 0 {
            return u64::MAX;
        }
        self.limit.saturating_sub(self.used(today))
    }
}

/// Global pause after the endpoint answered HTTP 429.
pub(crate) struct Cooldown {
    until: Mutex<Option<Instant>>,
}

impl Cooldown {
    pub fn new() -> Self {
        Self {
            until: Mutex::new(None),
        }
    }

    pub fn is_active(&self, now: Instant) -> bool {
        let mut until = self.until.lock();
        match *until {
            Some(t) if now < t => true,
            Some(_) => {
                *until = None;
                false
            }
            None => false,
        }
    }

    /// Starts (or extends) the pause so that it lasts until `now + dur`.
    pub fn start(&self, now: Instant, dur: Duration) {
        let end = now + dur;
        let mut until = self.until.lock();
        if until.is_none_or(|t| t < end) {
            *until = Some(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_rejects_the_n_plus_one_th_request() {
        let w = RateWindow::per_minute(3);
        let t0 = Instant::now();
        assert!(w.try_take(t0));
        assert!(w.try_take(t0 + Duration::from_secs(1)));
        assert!(w.try_take(t0 + Duration::from_secs(2)));
        assert!(!w.try_take(t0 + Duration::from_secs(3)));
        assert!(!w.try_take(t0 + Duration::from_secs(59)));
    }

    #[test]
    fn window_frees_slots_as_old_requests_age_out() {
        let w = RateWindow::per_minute(2);
        let t0 = Instant::now();
        assert!(w.try_take(t0));
        assert!(w.try_take(t0 + Duration::from_secs(30)));
        assert!(!w.try_take(t0 + Duration::from_secs(59)));
        // the first request is 60 s old now: one slot is free, the second is still held
        assert!(w.try_take(t0 + Duration::from_secs(60)));
        assert!(!w.try_take(t0 + Duration::from_secs(61)));
        assert!(w.try_take(t0 + Duration::from_secs(90)));
    }

    #[test]
    fn window_zero_limit_is_unlimited() {
        let w = RateWindow::per_minute(0);
        let t0 = Instant::now();
        for _ in 0..1000 {
            assert!(w.try_take(t0));
        }
    }

    #[test]
    fn budget_exhausts_and_resets_on_the_next_utc_day() {
        let b = DailyBudget::new(100, 10);
        assert!(!b.is_exhausted(10));
        assert_eq!(b.left(10), 100);
        b.charge(10, 60);
        assert_eq!(b.used(10), 60);
        assert_eq!(b.left(10), 40);
        assert!(!b.is_exhausted(10));
        b.charge(10, 70);
        assert!(b.is_exhausted(10));
        assert_eq!(b.left(10), 0);
        assert_eq!(b.used(10), 130);
        // midnight passes
        assert!(!b.is_exhausted(11));
        assert_eq!(b.used(11), 0);
        assert_eq!(b.left(11), 100);
        // the clock never moves the budget back
        b.charge(11, 5);
        assert_eq!(b.used(10), 5);
    }

    #[test]
    fn budget_zero_limit_is_unlimited() {
        let b = DailyBudget::new(0, 1);
        b.charge(1, 1_000_000);
        assert!(!b.is_exhausted(1));
        assert_eq!(b.left(1), u64::MAX);
        assert_eq!(b.used(1), 1_000_000);
    }

    #[test]
    fn cooldown_expires() {
        let c = Cooldown::new();
        let t0 = Instant::now();
        assert!(!c.is_active(t0));
        c.start(t0, Duration::from_secs(5));
        assert!(c.is_active(t0 + Duration::from_secs(4)));
        assert!(!c.is_active(t0 + Duration::from_secs(5)));
        assert!(!c.is_active(t0 + Duration::from_secs(6)));
    }

    #[test]
    fn cooldown_keeps_the_longer_pause() {
        let c = Cooldown::new();
        let t0 = Instant::now();
        c.start(t0, Duration::from_secs(30));
        c.start(t0, Duration::from_secs(5));
        assert!(c.is_active(t0 + Duration::from_secs(20)));
    }
}
