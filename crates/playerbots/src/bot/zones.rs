//! The conflict state of the zone groups, as the server announces it.
//!
//! The server sends `SCConflictZoneStatePacket` for every zone group at world entry and again at
//! each change ([`WorldEvent::ConflictZoneState`](aa_proto::sc_world::WorldEvent)); the bot keeps
//! the latest of each in a [`ZoneStates`] table, which
//! [`BotState::apply`](super::state::BotState::apply) feeds. Read it through
//! [`BotState::zone_state`](super::state::BotState::zone_state) (the state and the Unix second it
//! ends) and [`BotState::at_war`](super::state::BotState::at_war) (conflict or war: the zone is
//! fought over). The states are [`conflict_state`] (`TENSION` ... `PEACE`).
//!
//! The table is bounded ([`MAX_ZONE_STATES`]) and costs one pointer until the first packet. The end
//! time is the server's schedule, not a promise: the state is current until a new packet says
//! otherwise.

use aa_proto::sc_world::conflict_state;

/// Most zone groups kept (the server has a few dozen at most).
pub const MAX_ZONE_STATES: usize = 64;

/// One zone group's conflict state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneEntry {
    /// Zone group id.
    pub zone_group: u16,
    /// One of [`conflict_state`].
    pub state: u8,
    /// When the state ends, in Unix seconds (0 when the packet gave no end).
    pub end_unix_s: i64,
}

/// The entries behind the one pointer of [`ZoneStates`].
#[derive(Debug, Default)]
struct Table {
    entries: Vec<ZoneEntry>,
}

/// The latest conflict state of every zone group the server told the bot about.
#[derive(Debug, Default)]
pub struct ZoneStates(Option<Box<Table>>);

impl ZoneStates {
    /// Records the state of `zone_group`, replacing the earlier one. `false` when the table is
    /// full of other zone groups and the new one was not kept.
    pub fn set(&mut self, zone_group: u16, state: u8, end_unix_s: i64) -> bool {
        let entry = ZoneEntry {
            zone_group,
            state,
            end_unix_s,
        };
        let table = self.0.get_or_insert_with(Box::default);
        if let Some(e) = table
            .entries
            .iter_mut()
            .find(|e| e.zone_group == zone_group)
        {
            *e = entry;
            return true;
        }
        if table.entries.len() >= MAX_ZONE_STATES {
            return false;
        }
        table.entries.push(entry);
        true
    }

    /// The state of `zone_group` and the Unix second it ends; `None` when the server has not
    /// announced the zone group.
    pub fn get(&self, zone_group: u16) -> Option<(u8, i64)> {
        self.iter()
            .find(|e| e.zone_group == zone_group)
            .map(|e| (e.state, e.end_unix_s))
    }

    /// Whether the zone group is fought over: [`conflict_state::CONFLICT`] or
    /// [`conflict_state::WAR`]. `false` for a zone group the server has not announced.
    pub fn at_war(&self, zone_group: u16) -> bool {
        self.get(zone_group)
            .is_some_and(|(state, _)| conflict_state::at_war(state))
    }

    /// Every known zone group, in the order they were first announced.
    pub fn iter(&self) -> impl Iterator<Item = &ZoneEntry> {
        self.0.iter().flat_map(|t| t.entries.iter())
    }

    /// Number of zone groups known.
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |t| t.entries.len())
    }

    /// Whether no zone group is known.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conflict_state as cs;

    #[test]
    fn an_empty_table_knows_nothing_and_costs_a_pointer() {
        let z = ZoneStates::default();
        assert!(z.is_empty());
        assert_eq!(z.len(), 0);
        assert_eq!(z.get(7), None);
        assert!(!z.at_war(7));
        assert_eq!(z.iter().count(), 0);
        assert_eq!(
            std::mem::size_of::<ZoneStates>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn the_latest_packet_of_a_zone_group_wins() {
        let mut z = ZoneStates::default();
        assert!(z.set(7, cs::TENSION, 100));
        assert!(z.set(9, cs::PEACE, 0));
        assert_eq!(z.get(7), Some((cs::TENSION, 100)));
        assert_eq!(z.get(9), Some((cs::PEACE, 0)));
        assert!(z.set(7, cs::WAR, 200));
        assert_eq!(z.get(7), Some((cs::WAR, 200)));
        // (the other zone group and the order are untouched)
        assert_eq!(z.get(9), Some((cs::PEACE, 0)));
        assert_eq!(z.len(), 2);
        let order: Vec<u16> = z.iter().map(|e| e.zone_group).collect();
        assert_eq!(order, [7, 9]);
    }

    #[test]
    fn only_conflict_and_war_count_as_war() {
        let mut z = ZoneStates::default();
        for state in 0..=7u8 {
            z.set(u16::from(state) + 1, state, 0);
        }
        for state in 0..=7u8 {
            let want = state == cs::CONFLICT || state == cs::WAR;
            assert_eq!(z.at_war(u16::from(state) + 1), want, "state {state}");
        }
        // a zone group nobody announced is not at war, whatever its number
        assert!(!z.at_war(0));
        assert!(!z.at_war(u16::MAX));
        // and the end of a war is a state change like any other
        z.set(7, cs::PEACE, 5);
        assert!(!z.at_war(7));
    }

    #[test]
    fn the_table_is_bounded() {
        let mut z = ZoneStates::default();
        for g in 0..MAX_ZONE_STATES as u16 {
            assert!(z.set(g, cs::TENSION, 0), "{g}");
        }
        assert_eq!(z.len(), MAX_ZONE_STATES);
        // a new zone group does not fit ...
        assert!(!z.set(1000, cs::WAR, 0));
        assert_eq!(z.get(1000), None);
        assert_eq!(z.len(), MAX_ZONE_STATES);
        // ... a known one still updates
        assert!(z.set(3, cs::WAR, 9));
        assert_eq!(z.get(3), Some((cs::WAR, 9)));
        assert_eq!(z.len(), MAX_ZONE_STATES);
    }
}
