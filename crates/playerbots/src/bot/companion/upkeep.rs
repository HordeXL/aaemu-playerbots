//! What a bot in a real player's party keeps doing besides following: announce its role, give the
//! lead back to a player, and leave when it should. Pure: [`plan`] looks at the team and says what
//! to send.
//!
//! The server makes the next online member the owner when the owner leaves or goes offline, and a
//! bot that invited a player ("inv") owns the team until the player joins, so a bot may find itself
//! leading a human's party; it hands the lead to the first real player online. A bot leaves when
//! it was asked to, when no real player is left, when the real players have been offline for
//! [`GONE_LEAVE_MS`], or when the levels drifted far apart.

use super::policy::level_fits;

/// The real players are all offline this long (bot clock) and the companion leaves.
pub const GONE_LEAVE_MS: u64 = 3 * 60_000;
/// Gap between two attempts to hand the lead over.
pub const HAND_GAP_MS: u64 = 8_000;
/// Attempts to hand the lead over.
pub const HAND_TRIES: u8 = 4;
/// The level gap that makes a bot leave is the configured gap plus this: it joined within the gap
/// (when it could see the level) and leaves only when it grew clearly.
pub const GAP_SLACK: u8 = 5;

/// A member of the team as the plan sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Character id.
    pub char_id: u32,
    /// One of our bots (else a real player).
    pub bot: bool,
    /// Online.
    pub online: bool,
    /// Level.
    pub level: u8,
}

/// What the plan looks at.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The bot clock.
    pub now_ms: u64,
    /// The bot's character id.
    pub me: u32,
    /// The bot's level.
    pub my_level: u8,
    /// Character id of the owner of the team.
    pub owner: u32,
    /// Every member, the bot included.
    pub members: &'a [Member],
    /// The bot announced its role in this team already.
    pub role_sent: bool,
    /// Attempts to hand the lead over so far and the bot clock of the last one.
    pub hand: (u8, u64),
    /// Bot clock since which no real player was online (0 = one is).
    pub gone_since_ms: u64,
    /// A player asked the bot to leave.
    pub leave_asked: bool,
    /// `party.max_level_gap`.
    pub max_gap: u8,
}

/// Why a bot leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// The leader told it to.
    Asked,
    /// No real player is in the team any more.
    NoPlayers,
    /// The real players are offline for good.
    Gone,
    /// The levels drifted far apart.
    LevelGap,
}

impl Leave {
    /// A word for the log.
    pub fn word(self) -> &'static str {
        match self {
            Leave::Asked => "asked to leave",
            Leave::NoPlayers => "no real player left",
            Leave::Gone => "the players are offline",
            Leave::LevelGap => "level gap",
        }
    }
}

/// What to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// Announce the role.
    pub role: bool,
    /// Hand the lead to this character.
    pub hand_over: Option<u32>,
    /// Leave the team.
    pub leave: Option<Leave>,
    /// The new value of `gone_since_ms`.
    pub gone_since_ms: u64,
}

/// The leader a companion follows: the owner when a real player owns the team, else the first
/// real player online (a bot-owned team hands over to them).
pub fn leader_of(owner: u32, members: &[Member], me: u32) -> Option<u32> {
    let real = |m: &&Member| !m.bot && m.char_id != me;
    if let Some(o) = members.iter().filter(real).find(|m| m.char_id == owner) {
        return Some(o.char_id);
    }
    members
        .iter()
        .filter(real)
        .find(|m| m.online)
        .or_else(|| members.iter().find(real))
        .map(|m| m.char_id)
}

/// The plan for this tick.
pub fn plan(f: &Facts<'_>) -> Plan {
    let mut p = Plan {
        role: !f.role_sent,
        gone_since_ms: f.gone_since_ms,
        ..Plan::default()
    };
    let real: Vec<&Member> = f
        .members
        .iter()
        .filter(|m| !m.bot && m.char_id != f.me)
        .collect();
    let online = real.iter().find(|m| m.online);
    if f.leave_asked {
        p.leave = Some(Leave::Asked);
        return p;
    }
    if real.is_empty() {
        p.leave = Some(Leave::NoPlayers);
        return p;
    }
    match online {
        None => {
            if p.gone_since_ms == 0 {
                p.gone_since_ms = f.now_ms.max(1);
            } else if f.now_ms.saturating_sub(p.gone_since_ms) >= GONE_LEAVE_MS {
                p.leave = Some(Leave::Gone);
                return p;
            }
        }
        Some(_) => p.gone_since_ms = 0,
    }
    if let Some(l) =
        leader_of(f.owner, f.members, f.me).and_then(|c| real.iter().find(|m| m.char_id == c))
    {
        if l.level != 0 && !level_fits(f.my_level, l.level, f.max_gap.saturating_add(GAP_SLACK)) {
            p.leave = Some(Leave::LevelGap);
            return p;
        }
    }
    if f.owner == f.me {
        if let Some(target) = online {
            let (tries, last) = f.hand;
            if tries < HAND_TRIES && (tries == 0 || f.now_ms.saturating_sub(last) >= HAND_GAP_MS) {
                p.hand_over = Some(target.char_id);
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u32 = 10;

    fn m(char_id: u32, bot: bool, online: bool, level: u8) -> Member {
        Member {
            char_id,
            bot,
            online,
            level,
        }
    }

    fn facts<'a>(members: &'a [Member], owner: u32) -> Facts<'a> {
        Facts {
            now_ms: 100_000,
            me: ME,
            my_level: 20,
            owner,
            members,
            role_sent: true,
            hand: (0, 0),
            gone_since_ms: 0,
            leave_asked: false,
            max_gap: 10,
        }
    }

    #[test]
    fn the_leader_is_the_real_owner_or_the_first_real_player_online() {
        let team = [
            m(1, false, true, 30),
            m(ME, true, true, 20),
            m(3, false, true, 25),
        ];
        assert_eq!(leader_of(1, &team, ME), Some(1));
        assert_eq!(leader_of(3, &team, ME), Some(3));
        // a bot owns the team: the first real player online leads
        assert_eq!(leader_of(ME, &team, ME), Some(1));
        let offline = [
            m(1, false, false, 30),
            m(ME, true, true, 20),
            m(3, false, true, 25),
        ];
        assert_eq!(leader_of(ME, &offline, ME), Some(3));
        // a team of bots has none
        let bots = [m(ME, true, true, 20), m(4, true, true, 20)];
        assert_eq!(leader_of(ME, &bots, ME), None);
        // only offline real players: the first of them
        let off = [m(1, false, false, 30), m(ME, true, true, 20)];
        assert_eq!(leader_of(ME, &off, ME), Some(1));
    }

    #[test]
    fn a_new_member_announces_its_role_once() {
        let team = [m(1, false, true, 30), m(ME, true, true, 20)];
        let mut f = facts(&team, 1);
        f.role_sent = false;
        assert!(plan(&f).role);
        f.role_sent = true;
        assert!(!plan(&f).role);
        assert_eq!(plan(&f), Plan::default());
    }

    #[test]
    fn a_bot_that_owns_the_team_hands_it_to_the_player_a_few_times() {
        let team = [m(ME, true, true, 20), m(1, false, true, 30)];
        let mut f = facts(&team, ME);
        assert_eq!(plan(&f).hand_over, Some(1));
        // not again at once, but after the gap, and not forever
        f.hand = (1, f.now_ms - 1_000);
        assert_eq!(plan(&f).hand_over, None);
        f.hand = (1, f.now_ms - HAND_GAP_MS);
        assert_eq!(plan(&f).hand_over, Some(1));
        f.hand = (HAND_TRIES, 0);
        assert_eq!(plan(&f).hand_over, None);
        // an offline player cannot be handed the lead
        let off = [m(ME, true, true, 20), m(1, false, false, 30)];
        assert_eq!(plan(&facts(&off, ME)).hand_over, None);
        // a player who owns the team needs nothing
        assert_eq!(plan(&facts(&team, 1)).hand_over, None);
    }

    #[test]
    fn a_bot_leaves_when_asked_or_alone_or_when_the_players_stay_away() {
        let team = [m(1, false, true, 30), m(ME, true, true, 20)];
        let mut f = facts(&team, 1);
        f.leave_asked = true;
        assert_eq!(plan(&f).leave, Some(Leave::Asked));
        // bots only
        let bots = [m(ME, true, true, 20), m(4, true, true, 20)];
        assert_eq!(plan(&facts(&bots, ME)).leave, Some(Leave::NoPlayers));
        // the player goes offline: the clock starts, the bot waits, then leaves
        let off = [m(1, false, false, 30), m(ME, true, true, 20)];
        let mut f = facts(&off, 1);
        let p = plan(&f);
        assert_eq!((p.leave, p.gone_since_ms), (None, f.now_ms));
        f.gone_since_ms = p.gone_since_ms;
        f.now_ms += GONE_LEAVE_MS - 1;
        assert_eq!(plan(&f).leave, None);
        f.now_ms += 1;
        assert_eq!(plan(&f).leave, Some(Leave::Gone));
        // the player comes back: the clock stops
        let mut g = facts(&team, 1);
        g.gone_since_ms = 5_000;
        assert_eq!(plan(&g).gone_since_ms, 0);
    }

    #[test]
    fn a_leader_far_above_leaves_only_beyond_the_slack() {
        let mut team = [m(1, false, true, 30), m(ME, true, true, 20)];
        let f = facts(&team, 1);
        assert_eq!(plan(&f).leave, None);
        team[0].level = 20 + 10 + GAP_SLACK;
        assert_eq!(plan(&facts(&team, 1)).leave, None);
        team[0].level = 20 + 10 + GAP_SLACK + 1;
        assert_eq!(plan(&facts(&team, 1)).leave, Some(Leave::LevelGap));
        // a level the bot does not know (0) is no reason
        team[0].level = 0;
        assert_eq!(plan(&facts(&team, 1)).leave, None);
        for l in [Leave::Asked, Leave::NoPlayers, Leave::Gone, Leave::LevelGap] {
            assert!(!l.word().is_empty());
        }
    }
}
