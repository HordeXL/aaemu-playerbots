//! The `seen` section of the journal: the zones and places a bot has been (owned by the explore
//! feature).

use serde::{Deserialize, Serialize};

/// Zones kept at most (the oldest go first).
pub const MAX_ZONES: usize = 128;

/// Where the bot has been.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    /// Zone keys (`World::zone_key_at`) the bot has stood in, oldest first, at most
    /// [`MAX_ZONES`].
    pub zones: Vec<u16>,
}

impl Seen {
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
}
