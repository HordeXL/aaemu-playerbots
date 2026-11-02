//! The bot's mirror of the trade window.
//!
//! [`TradeLog`] lives in [`BotState`](crate::bot::state::BotState) and is updated from every
//! `ActivityEvent` (see [`TradeLog::apply`]), whatever goal runs: the trade goal reads it, the
//! brain looks at [`TradeLog::incoming`] to answer unexpected trade requests, and the bot task
//! ticks faster while [`TradeLog::active`].

use aa_proto::sc_activity::ActivityEvent;
use smallvec::SmallVec;

/// One item put up in the trade window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Put {
    /// Bag slot (only known for our own put-ups).
    pub slot: u8,
    /// Item template (only known for the partner's put-ups).
    pub template: u32,
    /// Units offered.
    pub amount: i32,
}

/// What one side offers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Offer {
    /// Items put up, at most [`MAX_PUTS`] are remembered.
    pub items: SmallVec<[Put; 2]>,
    /// Copper put up.
    pub money: i32,
}

/// Put-ups remembered per side (a trade window holds fewer than this).
pub const MAX_PUTS: usize = 8;

impl Offer {
    /// Whether nothing is offered.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.money <= 0
    }
}

/// The open trade window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Object id of the partner.
    pub peer_obj: u32,
    /// Bot clock (ms) when the window opened.
    pub opened_ms: u64,
    /// Bot clock (ms) of the last change of either offer.
    pub changed_ms: u64,
    /// Our offer as the server confirmed it.
    pub mine: Offer,
    /// The partner's offer.
    pub other: Offer,
    /// Our offer is locked.
    pub my_lock: bool,
    /// The partner's offer is locked.
    pub other_lock: bool,
    /// We confirmed.
    pub my_ok: bool,
    /// The partner confirmed.
    pub other_ok: bool,
}

impl Window {
    /// A fresh window with the partner `peer_obj`.
    pub fn new(peer_obj: u32, now_ms: u64) -> Window {
        Window {
            peer_obj,
            opened_ms: now_ms,
            changed_ms: now_ms,
            mine: Offer::default(),
            other: Offer::default(),
            my_lock: false,
            other_lock: false,
            my_ok: false,
            other_ok: false,
        }
    }
}

/// How the last trade ended; taken by the goal with [`Option::take`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `SCTradeMade`; the window as it was at the end.
    Made(Box<Window>),
    /// `SCTradeCanceled`.
    Canceled {
        /// Reason code (the server sends 0 for most).
        reason: i32,
        /// We caused it.
        by_me: bool,
        /// A window was open (a cancel without one is a stray answer).
        had_window: bool,
    },
}

/// What the bot knows about player trade. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TradeLog {
    /// An unanswered `SCCanStartTrade`: requester object id and bot clock (ms).
    pub incoming: Option<(u32, u64)>,
    /// The reason of the last `SCCannotStartTrade` (the partner declined or is busy).
    pub refused: Option<i32>,
    /// The open window.
    pub window: Option<Box<Window>>,
    /// The end of the last trade, until the goal takes it.
    pub outcome: Option<Outcome>,
    /// Trades completed in this session.
    pub made: u32,
    /// Windows opened in this session; a goal ignores the outcome of a window that opened before
    /// it started (the cancel of a trade its predecessor left behind).
    pub opened: u32,
    /// [`TradeLog::opened`] when [`TradeLog::outcome`] was set.
    pub outcome_seq: u32,
}

impl TradeLog {
    /// Whether a window is open or a request waits for an answer (the bot task ticks fast).
    pub fn active(&self) -> bool {
        self.window.is_some() || self.incoming.is_some()
    }

    /// Updates the log from one event; events other than trade packets are ignored.
    pub fn apply(&mut self, ev: &ActivityEvent, now_ms: u64) {
        use ActivityEvent as E;
        match ev {
            E::CanStartTrade { requester_obj } => {
                self.incoming = Some((*requester_obj, now_ms));
            }
            E::CannotStartTrade { reason, .. } => self.refused = Some(*reason),
            E::TradeStarted { peer_obj } => {
                self.incoming = None;
                self.refused = None;
                self.outcome = None;
                self.opened = self.opened.wrapping_add(1);
                self.window = Some(Box::new(Window::new(*peer_obj, now_ms)));
            }
            E::TradeCanceled {
                reason,
                caused_by_me,
            } => {
                self.outcome = Some(Outcome::Canceled {
                    reason: *reason,
                    by_me: *caused_by_me,
                    had_window: self.window.is_some(),
                });
                self.outcome_seq = self.opened;
                self.window = None;
                self.incoming = None;
            }
            E::TradeMade { .. } => {
                if let Some(w) = self.window.take() {
                    self.outcome = Some(Outcome::Made(w));
                    self.outcome_seq = self.opened;
                    self.made = self.made.saturating_add(1);
                }
                self.incoming = None;
            }
            _ => self.apply_window(ev, now_ms),
        }
    }

    /// The events that only matter inside an open window.
    fn apply_window(&mut self, ev: &ActivityEvent, now_ms: u64) {
        use ActivityEvent as E;
        let Some(w) = self.window.as_mut() else {
            return;
        };
        match ev {
            E::TradeItemPutup {
                slot, amount: n, ..
            } => {
                if w.mine.items.len() < MAX_PUTS {
                    w.mine.items.push(Put {
                        slot: *slot,
                        template: 0,
                        amount: *n,
                    });
                }
                w.changed_ms = now_ms;
            }
            E::OtherTradeItemPutup { item } => {
                if w.other.items.len() < MAX_PUTS {
                    w.other.items.push(Put {
                        slot: 0,
                        template: item.template_id,
                        amount: item.count,
                    });
                }
                w.changed_ms = now_ms;
            }
            E::TradeItemTookdown { slot, .. } => {
                if let Some(i) = w.mine.items.iter().position(|p| p.slot == *slot) {
                    w.mine.items.remove(i);
                }
                w.changed_ms = now_ms;
            }
            E::OtherTradeItemTookdown { item } => {
                if let Some(i) = w
                    .other
                    .items
                    .iter()
                    .position(|p| p.template == item.template_id)
                {
                    w.other.items.remove(i);
                }
                w.changed_ms = now_ms;
            }
            E::TradeMoneyPutup { amount } => {
                w.mine.money = *amount;
                w.changed_ms = now_ms;
            }
            E::OtherTradeMoneyPutup { amount } => {
                w.other.money = *amount;
                w.changed_ms = now_ms;
            }
            E::TradeLockUpdate { mine, other } => {
                w.my_lock = *mine;
                w.other_lock = *other;
                if !*mine || !*other {
                    // The server undoes both confirmations when a lock opens.
                    w.my_ok = false;
                    w.other_ok = false;
                }
            }
            E::TradeOkUpdate { mine, other } => {
                w.my_ok = *mine;
                w.other_ok = *other;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::sc_activity::ItemBrief;

    fn started() -> TradeLog {
        let mut t = TradeLog::default();
        t.apply(&ActivityEvent::TradeStarted { peer_obj: 77 }, 1000);
        t
    }

    #[test]
    fn a_request_is_latched_until_the_trade_starts() {
        let mut t = TradeLog::default();
        assert!(!t.active());
        t.apply(&ActivityEvent::CanStartTrade { requester_obj: 5 }, 100);
        assert_eq!(t.incoming, Some((5, 100)));
        assert!(t.active());
        t.apply(&ActivityEvent::TradeStarted { peer_obj: 5 }, 200);
        assert_eq!(t.incoming, None);
        assert_eq!(t.window.as_ref().unwrap().peer_obj, 5);
        assert!(t.active());
    }

    #[test]
    fn offers_and_locks_follow_the_events() {
        let mut t = started();
        t.apply(
            &ActivityEvent::TradeItemPutup {
                slot_type: 2,
                slot: 3,
                amount: 4,
            },
            1100,
        );
        t.apply(&ActivityEvent::TradeMoneyPutup { amount: 500 }, 1200);
        t.apply(
            &ActivityEvent::OtherTradeItemPutup {
                item: ItemBrief {
                    template_id: 3411,
                    count: 7,
                    ..ItemBrief::default()
                },
            },
            1300,
        );
        let w = t.window.as_ref().unwrap();
        assert_eq!(w.mine.items[0].slot, 3);
        assert_eq!(w.mine.items[0].amount, 4);
        assert_eq!(w.mine.money, 500);
        assert_eq!(w.other.items[0].template, 3411);
        assert_eq!(w.changed_ms, 1300);
        assert!(!w.mine.is_empty() && !w.other.is_empty());

        t.apply(
            &ActivityEvent::TradeLockUpdate {
                mine: true,
                other: true,
            },
            1400,
        );
        t.apply(
            &ActivityEvent::TradeOkUpdate {
                mine: true,
                other: false,
            },
            1500,
        );
        let w = t.window.as_ref().unwrap();
        assert!(w.my_lock && w.other_lock && w.my_ok && !w.other_ok);

        // a lock opening undoes the confirmations
        t.apply(
            &ActivityEvent::TradeLockUpdate {
                mine: false,
                other: false,
            },
            1600,
        );
        let w = t.window.as_ref().unwrap();
        assert!(!w.my_lock && !w.my_ok && !w.other_ok);
    }

    #[test]
    fn takedowns_remove_the_put_up() {
        let mut t = started();
        t.apply(
            &ActivityEvent::TradeItemPutup {
                slot_type: 2,
                slot: 1,
                amount: 2,
            },
            1100,
        );
        t.apply(
            &ActivityEvent::TradeItemTookdown {
                slot_type: 2,
                slot: 1,
            },
            1200,
        );
        assert!(t.window.as_ref().unwrap().mine.items.is_empty());
        t.apply(
            &ActivityEvent::OtherTradeItemPutup {
                item: ItemBrief {
                    template_id: 9,
                    count: 1,
                    ..ItemBrief::default()
                },
            },
            1300,
        );
        t.apply(
            &ActivityEvent::OtherTradeItemTookdown {
                item: ItemBrief {
                    template_id: 9,
                    count: 1,
                    ..ItemBrief::default()
                },
            },
            1400,
        );
        assert!(t.window.as_ref().unwrap().other.is_empty());
    }

    #[test]
    fn made_and_canceled_leave_an_outcome_and_close_the_window() {
        let mut t = started();
        t.apply(
            &ActivityEvent::TradeMade {
                action: 0,
                tasks: 2,
                items: Default::default(),
            },
            2000,
        );
        assert!(t.window.is_none() && !t.active());
        assert_eq!(t.made, 1);
        assert!(matches!(t.outcome, Some(Outcome::Made(_))));

        let mut t = started();
        t.apply(
            &ActivityEvent::TradeCanceled {
                reason: 0,
                caused_by_me: false,
            },
            2000,
        );
        assert!(t.window.is_none());
        assert_eq!(
            t.outcome,
            Some(Outcome::Canceled {
                reason: 0,
                by_me: false,
                had_window: true
            })
        );

        // a cancel without a window is marked as such
        let mut t = TradeLog::default();
        t.apply(
            &ActivityEvent::TradeCanceled {
                reason: 0,
                caused_by_me: true,
            },
            10,
        );
        assert!(matches!(
            t.outcome,
            Some(Outcome::Canceled {
                had_window: false,
                ..
            })
        ));
        // TradeMade without a window changes nothing
        let mut t = TradeLog::default();
        t.apply(
            &ActivityEvent::TradeMade {
                action: 0,
                tasks: 0,
                items: Default::default(),
            },
            10,
        );
        assert_eq!((t.made, t.outcome.is_none()), (0, true));
    }

    #[test]
    fn a_refusal_is_remembered_and_other_events_are_ignored() {
        let mut t = TradeLog::default();
        t.apply(&ActivityEvent::CannotStartTrade { obj: 3, reason: 2 }, 100);
        assert_eq!(t.refused, Some(2));
        t.apply(&ActivityEvent::TradeMoneyPutup { amount: 5 }, 200);
        assert!(t.window.is_none());
        t.apply(&ActivityEvent::TradeStarted { peer_obj: 3 }, 300);
        assert_eq!(t.refused, None);
    }
}
