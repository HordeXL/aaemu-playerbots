//! Which bots the presence schedules log in and out on one pass: a pure function of what every
//! bot is doing, what its schedule says and the bounds.
//!
//! # The rule
//!
//! * The schedules manage the **pool**: the roster bots that the population director does not
//!   own and that are not running on the operator's account. A bot the operator started by hand
//!   (or the start-up count without presence) is never stopped by presence; a bot the director
//!   started belongs to the director until it is offline and disowned.
//! * Every bot online counts towards the bounds, whoever started it: presence keeps the total
//!   between `min_online` and `max_online` (0: no upper bound) by starting and stopping bots of
//!   its own only.
//! * Of the pool, the bots whose schedule says "playing" are wanted; a bot the operator stopped
//!   by hand ([`Row::held`]) is left offline until its schedule ends that stretch. When the bounds
//!   cut the wanted set, bots already online stay first (no churn), then the ones a stable daily
//!   order picks. When the floor needs more, bots already online stay longer, then the ones whose
//!   next session is nearest come early.
//! * A bot is only stopped when it is safe ([`Row::stop_rank`], the director's
//!   [`stop_rank`](crate::population::select::stop_rank) plus "no real player in its party"): the
//!   most harmless first, the longest online among equals, at most `stops_left` per pass. Starts
//!   are capped at `starts_left` per pass and go through the supervisor's ramp.

/// One roster bot as a pass sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    /// Bot id.
    pub id: u32,
    /// In the world or logging in.
    pub online: bool,
    /// Presence started it (it may be queued for its log-in and still show as offline).
    pub owned: bool,
    /// The population director owns it: presence leaves it alone.
    pub director: bool,
    /// Its schedule says it plays now.
    pub scheduled: bool,
    /// The operator stopped it by hand: not started again until the schedule lets it go.
    pub held: bool,
    /// How willingly it is stopped now (lower first), `None` when it must not be stopped now.
    pub stop_rank: Option<u8>,
    /// Seconds in the world this session.
    pub online_secs: u32,
    /// Minutes until its schedule next logs it in (for the floor), `i64::MAX` when never.
    pub next_login_min: i64,
    /// A stable order for the day (ties and caps), lower first.
    pub order: u64,
}

/// The bounds of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// Bots online at least, all together.
    pub min_online: u32,
    /// Bots online at most, all together (0: no limit).
    pub max_online: u32,
}

/// What a pass asks the supervisor for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Orders {
    /// Bots to log in.
    pub start: Vec<u32>,
    /// Bots to log out, most harmless first.
    pub stop: Vec<u32>,
    /// How many bots the schedules want online now, after the bounds (their own and the others).
    pub target: u32,
    /// How many of the bots to stop must wait because they are busy.
    pub waiting: u32,
}

/// The pass. See the module docs.
pub fn decide(rows: &[Row], b: Bounds, starts_left: usize, stops_left: usize) -> Orders {
    // bots online that are not ours: the operator's and the director's
    let others = rows.iter().filter(|r| r.online && !r.owned).count() as u32;
    let in_pool = |r: &Row| !r.director && (r.owned || !r.online);
    let wanted = rows
        .iter()
        .filter(|r| in_pool(r) && r.scheduled && !r.held)
        .count() as u32;
    let available = rows.iter().filter(|r| in_pool(r) && !r.held).count() as u32;
    let floor = b.min_online.saturating_sub(others);
    let cap = if b.max_online == 0 {
        u32::MAX
    } else {
        b.max_online.saturating_sub(others)
    };
    let own_target = wanted.max(floor).min(cap).min(available);
    // rank the pool: who is online for presence after this pass
    let mut ranked: Vec<(u8, i64, u64, u32)> = rows
        .iter()
        .filter(|r| in_pool(r) && !r.held)
        .map(|r| {
            let group = match (r.scheduled, r.owned) {
                (true, true) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (false, false) => 3,
            };
            // (the floor brings the next sessions early, the nearest first)
            let soon = if group == 3 { r.next_login_min } else { 0 };
            (group, soon, r.order, r.id)
        })
        .collect();
    ranked.sort_unstable();
    let chosen: Vec<u32> = ranked
        .iter()
        .take(own_target as usize)
        .map(|k| k.3)
        .collect();
    let picked = |id: u32| chosen.contains(&id);
    let mut orders = Orders {
        target: own_target + others,
        ..Orders::default()
    };
    orders.start = ranked
        .iter()
        .take(own_target as usize)
        .filter(|k| k.0 == 1 || k.0 == 3)
        .map(|k| k.3)
        .take(starts_left)
        .collect();
    // presence's bots that are not chosen go, the most harmless and the longest online first
    let mut leaving: Vec<(u8, std::cmp::Reverse<u32>, u32)> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.owned && !r.director && !picked(r.id))
    {
        if !r.online {
            // (queued and not wanted any more: taken off the queue, free of the limit)
            orders.stop.push(r.id);
            continue;
        }
        match r.stop_rank {
            Some(rank) => leaving.push((rank, std::cmp::Reverse(r.online_secs), r.id)),
            None => orders.waiting += 1,
        }
    }
    leaving.sort_unstable();
    for (i, (_, _, id)) in leaving.into_iter().enumerate() {
        if i < stops_left {
            orders.stop.push(id);
        } else {
            orders.waiting += 1;
        }
    }
    orders
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32) -> Row {
        Row {
            id,
            online: false,
            owned: false,
            director: false,
            scheduled: false,
            held: false,
            stop_rank: Some(0),
            online_secs: 600,
            next_login_min: 60,
            order: u64::from(id),
        }
    }

    fn mine(id: u32, scheduled: bool) -> Row {
        Row {
            online: true,
            owned: true,
            scheduled,
            ..row(id)
        }
    }

    fn wants(id: u32) -> Row {
        Row {
            scheduled: true,
            ..row(id)
        }
    }

    const FREE: Bounds = Bounds {
        min_online: 0,
        max_online: 0,
    };

    #[test]
    fn scheduled_bots_log_in_and_the_others_log_out() {
        let rows = [mine(1, true), mine(2, false), wants(3), row(4)];
        let o = decide(&rows, FREE, 10, 10);
        assert_eq!(o.start, vec![3]);
        assert_eq!(o.stop, vec![2]);
        assert_eq!(o.target, 2);
        assert_eq!(o.waiting, 0);
    }

    #[test]
    fn the_operators_and_the_directors_bots_are_never_touched_but_count() {
        // 5: the operator's bot (online, not ours); 6: the director's (offline, but owned by it)
        let rows = [
            Row {
                online: true,
                ..row(5)
            },
            Row {
                director: true,
                scheduled: true,
                ..row(6)
            },
            mine(1, true),
            wants(2),
            wants(3),
        ];
        let o = decide(
            &rows,
            Bounds {
                min_online: 0,
                max_online: 3,
            },
            10,
            10,
        );
        // the operator's bot takes one of the three places: 1 stays, 2 comes, 3 waits
        assert_eq!(o.start, vec![2]);
        assert!(o.stop.is_empty());
        assert_eq!(o.target, 3);
        assert!(!o.start.contains(&6) && !o.stop.contains(&5));
    }

    #[test]
    fn the_cap_keeps_the_bots_online_first_and_stops_the_extra_ones_of_its_own() {
        let rows = [mine(1, true), mine(2, true), mine(3, true), wants(4)];
        let o = decide(
            &rows,
            Bounds {
                min_online: 0,
                max_online: 2,
            },
            10,
            10,
        );
        assert!(o.start.is_empty(), "no churn: 4 waits");
        assert_eq!(o.stop, vec![3]);
    }

    #[test]
    fn the_floor_keeps_bots_longer_and_brings_the_next_sessions_early() {
        let rows = [
            mine(1, false),
            Row {
                next_login_min: 300,
                ..row(2)
            },
            Row {
                next_login_min: 20,
                ..row(3)
            },
        ];
        let o = decide(
            &rows,
            Bounds {
                min_online: 2,
                max_online: 0,
            },
            10,
            10,
        );
        assert!(o.stop.is_empty(), "1 stays for the floor");
        assert_eq!(o.start, vec![3], "the nearest session comes early");
    }

    #[test]
    fn a_bot_the_operator_stopped_stays_offline() {
        let rows = [Row {
            held: true,
            ..wants(1)
        }];
        let o = decide(
            &rows,
            Bounds {
                min_online: 1,
                max_online: 0,
            },
            10,
            10,
        );
        assert!(o.start.is_empty());
        assert_eq!(o.target, 0);
    }

    #[test]
    fn busy_bots_wait_and_the_limits_hold() {
        let rows = [
            Row {
                stop_rank: None,
                ..mine(1, false)
            },
            Row {
                stop_rank: Some(1),
                online_secs: 9_000,
                ..mine(2, false)
            },
            Row {
                stop_rank: Some(0),
                online_secs: 100,
                ..mine(3, false)
            },
            Row {
                stop_rank: Some(0),
                online_secs: 5_000,
                ..mine(4, false)
            },
            wants(5),
            wants(6),
            wants(7),
        ];
        let o = decide(&rows, FREE, 2, 2);
        // the idle ones first (the longest online first), the trade-pack carrier waits too
        assert_eq!(o.stop, vec![4, 3]);
        assert_eq!(o.waiting, 2);
        assert_eq!(o.start, vec![5, 6]);
    }

    #[test]
    fn a_queued_bot_that_is_not_wanted_any_more_leaves_the_queue_for_free() {
        let rows = [
            Row {
                owned: true,
                ..row(1)
            },
            mine(2, false),
        ];
        let o = decide(&rows, FREE, 10, 0);
        assert_eq!(o.stop, vec![1]);
        assert_eq!(o.waiting, 1);
    }
}
