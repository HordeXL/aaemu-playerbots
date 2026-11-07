//! A bot's daily rhythm: when it plays, for how long, with which breaks, and which days it skips.
//!
//! Pure functions of the bot id, its profile and the local day, so a bot gets the same plan for a
//! day on every pass and after a restart, and the plans of many bots add up to a smooth curve. A
//! [`DayPlan`] holds the stretches of play of one local day in minutes from its midnight; a
//! stretch may start before midnight (a negative minute: late the day before) or run past it, so
//! [`online_at`] looks at the plans of the day before, the day and the day after.
//!
//! The rhythm:
//!
//! * **When**: around the peak of the chronotype (early bird 08:00, daytime 14:00, evening 20:30,
//!   night owl 23:30), shifted by up to 50 minutes for each bot and spread by about two hours
//!   from day to day.
//! * **How long**: a casual player (`dedication` 0) plays 20 to 60 minutes, a dedicated one
//!   (`dedication` 1) two and a half to six hours, longer at the weekend.
//! * **How often**: a casual player skips about four days in ten, a dedicated one hardly ever;
//!   dedicated players often come back for a second session, a few for a third.
//! * **Breaks**: a session of more than 100 minutes is cut by breaks of 5 to 20 minutes every
//!   hour or two.

use crate::roster::Chronotype;

/// Minutes in a day.
pub const DAY_MIN: i64 = 1440;
/// The most stretches of play in one day.
pub const MAX_STRETCHES: usize = 8;
/// A session longer than this (minutes) has breaks.
pub const BREAK_AFTER_MIN: i32 = 100;
/// The shortest stretch kept (minutes): a stretch the breaks cut shorter is dropped.
pub const MIN_STRETCH_MIN: i32 = 10;

/// What the schedule reads of a bot's profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Traits {
    /// How much it plays, 0 to 1.
    pub dedication: f32,
    /// When in the day.
    pub chronotype: Chronotype,
}

/// One stretch of play: from `start` (inclusive) to `end` (exclusive), minutes from the local
/// midnight of the plan's day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stretch {
    pub start: i32,
    pub end: i32,
}

/// The stretches of play of one local day, in order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayPlan {
    stretches: [Stretch; MAX_STRETCHES],
    len: u8,
}

impl DayPlan {
    /// The stretches.
    pub fn stretches(&self) -> &[Stretch] {
        &self.stretches[..usize::from(self.len)]
    }

    /// Whether the bot skips the day.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Minutes of play.
    pub fn minutes(&self) -> i32 {
        self.stretches().iter().map(|s| s.end - s.start).sum()
    }

    fn push(&mut self, s: Stretch) {
        if usize::from(self.len) < MAX_STRETCHES && s.end - s.start >= MIN_STRETCH_MIN {
            self.stretches[usize::from(self.len)] = s;
            self.len += 1;
        }
    }

    fn sort(&mut self) {
        let n = usize::from(self.len);
        self.stretches[..n].sort_unstable_by_key(|s| s.start);
    }
}

/// The weekday of local day `day` (days since 1970-01-01), 0 = Monday ... 6 = Sunday.
pub fn weekday(day: i64) -> u8 {
    // (1970-01-01 was a Thursday)
    (day + 3).rem_euclid(7) as u8
}

/// The minute of the day the chronotype's play centres on, and the spread (minutes) of the
/// session starts around it from day to day.
pub fn peak(c: Chronotype) -> (i32, i32) {
    match c {
        Chronotype::EarlyBird => (8 * 60, 140),
        Chronotype::Daytime => (14 * 60, 190),
        Chronotype::Evening => (20 * 60 + 30, 110),
        Chronotype::NightOwl => (23 * 60 + 30, 120),
    }
}

fn mix(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A fixed shift of bot `id`'s rhythm, -50 to +50 minutes.
pub fn bot_shift(id: u32) -> i32 {
    (mix(u64::from(id) ^ 0x5bd1_e995) % 101) as i32 - 50
}

/// A number from -1 to 1 that clusters around 0 (the mean of three uniform numbers, scaled).
fn bell(rng: &mut fastrand::Rng) -> f32 {
    (rng.f32() + rng.f32() + rng.f32()) / 1.5 - 1.0
}

/// The plan of bot `id` for local day `day` (days since 1970-01-01).
pub fn day_plan(id: u32, day: i64, t: Traits) -> DayPlan {
    let mut plan = DayPlan::default();
    let mut rng = fastrand::Rng::with_seed(mix(u64::from(id) << 24 ^ day as u64));
    let d = if t.dedication.is_finite() {
        t.dedication.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let weekend = weekday(day) >= 5;
    let mut skip = 0.04 + 0.36 * (1.0 - d) * (1.0 - d);
    if weekend {
        skip *= 0.6;
    }
    if rng.f32() < skip {
        return plan;
    }
    let mut sessions = 1;
    if rng.f32() < 0.1 + 0.55 * d {
        sessions += 1;
    }
    if rng.f32() < 0.4 * d * d {
        sessions += 1;
    }
    // session length in minutes: 20..60 for a casual, 150..360 for a no-lifer
    let lo = 20.0 * 7.5f32.powf(d);
    let hi = 60.0 * 6.0f32.powf(d);
    let stretch = if weekend { 1.25 } else { 1.0 };
    let (peak, spread) = peak(t.chronotype);
    let centre = peak + bot_shift(id) + (bell(&mut rng) * spread as f32 * 1.4) as i32;
    let mut sessions_out: [(i32, i32); 3] = [(0, 0); 3];
    let mut n = 0usize;
    for k in 0..sessions {
        let len = ((lo + (hi - lo) * rng.f32().powf(1.3)) * stretch) as i32;
        // later sessions are shorter
        let len = if k == 0 { len } else { (len * 2 / 3).max(20) };
        let (start, end) = if k == 0 {
            (centre - len / 2, centre - len / 2 + len)
        } else {
            let gap = 60 + rng.i32(0..240);
            let first = sessions_out[0];
            let last = sessions_out[n - 1];
            if rng.bool() && first.0 - gap - len > -180 {
                (first.0 - gap - len, first.0 - gap)
            } else {
                (last.1 + gap, last.1 + gap + len)
            }
        };
        sessions_out[n] = (start, end);
        n += 1;
        // (kept in order for the next session's gap)
        sessions_out[..n].sort_unstable_by_key(|s| s.0);
    }
    for &(start, end) in &sessions_out[..n] {
        let end = end.min(start + 10 * 60);
        if end - start <= BREAK_AFTER_MIN {
            plan.push(Stretch { start, end });
            continue;
        }
        let mut at = start;
        while at < end {
            let play = 60 + rng.i32(0..61);
            let stop = (at + play).min(end);
            plan.push(Stretch {
                start: at,
                end: stop,
            });
            at = stop + 5 + rng.i32(0..16);
        }
    }
    plan.sort();
    plan
}

/// Local time as minutes since 1970-01-01 00:00 local: `(day, minute of the day)`.
pub fn split(local_min: i64) -> (i64, i32) {
    (
        local_min.div_euclid(DAY_MIN),
        local_min.rem_euclid(DAY_MIN) as i32,
    )
}

/// Whether bot `id` plays at local minute `local_min` (minutes since 1970-01-01 local).
pub fn online_at(id: u32, t: Traits, local_min: i64) -> bool {
    let (day, m) = split(local_min);
    (-1..=1).any(|k| {
        let at = m - (k * DAY_MIN) as i32;
        day_plan(id, day + k, t)
            .stretches()
            .iter()
            .any(|s| s.start <= at && at < s.end)
    })
}

/// The next edge of bot `id`'s plan after `local_min`: `(local minute, logs in)`, the end of the
/// stretch it plays in now, else the start of its next one (within a day). `None` when there is
/// none.
pub fn next_edge(id: u32, t: Traits, local_min: i64) -> Option<(i64, bool)> {
    let (day, _) = split(local_min);
    let mut end: Option<i64> = None;
    let mut start: Option<i64> = None;
    for k in -1..=2 {
        let base = (day + k) * DAY_MIN;
        for s in day_plan(id, day + k, t).stretches() {
            let (a, b) = (base + i64::from(s.start), base + i64::from(s.end));
            if a <= local_min && local_min < b {
                end = Some(end.map_or(b, |e| e.max(b)));
            } else if a > local_min {
                start = Some(start.map_or(a, |x| x.min(a)));
            }
        }
    }
    match end {
        Some(e) => Some((e, false)),
        None => start.map(|a| (a, true)),
    }
}

/// Bots of `bots` that play at each hour of local day `day` (at half past), before any bound.
pub fn curve(bots: &[(u32, Traits)], day: i64) -> [u32; 24] {
    let mut out = [0u32; 24];
    for &(id, t) in bots {
        for (h, slot) in out.iter_mut().enumerate() {
            if online_at(id, t, day * DAY_MIN + h as i64 * 60 + 30) {
                *slot += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(d: f32, c: Chronotype) -> Traits {
        Traits {
            dedication: d,
            chronotype: c,
        }
    }

    #[test]
    fn a_plan_is_the_same_on_every_call_and_differs_between_bots_and_days() {
        let tr = t(0.6, Chronotype::Evening);
        assert_eq!(day_plan(7, 20_000, tr), day_plan(7, 20_000, tr));
        let days = (20_000..20_030).filter(|d| day_plan(7, *d, tr) != day_plan(7, d + 1, tr));
        assert!(days.count() > 25);
        let bots = (1..40).filter(|b| day_plan(*b, 20_000, tr) != day_plan(b + 1, 20_000, tr));
        assert!(bots.count() > 35);
    }

    #[test]
    fn casual_players_play_short_and_skip_days_and_dedicated_ones_play_hours() {
        let days = 20_000..20_200;
        let total = |d: f32| -> (i32, usize) {
            let mut minutes = 0;
            let mut skipped = 0;
            for bot in 1..=20 {
                for day in days.clone() {
                    let p = day_plan(bot, day, t(d, Chronotype::Daytime));
                    minutes += p.minutes();
                    skipped += usize::from(p.is_empty());
                }
            }
            (minutes / (20 * 200), skipped)
        };
        let (casual_min, casual_skip) = total(0.0);
        let (no_life_min, no_life_skip) = total(1.0);
        assert!((12..60).contains(&casual_min), "{casual_min}");
        assert!(no_life_min > 200, "{no_life_min}");
        assert!(
            casual_skip > 4 * no_life_skip,
            "{casual_skip} {no_life_skip}"
        );
        assert!(casual_skip > 1000 && casual_skip < 2200, "{casual_skip}");
        // a casual session is 20 to 60 minutes (75 at the weekend)
        for day in days.clone() {
            for s in day_plan(3, day, t(0.0, Chronotype::Daytime)).stretches() {
                assert!((20..=75).contains(&(s.end - s.start)), "{s:?}");
            }
        }
    }

    #[test]
    fn long_sessions_have_breaks_and_stretches_are_ordered_and_bounded() {
        let mut breaks = 0;
        for day in 20_000..20_100 {
            let p = day_plan(11, day, t(1.0, Chronotype::Evening));
            assert!(p.stretches().len() <= MAX_STRETCHES);
            for w in p.stretches().windows(2) {
                assert!(w[0].start <= w[1].start, "{p:?}");
                if w[1].start > w[0].end && w[1].start - w[0].end <= 20 {
                    breaks += 1;
                }
            }
            for s in p.stretches() {
                assert!(s.end - s.start >= MIN_STRETCH_MIN);
                assert!(s.end - s.start <= 121, "{s:?}");
            }
        }
        assert!(breaks > 50, "{breaks}");
    }

    #[test]
    fn each_chronotype_plays_around_its_peak() {
        // how many of 200 bots play at each hour, over two weeks
        let curve = |c: Chronotype| -> [u32; 24] {
            let mut out = [0u32; 24];
            for bot in 1..=200 {
                for day in 20_000..20_014i64 {
                    for (h, slot) in out.iter_mut().enumerate() {
                        let at = day * DAY_MIN + h as i64 * 60 + 30;
                        if online_at(bot, t(0.5, c), at) {
                            *slot += 1;
                        }
                    }
                }
            }
            out
        };
        let busiest = |c| {
            let v = curve(c);
            (0..24).max_by_key(|h| v[*h]).unwrap()
        };
        assert!((6..=10).contains(&busiest(Chronotype::EarlyBird)));
        assert!((11..=17).contains(&busiest(Chronotype::Daytime)));
        assert!((18..=22).contains(&busiest(Chronotype::Evening)));
        let owl = busiest(Chronotype::NightOwl);
        assert!(owl >= 22 || owl <= 1, "{owl}");
        // the curve is smooth: no hour jumps by more than half of the peak
        let v = curve(Chronotype::Evening);
        let top = *v.iter().max().unwrap();
        for h in 0..24 {
            let next = v[(h + 1) % 24];
            assert!(v[h].abs_diff(next) * 2 <= top, "{v:?}");
        }
    }

    #[test]
    fn a_session_past_midnight_counts_on_the_next_day() {
        let owl = t(0.8, Chronotype::NightOwl);
        let mut past_midnight = 0;
        for bot in 1..100 {
            let day = 20_000;
            for s in day_plan(bot, day, owl).stretches() {
                if s.end > DAY_MIN as i32 {
                    past_midnight += 1;
                    let at = day * DAY_MIN + i64::from(s.end) - 1;
                    assert!(online_at(bot, owl, at), "bot {bot} {s:?}");
                }
            }
        }
        assert!(past_midnight > 10, "{past_midnight}");
    }

    #[test]
    fn the_next_edge_flips_the_state() {
        let tr = t(0.5, Chronotype::Daytime);
        let mut checked = 0;
        for bot in 1..30 {
            let now = 20_000 * DAY_MIN + 10 * 60;
            if let Some((at, logs_in)) = next_edge(bot, tr, now) {
                assert!(at > now && at < now + 4 * DAY_MIN);
                assert_eq!(online_at(bot, tr, now), !logs_in, "bot {bot}");
                assert_eq!(online_at(bot, tr, at - 1), !logs_in, "bot {bot}");
                checked += 1;
            }
        }
        assert!(checked > 20);
        let c = curve(&[(1, tr), (2, tr)], 20_000);
        assert!(c.iter().all(|n| *n <= 2));
        assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
        assert_eq!(weekday(4), 0);
        assert_eq!(split(-1), (-1, 1439));
    }
}
