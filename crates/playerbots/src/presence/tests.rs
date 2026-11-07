//! The presence glue without a hub: the state the supervisor keeps up to date and the passes.

use super::*;
use crate::roster::Roster;

const EVENING: Traits = Traits {
    dedication: 0.5,
    chronotype: Chronotype::Evening,
};
/// Local day 20000 (a Wednesday), 20:30.
const NOW_LOCAL: i64 = 20_000 * DAY_MIN + 20 * 60 + 30;

/// The defaults without the limit on log-ins.
fn free() -> PresenceCfg {
    PresenceCfg {
        max_starts_per_min: 1000,
        ..PresenceCfg::default()
    }
}

fn inner(cfg: PresenceCfg, social: bool) -> Inner {
    let path = std::env::temp_dir().join("pb-presence-never-written.json");
    Inner {
        cfg: PresenceCfg {
            enabled: true,
            ..cfg
        },
        personality: true,
        social,
        roster: Arc::new(SharedRoster::new(Roster::new(), path)),
        state: Mutex::new(State::default()),
    }
}

/// Bots `1..=n` with the evening rhythm: the ones that play at [`NOW_LOCAL`] and the ones that do
/// not.
fn evening_bots(n: u32) -> (Vec<(u32, Traits)>, Vec<u32>, Vec<u32>) {
    let bots: Vec<(u32, Traits)> = (1..=n).map(|id| (id, EVENING)).collect();
    let (on, off): (Vec<u32>, Vec<u32>) =
        (1..=n).partition(|id| schedule::online_at(*id, EVENING, NOW_LOCAL));
    (bots, on, off)
}

fn online(ids: &[u32]) -> FxHashMap<u32, Live> {
    ids.iter()
        .map(|&id| {
            (
                id,
                Live {
                    online: true,
                    stop_rank: Some(0),
                    online_secs: 900,
                },
            )
        })
        .collect()
}

#[test]
fn a_pass_logs_in_the_scheduled_bots_and_out_its_own_others() {
    let p = inner(free(), false);
    let (bots, on, off) = evening_bots(60);
    assert!(on.len() > 5 && off.len() > 5, "{} {}", on.len(), off.len());
    let none = FxHashSet::default();
    let out = p.decide(1_000_000, NOW_LOCAL, &bots, &FxHashMap::default(), &none);
    let mut started = out.start.clone();
    started.sort_unstable();
    assert_eq!(started, on);
    assert!(out.stop.is_empty());
    assert_eq!(out.snapshot.scheduled as usize, on.len());
    assert_eq!(out.snapshot.pool, 60);
    // the supervisor queues them for presence; they come online
    let pres = Presence::default();
    let _ = pres.inner.set(p);
    pres.own(&started, 1_000_000);
    // one of presence's bots is not scheduled (any more): it is logged out
    let extra = off[0];
    pres.own(&[extra], 1_000_000);
    let mut up = on.clone();
    up.push(extra);
    let i = pres.inner.get().unwrap();
    let out = i.decide(1_030_000, NOW_LOCAL, &bots, &online(&up), &none);
    assert!(out.start.is_empty(), "{:?}", out.start);
    assert_eq!(out.stop, vec![extra]);
    assert_eq!(out.snapshot.own_online as usize, on.len() + 1);
    // a bot that someone else started is never logged out by presence
    let foreign = off[1];
    up.push(foreign);
    pres.disown(&[extra]);
    let out = i.decide(1_060_000, NOW_LOCAL, &bots, &online(&up), &none);
    assert!(!out.stop.contains(&foreign));
    // the chronotype rows add up
    let evening = out.snapshot.chrono[2];
    assert_eq!(evening.0, 60);
    assert_eq!(evening.2 as usize, on.len());
}

#[test]
fn the_directors_bots_are_left_alone_and_queued_bots_that_never_came_are_forgotten() {
    let p = inner(free(), false);
    let (bots, on, _) = evening_bots(40);
    let director: FxHashSet<u32> = [on[0]].into_iter().collect();
    let out = p.decide(1_000, NOW_LOCAL, &bots, &FxHashMap::default(), &director);
    assert!(!out.start.contains(&on[0]));
    assert_eq!(out.snapshot.pool, 39);
    // queued long ago and still offline: no longer presence's (and asked for again)
    p.state.lock().owned.insert(on[1], 1_000);
    let out = p.decide(
        1_000 + QUEUE_GRACE_MS,
        NOW_LOCAL,
        &bots,
        &FxHashMap::default(),
        &director,
    );
    assert!(!p.state.lock().owned.contains_key(&on[1]));
    assert!(out.start.contains(&on[1]));
}

#[test]
fn a_bot_the_operator_stopped_waits_for_its_schedule() {
    let pres = Presence::default();
    let _ = pres.inner.set(inner(PresenceCfg::default(), false));
    let i = pres.inner.get().unwrap();
    let (bots, on, _) = evening_bots(40);
    pres.own(&[on[0]], 0);
    pres.operator_stopped(&[on[0]]);
    assert!(!pres.is_owned(on[0]));
    let none = FxHashSet::default();
    let out = i.decide(10_000, NOW_LOCAL, &bots, &FxHashMap::default(), &none);
    assert!(!out.start.contains(&on[0]), "held while its session lasts");
    // later, when the schedule has let it go, the hold ends
    let later = (NOW_LOCAL..NOW_LOCAL + 2 * DAY_MIN)
        .find(|m| !schedule::online_at(on[0], EVENING, *m))
        .unwrap();
    i.decide(20_000, later, &bots, &FxHashMap::default(), &none);
    assert!(!i.state.lock().held.contains(&on[0]));
}

#[test]
fn a_goodbye_comes_one_pass_before_the_log_out() {
    let cfg = PresenceCfg {
        goodbye_chance: 1.0,
        ..PresenceCfg::default()
    };
    let pres = Presence::default();
    let _ = pres.inner.set(inner(cfg, true));
    let i = pres.inner.get().unwrap();
    let (bots, _, off) = evening_bots(40);
    pres.own(&[off[0]], 0);
    let live = online(&[off[0]]);
    let none = FxHashSet::default();
    let out = i.decide(10_000, NOW_LOCAL, &bots, &live, &none);
    assert!(out.stop.is_empty());
    assert_eq!(out.farewells.len(), 1);
    assert_eq!(out.farewells[0].0, off[0]);
    assert!(!out.farewells[0].1.is_empty());
    let out = i.decide(30_000, NOW_LOCAL, &bots, &live, &none);
    assert_eq!(out.stop, vec![off[0]], "the next pass logs it out");
    assert!(out.farewells.is_empty());
    // without [social] no goodbye is said
    let quiet = inner(
        PresenceCfg {
            goodbye_chance: 1.0,
            ..PresenceCfg::default()
        },
        false,
    );
    quiet.state.lock().owned.insert(off[0], 0);
    let out = quiet.decide(10_000, NOW_LOCAL, &bots, &live, &none);
    assert_eq!(out.stop, vec![off[0]]);
    assert!(out.farewells.is_empty());
}

#[test]
fn log_outs_and_log_ins_keep_to_their_limits_per_minute() {
    let cfg = PresenceCfg {
        max_stops_per_min: 2,
        max_starts_per_min: 3,
        goodbye_chance: 0.0,
        ..PresenceCfg::default()
    };
    let p = inner(cfg, false);
    let (bots, on, off) = evening_bots(80);
    for id in &off[..6] {
        p.state.lock().owned.insert(*id, 0);
    }
    let live = online(&off[..6]);
    let none = FxHashSet::default();
    let out = p.decide(100_000, NOW_LOCAL, &bots, &live, &none);
    assert_eq!(out.stop.len(), 2);
    assert_eq!(out.start.len(), 3.min(on.len()));
    assert_eq!(out.snapshot.waiting, 4);
    // within the same minute nothing more
    let out = p.decide(120_000, NOW_LOCAL, &bots, &live, &none);
    assert!(out.stop.is_empty() && out.start.is_empty());
    // a minute later the next ones
    let out = p.decide(161_000, NOW_LOCAL, &bots, &live, &none);
    assert_eq!(out.stop.len(), 2);
}

#[test]
fn the_view_shows_the_last_pass_and_is_off_when_presence_is() {
    let off = Presence::default();
    assert!(off.view().is_none(), "not attached");
    assert!(!off.enabled());
    let cfg = Config::default();
    off.attach(
        &cfg,
        Arc::new(SharedRoster::new(Roster::new(), "x.json".into())),
    );
    let v = off.view().unwrap();
    assert!(!v.enabled && v.next.is_empty());
    assert!(!off.enabled());

    let pres = Presence::default();
    let _ = pres.inner.set(inner(free(), false));
    let i = pres.inner.get().unwrap();
    let (bots, on, _) = evening_bots(30);
    let out = i.decide(
        unix_ms(),
        NOW_LOCAL,
        &bots,
        &FxHashMap::default(),
        &FxHashSet::default(),
    );
    i.state.lock().last = Some(Box::new(out.snapshot));
    let v = pres.view().unwrap();
    assert!(v.enabled);
    assert_eq!(v.scheduled as usize, on.len());
    assert_eq!(v.chronotypes.len(), 4);
    assert_eq!(v.chronotypes[2].chronotype, "evening");
    assert_eq!(v.curve.len(), 24);
    assert_eq!(v.logins_last_hour as usize, on.len());
    assert_eq!(v.local_time.len(), 5);
}

#[test]
fn the_clock_follows_the_offset() {
    let cfg = PresenceCfg {
        utc_offset_hours: Some(2.0),
        ..PresenceCfg::default()
    };
    // 1970-01-02 00:30 UTC
    let now = (DAY_MIN as u64 + 30) * 60_000;
    let (offset, local) = local_minutes(&cfg, now);
    assert_eq!(offset, 120);
    assert_eq!(schedule::split(local), (1, 150));
    assert_eq!(clock(local), "02:30");
    let west = PresenceCfg {
        utc_offset_hours: Some(-5.5),
        ..PresenceCfg::default()
    };
    let (_, local) = local_minutes(&west, now);
    assert_eq!(schedule::split(local), (0, 19 * 60));
    // the machine's own zone: some offset within a day
    let (offset, _) = local_minutes(&PresenceCfg::default(), now);
    assert!(offset.abs() <= 14 * 60);
    assert!(!goodbye(&mut fastrand::Rng::with_seed(1), false).is_empty());
}
