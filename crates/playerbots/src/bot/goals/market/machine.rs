//! The meeting of an agreed sale as a pure state machine: go to the place, wait for the other side,
//! trade, leave. No packets and no world here: [`Machine::step`] reads [`Facts`] the glue
//! collected from the tick and answers with [`Act`]s the glue carries out, so every path
//! (including the island's entry and exit and the timeouts) is tested with scripted facts.
//!
//! ```text
//!   Go ──(town: at the spot)──────────────────────────► Wait ──(peer here)──► Trade
//!    └──(Mirage: at the portal)──► Enter ──(loaded)──► Inside ──(at the spot)──┘   │
//!                                                                                  ▼
//!   Over ◄──(not on the island)────────────────────────────────────────────── Leave
//! ```
//!
//! Whatever ends the meeting (the trade, a timeout, the deal falling through), a bot that is on
//! the island goes through `Leave` first: it must not wander off in an instance.

use smallvec::SmallVec;

use crate::bot::market::ads::lines::Meet;
use crate::bot::market::ads::talk::{Closed, Role};

/// Walking to the place may take this long.
pub const GO_MS: u64 = 4 * 60_000;
/// Between two casts of the portal (or exit) skill.
pub const CAST_GAP_MS: u64 = 8_000;
/// From the accepted cast to the load of the island (or to leaving it) the server may take this
/// long; after it the bot reports the silence.
pub const LOAD_MS: u64 = 30_000;
/// At the portal's spawn point the doodad must come into view within this long.
pub const PORTAL_VIEW_MS: u64 = 20_000;
/// Casts of the portal or exit skill before the bot gives up.
pub const MAX_CASTS: u8 = 3;
/// At the place, waiting for the other side.
pub const WAIT_MS: u64 = 4 * 60_000;
/// The trade, from the first request to its end.
pub const TRADE_MS: u64 = 150_000;
/// Leaving the island may take this long.
pub const LEAVE_MS: u64 = 3 * 60_000;
/// The other side is seen at this distance or less: the meeting has begun.
pub const SEE_M: f32 = 18.0;
/// A buyer walks up to this distance before it asks for the trade.
pub const ASK_M: f32 = 5.0;

/// Where the meeting stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Walking to the town spot or to the portal.
    Go,
    /// At the portal: casting, waiting for the island to load.
    Enter,
    /// On the island, walking to the mailbox.
    Inside,
    /// At the spot, waiting for the other side.
    Wait,
    /// The trade goal runs.
    Trade,
    /// Leaving the island.
    Leave,
    /// Finished.
    Over,
}

/// Where the bot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    /// The main world.
    Main,
    /// Mirage Isle.
    Mirage,
    /// Some other instance (a dungeon).
    Other,
}

/// What the glue saw this tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Facts {
    /// The bot clock.
    pub now_ms: u64,
    /// The world the bot is in.
    pub at: Where,
    /// A world load is under way.
    pub loading: bool,
    /// Within [`THERE_M`] of the spot of this phase (town spot, portal, mailbox or exit).
    pub there: bool,
    /// The last walk was abandoned.
    pub stuck: bool,
    /// The portal doodad is in view and within reach.
    pub portal_ready: bool,
    /// The exit doodad is in view and within reach.
    pub exit_ready: bool,
    /// Distance to the other side's character, when it is in view.
    pub peer_m: Option<f32>,
    /// The other side asked for the trade.
    pub peer_asked: bool,
    /// The trade goal ended: `Some(true)` when the trade was made.
    pub trade_end: Option<bool>,
    /// The agreement still stands.
    pub deal_alive: bool,
    /// The server refused a cast since the last one.
    pub refused: bool,
}

#[cfg(test)]
impl Facts {
    /// Facts of a bot standing in the main world with nothing going on.
    pub fn idle(now_ms: u64) -> Facts {
        Facts {
            now_ms,
            at: Where::Main,
            loading: false,
            there: false,
            stuck: false,
            portal_ready: false,
            exit_ready: false,
            peer_m: None,
            peer_asked: false,
            trade_end: None,
            deal_alive: true,
            refused: false,
        }
    }
}

/// Something the server did not do that it should have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The portal cast was accepted, nothing loaded within [`LOAD_MS`].
    NeverLoaded {
        /// Casts made.
        casts: u8,
    },
    /// The exit cast was accepted, the bot is still on the island after [`LOAD_MS`].
    NeverLeft {
        /// Casts made.
        casts: u8,
    },
    /// The bot stands at a spawn point of the portal doodad and the doodad is not in view.
    PortalMissing,
}

/// What the glue does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Walk to the place of this phase (the town spot, the mailbox).
    ToSpot,
    /// Walk to the portal.
    ToPortal,
    /// Walk to the exit doodad.
    ToExit,
    /// Walk up to the other side.
    ToPeer,
    /// Stand still.
    Stop,
    /// Cast the portal skill.
    CastEnter,
    /// Cast the exit skill.
    CastExit,
    /// Tell the other side that the bot is there.
    Announce,
    /// Start the trade goal.
    StartTrade,
    /// File a report.
    Report(Problem),
}

/// How the goal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Going on.
    Running,
    /// Over, with this result.
    Over(Closed),
}

/// What [`Machine::step`] decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// To carry out, in order.
    pub acts: SmallVec<[Act; 3]>,
    /// The goal's status.
    pub status: Status,
}

/// The state machine. See the module docs.
#[derive(Debug, Clone)]
pub struct Machine {
    phase: Phase,
    since_ms: u64,
    role: Role,
    meet: Meet,
    casts: u8,
    first_cast_ms: u64,
    last_cast_ms: u64,
    announced: bool,
    told: bool,
    /// Bot clock at which the bot first stood at the portal's spawn point without seeing it.
    blind_ms: u64,
    outcome: Option<Closed>,
}

impl Machine {
    /// A meeting at `meet` where the bot is the `role`.
    pub fn new(role: Role, meet: Meet, now_ms: u64) -> Machine {
        Machine {
            phase: Phase::Go,
            since_ms: now_ms,
            role,
            meet,
            casts: 0,
            first_cast_ms: 0,
            last_cast_ms: 0,
            announced: false,
            told: false,
            blind_ms: 0,
            outcome: None,
        }
    }

    /// Where the meeting stands.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The meeting place.
    pub fn meet(&self) -> Meet {
        self.meet
    }

    /// The bot's side.
    pub fn role(&self) -> Role {
        self.role
    }

    /// How it ended, once decided (the bot may still be leaving).
    #[cfg(test)]
    pub fn outcome(&self) -> Option<Closed> {
        self.outcome
    }

    fn go(&mut self, phase: Phase, now_ms: u64) {
        self.phase = phase;
        self.since_ms = now_ms;
        self.casts = 0;
        self.first_cast_ms = 0;
        self.last_cast_ms = 0;
    }

    fn mirage(&self) -> bool {
        self.meet == Meet::Mirage
    }

    /// Decides how the meeting ends; leaves the island first when the bot is on it.
    fn finish(&mut self, how: Closed, f: &Facts) {
        self.outcome.get_or_insert(how);
        if f.at == Where::Mirage {
            self.go(Phase::Leave, f.now_ms);
        } else {
            self.go(Phase::Over, f.now_ms);
        }
    }

    /// One tick.
    pub fn step(&mut self, f: &Facts) -> Step {
        let now = f.now_ms;
        let mut acts: SmallVec<[Act; 3]> = SmallVec::new();
        // (a running trade is decided by its window, not by the ad state's memory of the deal)
        let deciding = matches!(self.phase, Phase::Trade | Phase::Leave | Phase::Over);
        if !f.deal_alive && !deciding {
            self.finish(Closed::Cancelled, f);
        }
        if f.at == Where::Other && !matches!(self.phase, Phase::Over) {
            // (something moved the bot into a dungeon: nothing to meet there)
            self.finish(Closed::Cancelled, f);
        }
        let waited = now.saturating_sub(self.since_ms);
        match self.phase {
            Phase::Go => {
                if self.mirage() {
                    if f.at == Where::Mirage {
                        self.go(Phase::Inside, now);
                    } else if f.portal_ready && f.there {
                        acts.push(Act::Stop);
                        self.go(Phase::Enter, now);
                    } else if f.there && !f.portal_ready {
                        // (at the spawn point and no doodad: wait a little, then say so)
                        if self.blind_ms == 0 {
                            self.blind_ms = now.max(1);
                            acts.push(Act::Stop);
                        } else if now.saturating_sub(self.blind_ms) > PORTAL_VIEW_MS {
                            acts.push(Act::Report(Problem::PortalMissing));
                            self.finish(Closed::Cancelled, f);
                        }
                    } else if f.stuck || waited > GO_MS {
                        self.finish(Closed::TimedOut, f);
                    } else {
                        acts.push(Act::ToPortal);
                    }
                } else if f.there {
                    acts.push(Act::Stop);
                    self.go(Phase::Wait, now);
                } else if f.stuck || waited > GO_MS {
                    self.finish(Closed::TimedOut, f);
                } else {
                    acts.push(Act::ToSpot);
                }
            }
            Phase::Enter => self.enter(f, &mut acts),
            Phase::Inside => {
                if f.at != Where::Mirage {
                    self.finish(Closed::Cancelled, f);
                } else if f.there {
                    acts.push(Act::Stop);
                    self.go(Phase::Wait, now);
                } else if f.stuck || waited > GO_MS {
                    self.finish(Closed::TimedOut, f);
                } else {
                    acts.push(Act::ToSpot);
                }
            }
            Phase::Wait => self.wait(f, &mut acts),
            Phase::Trade => match f.trade_end {
                Some(true) => self.finish(Closed::Done, f),
                Some(false) => self.finish(Closed::Cancelled, f),
                None if waited > TRADE_MS => self.finish(Closed::TimedOut, f),
                None => {}
            },
            Phase::Leave => self.leave(f, &mut acts),
            Phase::Over => {}
        }
        let status = match (self.phase, self.outcome) {
            (Phase::Over, o) => Status::Over(o.unwrap_or(Closed::Cancelled)),
            _ => Status::Running,
        };
        Step { acts, status }
    }

    fn enter(&mut self, f: &Facts, acts: &mut SmallVec<[Act; 3]>) {
        let now = f.now_ms;
        if f.at == Where::Mirage {
            self.go(Phase::Inside, now);
            return;
        }
        if f.loading {
            // the island is on its way; the load may take a moment
            return;
        }
        if self.casts > 0 && now.saturating_sub(self.first_cast_ms) >= LOAD_MS {
            if !f.refused && !self.told {
                self.told = true;
                acts.push(Act::Report(Problem::NeverLoaded { casts: self.casts }));
            }
            self.finish(Closed::Cancelled, f);
            return;
        }
        let gap_over = self.casts == 0 || now.saturating_sub(self.last_cast_ms) >= CAST_GAP_MS;
        if f.portal_ready && gap_over && self.casts < MAX_CASTS {
            acts.push(Act::CastEnter);
            self.casts += 1;
            if self.first_cast_ms == 0 {
                self.first_cast_ms = now.max(1);
            }
            self.last_cast_ms = now;
        } else if !f.portal_ready && self.casts == 0 {
            // (the portal is not in view yet: get closer)
            acts.push(Act::ToPortal);
            if now.saturating_sub(self.since_ms) > GO_MS {
                self.finish(Closed::TimedOut, f);
            }
        } else if self.casts >= MAX_CASTS
            && f.refused
            && now.saturating_sub(self.last_cast_ms) >= CAST_GAP_MS
        {
            // every cast was refused (an accepted one is waited for above)
            self.finish(Closed::Cancelled, f);
        }
    }

    fn wait(&mut self, f: &Facts, acts: &mut SmallVec<[Act; 3]>) {
        let now = f.now_ms;
        if !self.announced {
            self.announced = true;
            acts.push(Act::Announce);
        }
        if now.saturating_sub(self.since_ms) > WAIT_MS {
            self.finish(Closed::TimedOut, f);
            return;
        }
        let Some(d) = f.peer_m else { return };
        match self.role {
            Role::Seller => {
                if f.peer_asked || d <= SEE_M {
                    acts.push(Act::StartTrade);
                    self.go(Phase::Trade, now);
                }
            }
            Role::Buyer => {
                if d <= ASK_M || f.peer_asked {
                    acts.push(Act::Stop);
                    acts.push(Act::StartTrade);
                    self.go(Phase::Trade, now);
                } else {
                    acts.push(Act::ToPeer);
                }
            }
        }
    }

    fn leave(&mut self, f: &Facts, acts: &mut SmallVec<[Act; 3]>) {
        let now = f.now_ms;
        if f.at != Where::Mirage {
            self.go(Phase::Over, now);
            return;
        }
        let waited = now.saturating_sub(self.since_ms);
        if self.casts > 0 && now.saturating_sub(self.first_cast_ms) >= LOAD_MS && !self.told {
            self.told = true;
            if !f.refused {
                acts.push(Act::Report(Problem::NeverLeft { casts: self.casts }));
            }
        }
        if waited > LEAVE_MS {
            // (nothing more to try; the bot stays where it is)
            self.go(Phase::Over, now);
            return;
        }
        if f.loading {
            return;
        }
        let gap_over = self.casts == 0 || now.saturating_sub(self.last_cast_ms) >= CAST_GAP_MS;
        if f.exit_ready && gap_over {
            // (a cast the server took but ignored is tried again; the report above came first)
            acts.push(Act::CastExit);
            self.casts = self.casts.saturating_add(1);
            if self.first_cast_ms == 0 {
                self.first_cast_ms = now.max(1);
            }
            self.last_cast_ms = now;
        } else if !f.exit_ready {
            acts.push(Act::ToExit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(now: u64) -> Facts {
        Facts::idle(now)
    }

    fn town() -> Meet {
        Meet::Town { x: 10.0, y: 20.0 }
    }

    /// Steps the machine over `secs` seconds of unchanged facts and returns every act made.
    fn run(m: &mut Machine, from: u64, secs: u64, f: Facts) -> Vec<Act> {
        let mut acts = Vec::new();
        for s in 0..secs {
            let st = m.step(&Facts {
                now_ms: from + s * 1000,
                ..f
            });
            acts.extend(st.acts);
        }
        acts
    }

    #[test]
    fn a_seller_walks_to_the_town_waits_and_trades() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        assert_eq!(m.step(&facts(1000)).acts.as_slice(), [Act::ToSpot]);
        let there = Facts {
            there: true,
            ..facts(2000)
        };
        let st = m.step(&there);
        assert_eq!(st.acts.as_slice(), [Act::Stop]);
        assert_eq!(m.phase(), Phase::Wait);
        // at the spot it announces itself once and waits
        let st = m.step(&facts(3000));
        assert_eq!(st.acts.as_slice(), [Act::Announce]);
        assert!(m.step(&facts(4000)).acts.is_empty());
        // the buyer comes into view: the trade starts
        let st = m.step(&Facts {
            peer_m: Some(12.0),
            ..facts(5000)
        });
        assert_eq!(st.acts.as_slice(), [Act::StartTrade]);
        assert_eq!(m.phase(), Phase::Trade);
        // the trade was made: over, in town nothing to leave
        let st = m.step(&Facts {
            trade_end: Some(true),
            ..facts(9000)
        });
        assert_eq!(st.status, Status::Over(Closed::Done));
        assert_eq!(m.outcome(), Some(Closed::Done));
    }

    #[test]
    fn a_seller_far_from_the_buyer_keeps_waiting_and_gives_up_in_the_end() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        let far = Facts {
            peer_m: Some(60.0),
            ..facts(0)
        };
        let acts = run(&mut m, 1000, 200, far);
        assert_eq!(acts, [Act::Announce]);
        assert_eq!(m.phase(), Phase::Wait);
        let st = m.step(&facts(WAIT_MS + 5000));
        assert_eq!(st.status, Status::Over(Closed::TimedOut));
    }

    #[test]
    fn a_buyer_walks_up_to_the_seller_before_it_asks() {
        let mut m = Machine::new(Role::Buyer, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        let st = m.step(&Facts {
            peer_m: Some(15.0),
            ..facts(2000)
        });
        // (the first tick at the spot also announces)
        assert!(st.acts.contains(&Act::ToPeer), "{st:?}");
        assert_eq!(m.phase(), Phase::Wait);
        let st = m.step(&Facts {
            peer_m: Some(4.0),
            ..facts(3000)
        });
        assert_eq!(st.acts.as_slice(), [Act::Stop, Act::StartTrade]);
        assert_eq!(m.phase(), Phase::Trade);
    }

    #[test]
    fn a_seller_trades_when_the_buyer_asks_even_from_afar() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        let st = m.step(&Facts {
            peer_m: Some(40.0),
            peer_asked: true,
            ..facts(1000)
        });
        assert!(st.acts.contains(&Act::StartTrade));
    }

    #[test]
    fn a_walk_that_fails_or_takes_too_long_ends_the_meeting() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        let st = m.step(&Facts {
            stuck: true,
            ..facts(1000)
        });
        assert_eq!(st.status, Status::Over(Closed::TimedOut));
        let mut m = Machine::new(Role::Seller, town(), 0);
        assert_eq!(m.step(&facts(GO_MS - 1)).acts.as_slice(), [Act::ToSpot]);
        let st = m.step(&facts(GO_MS + 1));
        assert_eq!(st.status, Status::Over(Closed::TimedOut));
    }

    #[test]
    fn the_island_is_entered_waited_in_and_left() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        // far from the portal: walk
        assert_eq!(m.step(&facts(1000)).acts.as_slice(), [Act::ToPortal]);
        // at the portal: stop, then cast
        let at_portal = Facts {
            there: true,
            portal_ready: true,
            ..facts(2000)
        };
        assert_eq!(m.step(&at_portal).acts.as_slice(), [Act::Stop]);
        assert_eq!(m.phase(), Phase::Enter);
        let st = m.step(&Facts {
            portal_ready: true,
            ..facts(3000)
        });
        assert_eq!(st.acts.as_slice(), [Act::CastEnter]);
        // loading: nothing more is cast
        let loading = Facts {
            loading: true,
            portal_ready: true,
            ..facts(13_000)
        };
        assert!(m.step(&loading).acts.is_empty());
        // on the island: walk to the mailbox
        let inside = Facts {
            at: Where::Mirage,
            ..facts(15_000)
        };
        m.step(&inside);
        assert_eq!(m.phase(), Phase::Inside);
        assert_eq!(m.step(&Facts { ..inside }).acts.as_slice(), [Act::ToSpot]);
        let st = m.step(&Facts {
            there: true,
            ..inside
        });
        assert_eq!(st.acts.as_slice(), [Act::Stop]);
        assert_eq!(m.phase(), Phase::Wait);
        // the trade is made: leave through the exit
        m.step(&Facts {
            peer_m: Some(3.0),
            ..inside
        });
        assert_eq!(m.phase(), Phase::Trade);
        let st = m.step(&Facts {
            trade_end: Some(true),
            ..Facts {
                now_ms: 30_000,
                ..inside
            }
        });
        assert_eq!(st.status, Status::Running);
        assert_eq!(m.phase(), Phase::Leave);
        let at30 = Facts {
            now_ms: 31_000,
            ..inside
        };
        assert_eq!(m.step(&at30).acts.as_slice(), [Act::ToExit]);
        let st = m.step(&Facts {
            exit_ready: true,
            now_ms: 32_000,
            ..inside
        });
        assert_eq!(st.acts.as_slice(), [Act::CastExit]);
        // out: over, the trade's result stands
        let st = m.step(&facts(36_000));
        assert_eq!(st.status, Status::Over(Closed::Done));
    }

    #[test]
    fn a_portal_cast_that_loads_nothing_is_reported_once_and_ends_the_meeting() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        let ready = Facts {
            there: true,
            portal_ready: true,
            ..facts(0)
        };
        m.step(&ready);
        let mut acts = Vec::new();
        let mut last = None;
        for s in 1..60u64 {
            let st = m.step(&Facts {
                now_ms: s * 1000,
                ..ready
            });
            acts.extend(st.acts);
            last = Some(st.status);
        }
        let casts = acts.iter().filter(|a| **a == Act::CastEnter).count();
        assert_eq!(casts, 3, "{acts:?}");
        let reports: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Act::Report(_)))
            .collect();
        assert_eq!(reports, [&Act::Report(Problem::NeverLoaded { casts: 3 })]);
        assert_eq!(last, Some(Status::Over(Closed::Cancelled)));
    }

    #[test]
    fn a_portal_that_is_not_there_is_reported_once_and_ends_the_meeting() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        let blind = Facts {
            there: true,
            portal_ready: false,
            ..facts(0)
        };
        let mut acts = Vec::new();
        let mut last = None;
        for s in 1..40u64 {
            let st = m.step(&Facts {
                now_ms: s * 1000,
                ..blind
            });
            acts.extend(st.acts);
            last = Some(st.status);
        }
        let reports = acts
            .iter()
            .filter(|a| **a == Act::Report(Problem::PortalMissing))
            .count();
        assert_eq!(reports, 1, "{acts:?}");
        assert_eq!(last, Some(Status::Over(Closed::Cancelled)));
        // the doodad shows up in time: no report
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        m.step(&Facts {
            now_ms: 1000,
            ..blind
        });
        let st = m.step(&Facts {
            now_ms: 5000,
            portal_ready: true,
            ..blind
        });
        assert!(!st.acts.contains(&Act::Report(Problem::PortalMissing)));
        assert_eq!(m.phase(), Phase::Enter);
    }

    #[test]
    fn a_refused_portal_cast_is_no_report() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        let ready = Facts {
            there: true,
            portal_ready: true,
            refused: true,
            ..facts(0)
        };
        m.step(&ready);
        let mut acts = Vec::new();
        for s in 1..60u64 {
            acts.extend(
                m.step(&Facts {
                    now_ms: s * 1000,
                    ..ready
                })
                .acts,
            );
        }
        assert!(
            !acts.iter().any(|a| matches!(a, Act::Report(_))),
            "{acts:?}"
        );
        assert_eq!(m.phase(), Phase::Over);
    }

    #[test]
    fn a_deal_that_falls_through_on_the_island_leaves_it_first() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        let inside = Facts {
            at: Where::Mirage,
            ..facts(1000)
        };
        m.step(&inside);
        assert_eq!(m.phase(), Phase::Inside);
        let st = m.step(&Facts {
            deal_alive: false,
            now_ms: 2000,
            ..inside
        });
        assert_eq!(st.status, Status::Running);
        assert_eq!(m.phase(), Phase::Leave);
        assert_eq!(m.outcome(), Some(Closed::Cancelled));
        // out again
        let st = m.step(&Facts {
            deal_alive: false,
            ..facts(5000)
        });
        assert_eq!(st.status, Status::Over(Closed::Cancelled));
    }

    #[test]
    fn a_deal_that_falls_through_in_town_just_ends() {
        let mut m = Machine::new(Role::Buyer, town(), 0);
        let st = m.step(&Facts {
            deal_alive: false,
            ..facts(1000)
        });
        assert_eq!(st.status, Status::Over(Closed::Cancelled));
    }

    #[test]
    fn a_running_trade_is_not_dropped_because_the_ad_state_let_go_of_the_deal() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        m.step(&Facts {
            peer_m: Some(5.0),
            ..facts(1000)
        });
        assert_eq!(m.phase(), Phase::Trade);
        let st = m.step(&Facts {
            deal_alive: false,
            ..facts(2000)
        });
        assert_eq!(st.status, Status::Running, "the window decides");
        let st = m.step(&Facts {
            deal_alive: false,
            trade_end: Some(true),
            ..facts(3000)
        });
        assert_eq!(st.status, Status::Over(Closed::Done));
    }

    #[test]
    fn a_trade_that_hangs_times_out_and_one_that_fails_is_cancelled() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        m.step(&Facts {
            peer_m: Some(5.0),
            ..facts(1000)
        });
        let st = m.step(&facts(1000 + TRADE_MS + 1));
        assert_eq!(st.status, Status::Over(Closed::TimedOut));

        let mut m = Machine::new(Role::Seller, town(), 0);
        m.step(&Facts {
            there: true,
            ..facts(0)
        });
        m.step(&Facts {
            peer_m: Some(5.0),
            ..facts(1000)
        });
        let st = m.step(&Facts {
            trade_end: Some(false),
            ..facts(2000)
        });
        assert_eq!(st.status, Status::Over(Closed::Cancelled));
    }

    #[test]
    fn an_exit_cast_that_changes_nothing_is_reported_once() {
        let mut m = Machine::new(Role::Seller, Meet::Mirage, 0);
        let inside = Facts {
            at: Where::Mirage,
            deal_alive: false,
            exit_ready: true,
            ..facts(0)
        };
        m.step(&Facts {
            deal_alive: true,
            ..inside
        });
        let mut acts = Vec::new();
        for s in 1..100u64 {
            acts.extend(
                m.step(&Facts {
                    now_ms: s * 1000,
                    ..inside
                })
                .acts,
            );
        }
        let reports = acts
            .iter()
            .filter(|a| matches!(a, Act::Report(Problem::NeverLeft { .. })))
            .count();
        assert_eq!(reports, 1, "{acts:?}");
        assert!(acts.iter().filter(|a| **a == Act::CastExit).count() >= 3);
    }

    #[test]
    fn a_bot_pulled_into_a_dungeon_gives_up() {
        let mut m = Machine::new(Role::Seller, town(), 0);
        let st = m.step(&Facts {
            at: Where::Other,
            ..facts(1000)
        });
        assert_eq!(st.status, Status::Over(Closed::Cancelled));
    }
}
