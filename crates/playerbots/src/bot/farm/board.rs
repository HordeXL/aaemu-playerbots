//! The farm board: what all bots are growing, on the hub.
//!
//! Every bot that farms publishes the produce of its plots here ([`FarmBoard::publish`]) after it
//! plants or harvests, and the planner reads the sum per item ([`FarmBoard::units`]) to hold
//! back when enough of an item is on its way: thirty bots told to "boost Potato" should not all
//! plant potatoes. The web UI reads the same sums for the "being grown" column of the economy
//! panel ([`FarmBoard::totals`]).
//!
//! The board is bounded (at most [`MAX_ITEMS`] items per bot) and sits behind one short lock; a
//! bot that goes offline takes its entry down ([`FarmBoard::release_bot`]).

use parking_lot::Mutex;
use rustc_hash::FxHashMap;

/// The most items one bot publishes.
pub const MAX_ITEMS: usize = 16;

/// What one bot grows: `(item, expected count)`.
type Growing = Box<[(u32, f32)]>;

/// What the bots grow. See the module docs.
#[derive(Debug, Default)]
pub struct FarmBoard {
    by_bot: Mutex<FxHashMap<u32, Growing>>,
}

impl FarmBoard {
    /// Replaces what bot `bot` is growing with `items` as `(item, expected count)`; nothing at
    /// all removes the bot's entry.
    pub fn publish(&self, bot: u32, items: &[(u32, f32)]) {
        let mut map = self.by_bot.lock();
        if items.is_empty() {
            map.remove(&bot);
            return;
        }
        let items: Growing = items.iter().copied().take(MAX_ITEMS).collect();
        map.insert(bot, items);
    }

    /// Drops what bot `bot` was growing (it went offline: its plots are still there, but nobody
    /// tends them for now).
    pub fn release_bot(&self, bot: u32) {
        self.by_bot.lock().remove(&bot);
    }

    /// How many units of `item` all bots together are growing.
    pub fn units(&self, item: u32) -> f32 {
        self.by_bot
            .lock()
            .values()
            .flat_map(|v| v.iter())
            .filter(|e| e.0 == item)
            .map(|e| e.1)
            .sum()
    }

    /// How many units of `item` all bots except `bot` are growing.
    pub fn units_of_others(&self, item: u32, bot: u32) -> f32 {
        self.by_bot
            .lock()
            .iter()
            .filter(|(b, _)| **b != bot)
            .flat_map(|(_, v)| v.iter())
            .filter(|e| e.0 == item)
            .map(|e| e.1)
            .sum()
    }

    /// Everything being grown as `(item, units)`, most first.
    pub fn totals(&self) -> Vec<(u32, f32)> {
        let mut sums: FxHashMap<u32, f32> = FxHashMap::default();
        for v in self.by_bot.lock().values() {
            for &(item, n) in v.iter() {
                *sums.entry(item).or_insert(0.0) += n;
            }
        }
        let mut out: Vec<(u32, f32)> = sums.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }

    /// How many bots have something growing.
    pub fn farmers(&self) -> usize {
        self.by_bot.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_sums_what_the_bots_grow_and_forgets_the_ones_that_leave() {
        let b = FarmBoard::default();
        assert_eq!(b.units(7), 0.0);
        b.publish(1, &[(7, 3.0), (8, 1.5)]);
        b.publish(2, &[(7, 4.0)]);
        assert_eq!(b.units(7), 7.0);
        assert_eq!(b.units_of_others(7, 1), 4.0);
        assert_eq!(b.farmers(), 2);
        assert_eq!(b.totals(), vec![(7, 7.0), (8, 1.5)]);
        // a new publication replaces the old one
        b.publish(1, &[(8, 2.0)]);
        assert_eq!(b.units(7), 4.0);
        // nothing growing removes the entry
        b.publish(2, &[]);
        assert_eq!(b.farmers(), 1);
        b.release_bot(1);
        assert_eq!(b.farmers(), 0);
        assert!(b.totals().is_empty());
    }

    #[test]
    fn a_bot_publishes_a_bounded_list() {
        let b = FarmBoard::default();
        let many: Vec<(u32, f32)> = (0..100).map(|i| (i, 1.0)).collect();
        b.publish(1, &many);
        assert_eq!(b.totals().len(), MAX_ITEMS);
    }
}
