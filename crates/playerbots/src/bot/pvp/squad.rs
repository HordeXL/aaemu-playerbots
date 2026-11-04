//! Ambush squads: the coordinator's side.
//!
//! A bot that waits at its ambush post offers itself with a [`Standby`] (again every
//! [`REPEAT_MS`] while it waits and has no squad). The coordinator keeps the offers for
//! [`STANDBY_TTL`] and, whenever one arrives or its sweep runs, asks [`plan`] for squads: bots of
//! the same alliance that stand within [`NEAR_M`] of each other, up to the `[pvp] max_squad` the
//! first of them asked for. Every member gets a [`PartyOrder`](crate::bot::party::PartyOrder)
//! with the purpose [`PartyPurpose::Ambush`](crate::bot::party::PartyPurpose) (the post of the
//! first), which the brain turns into a new gank goal that knows its squad. The squad needs no
//! messages afterwards: all its members choose the same target by the same rule
//! ([`pick_target`]), and the one that lands the killing blow collects the pack.

use std::time::Duration;

use smallvec::SmallVec;

use crate::bot::party::MAX_PARTY;

/// A standby offer stays with the coordinator this long.
pub const STANDBY_TTL: Duration = Duration::from_secs(75);
/// A waiting bot repeats its offer this often (bot clock, ms).
pub const REPEAT_MS: u64 = 25_000;
/// Bots farther apart than this do not form a squad (metres).
pub const NEAR_M: f32 = 500.0;
/// Levels may differ by this much in a squad.
pub const LEVEL_DIFF: u8 = 12;
/// The most members of a squad (`PartyOrder` holds [`MAX_PARTY`]).
pub const MAX_SQUAD: usize = MAX_PARTY;

/// A bot waiting at an ambush post offers itself to a squad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Standby {
    /// The bot.
    pub bot: u32,
    /// Where it stands (x, y).
    pub at: (f32, f32),
    /// Its alliance (mother faction).
    pub alliance: u32,
    /// Its level.
    pub level: u8,
    /// The `[pvp] max_squad` it was configured with.
    pub max_squad: u8,
}

/// A squad the coordinator formed.
#[derive(Debug, Clone, PartialEq)]
pub struct Squad {
    /// Bot ids, the leader (the longest waiting) first.
    pub members: SmallVec<[u32; MAX_SQUAD]>,
    /// Where the squad lies in wait (the leader's place).
    pub post: (f32, f32),
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// Forms squads out of `standing` (longest waiting first): the first bot is the seed, the nearest
/// bots of its alliance and level range within [`NEAR_M`] join it, up to the seed's `max_squad`.
/// A seed that finds no one stays on the list. Returns the squads and the indices of the offers
/// they used.
pub fn plan(standing: &[Standby]) -> (Vec<Squad>, Vec<usize>) {
    let mut taken = vec![false; standing.len()];
    let mut squads = Vec::new();
    for (i, seed) in standing.iter().enumerate() {
        if taken[i] || seed.max_squad < 2 {
            continue;
        }
        let want = usize::from(seed.max_squad).min(MAX_SQUAD);
        let mut near: SmallVec<[(f32, usize); 8]> = standing
            .iter()
            .enumerate()
            .filter(|&(j, o)| {
                j != i
                    && !taken[j]
                    && o.alliance == seed.alliance
                    && o.bot != seed.bot
                    && o.max_squad >= 2
                    && o.level.abs_diff(seed.level) <= LEVEL_DIFF
                    && dist(o.at, seed.at) <= NEAR_M
            })
            .map(|(j, o)| (dist(o.at, seed.at), j))
            .collect();
        if near.is_empty() {
            continue;
        }
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.truncate(want - 1);
        let mut members: SmallVec<[u32; MAX_SQUAD]> = SmallVec::new();
        members.push(seed.bot);
        taken[i] = true;
        for &(_, j) in &near {
            members.push(standing[j].bot);
            taken[j] = true;
        }
        squads.push(Squad {
            members,
            post: seed.at,
        });
    }
    let used = taken
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.then_some(i))
        .collect();
    (squads, used)
}

/// The carrier a squad member goes for: the nearest to the squad's post, ties by object id, so
/// that every member that sees the same carriers picks the same one. `cands` are
/// `(object id, distance to the bot, distance to the post)`.
pub fn pick_target(cands: &[(u32, f32, f32)]) -> Option<u32> {
    cands
        .iter()
        .min_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)))
        .map(|c| c.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(bot: u32, x: f32, y: f32, alliance: u32, level: u8, max: u8) -> Standby {
        Standby {
            bot,
            at: (x, y),
            alliance,
            level,
            max_squad: max,
        }
    }

    #[test]
    fn bots_of_one_alliance_close_together_form_a_squad() {
        let s = [
            sb(1, 0.0, 0.0, 148, 30, 3),
            sb(2, 50.0, 0.0, 148, 31, 3),
            sb(3, 20.0, 20.0, 148, 29, 3),
            sb(4, 30.0, 0.0, 148, 30, 3),
        ];
        let (squads, used) = plan(&s);
        assert_eq!(squads.len(), 1);
        // the seed first, then the nearest two (max_squad 3)
        assert_eq!(&squads[0].members[..], [1, 3, 4]);
        assert_eq!(squads[0].post, (0.0, 0.0));
        assert_eq!(used, [0, 2, 3]);
    }

    #[test]
    fn a_squad_needs_two_and_the_same_alliance_and_the_same_place() {
        // alone
        assert!(plan(&[sb(1, 0.0, 0.0, 148, 30, 3)]).0.is_empty());
        // another alliance
        let s = [sb(1, 0.0, 0.0, 148, 30, 3), sb(2, 10.0, 0.0, 149, 30, 3)];
        assert!(plan(&s).0.is_empty());
        // too far
        let s = [
            sb(1, 0.0, 0.0, 148, 30, 3),
            sb(2, NEAR_M + 1.0, 0.0, 148, 30, 3),
        ];
        assert!(plan(&s).0.is_empty());
        // too different in level
        let s = [
            sb(1, 0.0, 0.0, 148, 30, 3),
            sb(2, 10.0, 0.0, 148, 30 + LEVEL_DIFF + 1, 3),
        ];
        assert!(plan(&s).0.is_empty());
        // the same bot offered twice is not its own partner
        let s = [sb(1, 0.0, 0.0, 148, 30, 3), sb(1, 10.0, 0.0, 148, 30, 3)];
        assert!(plan(&s).0.is_empty());
    }

    #[test]
    fn max_squad_one_means_no_squad() {
        let s = [sb(1, 0.0, 0.0, 148, 30, 1), sb(2, 10.0, 0.0, 148, 30, 1)];
        assert!(plan(&s).0.is_empty());
        // a bot that wants a squad is not joined by one that does not
        let s = [sb(1, 0.0, 0.0, 148, 30, 4), sb(2, 10.0, 0.0, 148, 30, 1)];
        assert!(plan(&s).0.is_empty());
    }

    #[test]
    fn a_big_crowd_makes_several_squads_of_at_most_max_squad() {
        let s: Vec<Standby> = (0..7)
            .map(|i| sb(i + 1, i as f32 * 5.0, 0.0, 149, 40, 3))
            .collect();
        let (squads, used) = plan(&s);
        assert_eq!(squads.len(), 2);
        assert!(squads.iter().all(|q| q.members.len() == 3));
        assert_eq!(used.len(), 6);
        // nobody is in two squads
        let mut all: Vec<u32> = squads
            .iter()
            .flat_map(|q| q.members.iter().copied())
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 6);
        // a request for more than the party limit is capped
        let s: Vec<Standby> = (0..9)
            .map(|i| sb(i + 1, i as f32, 0.0, 149, 40, 50))
            .collect();
        assert!(plan(&s).0.iter().all(|q| q.members.len() <= MAX_SQUAD));
    }

    #[test]
    fn the_squad_agrees_on_the_target() {
        // nearest to the post wins, whoever asks
        let from_a = [(10, 12.0, 30.0), (11, 8.0, 5.0)];
        let from_b = [(11, 40.0, 5.0), (10, 25.0, 30.0)];
        assert_eq!(pick_target(&from_a), Some(11));
        assert_eq!(pick_target(&from_b), Some(11));
        // a tie goes to the lower object id
        assert_eq!(pick_target(&[(9, 1.0, 5.0), (4, 1.0, 5.0)]), Some(4));
        assert_eq!(pick_target(&[]), None);
    }
}
