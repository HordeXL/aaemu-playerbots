//! What the auction house goal remembers between its runs (owned by the auction feature, goal
//! `auction`).
//!
//! [`AuctionMemory`] lives in [`Inner`](super::Inner) (`state.market.get_mut()`); until the
//! feature fills it in, it is empty. The feature reads `ctx.cfg.market` (`enabled`, `auction`),
//! [`Ledger::sellable_surplus`](super::ledger::Ledger::sellable_surplus) for the goods it may list
//! and `World::item_facts` for prices and categories, and keeps its own packets in the proto
//! crate.

/// What the bot remembers about its auction listings. Empty until the feature exists.
#[derive(Debug, Default)]
pub struct AuctionMemory {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_memory_costs_nothing() {
        assert_eq!(std::mem::size_of::<AuctionMemory>(), 0);
    }
}
