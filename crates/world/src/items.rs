//! Trade facts about every item template: what a vendor asks and pays, how the item binds, how
//! far it stacks and where the auction house files it.
//!
//! The market features (the auction house, trade-chat sales, selling to players) need to know
//! whether a bot may hand an item to somebody else at all and what it is worth, for 21 000
//! templates. [`ItemFacts`] keeps that in 20 bytes per item, in an [`IdMap`] (sorted ids, binary
//! search, no per-entry overhead): about half a megabyte for the whole 1.2 table, shared by all
//! bots. The English name is not repeated here; it lives in the name table behind
//! [`World::item_name`](crate::World::item_name).
//!
//! Source: the columns of `items` in the server's `compact.sqlite3`.

use anyhow::Result;
use rusqlite::Connection;

use crate::geom::IdMap;
use crate::sql::{each_row, RowExt};

/// What `items.bind_id` means (the server's `ItemBindType`, `data/items.rs`).
pub mod bind {
    /// Not bound: the item can be traded, mailed and auctioned.
    pub const NORMAL: u8 = 1;
    /// Bound when picked up (soul bound in the bag at once).
    pub const ON_PICKUP: u8 = 2;
    /// Bound when equipped.
    pub const ON_EQUIP: u8 = 3;
    /// Bound when unpacked (a trade pack's contents).
    pub const ON_UNPACK: u8 = 4;
    /// Bound when picked up, trade-pack flavour.
    pub const ON_PICKUP_PACK: u8 = 5;
    /// Bound when won at an auction.
    pub const ON_AUCTION_WIN: u8 = 6;
}

/// The trade facts of one item template. 20 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemFacts {
    /// What a vendor asks for one (`items.price`, copper).
    pub price: u32,
    /// What a vendor pays for one of the basic grade (`items.refund`, copper).
    pub refund: u32,
    /// Largest stack (`items.max_stack_size`); 1 for equipment.
    pub max_stack: u32,
    /// `items.bind_id`, see [`bind`].
    pub bind: u8,
    /// `items.level`.
    pub level: u8,
    /// The fixed grade the item always has (`items.fixed_grade`); 0 when it has none (an item
    /// without a fixed grade is graded when it is created).
    pub grade: u8,
    /// The auction house categories `(a, b, c)` (`items.auction_[abc]_category_id`) its searches
    /// filter by; `[0; 3]` when the item has none.
    pub auction_cat: [u8; 3],
    /// `items.sellable`: the vendors take it (the flag the server's store checks).
    pub sellable: bool,
}

impl ItemFacts {
    /// Whether a bot may hand the item to someone else (a player trade, the auction house): it is
    /// sellable and not bound ([`bind::NORMAL`]). An item that binds on pickup, on equip or on
    /// unpack is treated as untradable, also while it is still unbound in the bag: the bot cannot
    /// see the item's bound flag.
    pub fn tradable(&self) -> bool {
        self.sellable && self.bind == bind::NORMAL
    }
}

/// [`ItemFacts`] of every item template.
#[derive(Debug, Default)]
pub struct ItemTable {
    facts: IdMap<ItemFacts>,
}

impl ItemTable {
    /// Reads the trade columns of `items`.
    pub fn load(conn: &Connection) -> Result<ItemTable> {
        let mut rows: Vec<(u32, ItemFacts)> = Vec::with_capacity(22_000);
        each_row(
            conn,
            "SELECT id, price, refund, bind_id, max_stack_size, sellable, level, fixed_grade, \
             auction_a_category_id, auction_b_category_id, auction_c_category_id FROM items",
            |r| {
                rows.push((
                    r.u32(0),
                    ItemFacts {
                        price: r.u32(1),
                        refund: r.u32(2),
                        bind: r.u8(3),
                        max_stack: r.u32(4),
                        sellable: r.flag(5),
                        level: r.u8(6),
                        // (-1: no fixed grade)
                        grade: r.u8(7),
                        auction_cat: [r.u8(8), r.u8(9), r.u8(10)],
                    },
                ));
            },
        )?;
        Ok(ItemTable {
            facts: IdMap::build(rows),
        })
    }

    /// The facts of item `template`.
    pub fn get(&self, template: u32) -> Option<&ItemFacts> {
        self.facts.get(template)
    }

    /// Every item as `(template, facts)` in ascending template order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &ItemFacts)> {
        self.facts.iter()
    }

    /// Number of item templates.
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.facts.shallow_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INT, price INT, refund INT, bind_id INT, max_stack_size INT, \
               sellable NUM, level INT, fixed_grade INT, auction_a_category_id INT, \
               auction_b_category_id INT, auction_c_category_id INT);
             INSERT INTO items VALUES (3411, 40, 20, 1, 1000, 't', 1, -1, 7, 24, 36);
             INSERT INTO items VALUES (100, 9, 4, 2, 1, 't', 5, 3, NULL, NULL, NULL);
             INSERT INTO items VALUES (101, 9, 4, 1, 20, 'f', 5, 0, 1, 2, 3);
             INSERT INTO items VALUES (102, 0, 0, 3, 1, 't', 50, 2, 4, 5, 6);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn the_columns_are_read_leniently() {
        let t = ItemTable::load(&conn()).unwrap();
        assert_eq!(t.len(), 4);
        let ore = t.get(3411).unwrap();
        assert_eq!(
            *ore,
            ItemFacts {
                price: 40,
                refund: 20,
                max_stack: 1000,
                bind: 1,
                level: 1,
                grade: 0,
                auction_cat: [7, 24, 36],
                sellable: true,
            }
        );
        // NULL categories read as none, a fixed grade is kept
        let bop = t.get(100).unwrap();
        assert_eq!((bop.auction_cat, bop.grade, bop.level), ([0; 3], 3, 5));
        assert!(t.get(999).is_none());
        assert_eq!(t.bytes(), 4 * (4 + std::mem::size_of::<ItemFacts>()));
    }

    #[test]
    fn only_unbound_sellable_items_are_tradable() {
        let t = ItemTable::load(&conn()).unwrap();
        assert!(t.get(3411).unwrap().tradable());
        // binds when picked up
        assert!(!t.get(100).unwrap().tradable());
        // unbound but the vendors do not take it
        assert!(!t.get(101).unwrap().tradable());
        // binds when equipped
        assert!(!t.get(102).unwrap().tradable());
    }

    #[test]
    fn an_item_is_twenty_bytes() {
        assert_eq!(std::mem::size_of::<ItemFacts>(), 20);
    }
}
