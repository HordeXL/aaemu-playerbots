//! The `seen` section of the journal: the zones and places a bot has been (owned by the explore
//! feature).
//!
//! Every zone the bot stands in is recorded by the bot task, whatever it is doing (`Bot::publish`
//! adds the zone key when it changes). The explore goal adds the landmarks it went to see
//! ([`Seen::add_place`], 100 m cells), the time of its last trip and the destinations that did not
//! work out ([`Seen::avoid`], with an expiry).

use serde::{Deserialize, Serialize};

/// Zones kept at most (the oldest go first).
pub const MAX_ZONES: usize = 128;
/// Places (landmarks the explore goal went to) kept at most.
pub const MAX_PLACES: usize = 64;
/// Destinations to avoid kept at most.
pub const MAX_AVOID: usize = 8;
/// The side of the cells places are kept in (metres).
pub const PLACE_CELL_M: f32 = 100.0;

/// A destination that did not work out, and until when it is left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Avoid {
    /// The place, a [`PLACE_CELL_M`] cell.
    pub cell: [i32; 2],
    /// Unix seconds until which it is not chosen.
    pub until_unix: u64,
}

/// Where the bot has been.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    /// Zone keys (`World::zone_key_at`) the bot has stood in, oldest first, at most
    /// [`MAX_ZONES`].
    pub zones: Vec<u16>,
    /// The landmarks the bot went to see, as [`PLACE_CELL_M`] cells, oldest first, at most
    /// [`MAX_PLACES`].
    pub places: Vec<[i32; 2]>,
    /// Unix seconds of the end of the last exploring trip, 0 before the first.
    pub last_trip_unix: u64,
    /// Exploring trips made.
    pub trips: u32,
    /// Destinations to leave alone for a while, at most [`MAX_AVOID`].
    pub avoid: Vec<Avoid>,
}

/// The [`PLACE_CELL_M`] cell of (x, y).
fn place_cell(x: f32, y: f32) -> [i32; 2] {
    [
        (x / PLACE_CELL_M).floor() as i32,
        (y / PLACE_CELL_M).floor() as i32,
    ]
}

impl Seen {
    /// The cell (of [`PLACE_CELL_M`]) places are kept in.
    pub fn cell(x: f32, y: f32) -> [i32; 2] {
        place_cell(x, y)
    }

    /// Whether the bot has been in zone `key`.
    pub fn has_zone(&self, key: u16) -> bool {
        self.zones.contains(&key)
    }

    /// Records zone `key`; returns true when it is new.
    pub fn add_zone(&mut self, key: u16) -> bool {
        if self.has_zone(key) {
            return false;
        }
        if self.zones.len() >= MAX_ZONES {
            self.zones.remove(0);
        }
        self.zones.push(key);
        true
    }

    /// Whether the bot went to see the place at (x, y) (or one in the same cell).
    pub fn has_place(&self, x: f32, y: f32) -> bool {
        self.places.contains(&place_cell(x, y))
    }

    /// Records the place at (x, y); returns true when it is new.
    pub fn add_place(&mut self, x: f32, y: f32) -> bool {
        let cell = place_cell(x, y);
        if self.places.contains(&cell) {
            return false;
        }
        if self.places.len() >= MAX_PLACES {
            self.places.remove(0);
        }
        self.places.push(cell);
        true
    }

    /// A trip ended at `now_unix`.
    pub fn trip_done(&mut self, now_unix: u64) {
        self.last_trip_unix = now_unix;
        self.trips = self.trips.saturating_add(1);
    }

    /// Leaves the destination (x, y) alone until `until_unix`.
    pub fn avoid(&mut self, x: f32, y: f32, until_unix: u64, now_unix: u64) {
        let cell = place_cell(x, y);
        self.avoid
            .retain(|a| a.until_unix > now_unix && a.cell != cell);
        if self.avoid.len() >= MAX_AVOID {
            self.avoid.remove(0);
        }
        self.avoid.push(Avoid { cell, until_unix });
    }

    /// Whether the destination (x, y) is to be left alone at `now_unix`.
    pub fn avoided(&self, x: f32, y: f32, now_unix: u64) -> bool {
        let cell = place_cell(x, y);
        self.avoid
            .iter()
            .any(|a| a.cell == cell && a.until_unix > now_unix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_are_kept_once_and_bounded() {
        let mut s = Seen::default();
        assert!(s.add_zone(4));
        assert!(!s.add_zone(4));
        for z in 0..(MAX_ZONES as u16 + 10) {
            s.add_zone(1000 + z);
        }
        assert_eq!(s.zones.len(), MAX_ZONES);
        assert!(!s.has_zone(4), "the oldest went first");
    }

    #[test]
    fn places_are_cells_kept_once_and_bounded() {
        let mut s = Seen::default();
        assert!(s.add_place(150.0, 250.0));
        assert!(!s.add_place(199.0, 201.0), "the same 100 m cell");
        assert!(s.has_place(120.0, 280.0));
        assert!(!s.has_place(250.0, 250.0));
        assert_eq!(place_cell(-1.0, 0.0), [-1, 0]);
        for i in 0..(MAX_PLACES as i32 + 5) {
            s.add_place(10_000.0 + i as f32 * 100.0, 0.0);
        }
        assert_eq!(s.places.len(), MAX_PLACES);
        assert!(!s.has_place(150.0, 250.0), "the oldest went first");
        s.trip_done(77);
        assert_eq!((s.last_trip_unix, s.trips), (77, 1));
    }

    #[test]
    fn an_avoided_destination_expires_and_the_list_is_bounded() {
        let mut s = Seen::default();
        s.avoid(500.0, 500.0, 1_000, 0);
        assert!(s.avoided(520.0, 540.0, 999));
        assert!(!s.avoided(520.0, 540.0, 1_000), "expired");
        assert!(!s.avoided(900.0, 500.0, 10));
        // the same cell again replaces the old entry
        s.avoid(510.0, 510.0, 5_000, 10);
        assert_eq!(s.avoid.len(), 1);
        for i in 0..20 {
            s.avoid(i as f32 * 1000.0, 9_000.0, 9_999, 20);
        }
        assert_eq!(s.avoid.len(), MAX_AVOID);
        // expired entries are dropped when a new one comes
        s.avoid(1.0, 1.0, 20_000, 10_000);
        assert_eq!(s.avoid.len(), 1);
    }
}
