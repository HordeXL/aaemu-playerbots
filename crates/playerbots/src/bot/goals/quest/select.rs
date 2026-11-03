//! Which quest to accept next: eligibility and ordering of the quests the givers around the bot
//! offer.
//!
//! A quest is acceptable when the bot is high enough (`quest level <= own level + 1`), has not
//! done it, has done the quests the data lists as prerequisites, the quest is not backing off,
//! its giver is not hostile to the bot, and the bot can do all of it
//! ([`objective::supported`](super::objective::supported)). Among acceptable quests the story
//! line goes first (by chapter, then position in the chapter), then the nearest giver.

use std::cmp::Ordering;

use aa_world::{Quest, QuestStart};

/// Levels above its own a bot still accepts quests for.
pub const LEVEL_SLACK: u8 = 1;
/// First search radius around the bot (metres).
pub const NEAR_RADIUS: f32 = 300.0;
/// Search radius when nothing is near.
pub const FAR_RADIUS: f32 = 1500.0;

/// Who offers a quest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Giver {
    /// An NPC template.
    Npc(u32),
    /// A doodad template.
    Doodad(u32),
}

/// The ways of starting `quest` that a bot walks to, as `Giver`s.
pub fn givers(quest: &Quest) -> impl Iterator<Item = Giver> + '_ {
    quest.starts.iter().filter_map(|s| match s {
        QuestStart::Npc(t) => Some(Giver::Npc(*t)),
        QuestStart::Doodad(t) => Some(Giver::Doodad(*t)),
        _ => None,
    })
}

/// `quest_contexts.category_id` of the seasonal event quests (Hallowtide and the like): their
/// objects only exist while the event runs, so the bot takes them last.
pub const EVENT_CATEGORY: u32 = aa_world::CATEGORY_EVENT;

/// A quest a giver near the bot offers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// Quest id.
    pub quest: u32,
    /// Who offers it.
    pub giver: Giver,
    /// Where the giver spawns (x, y).
    pub at: (f32, f32),
    /// Horizontal distance from the bot.
    pub dist: f32,
    /// A seasonal event quest ([`EVENT_CATEGORY`]); done only when nothing else is left.
    pub event: bool,
    /// Part of a story line.
    pub story: bool,
    /// Far below the bot's level ([`super::progress::trivial`]): done after everything else, and
    /// not at all when a better place is known.
    pub trivial: bool,
    /// Story chapter.
    pub chapter: u16,
    /// Position in the chapter.
    pub idx: u16,
}

/// Why a quest is not acceptable. Only used for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The bot is too low.
    Level,
    /// Already done.
    Done,
    /// Already in the journal.
    Active,
    /// A prerequisite is not done.
    Prerequisite,
    /// Backing off or quarantined.
    Resting,
    /// Starts by itself or by something a bot cannot do.
    NoGiver,
}

/// What the bot knows when it judges a quest.
pub struct Judge<'a> {
    /// The bot's level.
    pub level: u8,
    /// Whether a quest is completed.
    pub done: &'a dyn Fn(u32) -> bool,
    /// Whether a quest is in the journal.
    pub active: &'a dyn Fn(u32) -> bool,
    /// Whether a quest may be tried now (not backing off, not quarantined).
    pub usable: &'a dyn Fn(u32) -> bool,
}

impl Judge<'_> {
    /// Whether `quest` may be accepted, or why not. Everything that needs the world (giver
    /// faction, objectives) is checked by the caller.
    pub fn check(&self, quest: &Quest) -> Result<(), Skip> {
        if quest.level > self.level.saturating_add(LEVEL_SLACK) {
            return Err(Skip::Level);
        }
        if (self.active)(quest.id) {
            return Err(Skip::Active);
        }
        if (self.done)(quest.id) {
            return Err(Skip::Done);
        }
        if !(self.usable)(quest.id) {
            return Err(Skip::Resting);
        }
        if quest.after.iter().any(|q| !(self.done)(*q)) {
            return Err(Skip::Prerequisite);
        }
        if givers(quest).next().is_none() {
            return Err(Skip::NoGiver);
        }
        Ok(())
    }
}

/// Whether `a` should be accepted before `b`: regular quests before event quests, story quests
/// first (lowest chapter, then position), then quests worth the bot's time before trivial ones,
/// then the nearest giver, then the lowest quest id.
pub fn order(a: &Candidate, b: &Candidate) -> Ordering {
    a.event
        .cmp(&b.event)
        .then_with(|| b.story.cmp(&a.story))
        .then_with(|| {
            if a.story && b.story {
                (a.chapter, a.idx).cmp(&(b.chapter, b.idx))
            } else {
                Ordering::Equal
            }
        })
        .then_with(|| a.trivial.cmp(&b.trivial))
        .then_with(|| a.dist.total_cmp(&b.dist))
        .then_with(|| a.quest.cmp(&b.quest))
}

#[cfg(test)]
mod tests {
    use super::super::objective::tests::quest as make_quest;
    use super::*;

    fn cand(quest: u32, story: bool, chapter: u16, idx: u16, dist: f32) -> Candidate {
        Candidate {
            quest,
            giver: Giver::Npc(1),
            at: (0.0, 0.0),
            dist,
            event: false,
            story,
            trivial: false,
            chapter,
            idx,
        }
    }

    #[test]
    fn story_comes_first_then_the_nearest() {
        let near_side = cand(330, false, 0, 0, 32.0);
        let far_side = cand(251, false, 0, 0, 224.0);
        let story2 = cand(2532, true, 1, 2, 239.0);
        let story1 = cand(2531, true, 1, 1, 167.0);
        let mut v = [near_side, far_side, story2, story1];
        v.sort_by(order);
        assert_eq!(
            v.iter().map(|c| c.quest).collect::<Vec<_>>(),
            vec![2531, 2532, 330, 251]
        );
    }

    #[test]
    fn event_quests_come_last_even_when_nearer() {
        let event = Candidate {
            event: true,
            ..cand(8000004, false, 0, 0, 5.0)
        };
        let side = cand(251, false, 0, 0, 900.0);
        let story = cand(2531, true, 1, 1, 1200.0);
        let mut v = [event, side, story];
        v.sort_by(order);
        assert_eq!(
            v.iter().map(|c| c.quest).collect::<Vec<_>>(),
            vec![2531, 251, 8000004]
        );
    }

    #[test]
    fn trivial_quests_come_after_the_ones_worth_the_time() {
        let trivial_near = Candidate {
            trivial: true,
            ..cand(5, false, 0, 0, 10.0)
        };
        let useful_far = cand(6, false, 0, 0, 1200.0);
        let story_far = cand(2531, true, 1, 1, 1400.0);
        let mut v = [trivial_near, useful_far, story_far];
        v.sort_by(order);
        assert_eq!(
            v.iter().map(|c| c.quest).collect::<Vec<_>>(),
            vec![2531, 6, 5]
        );
        // among trivial quests the nearest still goes first
        let a = Candidate {
            trivial: true,
            ..cand(5, false, 0, 0, 10.0)
        };
        let b = Candidate {
            trivial: true,
            ..cand(6, false, 0, 0, 20.0)
        };
        assert_eq!(order(&a, &b), Ordering::Less);
    }

    #[test]
    fn equal_distances_fall_back_to_the_quest_id() {
        let a = cand(5, false, 0, 0, 10.0);
        let b = cand(4, false, 0, 0, 10.0);
        assert_eq!(order(&a, &b), Ordering::Greater);
    }

    fn judge<'a>(
        level: u8,
        done: &'a dyn Fn(u32) -> bool,
        active: &'a dyn Fn(u32) -> bool,
        usable: &'a dyn Fn(u32) -> bool,
    ) -> Judge<'a> {
        Judge {
            level,
            done,
            active,
            usable,
        }
    }

    #[test]
    fn eligibility_rules() {
        let none = |_: u32| false;
        let all = |_: u32| true;
        let mut q = make_quest(10, vec![]);
        q.level = 2;
        assert_eq!(judge(1, &none, &none, &all).check(&q), Ok(()));
        q.level = 3;
        assert_eq!(judge(1, &none, &none, &all).check(&q), Err(Skip::Level));
        assert_eq!(judge(2, &none, &none, &all).check(&q), Ok(()));
        q.level = 1;
        assert_eq!(judge(1, &all, &none, &all).check(&q), Err(Skip::Done));
        assert_eq!(judge(1, &none, &all, &all).check(&q), Err(Skip::Active));
        assert_eq!(judge(1, &none, &none, &none).check(&q), Err(Skip::Resting));
    }

    #[test]
    fn prerequisites_must_be_done() {
        let none = |_: u32| false;
        let all = |_: u32| true;
        let done_20 = |q: u32| q == 20;
        let mut q = make_quest(10, vec![]);
        q.after = Box::new([20, 21]);
        assert_eq!(
            judge(5, &done_20, &none, &all).check(&q),
            Err(Skip::Prerequisite)
        );
        let both = |q: u32| q == 20 || q == 21;
        assert_eq!(judge(5, &both, &none, &all).check(&q), Ok(()));
    }

    #[test]
    fn only_npc_and_doodad_starts_have_a_giver() {
        let none = |_: u32| false;
        let all = |_: u32| true;
        let mut q = make_quest(10, vec![]);
        q.starts = Box::new([QuestStart::Level(3), QuestStart::Sphere(4)]);
        assert_eq!(judge(5, &none, &none, &all).check(&q), Err(Skip::NoGiver));
        q.starts = Box::new([QuestStart::Sphere(4), QuestStart::Doodad(5047)]);
        assert_eq!(judge(5, &none, &none, &all).check(&q), Ok(()));
        assert_eq!(givers(&q).collect::<Vec<_>>(), vec![Giver::Doodad(5047)]);
    }
}
