//! Following: where a companion stands next to its leader and when it moves.
//!
//! Pure geometry and thresholds; the goal feeds it the positions and carries the answer out. A
//! companion starts walking when the leader is more than [`START_M`] away, runs to within a
//! per-bot distance ([`stop_radius`], so a party of bots does not stack on one spot) and stands.
//! A trip in progress is kept while its destination is still near the leader, and aimed again
//! when the leader moved on. A hurt companion rests where it is (near the leader) instead of
//! following into the next fight.

use crate::bot::motion::Pace;

/// The leader is farther than this (metres) and the companion starts walking.
pub const START_M: f32 = 8.0;
/// The leader is farther than this and the companion is catching up: it ignores monsters that are
/// not attacking it.
pub const CATCH_UP_M: f32 = 20.0;
/// A trip is aimed again when the leader stands this far from its destination.
pub const RETARGET_M: f32 = 3.0;
/// Farther than this the way is a trip (portals, a long walk), not a follow.
pub const FAR_M: f32 = 120.0;
/// A companion with fewer hit points (percent) than this rests ...
pub const REST_BELOW_PCT: u8 = 40;
/// ... until it has this many.
pub const REST_UNTIL_PCT: u8 = 70;
/// A companion does not rest when the leader is farther than this (it would be left behind).
pub const REST_LEASH_M: f32 = 30.0;

/// How close to the leader the companion with slot `slot` stops: 2.5 to 6.1 metres.
pub fn stop_radius(slot: u8) -> f32 {
    2.5 + f32::from(slot % 5) * 0.9
}

/// What the decision looks at.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// Where the companion is.
    pub me: [f32; 3],
    /// Where the leader is.
    pub leader: [f32; 3],
    /// The companion is walking.
    pub moving: bool,
    /// The destination of the trip in progress.
    pub dest: Option<(f32, f32)>,
    /// Hit points in percent.
    pub hp_pct: u8,
    /// The companion was resting on the last decision.
    pub resting: bool,
    /// A monster is attacking the companion.
    pub threatened: bool,
    /// The companion's slot (a number that differs between the bots of a party).
    pub slot: u8,
}

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Stand.
    Hold,
    /// Stand and recover.
    Rest,
    /// The trip in progress is fine.
    Keep,
    /// Start a trip.
    Walk {
        /// Destination x (the leader's).
        x: f32,
        /// Destination y.
        y: f32,
        /// Stop this far from it.
        radius: f32,
        /// Pace.
        pace: Pace,
        /// The leader is [`FAR_M`] away: a trip, not a follow.
        far: bool,
        /// The leader is [`CATCH_UP_M`] away.
        catch_up: bool,
    },
}

fn flat(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The decision. See the module docs.
pub fn decide(i: &Inputs) -> Action {
    let d = flat(i.me, i.leader);
    let tired = if i.resting {
        i.hp_pct < REST_UNTIL_PCT
    } else {
        i.hp_pct < REST_BELOW_PCT
    };
    if tired && !i.threatened && d <= REST_LEASH_M {
        return Action::Rest;
    }
    let radius = stop_radius(i.slot);
    if i.moving {
        if let Some((dx, dy)) = i.dest {
            let off = flat([dx, dy, 0.0], [i.leader[0], i.leader[1], 0.0]);
            if off <= RETARGET_M {
                return Action::Keep;
            }
        }
    } else if d <= START_M {
        return Action::Hold;
    }
    if d <= radius {
        return Action::Hold;
    }
    Action::Walk {
        x: i.leader[0],
        y: i.leader[1],
        radius,
        pace: Pace::Run,
        far: d > FAR_M,
        catch_up: d > CATCH_UP_M,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(me: f32, leader: f32) -> Inputs {
        Inputs {
            me: [me, 0.0, 0.0],
            leader: [leader, 0.0, 0.0],
            moving: false,
            dest: None,
            hp_pct: 100,
            resting: false,
            threatened: false,
            slot: 0,
        }
    }

    #[test]
    fn a_companion_stands_near_its_leader_and_follows_when_it_walks_off() {
        assert_eq!(decide(&at(0.0, 3.0)), Action::Hold);
        assert_eq!(decide(&at(0.0, START_M - 0.1)), Action::Hold);
        match decide(&at(0.0, START_M + 1.0)) {
            Action::Walk {
                x,
                radius,
                pace,
                far,
                catch_up,
                ..
            } => {
                assert_eq!(
                    (x, radius, pace, far, catch_up),
                    (START_M + 1.0, 2.5, Pace::Run, false, false)
                );
            }
            other => panic!("{other:?}"),
        }
        match decide(&at(0.0, 50.0)) {
            Action::Walk { far, catch_up, .. } => assert!(!far && catch_up),
            other => panic!("{other:?}"),
        }
        match decide(&at(0.0, 500.0)) {
            Action::Walk { far, catch_up, .. } => assert!(far && catch_up),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_trip_is_kept_until_the_leader_moves_away_from_its_destination() {
        let mut i = at(0.0, 30.0);
        i.moving = true;
        i.dest = Some((29.0, 0.0));
        assert_eq!(decide(&i), Action::Keep);
        i.dest = Some((20.0, 0.0));
        assert!(matches!(decide(&i), Action::Walk { x, .. } if x == 30.0));
        // a trip with no known destination is aimed again
        i.dest = None;
        assert!(matches!(decide(&i), Action::Walk { .. }));
        // a companion that stands within the leash does not walk, whatever it did before
        let mut j = at(0.0, 2.0);
        j.moving = true;
        j.dest = Some((50.0, 0.0));
        assert_eq!(decide(&j), Action::Hold);
    }

    #[test]
    fn the_stop_distance_differs_between_the_bots_of_a_party() {
        let radii: Vec<f32> = (0..5).map(stop_radius).collect();
        assert!(radii.windows(2).all(|w| w[0] < w[1]));
        assert!(radii[0] >= 2.5 && radii[4] < 6.5);
        assert_eq!(stop_radius(5), stop_radius(0));
    }

    #[test]
    fn a_hurt_companion_rests_near_the_leader_with_hysteresis() {
        let mut i = at(0.0, 10.0);
        i.hp_pct = REST_BELOW_PCT - 1;
        assert_eq!(decide(&i), Action::Rest);
        // 60 % is enough to start resting only below 40: it carries on, but once resting it waits
        // until 70 %
        i.hp_pct = 60;
        assert!(matches!(decide(&i), Action::Walk { .. } | Action::Hold));
        i.resting = true;
        assert_eq!(decide(&i), Action::Rest);
        i.hp_pct = REST_UNTIL_PCT;
        assert!(!matches!(decide(&i), Action::Rest));
        // a monster on it, or a leader far away: no rest
        let mut j = at(0.0, 10.0);
        j.hp_pct = 10;
        j.threatened = true;
        assert!(!matches!(decide(&j), Action::Rest));
        j.threatened = false;
        j.leader = [REST_LEASH_M + 5.0, 0.0, 0.0];
        assert!(matches!(decide(&j), Action::Walk { .. }));
    }
}
