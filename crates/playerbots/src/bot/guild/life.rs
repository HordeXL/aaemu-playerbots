//! The life of a bot in a guild: who recruits, who is promoted, who leaves, who greets whom.
//!
//! Pure decisions over a [`View`] of the guild log, tested without a connection; the duty
//! ([`super::duty`]) turns them into packets. The numbers are the pace of a small, friendly
//! guild: an officer invites one bot every half minute at most, the guild adds a member every
//! 15 seconds at most, the owner makes one or two officers, a member stays at least a quarter of
//! an hour and then leaves with the chance the configuration gives.

use aa_proto::guild::role;
use smallvec::SmallVec;

use super::log::{GuildLog, Member};

/// Bots recruit until a guild has this many members.
pub const TARGET_SIZE: usize = 20;
/// A guild sends an invitation at most this often (all its officers together).
pub const INVITE_GAP_MS: u64 = 15_000;
/// An officer looks for somebody to invite this often (random in the range).
pub const RECRUIT_EVERY_MS: (u64, u64) = (25_000, 45_000);
/// The duty looks at the guild this often (random in the range).
pub const LIFE_EVERY_MS: (u64, u64) = (6_000, 10_000);
/// The owner looks for an officer to appoint this often.
pub const PROMOTE_EVERY_MS: u64 = 20_000;
/// A member is not made an officer before it has been in the guild this long.
pub const OFFICER_MIN_AGE_MS: u64 = 30_000;
/// A bot stays in a guild at least this long before it may leave.
pub const MIN_STAY_MS: u64 = 15 * 60_000;
/// The chance to leave is rolled this often.
pub const CHURN_EVERY_MS: u64 = 60_000;
/// A hand-over or a leave that the server does not act on is given up after this long.
pub const LEAVE_WAIT_MS: u64 = 8_000;
/// A bot that left a guild is not invited into another for this long.
pub const REJOIN_PAUSE_MS: u64 = 90_000;
/// A newcomer is greeted at the earliest after this long ...
pub const GREET_AFTER_MS: u64 = 3_000;
/// ... and not any more after this long.
pub const GREET_UNTIL_MS: u64 = 90_000;
/// A bot that answers an invitation waits this long first (random in the range).
pub const ANSWER_DELAY_MS: (u64, u64) = (1_500, 5_000);
/// An invitation older than this is not answered (the server's lasts 60 s).
pub const INVITE_STALE_MS: u64 = 55_000;
/// A bot joined and sent "join" waits this long for the guild before it reports the server.
pub const JOIN_WAIT_MS: u64 = 15_000;
/// An appointment as officer is expected to show within this long.
pub const PROMOTE_WAIT_MS: u64 = 10_000;

/// The hand-over and leave of a bot that goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// The owner hands the guild to `to` first.
    Handover {
        /// Character id of the new owner.
        to: u32,
        /// Bot clock when the request went out.
        sent_ms: u64,
    },
    /// The request to leave is out.
    Leaving {
        /// Bot clock when the request went out.
        sent_ms: u64,
        /// The guild that is being left.
        guild: u32,
    },
}

/// An answer to an invitation, due at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// Bot clock at which the answer goes out.
    pub due_ms: u64,
    /// Join (true) or decline.
    pub join: bool,
    /// The guild invited to.
    pub guild: u32,
    /// The inviter's character id.
    pub inviter: u32,
}

/// A real player who asked for an invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The player's character name.
    pub name: Box<str>,
    /// Bot clock at which the answer goes out.
    pub due_ms: u64,
}

/// A line of guild chat waiting for its moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// Bot clock at which it is said.
    pub due_ms: u64,
    /// The text.
    pub text: Box<str>,
}

/// The duty's clocks and plans (bot clock, milliseconds). All zero at the start.
#[derive(Debug, Default)]
pub struct Timers {
    /// Next publication of the bot's card.
    pub next_card_ms: u64,
    /// What the last card said (guild, flags, level), to publish at once when it changes.
    pub card_key: (u32, u8, u8),
    /// The log version the book was told about.
    pub book_version: u32,
    /// The log version the web snapshot was built from (plus one: 0 is "never").
    pub web_version: u32,
    /// The answer to an invitation.
    pub reply: Option<Reply>,
    /// A join answer was sent for this guild at this time and the guild must follow.
    pub join_sent: Option<(u32, u64)>,
    /// Next look at the guild.
    pub next_life_ms: u64,
    /// Next look for somebody to invite.
    pub next_recruit_ms: u64,
    /// Next look for an officer to appoint.
    pub next_promote_ms: u64,
    /// An appointment that was sent: (character id, bot clock).
    pub promote_sent: Option<(u32, u64)>,
    /// Next roll of the chance to leave.
    pub next_churn_ms: u64,
    /// Not before this bot clock may the bot leave (after a failed hand-over).
    pub churn_block_until_ms: u64,
    /// The hand-over / leave under way.
    pub leave: Option<Leave>,
    /// A line to say.
    pub say: Option<Due>,
    /// Next small-talk line.
    pub next_talk_ms: u64,
    /// The bot said its thanks for the invitation.
    pub thanked: bool,
    /// Bot clock at which a thank-you for the invitation is due.
    pub thanks_due: Option<u64>,
    /// The founder said its welcome.
    pub welcomed: bool,
    /// Real players who asked for an invitation.
    pub requests: SmallVec<[Request; 2]>,
    /// Players who asked lately, to answer each once in a while: (name hash, bot clock).
    pub asked: SmallVec<[(u64, u64); 4]>,
    /// The goal `guild` posted a wish at this bot clock (0 never).
    pub wished_ms: u64,
    /// The bot is in a founding team (the goal `guild` runs with an order).
    pub founding: bool,
    /// Founding is blocked until this bot clock (a failed founding rests).
    pub found_block_until_ms: u64,
    /// The bot may be invited again after this bot clock (it left a guild).
    pub rejoin_after_ms: u64,
    /// Anti-repeat memory of the guild lines.
    pub recent: super::chat::Recent,
    /// Anti-repeat memory of the answers to real players.
    pub reply_recent: crate::bot::goals::social::canned::Recent,
    /// What real players said in guild chat, to be answered by one bot: (who, text, key, at).
    pub heard: SmallVec<[Heard; 2]>,
}

/// A guild chat line of a real player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    /// Sender name.
    pub who: Box<str>,
    /// Text.
    pub text: Box<str>,
    /// Bot clock.
    pub at_ms: u64,
}

/// What the decisions look at.
pub struct View<'a> {
    /// The bot clock.
    pub now_ms: u64,
    /// The bot's character id.
    pub me: u32,
    /// The guild log.
    pub log: &'a GuildLog,
    /// Whether a character is one of our bots.
    pub is_bot: &'a dyn Fn(u32) -> bool,
}

/// How many officers a guild of `members` has: none below four members, one below eight, then two.
pub fn desired_officers(members: usize) -> usize {
    match members {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

impl View<'_> {
    /// Whether a bot owns the guild (the bots recruit and promote only in such a guild).
    pub fn bot_guild(&self) -> bool {
        self.log.owner != 0 && (self.is_bot)(self.log.owner)
    }

    fn me_owner(&self) -> bool {
        self.log.is_owner(self.me)
    }

    /// Whether the bot may recruit: it can invite in a bot guild that wants members.
    pub fn may_recruit(&self) -> bool {
        self.log.in_guild()
            && self.log.loaded
            && self.log.can_invite(self.me)
            && self.bot_guild()
            && self.log.members.len() < TARGET_SIZE
    }

    /// The member the owner makes an officer next: a bot that has been in the guild a while, the
    /// highest level first. `None` when the guild has the officers it wants or the bot is no
    /// owner.
    pub fn promotion(&self) -> Option<u32> {
        if !self.me_owner() || !self.log.loaded || !self.bot_guild() {
            return None;
        }
        let have = self.log.officers().count();
        if have >= desired_officers(self.log.members.len()) {
            return None;
        }
        self.log
            .members
            .iter()
            .filter(|m| {
                m.char_id != self.me
                    && m.role < role::OFFICER
                    && m.online
                    && (self.is_bot)(m.char_id)
                    && self.now_ms.saturating_sub(m.since_ms) >= OFFICER_MIN_AGE_MS
            })
            .max_by_key(|m| (m.level, std::cmp::Reverse(m.char_id)))
            .map(|m| m.char_id)
    }

    /// The member an owner who leaves hands the guild to: an online bot, an officer first, then
    /// the highest level. `None` when there is no bot to take it (then the owner stays).
    pub fn successor(&self) -> Option<u32> {
        let rank = |m: &&Member| {
            (
                m.role >= role::OFFICER && m.role != role::OWNER,
                m.level,
                std::cmp::Reverse(m.char_id),
            )
        };
        self.log
            .members
            .iter()
            .filter(|m| m.char_id != self.me && m.online && (self.is_bot)(m.char_id))
            .max_by_key(rank)
            .map(|m| m.char_id)
    }

    /// Whether the bot may leave now (before the chance is rolled): it has been in the guild a
    /// while, the guild is a bot guild with a few members, and an owner has a successor.
    pub fn may_leave(&self) -> bool {
        self.log.in_guild()
            && self.log.loaded
            && self.bot_guild()
            && self.log.members.len() >= 3
            && self.now_ms.saturating_sub(self.log.joined_ms) >= MIN_STAY_MS
            && (!self.me_owner() || self.successor().is_some())
    }

    /// The newcomer a greeter says hello to: the oldest one still fresh. Only an owner or an
    /// officer greets.
    pub fn newcomer_to_greet(&self) -> Option<&Member> {
        if !self.log.can_invite(self.me) {
            return None;
        }
        self.log
            .newcomers
            .iter()
            .find(|n| {
                let age = self.now_ms.saturating_sub(n.at_ms);
                (GREET_AFTER_MS..=GREET_UNTIL_MS).contains(&age)
            })
            .and_then(|n| self.log.member(n.char_id))
    }

    /// Whether a real player (a member that is not one of our bots) is online in the guild.
    pub fn real_member_online(&self) -> bool {
        self.log
            .members
            .iter()
            .any(|m| m.online && m.char_id != self.me && !(self.is_bot)(m.char_id))
    }

    /// An online officer or owner other than the bot, by name (for "ask ...").
    pub fn inviter_other_than_me(&self) -> Option<&Member> {
        self.log
            .members
            .iter()
            .find(|m| m.online && m.char_id != self.me && role::can_invite(m.role))
    }
}

/// A random time in `range` milliseconds.
pub fn jitter(rng: &mut fastrand::Rng, (lo, hi): (u64, u64)) -> u64 {
    if hi <= lo {
        lo
    } else {
        lo + rng.u64(0..hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::guild::log::{GuildLog, Newcomer};

    const ME: u32 = 1;

    fn member(char_id: u32, role: u8, level: u8, online: bool, since_ms: u64) -> Member {
        Member {
            char_id,
            name: format!("M{char_id}").into(),
            level,
            role,
            online,
            since_ms,
        }
    }

    fn log(members: Vec<Member>, owner: u32) -> GuildLog {
        GuildLog {
            id: 5,
            name: "Guild".into(),
            owner,
            members,
            loaded: true,
            joined_ms: 1,
            ..GuildLog::default()
        }
    }

    /// Characters below 100 are bots.
    fn bots(c: u32) -> bool {
        c < 100
    }

    fn view(log: &GuildLog, me: u32, now_ms: u64) -> View<'_> {
        View {
            now_ms,
            me,
            log,
            is_bot: &bots,
        }
    }

    #[test]
    fn officers_are_wanted_by_the_size_of_the_guild() {
        assert_eq!(desired_officers(1), 0);
        assert_eq!(desired_officers(3), 0);
        assert_eq!(desired_officers(4), 1);
        assert_eq!(desired_officers(7), 1);
        assert_eq!(desired_officers(8), 2);
        assert_eq!(desired_officers(20), 2);
    }

    #[test]
    fn the_owner_appoints_the_best_bot_that_has_been_there_a_while() {
        let l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::APPRENTICE, 12, true, 0),
                member(3, role::APPRENTICE, 15, true, 0),
                member(4, role::APPRENTICE, 18, true, 90_000), // just joined
                member(150, role::APPRENTICE, 40, true, 0),    // a real player
                member(6, role::APPRENTICE, 30, false, 0),     // offline
            ],
            ME,
        );
        assert_eq!(view(&l, ME, 100_000).promotion(), Some(3));
        // a guild below four members has no officers
        let small = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::APPRENTICE, 12, true, 0),
            ],
            ME,
        );
        assert_eq!(view(&small, ME, 100_000).promotion(), None);
    }

    #[test]
    fn nobody_but_the_owner_appoints_and_the_wanted_number_is_not_exceeded() {
        let mut ms = vec![
            member(ME, role::OWNER, 20, true, 0),
            member(2, role::OFFICER, 12, true, 0),
        ];
        for i in 3..=6 {
            ms.push(member(i, role::APPRENTICE, 10 + i as u8, true, 0));
        }
        let l = log(ms, ME);
        // six members want one officer, and there is one
        assert_eq!(view(&l, ME, 100_000).promotion(), None);
        // an officer does not appoint
        assert_eq!(view(&l, 2, 100_000).promotion(), None);
        // eight members want two
        let mut ms = l.members.clone();
        ms.push(member(7, role::MEMBER, 30, true, 0));
        ms.push(member(8, role::MEMBER, 11, true, 0));
        let l = log(ms, ME);
        assert_eq!(view(&l, ME, 100_000).promotion(), Some(7));
        // a guild with a real player as owner is not ours to arrange
        let l = log(l.members.clone(), 150);
        assert_eq!(view(&l, ME, 100_000).promotion(), None);
    }

    #[test]
    fn recruiting_is_for_officers_of_bot_guilds_that_want_members() {
        let l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::OFFICER, 12, true, 0),
                member(3, role::MEMBER, 15, true, 0),
            ],
            ME,
        );
        assert!(view(&l, ME, 10).may_recruit());
        assert!(view(&l, 2, 10).may_recruit());
        assert!(!view(&l, 3, 10).may_recruit(), "a member cannot invite");
        // a guild of a real player
        let l2 = log(l.members.clone(), 150);
        assert!(!view(&l2, 2, 10).may_recruit());
        // a full guild
        let mut full = l.clone();
        for i in 10..(10 + TARGET_SIZE as u32) {
            full.members.push(member(i, role::MEMBER, 5, true, 0));
        }
        assert!(!view(&full, ME, 10).may_recruit());
        // the list has not come
        let mut waiting = l.clone();
        waiting.loaded = false;
        assert!(!view(&waiting, ME, 10).may_recruit());
        // no guild
        assert!(!view(&GuildLog::default(), ME, 10).may_recruit());
    }

    #[test]
    fn an_owner_hands_the_guild_to_an_online_bot_officers_first() {
        let l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::MEMBER, 30, true, 0),
                member(3, role::OFFICER, 12, true, 0),
                member(4, role::OFFICER, 14, false, 0),
                member(150, role::OFFICER, 50, true, 0),
            ],
            ME,
        );
        assert_eq!(view(&l, ME, 1).successor(), Some(3));
        // without officers, the highest level
        let l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::MEMBER, 30, true, 0),
                member(3, role::APPRENTICE, 12, true, 0),
            ],
            ME,
        );
        assert_eq!(view(&l, ME, 1).successor(), Some(2));
        // nobody but a real player and an absent bot: no successor
        let l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(4, role::MEMBER, 14, false, 0),
                member(150, role::MEMBER, 50, true, 0),
            ],
            ME,
        );
        assert_eq!(view(&l, ME, 1).successor(), None);
    }

    #[test]
    fn leaving_needs_time_in_the_guild_a_bot_guild_and_for_an_owner_a_successor() {
        let ms = vec![
            member(ME, role::OWNER, 20, true, 0),
            member(2, role::MEMBER, 30, true, 0),
            member(3, role::MEMBER, 12, true, 0),
        ];
        let l = log(ms.clone(), ME);
        assert!(!view(&l, 2, MIN_STAY_MS - 1).may_leave(), "too soon");
        assert!(view(&l, 2, MIN_STAY_MS + 5).may_leave());
        assert!(
            view(&l, ME, MIN_STAY_MS + 5).may_leave(),
            "the owner has a successor"
        );
        // an owner with nobody to hand the guild to stays
        let mut alone = ms.clone();
        alone[1].online = false;
        alone[2].online = false;
        let l = log(alone, ME);
        assert!(!view(&l, ME, MIN_STAY_MS + 5).may_leave());
        // a guild of two is not left
        let l = log(ms[..2].to_vec(), ME);
        assert!(!view(&l, 2, MIN_STAY_MS + 5).may_leave());
        // a guild a real player owns is not ours to leave by chance
        let l = log(ms, 150);
        assert!(!view(&l, 2, MIN_STAY_MS + 5).may_leave());
    }

    #[test]
    fn only_an_owner_or_officer_greets_and_only_a_fresh_newcomer() {
        let mut l = log(
            vec![
                member(ME, role::OWNER, 20, true, 0),
                member(2, role::MEMBER, 30, true, 0),
                member(7, role::APPRENTICE, 5, true, 1000),
            ],
            ME,
        );
        l.newcomers.push(Newcomer {
            char_id: 7,
            at_ms: 1000,
        });
        assert!(view(&l, ME, 1000 + GREET_AFTER_MS - 1)
            .newcomer_to_greet()
            .is_none());
        assert_eq!(
            view(&l, ME, 1000 + GREET_AFTER_MS)
                .newcomer_to_greet()
                .map(|m| m.char_id),
            Some(7)
        );
        assert!(view(&l, ME, 1000 + GREET_UNTIL_MS + 1)
            .newcomer_to_greet()
            .is_none());
        assert!(view(&l, 2, 1000 + GREET_AFTER_MS)
            .newcomer_to_greet()
            .is_none());
    }

    #[test]
    fn real_members_and_the_other_inviter_are_found() {
        let l = log(
            vec![
                member(ME, role::OFFICER, 20, true, 0),
                member(2, role::OWNER, 30, true, 0),
                member(3, role::MEMBER, 12, true, 0),
            ],
            2,
        );
        let v = view(&l, ME, 1);
        assert!(!v.real_member_online());
        assert_eq!(v.inviter_other_than_me().map(|m| m.char_id), Some(2));
        let mut l = l;
        l.members.push(member(150, role::MEMBER, 50, true, 0));
        assert!(view(&l, ME, 1).real_member_online());
        l.members[3].online = false;
        assert!(!view(&l, ME, 1).real_member_online());
    }

    #[test]
    fn the_jitter_stays_in_the_range() {
        let mut rng = fastrand::Rng::with_seed(5);
        for _ in 0..500 {
            let v = jitter(&mut rng, LIFE_EVERY_MS);
            assert!((LIFE_EVERY_MS.0..LIFE_EVERY_MS.1).contains(&v));
        }
        assert_eq!(jitter(&mut rng, (5, 5)), 5);
    }
}
