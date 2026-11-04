//! Following a leader through a dungeon door: the phases of a companion as a pure state machine.
//!
//! ```text
//!  Outside ─ the leader is in a dungeon ─► Door ─ at the entrance ─► Cast ─► Load ─► Inside
//!     ▲                                      │                         │                │
//!     └──── the leader is outside again ◄────┴────────────────────────┘                │ the leader left
//!     │                                                                                ▼
//!     └───────────────────────────── out ◄──── Exit (walk to the exit, cast it) ◄──────┘
//!                          any failure: Away (stand where you are until the leader is out)
//! ```
//!
//! The glue (the companion goal) builds a [`Facts`] every tick from the team log, the bot's place
//! and the doodads in view; [`Machine::step`] says what to do ([`Intent`]). The server routes a
//! team member who casts the entrance skill to the team's instance, so a companion walks to the
//! entrance the leader used, casts the skill the way the dungeon goal does (and answers the same
//! refusals, see [`crate::bot::goals::dungeon`]'s `entry` module) and then follows inside. The
//! leader's state is from the position packets (`SCTeamRemoteMembersEx`) or, when none arrive,
//! from the leader vanishing at an entrance.

use crate::bot::goals::dungeon::entry::{self, EntryError, Reaction};

/// A leader state must hold this long before the machine acts on it (a position packet in the
/// middle of a load may show an odd instance for a moment).
pub const STABLE_MS: u64 = 1_500;
/// Waiting for the entrance doodad to show once the bot stands at its place.
pub const DOOR_WAIT_MS: u64 = 20_000;
/// Gap between two casts of the entrance skill.
pub const CAST_GAP_MS: u64 = 7_000;
/// Casts of the entrance skill.
pub const CAST_TRIES: u8 = 4;
/// The whole load, from the first sign of it.
pub const LOAD_MS: u64 = 60_000;
/// Gap between two casts of the exit skill.
pub const EXIT_GAP_MS: u64 = 6_000;
/// Casts of the exit skill.
pub const EXIT_TRIES: u8 = 4;
/// Waiting for the exit doodad to show.
pub const EXIT_DOOR_WAIT_MS: u64 = 20_000;
/// Failed trips to a door before the bot gives up.
pub const TRIP_RETRIES: u8 = 3;
/// The pace (m/s) a trip to the entrance is budgeted at (the dungeon goal's figure).
pub const TRAVEL_BUDGET_MPS: f32 = 3.0;
/// The shortest budget of a trip to the entrance.
pub const TRAVEL_MIN_MS: u64 = 3 * 60_000;
/// The longest budget of a trip to the entrance.
pub const TRAVEL_MAX_MS: u64 = 30 * 60_000;

/// Where the leader is, as far as the bot can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leader {
    /// Nothing known (no position packet, not in view).
    Unknown,
    /// In the main world.
    Main,
    /// In an instance (its id).
    Dungeon(u32),
    /// It was in view next to an entrance and is gone, and no position packet says otherwise.
    Vanished,
}

/// The phase of the companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Following in the main world.
    Outside,
    /// Walking to the entrance.
    Door,
    /// Casting the entrance skill.
    Cast,
    /// Between the server's load and the bot's answer.
    Load,
    /// In the dungeon with the leader.
    Inside,
    /// Walking to the exit and casting it.
    Exit,
    /// Gave up (see [`Machine::why`]): stands until the leader is out.
    Away,
}

impl Phase {
    /// A word for the detail line.
    pub fn word(self) -> &'static str {
        match self {
            Phase::Outside => "following",
            Phase::Door => "going to the dungeon door",
            Phase::Cast => "entering the dungeon",
            Phase::Load => "loading the dungeon",
            Phase::Inside => "in the dungeon",
            Phase::Exit => "leaving the dungeon",
            Phase::Away => "cannot follow into the dungeon",
        }
    }
}

/// Why the machine gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The leader's dungeon is not known to the bot, or has no entrance.
    NoDungeon,
    /// The server refused the entry (typed).
    Refused(EntryError),
    /// The level is too low and cannot be raised.
    Level,
    /// The entrance doodad never showed.
    NoDoor,
    /// The walk to the entrance failed.
    Stuck,
    /// The walk took too long.
    Timeout,
    /// The cast was refused (out of range, ...).
    CastRefused,
    /// The server never loaded the dungeon.
    NoLoad,
    /// The exit does nothing.
    NoExit,
}

impl Why {
    /// A word for the log.
    pub fn word(self) -> &'static str {
        match self {
            Why::NoDungeon => "dungeon unknown",
            Why::Refused(e) => e.word(),
            Why::Level => "level too low",
            Why::NoDoor => "no entrance in view",
            Why::Stuck => "stuck on the way",
            Why::Timeout => "took too long",
            Why::CastRefused => "cast refused",
            Why::NoLoad => "no load",
            Why::NoExit => "exit does nothing",
        }
    }
}

/// What the machine looks at.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// The bot clock.
    pub now_ms: u64,
    /// `follow_into_dungeons` is on and the bot was not told to stay.
    pub enabled: bool,
    /// Where the leader is.
    pub leader: Leader,
    /// The bot is in an instance.
    pub in_instance: bool,
    /// The instance the bot is in (0 = main world).
    pub my_instance: u32,
    /// A load is under way.
    pub loading: bool,
    /// The leader's dungeon has an entrance the bot knows.
    pub door_known: bool,
    /// Metres to the entrance.
    pub door_dist: f32,
    /// The bot stands within reach of the entrance doodad.
    pub at_door: bool,
    /// The entrance doodad is in view.
    pub door_in_view: bool,
    /// The walk of the bot failed (stuck or timed out).
    pub trip_failed: bool,
    /// The refusal the server sent since the last cast.
    pub refused: Option<EntryError>,
    /// The last cast of the entrance skill was refused.
    pub cast_refused: bool,
    /// The bot's level is below the dungeon's minimum, and it cannot raise it.
    pub level_low: bool,
    /// The bot's level is below the dungeon's minimum and it may raise it (GM).
    pub can_raise: bool,
    /// The bot stands within reach of an exit doodad.
    pub at_exit: bool,
    /// An exit doodad is in view.
    pub exit_in_view: bool,
    /// The last cast of the exit skill was refused.
    pub exit_refused: bool,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            now_ms: 0,
            enabled: true,
            leader: Leader::Unknown,
            in_instance: false,
            my_instance: 0,
            loading: false,
            door_known: true,
            door_dist: 0.0,
            at_door: false,
            door_in_view: false,
            trip_failed: false,
            refused: None,
            cast_refused: false,
            level_low: false,
            can_raise: false,
            at_exit: false,
            exit_in_view: false,
            exit_refused: false,
        }
    }
}

/// What to do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Follow the leader (in view or by the position packets); the machine has nothing to add.
    Follow,
    /// Walk to the entrance.
    GoToDoor,
    /// Stand and wait.
    Stand,
    /// Cast the entrance skill.
    CastEntry,
    /// Raise the level to the dungeon's minimum (GM), then cast again.
    RaiseLevel,
    /// Walk to the nearest exit.
    GoToExit,
    /// Cast the exit skill.
    CastExit,
    /// Leave the team: the exit does nothing and the server moves a bot that left after a while.
    LeaveTeam,
}

/// The machine. See the module docs.
#[derive(Debug, Clone)]
pub struct Machine {
    phase: Phase,
    why: Option<Why>,
    /// Bot clock of the last phase change.
    since_ms: u64,
    /// What the leader showed last and since when.
    leader: (Leader, u64),
    casts: u8,
    last_cast_ms: u64,
    trips_failed: u8,
    /// Walking budget for the trip to the door, set when the trip starts.
    budget_ms: u64,
    /// The instance id the bot gave up for (a different leader instance starts over).
    gave_up_for: Leader,
    exit_left_team: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Machine::new()
    }
}

impl Machine {
    /// A machine that follows in the main world.
    pub fn new() -> Machine {
        Machine {
            phase: Phase::Outside,
            why: None,
            since_ms: 0,
            leader: (Leader::Unknown, 0),
            casts: 0,
            last_cast_ms: 0,
            trips_failed: 0,
            budget_ms: TRAVEL_MIN_MS,
            gave_up_for: Leader::Unknown,
            exit_left_team: false,
        }
    }

    /// The phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Why the machine gave up, in [`Phase::Away`].
    pub fn why(&self) -> Option<Why> {
        self.why
    }

    /// Bot clock of the last phase change.
    pub fn since_ms(&self) -> u64 {
        self.since_ms
    }

    /// Whether the glue should drive the combat controller's neighbourhood itself (inside, where
    /// the bot arrives among monsters).
    pub fn inside(&self) -> bool {
        matches!(self.phase, Phase::Inside | Phase::Load | Phase::Exit)
    }

    /// The most the trip to the door may take, in milliseconds, for a bot `dist` metres away.
    pub fn travel_budget_ms(dist: f32) -> u64 {
        if !dist.is_finite() || dist <= 0.0 {
            return TRAVEL_MIN_MS;
        }
        ((dist / TRAVEL_BUDGET_MPS * 1000.0) as u64 + 2 * 60_000)
            .clamp(TRAVEL_MIN_MS, TRAVEL_MAX_MS)
    }

    fn go(&mut self, phase: Phase, now_ms: u64) {
        self.phase = phase;
        self.since_ms = now_ms;
        if phase != Phase::Away {
            self.why = None;
        }
    }

    fn give_up(&mut self, why: Why, f: &Facts) -> Intent {
        self.go(Phase::Away, f.now_ms);
        self.why = Some(why);
        self.gave_up_for = self.leader.0;
        Intent::Follow
    }

    /// The leader state that has held for [`STABLE_MS`], else the one before it.
    fn stable(&self, f: &Facts) -> Leader {
        if f.now_ms.saturating_sub(self.leader.1) >= STABLE_MS {
            self.leader.0
        } else {
            Leader::Unknown
        }
    }

    /// One tick.
    pub fn step(&mut self, f: &Facts) -> Intent {
        if f.leader != self.leader.0 {
            self.leader = (f.leader, f.now_ms);
        }
        let leader = self.stable(f);
        let in_dungeon = matches!(leader, Leader::Dungeon(_) | Leader::Vanished);
        match self.phase {
            Phase::Outside => {
                if f.loading {
                    self.go(Phase::Load, f.now_ms);
                    return Intent::Stand;
                }
                if f.in_instance {
                    return self.arrived(leader, f);
                }
                if f.enabled && in_dungeon {
                    if !f.door_known {
                        return self.give_up(Why::NoDungeon, f);
                    }
                    self.casts = 0;
                    self.last_cast_ms = 0;
                    self.trips_failed = 0;
                    self.budget_ms = Machine::travel_budget_ms(f.door_dist);
                    self.go(Phase::Door, f.now_ms);
                    return Intent::GoToDoor;
                }
                Intent::Follow
            }
            Phase::Door => {
                if !f.enabled || leader == Leader::Main {
                    self.go(Phase::Outside, f.now_ms);
                    return Intent::Follow;
                }
                if f.loading || f.in_instance {
                    self.go(Phase::Load, f.now_ms);
                    return Intent::Stand;
                }
                if f.at_door {
                    if f.door_in_view {
                        self.go(Phase::Cast, f.now_ms);
                        return self.cast(f);
                    }
                    if f.now_ms.saturating_sub(self.since_ms) > DOOR_WAIT_MS {
                        return self.give_up(Why::NoDoor, f);
                    }
                    return Intent::Stand;
                }
                if f.trip_failed {
                    self.trips_failed += 1;
                    if self.trips_failed >= TRIP_RETRIES {
                        return self.give_up(Why::Stuck, f);
                    }
                }
                if f.now_ms.saturating_sub(self.since_ms) > self.budget_ms {
                    return self.give_up(Why::Timeout, f);
                }
                Intent::GoToDoor
            }
            Phase::Cast => {
                if f.loading || f.in_instance {
                    self.go(Phase::Load, f.now_ms);
                    return Intent::Stand;
                }
                if !f.enabled || leader == Leader::Main {
                    self.go(Phase::Outside, f.now_ms);
                    return Intent::Follow;
                }
                if let Some(e) = f.refused {
                    match entry::reaction(e) {
                        Reaction::Retry => {}
                        Reaction::RaiseLevel if f.can_raise => {
                            self.last_cast_ms = f.now_ms;
                            return Intent::RaiseLevel;
                        }
                        Reaction::RaiseLevel => return self.give_up(Why::Level, f),
                        Reaction::GiveUp { .. } => return self.give_up(Why::Refused(e), f),
                    }
                }
                if f.level_low {
                    return self.give_up(Why::Level, f);
                }
                if f.cast_refused && self.casts >= 2 {
                    return self.give_up(Why::CastRefused, f);
                }
                if f.now_ms.saturating_sub(self.last_cast_ms) >= CAST_GAP_MS {
                    if self.casts >= CAST_TRIES {
                        return self.give_up(Why::NoLoad, f);
                    }
                    return self.cast(f);
                }
                Intent::Stand
            }
            Phase::Load => {
                if f.loading {
                    if f.now_ms.saturating_sub(self.since_ms) > LOAD_MS {
                        return self.give_up(Why::NoLoad, f);
                    }
                    return Intent::Stand;
                }
                if f.in_instance {
                    return self.arrived(leader, f);
                }
                // the load ended without a dungeon: the entry did not work
                if f.now_ms.saturating_sub(self.since_ms) > CAST_GAP_MS {
                    self.go(Phase::Outside, f.now_ms);
                }
                Intent::Stand
            }
            Phase::Inside => {
                if !f.in_instance {
                    self.go(Phase::Outside, f.now_ms);
                    return Intent::Follow;
                }
                self.arrived(leader, f)
            }
            Phase::Exit => {
                if !f.in_instance {
                    self.go(Phase::Outside, f.now_ms);
                    return Intent::Follow;
                }
                if leader == Leader::Dungeon(f.my_instance) {
                    self.go(Phase::Inside, f.now_ms);
                    return Intent::Follow;
                }
                if self.exit_left_team {
                    return Intent::Stand;
                }
                if f.at_exit && f.exit_in_view {
                    if f.exit_refused && self.casts >= 2 {
                        self.exit_left_team = true;
                        return Intent::LeaveTeam;
                    }
                    if f.now_ms.saturating_sub(self.last_cast_ms) >= EXIT_GAP_MS {
                        if self.casts >= EXIT_TRIES {
                            self.exit_left_team = true;
                            return Intent::LeaveTeam;
                        }
                        self.casts += 1;
                        self.last_cast_ms = f.now_ms.max(1);
                        return Intent::CastExit;
                    }
                    return Intent::Stand;
                }
                if f.now_ms.saturating_sub(self.since_ms) > EXIT_DOOR_WAIT_MS + 5 * 60_000 {
                    return self.give_up(Why::NoExit, f);
                }
                Intent::GoToExit
            }
            Phase::Away => {
                // stand until the leader is out of the dungeon (or somewhere else than where the
                // bot gave up for), then follow again
                if f.in_instance && leader == Leader::Main {
                    self.go(Phase::Exit, f.now_ms);
                    self.casts = 0;
                    self.last_cast_ms = 0;
                    self.exit_left_team = false;
                    return Intent::GoToExit;
                }
                if !f.in_instance && (leader == Leader::Main || leader != self.gave_up_for) {
                    self.go(Phase::Outside, f.now_ms);
                }
                Intent::Follow
            }
        }
    }

    /// The bot is in an instance: with the leader, or the leader is elsewhere.
    fn arrived(&mut self, leader: Leader, f: &Facts) -> Intent {
        match leader {
            Leader::Main => {
                self.casts = 0;
                self.last_cast_ms = 0;
                self.exit_left_team = false;
                self.go(Phase::Exit, f.now_ms);
                Intent::GoToExit
            }
            Leader::Dungeon(i) if i != f.my_instance && i != 0 => {
                // the leader is in another run of the dungeon: out and in again
                self.casts = 0;
                self.last_cast_ms = 0;
                self.exit_left_team = false;
                self.go(Phase::Exit, f.now_ms);
                Intent::GoToExit
            }
            _ => {
                if self.phase != Phase::Inside {
                    self.go(Phase::Inside, f.now_ms);
                }
                Intent::Follow
            }
        }
    }

    fn cast(&mut self, f: &Facts) -> Intent {
        if f.can_raise && f.level_low {
            return Intent::RaiseLevel;
        }
        self.casts += 1;
        self.last_cast_ms = f.now_ms.max(1);
        Intent::CastEntry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(now_ms: u64, leader: Leader) -> Facts {
        Facts {
            now_ms,
            leader,
            ..Facts::default()
        }
    }

    /// Steps `m` with the same facts at `from..=to` ms in 500 ms steps; returns the last intent.
    fn run(m: &mut Machine, f: Facts, from: u64, to: u64) -> Intent {
        let mut last = Intent::Follow;
        let mut t = from;
        while t <= to {
            last = m.step(&Facts { now_ms: t, ..f });
            t += 500;
        }
        last
    }

    #[test]
    fn a_companion_outside_follows_while_the_leader_is_outside() {
        let mut m = Machine::new();
        assert_eq!(m.step(&at(0, Leader::Main)), Intent::Follow);
        assert_eq!(m.step(&at(1_000, Leader::Unknown)), Intent::Follow);
        assert_eq!(m.phase(), Phase::Outside);
    }

    #[test]
    fn the_leader_going_in_sends_the_companion_to_the_door_after_a_moment() {
        let mut m = Machine::new();
        m.step(&at(0, Leader::Main));
        // the leader flips to an instance: not acted on at once
        assert_eq!(m.step(&at(1_000, Leader::Dungeon(7))), Intent::Follow);
        assert_eq!(
            m.step(&at(1_000 + STABLE_MS - 1, Leader::Dungeon(7))),
            Intent::Follow
        );
        // a flicker back resets the clock
        assert_eq!(m.step(&at(2_600, Leader::Main)), Intent::Follow);
        assert_eq!(m.step(&at(2_700, Leader::Dungeon(7))), Intent::Follow);
        let go = m.step(&at(2_700 + STABLE_MS, Leader::Dungeon(7)));
        assert_eq!(go, Intent::GoToDoor);
        assert_eq!(m.phase(), Phase::Door);
    }

    #[test]
    fn the_whole_way_in_and_out() {
        let mut m = Machine::new();
        let inside = Leader::Dungeon(7);
        run(&mut m, at(0, inside), 0, 2_000);
        assert_eq!(m.phase(), Phase::Door);
        // walking
        let walking = Facts {
            door_dist: 300.0,
            ..at(0, inside)
        };
        assert_eq!(run(&mut m, walking, 2_500, 5_000), Intent::GoToDoor);
        // at the door but the doodad is not in view yet: stand
        let wait = Facts {
            at_door: true,
            ..at(0, inside)
        };
        assert_eq!(run(&mut m, wait, 5_500, 6_000), Intent::Stand);
        // in view: cast
        let door = Facts {
            at_door: true,
            door_in_view: true,
            ..at(0, inside)
        };
        assert_eq!(
            m.step(&Facts {
                now_ms: 6_500,
                ..door
            }),
            Intent::CastEntry
        );
        assert_eq!(m.phase(), Phase::Cast);
        // the server answers: processing, then the load
        let loading = Facts {
            loading: true,
            ..door
        };
        assert_eq!(
            m.step(&Facts {
                now_ms: 8_000,
                ..loading
            }),
            Intent::Stand
        );
        assert_eq!(m.phase(), Phase::Load);
        let inst = Facts {
            in_instance: true,
            my_instance: 7,
            ..at(0, inside)
        };
        assert_eq!(
            m.step(&Facts {
                now_ms: 12_000,
                ..inst
            }),
            Intent::Follow
        );
        assert_eq!(m.phase(), Phase::Inside);
        // the leader comes out: the companion goes to the exit and casts it
        let out = Facts {
            leader: Leader::Main,
            ..inst
        };
        assert_eq!(run(&mut m, out, 13_000, 15_000), Intent::GoToExit);
        assert_eq!(m.phase(), Phase::Exit);
        let at_exit = Facts {
            at_exit: true,
            exit_in_view: true,
            ..out
        };
        assert_eq!(
            m.step(&Facts {
                now_ms: 16_000,
                ..at_exit
            }),
            Intent::CastExit
        );
        assert_eq!(
            m.step(&Facts {
                now_ms: 17_000,
                ..at_exit
            }),
            Intent::Stand
        );
        // back in the main world
        let main = at(20_000, Leader::Main);
        assert_eq!(m.step(&main), Intent::Follow);
        assert_eq!(m.phase(), Phase::Outside);
    }

    #[test]
    fn the_entrance_skill_is_cast_a_few_times_and_then_the_load_is_missing() {
        let mut m = Machine::new();
        let inside = Leader::Dungeon(7);
        run(&mut m, at(0, inside), 0, 2_000);
        let door = Facts {
            at_door: true,
            door_in_view: true,
            ..at(0, inside)
        };
        let mut casts = 0;
        let mut t = 2_500;
        let mut gave_up = false;
        while t < 60_000 {
            match m.step(&Facts { now_ms: t, ..door }) {
                Intent::CastEntry => casts += 1,
                _ if m.phase() == Phase::Away => {
                    gave_up = true;
                    break;
                }
                _ => {}
            }
            t += 500;
        }
        assert!(gave_up, "the machine must give up");
        assert_eq!(casts, usize::from(CAST_TRIES));
        assert_eq!(m.why(), Some(Why::NoLoad));
        // gave up: it follows the leader on the ground and tries again for a new dungeon only
        // when the leader was outside in between
        assert_eq!(
            m.step(&Facts {
                now_ms: t + 500,
                ..door
            }),
            Intent::Follow
        );
        assert_eq!(m.phase(), Phase::Away);
        assert_eq!(m.step(&at(t + 1_000, Leader::Main)), Intent::Follow);
        assert_eq!(m.step(&at(t + 1_500, Leader::Main)), Intent::Follow);
        assert_eq!(m.phase(), Phase::Outside);
    }

    #[test]
    fn refusals_are_answered_by_kind() {
        let go = |refused: Option<EntryError>, can_raise: bool| {
            let mut m = Machine::new();
            let inside = Leader::Dungeon(7);
            run(&mut m, at(0, inside), 0, 2_000);
            let door = Facts {
                at_door: true,
                door_in_view: true,
                ..at(0, inside)
            };
            assert_eq!(
                m.step(&Facts {
                    now_ms: 2_500,
                    ..door
                }),
                Intent::CastEntry
            );
            let i = m.step(&Facts {
                now_ms: 3_000,
                refused,
                can_raise,
                ..door
            });
            (m, i)
        };
        // a full server: wait and cast again after the gap
        let (mut m, i) = go(Some(EntryError::TryLater), false);
        assert_eq!((i, m.phase()), (Intent::Stand, Phase::Cast));
        let door = Facts {
            at_door: true,
            door_in_view: true,
            ..at(0, Leader::Dungeon(7))
        };
        assert_eq!(
            m.step(&Facts {
                now_ms: 2_500 + CAST_GAP_MS,
                ..door
            }),
            Intent::CastEntry
        );
        // the entry limit: give up for this dungeon
        let (m, i) = go(Some(EntryError::VisitLimit), false);
        assert_eq!(
            (i, m.phase(), m.why()),
            (
                Intent::Follow,
                Phase::Away,
                Some(Why::Refused(EntryError::VisitLimit))
            )
        );
        // a low level: raise it when allowed, else give up
        let (m, i) = go(Some(EntryError::Level), true);
        assert_eq!((i, m.phase()), (Intent::RaiseLevel, Phase::Cast));
        let (m, i) = go(Some(EntryError::Level), false);
        assert_eq!(
            (i, m.phase(), m.why()),
            (Intent::Follow, Phase::Away, Some(Why::Level))
        );
    }

    #[test]
    fn the_leader_coming_out_before_the_bot_got_in_ends_the_trip() {
        let mut m = Machine::new();
        run(&mut m, at(0, Leader::Dungeon(7)), 0, 2_000);
        assert_eq!(m.phase(), Phase::Door);
        run(&mut m, at(0, Leader::Main), 2_500, 5_000);
        assert_eq!(m.phase(), Phase::Outside);
    }

    #[test]
    fn a_leader_that_vanished_at_an_entrance_counts_as_inside() {
        let mut m = Machine::new();
        run(&mut m, at(0, Leader::Vanished), 0, 2_000);
        assert_eq!(m.phase(), Phase::Door);
    }

    #[test]
    fn a_dungeon_without_a_door_or_a_switch_that_is_off_keeps_the_bot_following() {
        let mut m = Machine::new();
        let f = Facts {
            door_known: false,
            ..at(0, Leader::Dungeon(7))
        };
        run(&mut m, f, 0, 2_000);
        assert_eq!((m.phase(), m.why()), (Phase::Away, Some(Why::NoDungeon)));
        let mut off = Machine::new();
        let f = Facts {
            enabled: false,
            ..at(0, Leader::Dungeon(7))
        };
        assert_eq!(run(&mut off, f, 0, 5_000), Intent::Follow);
        assert_eq!(off.phase(), Phase::Outside);
    }

    #[test]
    fn a_long_walk_has_a_budget_and_a_stuck_bot_gives_up() {
        assert_eq!(Machine::travel_budget_ms(0.0), TRAVEL_MIN_MS);
        assert_eq!(Machine::travel_budget_ms(f32::NAN), TRAVEL_MIN_MS);
        assert!(Machine::travel_budget_ms(3_000.0) > Machine::travel_budget_ms(300.0));
        assert_eq!(Machine::travel_budget_ms(1.0e9), TRAVEL_MAX_MS);
        let mut m = Machine::new();
        let inside = Leader::Dungeon(7);
        run(&mut m, at(0, inside), 0, 2_000);
        let stuck = Facts {
            trip_failed: true,
            door_dist: 100.0,
            ..at(0, inside)
        };
        let mut last = Intent::GoToDoor;
        for k in 0..u64::from(TRIP_RETRIES) + 1 {
            last = m.step(&Facts {
                now_ms: 3_000 + k * 500,
                ..stuck
            });
        }
        assert_eq!((last, m.why()), (Intent::Follow, Some(Why::Stuck)));
        // the budget: standing still for ever
        let mut m = Machine::new();
        run(&mut m, at(0, inside), 0, 2_000);
        let far = Facts {
            door_dist: 100.0,
            ..at(0, inside)
        };
        m.step(&Facts {
            now_ms: 3_000,
            ..far
        });
        let late = 2_000 + Machine::travel_budget_ms(100.0) + 1_000;
        m.step(&Facts {
            now_ms: late,
            ..far
        });
        assert_eq!(m.why(), Some(Why::Timeout));
    }

    #[test]
    fn a_bot_inside_with_an_absent_leader_goes_out_and_a_leader_in_another_run_means_out_and_in() {
        // a bot that is in the dungeon when the leader is outside leaves
        let mut m = Machine::new();
        let f = Facts {
            in_instance: true,
            my_instance: 7,
            ..at(0, Leader::Main)
        };
        run(&mut m, f, 0, 2_000);
        assert_eq!(m.phase(), Phase::Exit);
        // the leader in the same instance: it stays
        let mut m = Machine::new();
        let f = Facts {
            in_instance: true,
            my_instance: 7,
            ..at(0, Leader::Dungeon(7))
        };
        assert_eq!(run(&mut m, f, 0, 2_000), Intent::Follow);
        assert_eq!(m.phase(), Phase::Inside);
        // the leader in another instance
        let g = Facts {
            leader: Leader::Dungeon(9),
            ..f
        };
        run(&mut m, g, 2_500, 5_000);
        assert_eq!(m.phase(), Phase::Exit);
        // an unknown leader state inside keeps the bot where it is
        let mut m = Machine::new();
        let u = Facts {
            in_instance: true,
            my_instance: 7,
            ..at(0, Leader::Unknown)
        };
        assert_eq!(run(&mut m, u, 0, 5_000), Intent::Follow);
        assert_eq!(m.phase(), Phase::Inside);
    }

    #[test]
    fn an_exit_that_does_nothing_makes_the_bot_leave_the_team() {
        let mut m = Machine::new();
        let f = Facts {
            in_instance: true,
            my_instance: 7,
            at_exit: true,
            exit_in_view: true,
            ..at(0, Leader::Main)
        };
        let mut casts = 0;
        let mut left = false;
        let mut t = 0;
        while t < 80_000 && !left {
            match m.step(&Facts { now_ms: t, ..f }) {
                Intent::CastExit => casts += 1,
                Intent::LeaveTeam => left = true,
                _ => {}
            }
            t += 500;
        }
        assert!(left);
        assert_eq!(casts, usize::from(EXIT_TRIES));
        // (once it left, it waits for the server)
        assert_eq!(
            m.step(&Facts {
                now_ms: t + 500,
                ..f
            }),
            Intent::Stand
        );
    }

    #[test]
    fn every_phase_and_reason_has_a_word() {
        for p in [
            Phase::Outside,
            Phase::Door,
            Phase::Cast,
            Phase::Load,
            Phase::Inside,
            Phase::Exit,
            Phase::Away,
        ] {
            assert!(!p.word().is_empty());
        }
        for w in [
            Why::NoDungeon,
            Why::Refused(EntryError::Full),
            Why::Level,
            Why::NoDoor,
            Why::Stuck,
            Why::Timeout,
            Why::CastRefused,
            Why::NoLoad,
            Why::NoExit,
        ] {
            assert!(!w.word().is_empty());
        }
        assert!(Machine::new().inside() == false);
    }
}
