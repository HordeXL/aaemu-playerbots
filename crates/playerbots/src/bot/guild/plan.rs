//! The founding team: which bots sign a charter together.
//!
//! A bot that wants to found a guild sends the coordinator a [`GuildWish`]. The coordinator keeps
//! them for [`WISH_TTL`] and asks [`plan`] for a team: `size` wishes of one mother faction
//! (the server refuses a team with two), the one that waited longest first, and among them a
//! founder, who must be able to pay (the server charges the founder only) and, of those, has the
//! highest level. Nothing here looks at the bots' positions: a team is formed with invitations by
//! name and the server checks no distance.

use smallvec::SmallVec;

/// How long a wish waits for a team.
pub const WISH_TTL: std::time::Duration = std::time::Duration::from_secs(20 * 60);
/// The most members of a founding team (a party holds five).
pub const MAX_TEAM: usize = 5;

/// A bot that wants to found a guild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuildWish {
    /// The bot.
    pub bot: u32,
    /// Its mother faction.
    pub mother: u32,
    /// Its level.
    pub level: u8,
    /// It can pay the founding cost (it has the copper, or may mint it).
    pub can_pay: bool,
}

/// The size of the founding team for `[guild] party_size`: a party holds five, so more is five.
pub fn team_size(party_size: u8) -> usize {
    usize::from(party_size).clamp(1, MAX_TEAM)
}

/// Chooses a team out of `wishes` (each with the seconds it has waited): the indices of the
/// members, the founder first. `None` when no faction has `size` wishes with a bot that can pay.
pub fn plan(wishes: &[(GuildWish, u64)], size: usize) -> Option<SmallVec<[usize; MAX_TEAM]>> {
    if size == 0 || wishes.len() < size {
        return None;
    }
    // the factions in the order of their longest-waiting wish
    let mut order: SmallVec<[usize; 16]> = (0..wishes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(wishes[i].1));
    let mut tried: SmallVec<[u32; 4]> = SmallVec::new();
    for &seed in &order {
        let mother = wishes[seed].0.mother;
        if tried.contains(&mother) {
            continue;
        }
        tried.push(mother);
        let group: SmallVec<[usize; 16]> = order
            .iter()
            .copied()
            .filter(|&i| wishes[i].0.mother == mother)
            .collect();
        if group.len() < size {
            continue;
        }
        // the longest-waiting `size`; if none of them can pay, the first who can replaces the
        // last one
        let mut team: SmallVec<[usize; MAX_TEAM]> = group.iter().copied().take(size).collect();
        if !team.iter().any(|&i| wishes[i].0.can_pay) {
            let Some(payer) = group.iter().copied().find(|&i| wishes[i].0.can_pay) else {
                continue;
            };
            team.pop();
            team.push(payer);
        }
        // the founder: the highest level of those who can pay (the stable sort keeps the one that
        // waited longest first on a tie)
        let founder = team
            .iter()
            .copied()
            .filter(|&i| wishes[i].0.can_pay)
            .max_by_key(|&i| (wishes[i].0.level, std::cmp::Reverse(wishes[i].1)))?;
        team.retain(|i| *i != founder);
        team.insert(0, founder);
        return Some(team);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wish(bot: u32, mother: u32, level: u8, can_pay: bool, waited: u64) -> (GuildWish, u64) {
        (
            GuildWish {
                bot,
                mother,
                level,
                can_pay,
            },
            waited,
        )
    }

    fn bots(wishes: &[(GuildWish, u64)], team: &[usize]) -> Vec<u32> {
        team.iter().map(|&i| wishes[i].0.bot).collect()
    }

    #[test]
    fn a_party_holds_five() {
        assert_eq!(team_size(5), 5);
        assert_eq!(team_size(3), 3);
        assert_eq!(team_size(50), 5);
        assert_eq!(team_size(0), 1);
    }

    #[test]
    fn five_of_one_faction_found_together_with_the_richest_highest_level_first() {
        let w = [
            wish(1, 101, 10, true, 50),
            wish(2, 101, 14, true, 40),
            wish(3, 101, 12, false, 30),
            wish(4, 101, 11, true, 20),
            wish(5, 101, 9, true, 10),
        ];
        let team = plan(&w, 5).unwrap();
        assert_eq!(team.len(), 5);
        assert_eq!(
            bots(&w, &team)[0],
            2,
            "the highest level that can pay leads"
        );
        let mut all = bots(&w, &team);
        all.sort_unstable();
        assert_eq!(all, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn two_factions_never_share_a_team() {
        let w = [
            wish(1, 101, 10, true, 50),
            wish(2, 115, 10, true, 49),
            wish(3, 101, 10, true, 48),
            wish(4, 115, 10, true, 47),
            wish(5, 101, 10, true, 46),
        ];
        assert!(plan(&w, 5).is_none());
        // with three of each, a team of three forms in the faction that waited longest
        let team = plan(&w, 3).unwrap();
        let b = bots(&w, &team);
        assert!(b.iter().all(|x| x % 2 == 1), "{b:?}");
    }

    #[test]
    fn the_team_needs_somebody_who_can_pay() {
        let w = [
            wish(1, 101, 10, false, 50),
            wish(2, 101, 10, false, 40),
            wish(3, 101, 10, false, 30),
        ];
        assert!(plan(&w, 3).is_none());
        // a payer who waited the least takes the place of the one who waited the least in the team
        let w = [
            wish(1, 101, 10, false, 50),
            wish(2, 101, 10, false, 40),
            wish(3, 101, 10, false, 30),
            wish(4, 101, 10, true, 5),
        ];
        let team = plan(&w, 3).unwrap();
        let b = bots(&w, &team);
        assert_eq!(b[0], 4);
        assert!(b.contains(&1) && b.contains(&2) && !b.contains(&3), "{b:?}");
    }

    #[test]
    fn the_longest_waiting_are_taken_first() {
        let w = [
            wish(1, 101, 10, true, 5),
            wish(2, 101, 10, true, 500),
            wish(3, 101, 10, true, 400),
            wish(4, 101, 10, true, 300),
        ];
        let team = plan(&w, 3).unwrap();
        let mut b = bots(&w, &team);
        b.sort_unstable();
        assert_eq!(b, [2, 3, 4]);
    }

    #[test]
    fn too_few_wishes_or_a_size_of_zero_form_nothing() {
        let w = [wish(1, 101, 10, true, 5), wish(2, 101, 10, true, 5)];
        assert!(plan(&w, 3).is_none());
        assert!(plan(&w, 0).is_none());
        assert!(plan(&[], 1).is_none());
        // a team of one is allowed (a test setup with a server that asks for no team)
        assert_eq!(plan(&w, 1).unwrap().len(), 1);
    }
}
