//! What the auction house goal remembers (goal `auction`, [`crate::bot::goals::auction`]).
//!
//! [`AuctionMemory`] lives in [`Inner`](super::Inner) (`state.market.inner_mut().auction`). It is
//! fed with every server event ([`MarketState::on_server_event`](super::MarketState)) and keeps
//!
//! * the **answers** the goal waits for (the lot that was posted, the search page, the bid, the
//!   cancel): [`Inbox`], filled by the events and emptied by the goal;
//! * the **prices** it learned, per item, for a while ([`PriceNote`], at most [`MAX_PRICES`]);
//! * the bot's **own lots** it knows of ([`OwnLot`], at most [`MAX_OWN`]);
//! * the **mail** the auction house sends ([`MailTracker`]) and what the detectors compare:
//!   lots that vanished against result mails that came;
//! * the pacing (the end of the last visit) and a few counters.
//!
//! Everything is bounded; the memory is forgotten with the rest of the bot's state when the
//! connection is lost.

pub mod mail;
pub mod price;

use aa_proto::auction::{AuctionEvent, Lot};
use aa_proto::sc::ServerEvent;
use aa_proto::sc_activity::ActivityEvent;

pub use self::mail::{Counts, Fault, Kind, MailTracker, Settled};
use self::price::Quote;

/// The most prices remembered.
pub const MAX_PRICES: usize = 24;
/// The most own lots remembered.
pub const MAX_OWN: usize = 16;
/// The most templates kept out of the listings at once.
pub const MAX_SKIPS: usize = 8;
/// The most buyouts waited for at once.
pub const MAX_WINS: usize = 4;
/// How long a price is trusted, in bot clock milliseconds.
pub const PRICE_TTL_MS: u64 = 45 * 60_000;
/// How long a win mail may take to arrive after a buyout.
pub const WIN_MAIL_WAIT_MS: u64 = 90_000;
/// How long before a buyout is handled a win mail may have been seen and still belong to it (the
/// server sends the mail with the answer, and the goal reads the answer on its next tick).
pub const EARLY_WIN_MS: u64 = 10_000;
/// How long the result mail of a lot that vanished may take (the server settles on its 5 second
/// timer and retries; the bot lists the mailbox in the meantime).
pub const SALE_MAIL_WAIT_MS: u64 = 75_000;

/// What the bot learned about the market price of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceNote {
    /// Item template.
    pub template: u32,
    /// The quote; `None` when no lot of the item was up.
    pub quote: Option<Quote>,
    /// Bot clock (ms) of the search.
    pub at_ms: u64,
}

/// One of the bot's own lots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnLot {
    /// Lot id.
    pub id: u64,
    /// Item template.
    pub template: u32,
    /// Stack size.
    pub count: i32,
    /// Buyout price in copper.
    pub buyout: i32,
    /// Bot clock (ms) the lot was posted (or first seen).
    pub posted_ms: u64,
    /// The bot posted it in this session (so the house must list it).
    pub posted_here: bool,
    /// Seen in the last survey.
    pub seen: bool,
}

/// An answer the goal waits for, with the bot clock at which it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer<T> {
    /// The answer.
    pub what: T,
    /// Bot clock (ms) of its arrival.
    pub at_ms: u64,
}

/// One result page of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The page number the server reports.
    pub page: i32,
    /// Its lots.
    pub lots: Vec<Lot>,
    /// The server's error field.
    pub error: i16,
}

/// A buyout or bid the server accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidDone {
    /// Lot id.
    pub lot_id: u64,
    /// Copper bid.
    pub money: i32,
    /// The lot was bought out.
    pub buyout: bool,
    /// Item template.
    pub template: u32,
}

/// The answers of the auction house that nobody has read yet.
#[derive(Debug, Default)]
pub struct Inbox {
    /// `SCAuctionPosted`.
    pub posted: Option<Answer<Box<Lot>>>,
    /// `SCAuctionSearched`.
    pub page: Option<Answer<Box<Page>>>,
    /// `SCAuctionBid`.
    pub bid: Option<Answer<BidDone>>,
    /// `SCAuctionCanceled`: the lot id.
    pub canceled: Option<Answer<u64>>,
}

/// A buyout whose item has not come by mail yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinWait {
    /// Lot id.
    pub lot_id: u64,
    /// Item template.
    pub template: u32,
    /// Bot clock (ms) after which the mail is late.
    pub deadline_ms: u64,
    /// The `win_mails` counter of the tracker when the buyout was made.
    pub wins_before: u32,
}

/// Counters of the bot's auction house activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Visits to an auctioneer.
    pub visits: u32,
    /// Lots posted.
    pub posted: u32,
    /// Listing fees paid (copper).
    pub fees: i64,
    /// Own lots cancelled.
    pub cancelled: u32,
    /// Lots bought out.
    pub bought: u32,
    /// Copper spent on them.
    pub spent: i64,
    /// Requests that got no answer.
    pub timeouts: u32,
    /// Requests the server refused with an error message.
    pub refused: u32,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct AuctionMemory {
    /// Answers nobody has read yet.
    pub inbox: Inbox,
    prices: Vec<PriceNote>,
    own: Vec<OwnLot>,
    /// Templates kept out of the listings until the bot clock reaches the number.
    skips: Vec<(u32, u64)>,
    /// The auction mail.
    pub mail: MailTracker,
    /// Counters.
    pub stats: Stats,
    /// Own lots that vanished from the house since the connection started.
    pub lots_gone: u32,
    /// Bot clock (ms) at which `lots_gone` first exceeded the seller mails seen, 0 = it does not.
    deficit_since_ms: u64,
    /// Buyouts whose win mail has not come.
    wins: Vec<WinWait>,
    /// Bot clock (ms) of the win mails seen while no buyout was waiting (see [`EARLY_WIN_MS`]).
    early_wins: Vec<u64>,
    /// Bot clock (ms) at which the last visit ended, 0 = no visit yet.
    pub last_visit_end_ms: u64,
    /// Bot clock (ms) of the last lot the bot posted, 0 = none.
    pub last_post_ms: u64,
}

impl AuctionMemory {
    // ---- events -------------------------------------------------------------------------

    /// Whether `ev` has to reach the memory even when the bot has none yet (an auction answer or
    /// an auction mail).
    pub fn concerns(ev: &ServerEvent) -> bool {
        match ev {
            ServerEvent::Activity(ActivityEvent::Auction(_)) => true,
            ServerEvent::Activity(a) => MailTracker::concerns(a),
            ServerEvent::World(_) => false,
        }
    }

    /// Feeds the memory one server event at bot clock `now_ms`.
    pub fn observe(&mut self, ev: &ServerEvent, now_ms: u64) {
        let ServerEvent::Activity(a) = ev else { return };
        match a {
            ActivityEvent::Auction(ev) => self.on_auction(ev, now_ms),
            other => {
                let before = self.mail.win_mails;
                self.mail.observe(other, now_ms);
                for _ in before..self.mail.win_mails {
                    // a win mail arrived: the oldest wait is over; with none waiting the mail
                    // came ahead of the answer of the buyout it belongs to
                    if !self.wins.is_empty() {
                        self.wins.remove(0);
                    } else {
                        if self.early_wins.len() >= MAX_WINS {
                            self.early_wins.remove(0);
                        }
                        self.early_wins.push(now_ms);
                    }
                }
            }
        }
    }

    fn on_auction(&mut self, ev: &AuctionEvent, now_ms: u64) {
        match ev {
            AuctionEvent::Posted(lot) => {
                self.inbox.posted = Some(Answer {
                    what: lot.clone(),
                    at_ms: now_ms,
                });
            }
            AuctionEvent::Searched {
                page, lots, error, ..
            } => {
                self.inbox.page = Some(Answer {
                    what: Box::new(Page {
                        page: *page,
                        lots: lots.clone(),
                        error: *error,
                    }),
                    at_ms: now_ms,
                });
            }
            AuctionEvent::Bid {
                lot_id,
                money,
                buyout,
                template,
                ..
            } => {
                self.inbox.bid = Some(Answer {
                    what: BidDone {
                        lot_id: *lot_id,
                        money: *money,
                        buyout: *buyout,
                        template: *template,
                    },
                    at_ms: now_ms,
                });
            }
            AuctionEvent::Canceled(lot) => {
                self.inbox.canceled = Some(Answer {
                    what: lot.id,
                    at_ms: now_ms,
                });
            }
            AuctionEvent::LowestPrice { .. } => {}
        }
    }

    /// Forgets the answers (the goal sends a request and waits for a fresh answer).
    pub fn clear_inbox(&mut self) {
        self.inbox = Inbox::default();
    }

    // ---- prices ---------------------------------------------------------------------------

    /// Remembers the market price of `template` found at `now_ms`.
    pub fn note_price(&mut self, template: u32, quote: Option<Quote>, now_ms: u64) {
        let note = PriceNote {
            template,
            quote,
            at_ms: now_ms,
        };
        if let Some(p) = self.prices.iter_mut().find(|p| p.template == template) {
            *p = note;
            return;
        }
        if self.prices.len() >= MAX_PRICES {
            // the oldest goes
            if let Some(i) = self
                .prices
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.at_ms)
                .map(|(i, _)| i)
            {
                self.prices.remove(i);
            }
        }
        self.prices.push(note);
    }

    /// The price of `template` if it was looked up less than [`PRICE_TTL_MS`] ago.
    pub fn price(&self, template: u32, now_ms: u64) -> Option<&PriceNote> {
        self.prices
            .iter()
            .find(|p| p.template == template && now_ms.saturating_sub(p.at_ms) < PRICE_TTL_MS)
    }

    /// The prices remembered.
    pub fn prices(&self) -> &[PriceNote] {
        &self.prices
    }

    // ---- skips ----------------------------------------------------------------------------

    /// Keeps `template` out of the listings until the bot clock reaches `until_ms`.
    pub fn skip(&mut self, template: u32, until_ms: u64, now_ms: u64) {
        self.skips.retain(|s| s.1 > now_ms);
        if let Some(s) = self.skips.iter_mut().find(|s| s.0 == template) {
            s.1 = until_ms;
            return;
        }
        if self.skips.len() >= MAX_SKIPS {
            self.skips.remove(0);
        }
        self.skips.push((template, until_ms));
    }

    /// Whether `template` is kept out of the listings at `now_ms`.
    pub fn skipped(&self, template: u32, now_ms: u64) -> bool {
        self.skips.iter().any(|s| s.0 == template && s.1 > now_ms)
    }

    // ---- own lots -------------------------------------------------------------------------

    /// The own lots known.
    pub fn own(&self) -> &[OwnLot] {
        &self.own
    }

    /// Whether an own lot of `template` is up.
    pub fn has_lot_of(&self, template: u32) -> bool {
        self.own.iter().any(|l| l.template == template)
    }

    /// A lot of the bot was posted (or seen): remembers it. The oldest goes when full.
    pub fn note_own(&mut self, lot: OwnLot) {
        if let Some(l) = self.own.iter_mut().find(|l| l.id == lot.id) {
            l.seen |= lot.seen;
            return;
        }
        if self.own.len() >= MAX_OWN {
            self.own.remove(0);
        }
        self.own.push(lot);
    }

    /// Forgets own lot `id` without a trace (the house never listed it).
    pub fn forget_own(&mut self, id: u64) {
        self.own.retain(|l| l.id != id);
    }

    /// Own lot `id` was cancelled: it is forgotten, and the mail that returns its item is one the
    /// detectors expect like the mail of a lot that sold.
    pub fn own_cancelled(&mut self, id: u64) {
        if self.own.iter().any(|l| l.id == id) {
            self.own.retain(|l| l.id != id);
            self.lots_gone += 1;
            self.stats.cancelled += 1;
        }
    }

    /// Starts a survey of the own lots: nothing is seen yet.
    pub fn begin_survey(&mut self) {
        for l in &mut self.own {
            l.seen = false;
        }
    }

    /// Notes the own lot `lot` as seen in the survey; a lot the bot did not know is added.
    pub fn survey_saw(&mut self, lot: &Lot, now_ms: u64) {
        match self.own.iter_mut().find(|l| l.id == lot.id) {
            Some(l) => l.seen = true,
            None => self.note_own(OwnLot {
                id: lot.id,
                template: lot.template,
                count: lot.count,
                buyout: lot.buyout,
                posted_ms: now_ms,
                posted_here: false,
                seen: true,
            }),
        }
    }

    /// Ends a survey: the own lots that were not seen are gone (sold, expired or cancelled). They
    /// are forgotten and returned: `(lot, posted in this session)`.
    pub fn end_survey(&mut self) -> Vec<OwnLot> {
        let gone: Vec<OwnLot> = self.own.iter().filter(|l| !l.seen).copied().collect();
        self.own.retain(|l| l.seen);
        self.lots_gone += gone.len() as u32;
        if !gone.is_empty() {
            // their mail may have come before the bot knew they were gone, or it comes now
            self.mail.want_list();
        }
        gone
    }

    // ---- results by mail ------------------------------------------------------------------

    /// Seller result mails that are missing for the lots that vanished: lots gone minus the
    /// mails seen (never negative).
    pub fn missing_seller_mails(&self) -> u32 {
        self.lots_gone.saturating_sub(self.mail.seller_mails)
    }

    /// The number of seller result mails that are overdue: the lots gone outnumber the mails seen
    /// for [`SALE_MAIL_WAIT_MS`], with the mailbox listed since and nothing being collected. Once:
    /// the missing mails are written off.
    pub fn sale_mail_overdue(&mut self, now_ms: u64) -> Option<u32> {
        let missing = self.missing_seller_mails();
        if missing == 0 {
            self.deficit_since_ms = 0;
            return None;
        }
        if self.deficit_since_ms == 0 {
            self.deficit_since_ms = now_ms.max(1);
            return None;
        }
        if now_ms < self.deficit_since_ms + SALE_MAIL_WAIT_MS || self.mail.collecting() > 0 {
            return None;
        }
        if !self.mail.listed_since(self.deficit_since_ms) {
            self.mail.want_list();
            return None;
        }
        self.lots_gone = self.mail.seller_mails;
        self.deficit_since_ms = 0;
        Some(missing)
    }

    /// A buyout of `template` was accepted: its win mail is due within [`WIN_MAIL_WAIT_MS`].
    pub fn expect_win(&mut self, lot_id: u64, template: u32, now_ms: u64) {
        self.early_wins
            .retain(|&t| now_ms.saturating_sub(t) <= EARLY_WIN_MS);
        if !self.early_wins.is_empty() {
            // its mail was seen before the answer was handled: nothing left to wait for
            self.early_wins.remove(0);
            return;
        }
        if self.wins.len() >= MAX_WINS {
            self.wins.remove(0);
        }
        self.wins.push(WinWait {
            lot_id,
            template,
            deadline_ms: now_ms + WIN_MAIL_WAIT_MS,
            wins_before: self.mail.win_mails,
        });
    }

    /// The oldest buyout whose win mail is late at `now_ms` (removed from the waits).
    pub fn take_late_win(&mut self, now_ms: u64) -> Option<WinWait> {
        let i = self.wins.iter().position(|w| now_ms >= w.deadline_ms)?;
        Some(self.wins.remove(i))
    }

    /// Buyouts whose item has not come yet.
    pub fn waiting_wins(&self) -> usize {
        self.wins.len()
    }

    /// Whether the bot has reason to look for auction mail now and then: lots up, or wins due.
    pub fn awaiting_mail(&self) -> bool {
        !self.own.is_empty() || !self.wins.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aa_proto::sc_activity::{MailBodyInfo, MailHeader};

    fn lot(id: u64, template: u32) -> Lot {
        Lot {
            id,
            template,
            count: 5,
            buyout: 100,
            ..Lot::default()
        }
    }

    fn auction(ev: AuctionEvent) -> ServerEvent {
        ServerEvent::Activity(ActivityEvent::Auction(Box::new(ev)))
    }

    #[test]
    fn the_answers_land_in_the_inbox_and_are_cleared() {
        let mut m = AuctionMemory::default();
        m.observe(&auction(AuctionEvent::Posted(Box::new(lot(1, 3411)))), 100);
        m.observe(
            &auction(AuctionEvent::Searched {
                page: 2,
                lots: vec![lot(2, 5), lot(3, 6)],
                error: 0,
                server_time: 0,
            }),
            200,
        );
        m.observe(
            &auction(AuctionEvent::Bid {
                lot_id: 9,
                bidder_id: 1,
                bidder_name: "x".into(),
                money: 77,
                buyout: true,
                template: 3411,
            }),
            300,
        );
        m.observe(&auction(AuctionEvent::Canceled(Box::new(lot(4, 5)))), 400);
        let i = &m.inbox;
        assert_eq!(
            i.posted.as_ref().map(|a| (a.what.id, a.at_ms)),
            Some((1, 100))
        );
        let page = i.page.as_ref().unwrap();
        assert_eq!(
            (page.what.page, page.what.lots.len(), page.at_ms),
            (2, 2, 200)
        );
        assert_eq!(i.bid.as_ref().unwrap().what.money, 77);
        assert_eq!(i.canceled.as_ref().map(|a| a.what), Some(4));
        // a newer answer replaces the older
        m.observe(&auction(AuctionEvent::Posted(Box::new(lot(8, 5)))), 500);
        assert_eq!(m.inbox.posted.as_ref().unwrap().what.id, 8);
        m.clear_inbox();
        assert!(m.inbox.posted.is_none() && m.inbox.page.is_none());
        assert!(m.inbox.bid.is_none() && m.inbox.canceled.is_none());
        // lowest price answers are not kept
        m.observe(
            &auction(AuctionEvent::LowestPrice {
                template: 1,
                grade: 0,
                money: 5,
            }),
            600,
        );
    }

    #[test]
    fn prices_are_remembered_for_a_while_and_bounded() {
        let mut m = AuctionMemory::default();
        let q = Quote {
            unit: 30,
            lowest: 25,
            lots: 3,
        };
        m.note_price(7, Some(q), 1_000);
        assert_eq!(m.price(7, 2_000).unwrap().quote, Some(q));
        assert!(m.price(7, 1_000 + PRICE_TTL_MS).is_none(), "too old");
        assert!(m.price(8, 2_000).is_none());
        // an update replaces
        m.note_price(7, None, 5_000);
        assert_eq!(m.prices().len(), 1);
        assert_eq!(m.price(7, 5_001).unwrap().quote, None);
        // bounded: the oldest goes
        for t in 100..100 + 3 * MAX_PRICES as u32 {
            m.note_price(t, Some(q), 10_000 + u64::from(t) * 10);
        }
        assert_eq!(m.prices().len(), MAX_PRICES);
        assert!(m
            .prices()
            .iter()
            .all(|p| p.template >= 100 + 2 * MAX_PRICES as u32));
    }

    #[test]
    fn skips_expire_and_are_bounded() {
        let mut m = AuctionMemory::default();
        m.skip(5, 10_000, 0);
        assert!(m.skipped(5, 9_999) && !m.skipped(5, 10_000) && !m.skipped(6, 0));
        m.skip(5, 20_000, 0);
        assert!(m.skipped(5, 15_000));
        for t in 10..40 {
            m.skip(t, 1_000_000, 0);
        }
        assert!(m.skips.len() <= MAX_SKIPS);
        // an expired one is dropped on the next skip
        let mut m = AuctionMemory::default();
        m.skip(1, 100, 0);
        m.skip(2, 1_000, 500);
        assert_eq!(m.skips.len(), 1);
    }

    #[test]
    fn the_survey_finds_the_lots_that_vanished() {
        let mut m = AuctionMemory::default();
        for (id, t) in [(1, 10), (2, 11), (3, 12)] {
            m.note_own(OwnLot {
                id,
                template: t,
                count: 1,
                buyout: 50,
                posted_ms: 0,
                posted_here: true,
                seen: true,
            });
        }
        assert!(m.has_lot_of(11) && !m.has_lot_of(13));
        m.begin_survey();
        m.survey_saw(&lot(1, 10), 5);
        m.survey_saw(&lot(3, 12), 5);
        // a lot the bot did not know
        m.survey_saw(&lot(9, 14), 5);
        let gone = m.end_survey();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].id, 2);
        assert_eq!(m.own().len(), 3);
        assert_eq!(m.lots_gone, 1);
        assert_eq!(m.missing_seller_mails(), 1);
        // the mail for it comes
        let header = |id| ActivityEvent::MailReceived {
            mail: Box::new(MailHeader {
                mail_id: id,
                mail_type: 14,
                status: 0,
                title: "".into(),
                sender: "".into(),
                attachments: 0,
                receiver: "".into(),
                open_date: 0,
                returned: false,
                extra: 0,
            }),
            unread: [0, 1, 0, 0],
            is_cancel: false,
            body: None,
        };
        m.observe(&ServerEvent::Activity(header(4)), 10);
        assert_eq!(m.missing_seller_mails(), 0);
        // a mail from before the survey does not make the count negative
        m.observe(&ServerEvent::Activity(header(5)), 10);
        assert_eq!(m.missing_seller_mails(), 0);
    }

    #[test]
    fn a_missing_sale_mail_is_reported_once_after_the_wait_and_a_listing() {
        let mut m = AuctionMemory::default();
        m.note_own(OwnLot {
            id: 1,
            template: 7,
            count: 1,
            buyout: 10,
            posted_ms: 0,
            posted_here: true,
            seen: true,
        });
        m.begin_survey();
        assert_eq!(m.end_survey().len(), 1);
        assert_eq!(m.missing_seller_mails(), 1);
        // the first look starts the clock; the mailbox is to be listed
        assert_eq!(m.sale_mail_overdue(1_000), None);
        let mut out = Vec::new();
        m.mail.tick(1_000, false, &mut out);
        assert_eq!(out.len(), 1, "the mailbox was listed");
        assert_eq!(m.sale_mail_overdue(1_000 + SALE_MAIL_WAIT_MS - 1), None);
        assert_eq!(m.sale_mail_overdue(1_000 + SALE_MAIL_WAIT_MS), Some(1));
        // written off: no second report
        assert_eq!(m.sale_mail_overdue(500_000), None);
        assert_eq!(m.missing_seller_mails(), 0);
        // without a listing after the deficit began the report waits for one
        let mut m = AuctionMemory {
            lots_gone: 1,
            ..AuctionMemory::default()
        };
        assert_eq!(m.sale_mail_overdue(10), None);
        assert_eq!(m.sale_mail_overdue(10 + SALE_MAIL_WAIT_MS), None);
        let mut out = Vec::new();
        m.mail.tick(10 + SALE_MAIL_WAIT_MS, false, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(m.sale_mail_overdue(20 + SALE_MAIL_WAIT_MS), Some(1));
        // the mail came in time: nothing to report
        let mut m = AuctionMemory {
            lots_gone: 1,
            ..AuctionMemory::default()
        };
        assert_eq!(m.sale_mail_overdue(10), None);
        m.mail.seller_mails = 1;
        assert_eq!(m.sale_mail_overdue(10 + SALE_MAIL_WAIT_MS * 2), None);
    }

    #[test]
    fn own_lots_are_bounded() {
        let mut m = AuctionMemory::default();
        for id in 0..(MAX_OWN as u64 * 3) {
            m.note_own(OwnLot {
                id,
                template: 1,
                count: 1,
                buyout: 1,
                posted_ms: 0,
                posted_here: false,
                seen: true,
            });
        }
        assert_eq!(m.own().len(), MAX_OWN);
        assert_eq!(m.own()[0].id, MAX_OWN as u64 * 2);
        m.forget_own(m.own()[0].id);
        assert_eq!(m.own().len(), MAX_OWN - 1);
        assert_eq!(m.lots_gone, 0);
        // a cancel counts as a lot gone: its item comes back by mail
        let id = m.own()[0].id;
        m.own_cancelled(id);
        m.own_cancelled(id);
        assert_eq!((m.lots_gone, m.stats.cancelled), (1, 1));
    }

    fn win_mail(mail_id: i64) -> ServerEvent {
        ServerEvent::Activity(ActivityEvent::MailReceived {
            mail: Box::new(MailHeader {
                mail_id,
                mail_type: 16,
                status: 0,
                title: "".into(),
                sender: "".into(),
                attachments: 1,
                receiver: "".into(),
                open_date: 0,
                returned: false,
                extra: 0,
            }),
            unread: [0, 1, 0, 0],
            is_cancel: false,
            body: None,
        })
    }

    #[test]
    fn a_win_mail_seen_before_the_answer_is_handled_ends_no_wait() {
        let mut m = AuctionMemory::default();
        // the mail came with the answer of the buyout, before the goal read the answer
        m.observe(&win_mail(1), 1_000);
        m.expect_win(7, 3411, 1_800);
        assert_eq!(m.waiting_wins(), 0, "nothing is late afterwards");
        assert!(m.take_late_win(1_000_000).is_none());
        // a mail seen long before (a mailbox listing at login) is not taken for the next buyout's
        m.observe(&win_mail(2), 2_000);
        m.expect_win(8, 3411, 2_000 + EARLY_WIN_MS + 1);
        assert_eq!(m.waiting_wins(), 1);
        // the usual order still works
        m.observe(&win_mail(3), 30_000);
        assert_eq!(m.waiting_wins(), 0);
    }

    #[test]
    fn a_buyout_expects_its_win_mail() {
        let mut m = AuctionMemory::default();
        assert!(!m.awaiting_mail());
        m.expect_win(7, 3411, 1_000);
        assert!(m.awaiting_mail());
        assert_eq!(m.waiting_wins(), 1);
        assert!(m.take_late_win(1_000 + WIN_MAIL_WAIT_MS - 1).is_none());
        let late = m.take_late_win(1_000 + WIN_MAIL_WAIT_MS).unwrap();
        assert_eq!((late.lot_id, late.template), (7, 3411));
        assert_eq!(m.waiting_wins(), 0);
        // a win mail ends the wait
        m.expect_win(8, 3411, 0);
        let win = win_mail(1);
        assert!(AuctionMemory::concerns(&win));
        m.observe(&win, 10);
        assert_eq!(m.waiting_wins(), 0);
        // bounded
        for i in 0..10 {
            m.expect_win(i, 1, 0);
        }
        assert_eq!(m.waiting_wins(), MAX_WINS);
    }

    #[test]
    fn only_auction_events_concern_a_bot_without_memory() {
        use aa_proto::sc_world::WorldEvent;
        assert!(AuctionMemory::concerns(&auction(
            AuctionEvent::LowestPrice {
                template: 1,
                grade: 0,
                money: 1
            }
        )));
        assert!(!AuctionMemory::concerns(&ServerEvent::World(
            WorldEvent::UnitsRemoved {
                objs: Default::default()
            }
        )));
        let body = ServerEvent::Activity(ActivityEvent::MailBody {
            is_prepare: false,
            is_sent: false,
            body: Box::new(MailBodyInfo {
                mail_id: 1,
                mail_type: 14,
                receiver: "".into(),
                title: "".into(),
                text: "".into(),
                money: 0,
                billing: 0,
                money2: 0,
                send_date: 0,
                recv_date: 0,
                open_date: 0,
                items: Vec::new(),
            }),
            open_date_modified: false,
            unread: [0; 4],
        });
        assert!(!AuctionMemory::concerns(&body));
    }
}
