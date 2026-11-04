//! Whether a bot may join an event and where it stands there. Pure functions, no `Ctx`.

use aa_world::event::{EventDef, Step};

use crate::config::EventsCfg;

/// A bot starts an event below this many percent of hit points: no. (Percent.)
pub const JOIN_HP_PCT: u8 = 70;
/// A bot in an event leaves below this many percent of hit points.
pub const LEAVE_HP_PCT: u8 = 40;
/// The bot stays within this many metres of the defended target.
pub const HOLD_M: f32 = 40.0;
/// Monsters further than this from the defended target are not the event's business.
pub const ENGAGE_M: f32 = 55.0;
/// Monsters further than this from the bot are not considered.
pub const VIEW_M: f32 = 70.0;
/// A bot joins only if the way is no longer than this on foot.
pub const MAX_WALK_M: f32 = 4_000.0;
/// ... or this far when portals may carry it (the goal falls back to walking if they cannot).
pub const MAX_PORTAL_M: f32 = 15_000.0;
/// The ring around the target the bots spread over, inner and outer radius.
pub const SPOT_M: (f32, f32) = (6.0, 18.0);
/// How a bot's level may fall short of the event's lowest kill target.
pub const LEVEL_SLACK: u8 = 0;
/// Nothing to fight for this long and the bot goes home.
pub const IDLE_LEAVE_MS: u64 = 8 * 60_000;
/// A wave with a timer is waited out: after the timer the monsters still have to march to the
/// target.
pub const TIMER_SLACK_MS: u64 = 3 * 60_000;

/// What the join decision looks at.
#[derive(Debug, Clone, Copy)]
pub struct Who {
    /// The bot's level.
    pub level: u8,
    /// Hit points in percent.
    pub hp_pct: u8,
    /// The bot is in an instance world, loading or dead: not available.
    pub away: bool,
    /// Distance to the target (m).
    pub dist_m: f32,
    /// The bot may be carried by a portal or a GM move: the far limit is lifted.
    pub carried: bool,
    /// Portals are switched on for the bot.
    pub portals: bool,
}

/// Why a bot does not join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// `[events] enabled` is off.
    Off,
    /// The bot is in an instance, loading, or dead.
    Away,
    /// Below [`JOIN_HP_PCT`].
    Hurt,
    /// Below the level of the event's monsters.
    Level,
    /// The event is a war, or its zone group is at war and `avoid_war_zones` is on.
    War,
    /// Too far to walk (and no portal or move to carry the bot).
    Far,
}

/// May the bot join `def`? `zone_at_war`: the event's zone group is at war.
pub fn may_join(cfg: &EventsCfg, def: &EventDef, who: &Who, zone_at_war: bool) -> Result<(), Skip> {
    if !cfg.enabled {
        return Err(Skip::Off);
    }
    if who.away {
        return Err(Skip::Away);
    }
    if !def.joinable() || (cfg.avoid_war_zones && zone_at_war) {
        return Err(Skip::War);
    }
    if who.level.saturating_add(LEVEL_SLACK) < def.min_level {
        return Err(Skip::Level);
    }
    if who.hp_pct < JOIN_HP_PCT {
        return Err(Skip::Hurt);
    }
    let limit = if who.carried {
        f32::INFINITY
    } else if who.portals {
        MAX_PORTAL_M
    } else {
        MAX_WALK_M
    };
    if who.dist_m > limit {
        return Err(Skip::Far);
    }
    Ok(())
}

/// How long a bot at the target may wait for something to fight in wave `step` before it gives
/// up: [`IDLE_LEAVE_MS`], or the wave's timer and [`TIMER_SLACK_MS`] when that is longer (a wave
/// that is only a timer, such as the first one, has nothing to fight until it runs out).
pub fn idle_limit_ms(step: Option<&Step>) -> u64 {
    let timer_ms = step.map_or(0, |s| (s.timer_s.max(0.0) * 1000.0) as u64);
    IDLE_LEAVE_MS.max(timer_ms.saturating_add(TIMER_SLACK_MS))
}

/// How long a trip of `dist_m` may take: three minutes and the walk at 3 m/s, at most 25 minutes.
pub fn trip_limit_ms(dist_m: f32) -> u64 {
    let walk = (dist_m.max(0.0) / 3.0 * 1000.0) as u64;
    (180_000 + walk).min(25 * 60_000)
}

/// The bot's own spot around the target: the same for the same bot, spread over the ring
/// [`SPOT_M`], so a dozen bots do not stand on one point.
pub fn spot(rally: [f32; 3], bot: u32, seed: u32) -> [f32; 2] {
    let h = u64::from(bot)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(seed).wrapping_mul(0xBF58_476D_1CE4_E5B9));
    let h = (h ^ (h >> 29)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let h = h ^ (h >> 32);
    let angle = (h & 0xFFFF) as f32 / 65_536.0 * std::f32::consts::TAU;
    let r = SPOT_M.0 + ((h >> 16) & 0xFFFF) as f32 / 65_536.0 * (SPOT_M.1 - SPOT_M.0);
    [rally[0] + angle.cos() * r, rally[1] + angle.sin() * r]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_world::event::{EventDef, Kill, Step};

    fn def(min_level: u8, war: Option<u16>) -> EventDef {
        EventDef {
            id: 3,
            name: "Rift".into(),
            first_wave_after_s: 0.0,
            force_end_s: 3600.0,
            target_spawner: 1,
            end_kill: (0, 0),
            steps: vec![Step {
                prog: 1,
                timer_s: 0.0,
                all: true,
                kills: vec![Kill {
                    template: 5,
                    count: 1,
                }]
                .into(),
                spawners: Box::default(),
            }]
            .into(),
            controllers: Box::default(),
            min_level,
            max_level: min_level + 5,
            war_zone_group: war,
        }
    }

    fn cfg() -> EventsCfg {
        EventsCfg {
            enabled: true,
            ..EventsCfg::default()
        }
    }

    fn who() -> Who {
        Who {
            level: 42,
            hp_pct: 100,
            away: false,
            dist_m: 1_000.0,
            carried: false,
            portals: true,
        }
    }

    #[test]
    fn a_fit_bot_joins() {
        assert_eq!(may_join(&cfg(), &def(40, None), &who(), false), Ok(()));
    }

    #[test]
    fn each_reason_to_stay_away() {
        let c = cfg();
        let d = def(40, None);
        let w = who();
        let off = EventsCfg::default();
        assert_eq!(may_join(&off, &d, &w, false), Err(Skip::Off));
        assert_eq!(
            may_join(&c, &d, &Who { away: true, ..w }, false),
            Err(Skip::Away)
        );
        // level: the lowest kill target is the bar
        assert_eq!(
            may_join(&c, &d, &Who { level: 39, ..w }, false),
            Err(Skip::Level)
        );
        assert_eq!(may_join(&c, &d, &Who { level: 40, ..w }, false), Ok(()));
        assert_eq!(may_join(&c, &def(60, None), &w, false), Err(Skip::Level));
        // hit points
        assert_eq!(
            may_join(&c, &d, &Who { hp_pct: 69, ..w }, false),
            Err(Skip::Hurt)
        );
        assert_eq!(may_join(&c, &d, &Who { hp_pct: 70, ..w }, false), Ok(()));
        // war: the event is a war, or its zone group is at war
        assert_eq!(may_join(&c, &def(40, Some(22)), &w, false), Err(Skip::War));
        assert_eq!(may_join(&c, &d, &w, true), Err(Skip::War));
        let tolerant = EventsCfg {
            avoid_war_zones: false,
            ..cfg()
        };
        assert_eq!(may_join(&tolerant, &d, &w, true), Ok(()));
        // an event the data says nothing about (no kill targets) is not joinable
        assert_eq!(may_join(&c, &def(0, None), &w, false), Err(Skip::War));
    }

    #[test]
    fn distance_depends_on_how_the_bot_gets_there() {
        let c = cfg();
        let d = def(40, None);
        let far = |dist_m, carried, portals| Who {
            dist_m,
            carried,
            portals,
            ..who()
        };
        assert_eq!(may_join(&c, &d, &far(3_900.0, false, false), false), Ok(()));
        assert_eq!(
            may_join(&c, &d, &far(4_100.0, false, false), false),
            Err(Skip::Far)
        );
        assert_eq!(may_join(&c, &d, &far(14_000.0, false, true), false), Ok(()));
        assert_eq!(
            may_join(&c, &d, &far(16_000.0, false, true), false),
            Err(Skip::Far)
        );
        assert_eq!(may_join(&c, &d, &far(30_000.0, true, true), false), Ok(()));
    }

    #[test]
    fn a_wave_with_a_timer_is_waited_out() {
        let d = def(40, None);
        let s = d.step(1).unwrap();
        // a one second timer changes nothing; no wave at all is the plain limit
        assert_eq!(idle_limit_ms(Some(s)), IDLE_LEAVE_MS);
        assert_eq!(idle_limit_ms(None), IDLE_LEAVE_MS);
        // the first wave of the Grimghast Rift: ten minutes and the march
        let timer = Step {
            timer_s: 600.0,
            ..s.clone()
        };
        assert_eq!(idle_limit_ms(Some(&timer)), 600_000 + TIMER_SLACK_MS);
        let silly = Step {
            timer_s: f32::MAX,
            ..s.clone()
        };
        assert!(idle_limit_ms(Some(&silly)) >= IDLE_LEAVE_MS);
    }

    #[test]
    fn a_trip_has_time_for_the_walk() {
        assert_eq!(trip_limit_ms(0.0), 180_000);
        assert_eq!(trip_limit_ms(900.0), 180_000 + 300_000);
        assert_eq!(trip_limit_ms(1e9), 25 * 60_000);
        assert_eq!(trip_limit_ms(-5.0), 180_000);
    }

    #[test]
    fn bots_spread_over_the_ring_and_keep_their_spot() {
        let rally = [14_800.0, 12_060.0, 180.0];
        let mut seen = Vec::new();
        for bot in 1..=60 {
            let s = spot(rally, bot, 3);
            let r = ((s[0] - rally[0]).powi(2) + (s[1] - rally[1]).powi(2)).sqrt();
            assert!(
                (SPOT_M.0 - 0.01..=SPOT_M.1 + 0.01).contains(&r),
                "bot {bot} at {r} m"
            );
            assert_eq!(s, spot(rally, bot, 3), "stable");
            seen.push(s);
        }
        // the quadrants are all used
        for (sx, sy) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
            assert!(
                seen.iter()
                    .any(|s| (s[0] - rally[0]) * sx > 3.0 && (s[1] - rally[1]) * sy > 3.0),
                "nobody in quadrant {sx} {sy}"
            );
        }
        // another event puts the same bot elsewhere
        assert_ne!(spot(rally, 5, 3), spot(rally, 5, 4));
    }
}
