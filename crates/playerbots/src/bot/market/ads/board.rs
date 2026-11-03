//! What all the bots of one process share about the market: how many advertisements were posted
//! this hour, how much the market talked this minute, where the other bots are and where two of
//! them agreed to meet.
//!
//! Bots run on their own tasks and know the world only through their own connection. The trade
//! channel is shared by everybody, so the limits on what the bots say are process-wide, and two
//! bots that agreed on a deal in chat find each other (faction, position, meeting point) here
//! instead of through a hub round-trip. Times are unix milliseconds, the same for all bots.

use std::collections::VecDeque;
use std::sync::OnceLock;

use parking_lot::Mutex;

use super::lines::Meet;

/// What the board knows about another bot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peer {
    /// Mother faction (0 = unknown).
    pub faction: u32,
    /// Map position.
    pub pos: [f32; 2],
    /// Zone group the bot reads the trade chat of (0 = none, e.g. inside a dungeon).
    pub group: u32,
    /// Unix milliseconds of the last report.
    pub at_ms: u64,
}

/// The bots known at most (the oldest report goes first).
const MAX_PEERS: usize = 512;
/// Meeting points kept.
const MAX_MEETS: usize = 64;
/// A meeting point is good for this long.
const MEET_TTL_MS: u64 = 20 * 60_000;
/// A bot that did not report for this long is not counted as an audience.
pub const PEER_FRESH_MS: u64 = 90_000;

#[derive(Debug, Default)]
struct Inner {
    /// Unix ms of the advertisements of the last hour.
    ads: VecDeque<u64>,
    /// Unix ms of the market lines of the last minute.
    lines: VecDeque<u64>,
    /// `(lowercase name, report)`.
    peers: Vec<(Box<str>, Peer)>,
    /// `(seller, buyer, place, unix ms)`, names lowercase.
    meets: Vec<(Box<str>, Box<str>, Meet, u64)>,
}

/// The shared state. See the module docs.
#[derive(Debug, Default)]
pub struct Board {
    inner: Mutex<Inner>,
}

/// The board of this process.
pub fn global() -> &'static Board {
    static BOARD: OnceLock<Board> = OnceLock::new();
    BOARD.get_or_init(Board::default)
}

/// A character name as a key.
pub fn key(name: &str) -> Box<str> {
    name.trim().to_lowercase().into()
}

impl Board {
    /// Takes one of the `max_per_hour` advertisement slots of the last hour; `false` when they
    /// are used up (or `max_per_hour` is 0).
    pub fn take_ad(&self, now_ms: u64, max_per_hour: u32) -> bool {
        let mut g = self.inner.lock();
        while g
            .ads
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= 3_600_000)
        {
            g.ads.pop_front();
        }
        if g.ads.len() >= max_per_hour as usize {
            return false;
        }
        g.ads.push_back(now_ms);
        true
    }

    /// Advertisements taken in the last hour.
    pub fn ads_this_hour(&self, now_ms: u64) -> usize {
        let g = self.inner.lock();
        g.ads
            .iter()
            .filter(|t| now_ms.saturating_sub(**t) < 3_600_000)
            .count()
    }

    /// Takes one of the `per_min` market lines (advertisements and replies) of the last minute.
    pub fn take_line(&self, now_ms: u64, per_min: u32) -> bool {
        let mut g = self.inner.lock();
        while g
            .lines
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= 60_000)
        {
            g.lines.pop_front();
        }
        if g.lines.len() >= per_min as usize {
            return false;
        }
        g.lines.push_back(now_ms);
        true
    }

    /// Records where a bot is.
    pub fn set_peer(&self, name: &str, peer: Peer) {
        let k = key(name);
        let mut g = self.inner.lock();
        if let Some(e) = g.peers.iter_mut().find(|e| e.0 == k) {
            e.1 = peer;
            return;
        }
        if g.peers.len() >= MAX_PEERS {
            let oldest = g
                .peers
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.1.at_ms)
                .map(|(i, _)| i);
            if let Some(i) = oldest {
                g.peers.swap_remove(i);
            }
        }
        g.peers.push((k, peer));
    }

    /// What a bot last reported, if it did within `max_age_ms`.
    pub fn peer(&self, name: &str, now_ms: u64, max_age_ms: u64) -> Option<Peer> {
        let k = key(name);
        self.inner
            .lock()
            .peers
            .iter()
            .find(|e| e.0 == k && now_ms.saturating_sub(e.1.at_ms) <= max_age_ms)
            .map(|e| e.1)
    }

    /// How many other bots read the trade chat of zone group `group`: those that reported within
    /// [`PEER_FRESH_MS`].
    pub fn audience(&self, me: &str, group: u32, now_ms: u64) -> usize {
        if group == 0 {
            return 0;
        }
        let k = key(me);
        self.inner
            .lock()
            .peers
            .iter()
            .filter(|e| {
                e.0 != k && e.1.group == group && now_ms.saturating_sub(e.1.at_ms) <= PEER_FRESH_MS
            })
            .count()
    }

    /// Remembers where `seller` and `buyer` agreed to meet.
    pub fn note_meet(&self, seller: &str, buyer: &str, meet: Meet, now_ms: u64) {
        let (s, b) = (key(seller), key(buyer));
        let mut g = self.inner.lock();
        g.meets.retain(|m| now_ms.saturating_sub(m.3) < MEET_TTL_MS);
        if let Some(m) = g.meets.iter_mut().find(|m| m.0 == s && m.1 == b) {
            m.2 = meet;
            m.3 = now_ms;
            return;
        }
        if g.meets.len() >= MAX_MEETS {
            g.meets.remove(0);
        }
        g.meets.push((s, b, meet, now_ms));
    }

    /// Where `seller` and `buyer` agreed to meet.
    pub fn meet_for(&self, seller: &str, buyer: &str, now_ms: u64) -> Option<Meet> {
        let (s, b) = (key(seller), key(buyer));
        self.inner
            .lock()
            .meets
            .iter()
            .find(|m| m.0 == s && m.1 == b && now_ms.saturating_sub(m.3) < MEET_TTL_MS)
            .map(|m| m.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(group: u32, at_ms: u64) -> Peer {
        Peer {
            faction: 1,
            pos: [1.0, 2.0],
            group,
            at_ms,
        }
    }

    #[test]
    fn advertisements_are_limited_per_hour_for_everybody() {
        let b = Board::default();
        assert!(!b.take_ad(0, 0), "0 means none");
        for i in 0..3 {
            assert!(b.take_ad(i * 1000, 3));
        }
        assert!(!b.take_ad(5_000, 3));
        assert_eq!(b.ads_this_hour(5_000), 3);
        // the first slot is free again an hour after it was used
        assert!(!b.take_ad(3_599_999, 3));
        assert!(b.take_ad(3_600_000, 3));
        assert!(!b.take_ad(3_600_500, 3));
        assert!(b.take_ad(3_601_000, 3));
    }

    #[test]
    fn market_lines_are_limited_per_minute() {
        let b = Board::default();
        for i in 0..4 {
            assert!(b.take_line(i * 100, 4));
        }
        assert!(!b.take_line(500, 4));
        assert!(!b.take_line(59_000, 4));
        assert!(b.take_line(60_000, 4));
        assert!(!b.take_line(0, 0));
    }

    #[test]
    fn peers_are_found_by_name_and_age() {
        let b = Board::default();
        b.set_peer("Bob", peer(3, 1_000));
        assert_eq!(b.peer("bob", 2_000, 5_000), Some(peer(3, 1_000)));
        assert_eq!(b.peer("BOB ", 2_000, 5_000), Some(peer(3, 1_000)));
        assert_eq!(b.peer("bob", 20_000, 5_000), None, "too old");
        assert_eq!(b.peer("ann", 2_000, 5_000), None);
        // a newer report replaces the older
        b.set_peer("bob", peer(4, 3_000));
        assert_eq!(b.peer("bob", 4_000, 5_000).map(|p| p.group), Some(4));
    }

    #[test]
    fn the_audience_is_the_other_fresh_bots_of_the_zone_group() {
        let b = Board::default();
        b.set_peer("Me", peer(3, 60_000));
        b.set_peer("A", peer(3, 60_000));
        b.set_peer("B", peer(3, 60_000));
        b.set_peer("C", peer(9, 60_000));
        b.set_peer("Old", peer(3, 0));
        assert_eq!(b.audience("me", 3, 100_000), 2, "A and B; not me, C or Old");
        assert_eq!(b.audience("me", 3, 300_000), 0, "everybody is stale");
        assert_eq!(b.audience("me", 0, 100_000), 0, "group 0 reads nothing");
    }

    #[test]
    fn peers_are_bounded() {
        let b = Board::default();
        for i in 0..(MAX_PEERS + 20) {
            b.set_peer(&format!("bot{i}"), peer(1, i as u64));
        }
        assert_eq!(b.inner.lock().peers.len(), MAX_PEERS);
        assert!(
            b.peer("bot0", 1_000_000, u64::MAX).is_none(),
            "the oldest went"
        );
        assert!(b
            .peer(&format!("bot{}", MAX_PEERS + 19), 1_000_000, u64::MAX)
            .is_some());
    }

    #[test]
    fn meeting_points_are_shared_between_the_two_and_expire() {
        let b = Board::default();
        let town = Meet::Town { x: 10.0, y: 20.0 };
        b.note_meet("Ann", "Bob", town, 1_000);
        assert_eq!(b.meet_for("ann", "bob", 2_000), Some(town));
        assert_eq!(b.meet_for("bob", "ann", 2_000), None, "seller first");
        b.note_meet("Ann", "Bob", Meet::Mirage, 3_000);
        assert_eq!(b.meet_for("Ann", "Bob", 4_000), Some(Meet::Mirage));
        assert_eq!(b.meet_for("Ann", "Bob", 3_000 + MEET_TTL_MS), None);
        for i in 0..(MAX_MEETS + 5) {
            b.note_meet(&format!("s{i}"), "x", town, 5_000);
        }
        assert!(b.inner.lock().meets.len() <= MAX_MEETS);
    }

    #[test]
    fn the_global_board_is_one_board() {
        assert!(std::ptr::eq(global(), global()));
    }
}
