//! The coordinator's side of founding: the wishes of the bots that want to sign a charter and the
//! teams made of them.
//!
//! [`Wishes`] is a plain list the [`Coordinator`](crate::bot::coordinator::Coordinator) keeps
//! beside its dungeon wishes. Whenever a wish arrives or the sweep runs, [`Wishes::form`] drops the
//! wishes of bots that went away or joined a guild (the [`GuildBook`] knows), asks
//! [`plan::plan`] for a team of the configured size, reserves a place for the new guild in the
//! book (`[guild] max_guilds` counts the guilds that exist and the foundings under way) and hands
//! the team back; the coordinator then orders every member.

use std::time::Instant;

use smallvec::SmallVec;

use super::book::GuildBook;
use super::plan::{self, GuildWish, MAX_TEAM, WISH_TTL};

/// A team that signs a charter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    /// The coordinator's id of the team (also the id of the reservation in the book).
    pub party_id: u32,
    /// The bots, the founder first.
    pub members: SmallVec<[u32; MAX_TEAM]>,
}

/// The wishes for a founding team.
#[derive(Debug, Default)]
pub struct Wishes {
    list: Vec<(GuildWish, Instant)>,
    size: usize,
    max_guilds: u32,
}

impl Wishes {
    /// A list that forms teams of `party_size` bots while the bots run fewer than `max_guilds`
    /// guilds.
    pub fn new(party_size: u8, max_guilds: u32) -> Wishes {
        Wishes {
            list: Vec::new(),
            size: plan::team_size(party_size),
            max_guilds,
        }
    }

    /// The bots that wait for a team.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether nobody waits.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Adds a wish (a bot has one at a time).
    pub fn add(&mut self, wish: GuildWish, now: Instant) {
        self.list.retain(|w| w.0.bot != wish.bot);
        self.list.push((wish, now));
    }

    /// Withdraws the wish of a bot.
    pub fn cancel(&mut self, bot: u32) {
        self.list.retain(|w| w.0.bot != bot);
    }

    /// Drops the wishes that waited longer than [`WISH_TTL`].
    pub fn expire(&mut self, now: Instant) {
        self.list
            .retain(|w| now.saturating_duration_since(w.1) < WISH_TTL);
    }

    /// Forms one team if the wishes allow it: `party_id` is the id the team gets, `unix_ms` the
    /// time of the book. The members leave the list and a place for their guild is reserved.
    pub fn form(
        &mut self,
        now: Instant,
        book: &GuildBook,
        unix_ms: u64,
        party_id: u32,
    ) -> Option<Team> {
        if self.list.len() < self.size {
            return None;
        }
        // (a bot that is gone, joined a guild or changed faction no longer wishes)
        self.list.retain(|(w, _)| {
            book.card(w.bot, unix_ms)
                .is_some_and(|c| c.guild == 0 && c.mother == w.mother)
        });
        // (one that cannot sign right now keeps its place but is not asked)
        let mut index: SmallVec<[usize; 16]> = SmallVec::new();
        let mut wishes: SmallVec<[(GuildWish, u64); 16]> = SmallVec::new();
        for (i, (w, since)) in self.list.iter().enumerate() {
            if book.card(w.bot, unix_ms).is_some_and(|c| c.can_found()) {
                index.push(i);
                wishes.push((*w, now.saturating_duration_since(*since).as_secs()));
            }
        }
        let picked = plan::plan(&wishes, self.size)?;
        if !book.reserve(party_id, self.max_guilds, unix_ms) {
            return None;
        }
        let members: SmallVec<[u32; MAX_TEAM]> = picked.iter().map(|&p| wishes[p].0.bot).collect();
        let mut gone: SmallVec<[usize; MAX_TEAM]> = picked.iter().map(|&p| index[p]).collect();
        gone.sort_unstable_by(|a, b| b.cmp(a));
        for i in gone {
            self.list.remove(i);
        }
        Some(Team { party_id, members })
    }
}

#[cfg(test)]
mod tests {
    use super::super::book::{Card, Membership, CAN_FOUND, CAN_JOIN, WISHING};
    use super::*;

    const NOW: u64 = 1_000_000;

    fn card(bot: u32, mother: u32, level: u8, flags: u8, guild: u32) -> Card {
        Card {
            bot,
            char_id: 100 + bot,
            name: format!("Bot{bot}").into(),
            mother,
            level,
            guild,
            flags,
            seen_ms: 0,
        }
    }

    fn wish(bot: u32, mother: u32, level: u8) -> GuildWish {
        GuildWish {
            bot,
            mother,
            level,
            can_pay: true,
        }
    }

    /// Bots 1..=n of one faction, all wishing and able.
    fn room(n: u32, book: &GuildBook, w: &mut Wishes, now: Instant) {
        for b in 1..=n {
            book.update_card(
                card(b, 7, 10 + b as u8, CAN_JOIN | CAN_FOUND | WISHING, 0),
                NOW,
            );
            w.add(wish(b, 7, 10 + b as u8), now);
        }
    }

    #[test]
    fn a_team_forms_when_enough_able_bots_wait_and_the_founder_is_the_highest_level() {
        let (book, now) = (GuildBook::default(), Instant::now());
        let mut w = Wishes::new(5, 3);
        room(4, &book, &mut w, now);
        assert!(w.form(now, &book, NOW, 1).is_none(), "four are too few");
        room(5, &book, &mut w, now);
        let t = w.form(now, &book, NOW, 1).expect("a team");
        assert_eq!(t.party_id, 1);
        assert_eq!(t.members[0], 5, "the highest level founds");
        let mut all: Vec<u32> = t.members.to_vec();
        all.sort_unstable();
        assert_eq!(all, [1, 2, 3, 4, 5]);
        assert!(w.is_empty());
        assert_eq!(book.foundings(), 1, "a place for the guild is reserved");
    }

    #[test]
    fn the_guild_limit_holds_the_wishes_back() {
        let (book, now) = (GuildBook::default(), Instant::now());
        let mut w = Wishes::new(2, 1);
        room(4, &book, &mut w, now);
        assert!(w.form(now, &book, NOW, 1).is_some());
        // one guild is being founded: the next team has to wait
        assert!(w.form(now, &book, NOW, 2).is_none());
        assert_eq!(w.len(), 2, "the wishes stay");
        // the founding ends without a guild: the place is free again
        book.release(1);
        assert!(w.form(now, &book, NOW, 3).is_some());
        // and a guild that exists keeps the place
        let mut w = Wishes::new(2, 1);
        let book = GuildBook::default();
        room(2, &book, &mut w, now);
        book.note_member(
            9,
            Some(Membership {
                id: 4,
                name: "Iron Pilgrims".into(),
                mother: 7,
            }),
        );
        assert!(w.form(now, &book, NOW, 1).is_none());
    }

    #[test]
    fn bots_that_left_joined_a_guild_or_cannot_sign_are_not_asked() {
        let (book, now) = (GuildBook::default(), Instant::now());
        let mut w = Wishes::new(2, 3);
        room(4, &book, &mut w, now);
        // 1 joined a guild (dropped), 2 is busy (kept, not asked), 3 left (stale card, dropped)
        book.update_card(card(1, 7, 11, CAN_JOIN, 5), NOW);
        book.update_card(card(2, 7, 12, CAN_JOIN | WISHING, 0), NOW);
        book.update_card(card(3, 7, 13, CAN_JOIN | CAN_FOUND, 0), NOW - 60_000);
        let t = w.form(now, &book, NOW, 1);
        assert!(t.is_none(), "only bot 4 is left to sign");
        assert_eq!(w.len(), 2, "bots 2 and 4 keep their place");
        // 2 is ready again: the pair forms
        book.update_card(card(2, 7, 12, CAN_JOIN | CAN_FOUND, 0), NOW);
        let t = w.form(now, &book, NOW, 2).expect("a team");
        let mut m = t.members.to_vec();
        m.sort_unstable();
        assert_eq!(m, [2, 4]);
    }

    #[test]
    fn a_team_is_of_one_faction() {
        let (book, now) = (GuildBook::default(), Instant::now());
        let mut w = Wishes::new(2, 3);
        for (b, mother) in [(1, 7), (2, 8), (3, 7)] {
            book.update_card(card(b, mother, 10, CAN_JOIN | CAN_FOUND, 0), NOW);
            w.add(wish(b, mother, 10), now);
        }
        let t = w.form(now, &book, NOW, 1).expect("a team");
        let mut m = t.members.to_vec();
        m.sort_unstable();
        assert_eq!(m, [1, 3]);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn wishes_are_replaced_cancelled_and_expire() {
        let (book, now) = (GuildBook::default(), Instant::now());
        let mut w = Wishes::new(2, 3);
        w.add(wish(1, 7, 10), now);
        w.add(wish(1, 7, 11), now);
        assert_eq!(w.len(), 1);
        w.add(wish(2, 7, 10), now);
        w.cancel(1);
        assert_eq!(w.len(), 1);
        w.expire(now + WISH_TTL - std::time::Duration::from_secs(1));
        assert_eq!(w.len(), 1);
        w.expire(now + WISH_TTL);
        assert!(w.is_empty());
        assert!(w.form(now, &book, NOW, 1).is_none());
    }
}
