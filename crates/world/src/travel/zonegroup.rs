//! Zone groups: who hears whom in the shout, trade and group-finding chat.
//!
//! The server sends those three chat types to everybody online in the *zone group* of the sender's
//! zone (`zones.group_id`, `world/session.rs: zone_chat`), whatever their faction. The bots'
//! trade-chat advertisements are only worth posting when somebody could read them, so the market
//! asks which bots stand in the same zone group.

use crate::sql::{each_row, RowExt};

/// `zones.zone_key -> zones.group_id`.
#[derive(Debug, Default, Clone)]
pub struct ZoneGroups {
    /// `(zone key, zone group)`, sorted by zone key.
    groups: Vec<(u16, u32)>,
}

impl ZoneGroups {
    pub(crate) fn load(conn: &rusqlite::Connection) -> ZoneGroups {
        let mut groups: Vec<(u16, u32)> = Vec::new();
        // (a compact without zones leaves every zone in no group)
        let _ = each_row(conn, "SELECT zone_key, group_id FROM zones", |r| {
            groups.push((r.u16(0), r.u32(1)));
        });
        groups.sort_unstable();
        groups.dedup_by_key(|g| g.0);
        ZoneGroups { groups }
    }

    /// The zone group of a zone key.
    pub fn of_key(&self, zone_key: u16) -> Option<u32> {
        self.groups
            .binary_search_by_key(&zone_key, |g| g.0)
            .ok()
            .map(|i| self.groups[i].1)
    }

    /// Approximate heap bytes held.
    pub fn bytes(&self) -> usize {
        self.groups.capacity() * std::mem::size_of::<(u16, u32)>()
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn zone_keys_map_to_their_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE zones (id INT, zone_key INT, group_id INT);
             INSERT INTO zones VALUES (1, 7, 3), (2, 9, 3), (3, 260, 49), (4, 7, 99);",
        )
        .unwrap();
        let z = ZoneGroups::load(&conn);
        assert_eq!(z.of_key(9), Some(3));
        assert_eq!(z.of_key(260), Some(49));
        assert!(z.of_key(7).is_some());
        assert_eq!(z.of_key(8), None);
        assert!(z.bytes() > 0);
        // a database without the table
        let z = ZoneGroups::load(&Connection::open_in_memory().unwrap());
        assert_eq!(z.of_key(7), None);
    }
}
