//! The `gear` section of the journal: what a bot learned about its equipment (owned by the gear
//! feature): its last change, the money it spent on gear, what it minted, and the socketings that
//! failed (which took the gems of a piece out), so a bot that restarts still knows its luck.

use serde::{Deserialize, Serialize};

/// Failed socketings kept at most.
pub const MAX_FAILED_SOCKETS: usize = 8;
/// The longest text of the last change kept.
pub const MAX_CHANGE_CHARS: usize = 120;

/// A socketing that failed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailedSocket {
    /// Unix seconds.
    pub unix: u64,
    /// The piece's template.
    pub piece: u32,
    /// The gem's template.
    pub gem: u32,
    /// Gems the piece held (and lost).
    pub held: u8,
}

/// What the bot learned about its gear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GearNotes {
    /// Unix seconds of the last time the bot changed its gear, 0 if never.
    pub last_change_unix: u64,
    /// The last change in words (at most [`MAX_CHANGE_CHARS`] characters).
    pub last_change: String,
    /// Pieces put on.
    pub changes: u32,
    /// Pieces bought from merchants.
    pub bought: u32,
    /// Copper spent on gear.
    pub spent: u64,
    /// Pieces minted with GM commands.
    pub minted: u32,
    /// Gems socketed.
    pub socketed: u32,
    /// The latest failed socketings, oldest first (at most [`MAX_FAILED_SOCKETS`]).
    pub failed_sockets: Vec<FailedSocket>,
    /// Regrades that raised the grade.
    pub regrades: u32,
    /// Regrades that did not.
    pub regrade_fails: u32,
}

impl GearNotes {
    /// A piece was put on.
    pub fn changed(&mut self, text: &str, unix: u64) {
        self.last_change_unix = unix;
        self.last_change = cut(text, MAX_CHANGE_CHARS).to_owned();
        self.changes = self.changes.saturating_add(1);
    }

    /// A piece was bought for `price` copper.
    pub fn bought(&mut self, price: u32, unix: u64) {
        self.bought = self.bought.saturating_add(1);
        self.spent = self.spent.saturating_add(u64::from(price));
        self.last_change_unix = unix;
    }

    /// A piece was minted.
    pub fn minted(&mut self, unix: u64) {
        self.minted = self.minted.saturating_add(1);
        self.last_change_unix = unix;
    }

    /// A socketing of `gem` into `piece` (holding `held` gems) worked (`ok`) or failed.
    pub fn socket(&mut self, piece: u32, gem: u32, held: u8, ok: bool, unix: u64) {
        self.last_change_unix = unix;
        if ok {
            self.socketed = self.socketed.saturating_add(1);
            return;
        }
        if self.failed_sockets.len() >= MAX_FAILED_SOCKETS {
            self.failed_sockets.remove(0);
        }
        self.failed_sockets.push(FailedSocket {
            unix,
            piece,
            gem,
            held,
        });
    }

    /// A regrade raised the grade (`up`) or did not.
    pub fn regrade(&mut self, up: bool, unix: u64) {
        self.last_change_unix = unix;
        if up {
            self.regrades = self.regrades.saturating_add(1);
        } else {
            self.regrade_fails = self.regrade_fails.saturating_add(1);
        }
    }
}

/// `text` cut to at most `max` characters.
fn cut(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notes_stay_bounded_and_count_what_happened() {
        let mut n = GearNotes::default();
        n.changed(&"x".repeat(500), 10);
        assert_eq!(n.last_change.chars().count(), MAX_CHANGE_CHARS);
        assert_eq!((n.changes, n.last_change_unix), (1, 10));
        n.bought(250, 11);
        n.bought(50, 12);
        assert_eq!((n.bought, n.spent), (2, 300));
        n.minted(13);
        for i in 0..20 {
            n.socket(22_229, 30_915, 2, false, 100 + i);
        }
        n.socket(22_229, 30_915, 0, true, 200);
        assert_eq!(n.failed_sockets.len(), MAX_FAILED_SOCKETS);
        assert_eq!(n.failed_sockets[0].unix, 112, "the oldest went first");
        assert_eq!((n.socketed, n.minted), (1, 1));
        n.regrade(true, 300);
        n.regrade(false, 301);
        assert_eq!((n.regrades, n.regrade_fails), (1, 1));
        // an old journal without the new keys loads
        let old: GearNotes = serde_json::from_str(r#"{"last_change_unix": 5}"#).unwrap();
        assert_eq!(old.last_change_unix, 5);
        let back: GearNotes = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
    }
}
