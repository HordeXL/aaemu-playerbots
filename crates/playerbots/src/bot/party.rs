//! Party orders: the coordinator's instruction to every bot of a group that is to do something
//! together (owned by the dungeons feature).
//!
//! A [`PartyOrder`] reaches each member as [`BotCmd::Party`](super::cmd::BotCmd::Party); the
//! brain queues the goal the [`PartyPurpose`] names (today only [`GoalKind::Dungeon`](super::cmd::GoalKind))
//! and hands the order to it, where the goal reads it through
//! [`Goal::with_party`](super::goals::Goal::with_party). Like a [`PairOrder`](super::cmd::PairOrder),
//! an order is consumed by the goal it starts: a goal started any other way gets none.
//!
//! Everything here is plain data. The party itself (the server-side `CSInviteToTeam` flow, who
//! leads, when to enter) is the dungeon goal's business.

use smallvec::SmallVec;

/// The most members of one party (the server's team size for dungeons is 5).
pub const MAX_PARTY: usize = 5;

/// What a party is formed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartyPurpose {
    /// Clear an instance dungeon.
    Dungeon {
        /// The `indun_zones` zone group of the dungeon (what `aa_world::World::dungeons()`
        /// orders by).
        zone_group: u32,
    },
}

/// The coordinator's order to one member of a party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyOrder {
    /// Coordinator-assigned id of this group (unique per process; not the server's team id).
    pub party_id: u32,
    /// Bot id of the leader (one of `members`).
    pub leader: u32,
    /// Bot ids of every member, the leader included.
    pub members: SmallVec<[u32; MAX_PARTY]>,
    /// What the party is for.
    pub purpose: PartyPurpose,
}

impl PartyOrder {
    /// Whether bot `bot` leads the party.
    pub fn is_leader(&self, bot: u32) -> bool {
        self.leader == bot
    }

    /// Whether bot `bot` is a member.
    pub fn has_member(&self, bot: u32) -> bool {
        self.members.contains(&bot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_and_members() {
        let o = PartyOrder {
            party_id: 7,
            leader: 3,
            members: SmallVec::from_slice(&[3, 4, 9]),
            purpose: PartyPurpose::Dungeon { zone_group: 12 },
        };
        assert!(o.is_leader(3) && !o.is_leader(4));
        assert!(o.has_member(9) && !o.has_member(5));
        assert!(!o.members.spilled());
    }
}
