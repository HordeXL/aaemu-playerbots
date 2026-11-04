//! Where the leader is, as far as the bot can tell, and in which world.
//!
//! Pure: [`leader_state`] turns what the bot sees (the leader's unit in view) and what the server
//! tells (the position records of `SCTeamRemoteMembersEx`, see [`crate::bot::party::Remote`])
//! into the [`Leader`] the dungeon-follow machine works with, and [`position`] says where to walk
//! to. A record carries the instance id of the leader's world (0 = the main world), so the bot
//! knows when the leader went through a door even when it is too far away to see it; a leader
//! that was seen next to an entrance and left the view without any newer record counts as
//! [`Leader::Vanished`] (the server sends the record on every move of a member, so the gap is a
//! second or two).

use super::enter::Leader;

/// A position record is trusted this long (bot clock, milliseconds).
pub const REC_FRESH_MS: u64 = 30_000;
/// A leader that left the view this long ago and showed nothing since is no longer "vanished".
pub const VANISH_MS: u64 = 10_000;
/// The leader stood this close (metres) to an entrance when it was last seen: "vanished" counts.
pub const ENTRANCE_M: f32 = 14.0;

/// The position record of the leader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rec {
    /// Bot clock of the packet (0 = none yet).
    pub seen_ms: u64,
    /// Zone key of the leader's position (an instance's zone key inside a dungeon).
    pub zone_id: u32,
    /// The instance of the leader's world (0 = the main world).
    pub instance_id: u32,
    /// Position in the leader's own world.
    pub pos: [f32; 3],
}

/// Where the leader was last in view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sight {
    /// Bot clock (0 = never).
    pub at_ms: u64,
    /// Position then.
    pub pos: [f32; 3],
    /// An entrance of a dungeon lay within [`ENTRANCE_M`] of it.
    pub near_entrance: bool,
}

/// What the state looks at.
#[derive(Debug, Clone, Copy)]
pub struct In {
    /// The bot clock.
    pub now_ms: u64,
    /// The leader is online.
    pub online: bool,
    /// The leader's unit is in view.
    pub in_view: bool,
    /// The leader's last position record.
    pub rec: Option<Rec>,
    /// The bot is in an instance.
    pub in_instance: bool,
    /// The instance the bot is in (0 = the main world).
    pub my_instance: u32,
    /// Where the leader was last in view.
    pub sight: Sight,
}

fn fresh(rec: Option<Rec>, now_ms: u64) -> Option<Rec> {
    rec.filter(|r| r.seen_ms != 0 && now_ms.saturating_sub(r.seen_ms) <= REC_FRESH_MS)
}

fn of_rec(r: Rec) -> Leader {
    if r.instance_id == 0 {
        Leader::Main
    } else {
        Leader::Dungeon(r.instance_id)
    }
}

/// The world the leader is in. See the module docs.
pub fn leader_state(i: &In) -> Leader {
    if !i.online {
        return Leader::Unknown;
    }
    if i.in_view {
        // (what is in view is in the bot's own world)
        return if i.in_instance {
            Leader::Dungeon(i.my_instance)
        } else {
            Leader::Main
        };
    }
    let rec = fresh(i.rec, i.now_ms);
    // a record newer than the last time the leader was seen is the news
    if let Some(r) = rec.filter(|r| r.seen_ms > i.sight.at_ms) {
        return of_rec(r);
    }
    let gone = i.sight.at_ms != 0
        && !i.in_instance
        && i.sight.near_entrance
        && i.now_ms.saturating_sub(i.sight.at_ms) <= VANISH_MS;
    if gone {
        return Leader::Vanished;
    }
    rec.map_or(Leader::Unknown, of_rec)
}

/// Where to walk to follow the leader: its unit in view, else the last record when the leader is
/// in the world the bot is in.
pub fn position(
    view: Option<[f32; 3]>,
    rec: Option<Rec>,
    my_instance: u32,
    now_ms: u64,
) -> Option<[f32; 3]> {
    view.or_else(|| {
        fresh(rec, now_ms)
            .filter(|r| r.instance_id == my_instance)
            .map(|r| r.pos)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seen_ms: u64, instance_id: u32) -> Rec {
        Rec {
            seen_ms,
            zone_id: 7,
            instance_id,
            pos: [10.0, 20.0, 30.0],
        }
    }

    fn base() -> In {
        In {
            now_ms: 100_000,
            online: true,
            in_view: false,
            rec: None,
            in_instance: false,
            my_instance: 0,
            sight: Sight::default(),
        }
    }

    #[test]
    fn a_leader_in_view_is_in_the_bots_own_world() {
        let mut i = base();
        i.in_view = true;
        assert_eq!(leader_state(&i), Leader::Main);
        i.in_instance = true;
        i.my_instance = 9;
        assert_eq!(leader_state(&i), Leader::Dungeon(9));
        i.online = false;
        assert_eq!(leader_state(&i), Leader::Unknown);
    }

    #[test]
    fn the_record_says_which_world_the_leader_is_in() {
        let mut i = base();
        assert_eq!(leader_state(&i), Leader::Unknown);
        i.rec = Some(rec(99_000, 0));
        assert_eq!(leader_state(&i), Leader::Main);
        i.rec = Some(rec(99_000, 41));
        assert_eq!(leader_state(&i), Leader::Dungeon(41));
        // a record from long ago is no news
        i.rec = Some(rec(100_000 - REC_FRESH_MS - 1, 41));
        assert_eq!(leader_state(&i), Leader::Unknown);
        // a position packet never received
        i.rec = Some(rec(0, 41));
        assert_eq!(leader_state(&i), Leader::Unknown);
    }

    #[test]
    fn a_leader_that_left_the_view_at_an_entrance_vanished_until_a_record_says_more() {
        let mut i = base();
        i.sight = Sight {
            at_ms: 98_000,
            pos: [1.0, 2.0, 3.0],
            near_entrance: true,
        };
        // (the last record is older than the sight: it still says "main world")
        i.rec = Some(rec(97_000, 0));
        assert_eq!(leader_state(&i), Leader::Vanished);
        // news after the sight win
        i.rec = Some(rec(99_000, 41));
        assert_eq!(leader_state(&i), Leader::Dungeon(41));
        i.rec = Some(rec(99_000, 0));
        assert_eq!(leader_state(&i), Leader::Main);
        // too long ago, or not at an entrance, or the bot is inside itself: not vanished
        i.rec = Some(rec(97_000, 0));
        i.now_ms = 98_000 + VANISH_MS + 1;
        assert_eq!(leader_state(&i), Leader::Main);
        i.now_ms = 100_000;
        i.sight.near_entrance = false;
        assert_eq!(leader_state(&i), Leader::Main);
        i.sight.near_entrance = true;
        i.in_instance = true;
        i.my_instance = 5;
        assert_eq!(leader_state(&i), Leader::Main);
    }

    #[test]
    fn the_bot_walks_to_the_leader_in_view_or_to_its_record_in_the_same_world() {
        let view = Some([1.0, 2.0, 3.0]);
        let r = Some(rec(99_000, 0));
        assert_eq!(position(view, r, 0, 100_000), view);
        assert_eq!(position(None, r, 0, 100_000), Some([10.0, 20.0, 30.0]));
        // another world: the coordinates mean nothing here
        assert_eq!(position(None, r, 7, 100_000), None);
        assert_eq!(
            position(None, Some(rec(99_000, 7)), 7, 100_000),
            Some([10.0, 20.0, 30.0])
        );
        // too old
        assert_eq!(position(None, Some(rec(1, 0)), 0, 100_000), None);
        assert_eq!(position(None, None, 0, 100_000), None);
    }
}
