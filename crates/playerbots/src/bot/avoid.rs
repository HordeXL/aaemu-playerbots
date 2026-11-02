//! Targets a bot gave up on and should leave alone for a good while.
//!
//! A monster the bot could not hurt (it stands on a ledge, behind a wall, evades, or every cast at
//! it is refused for a missing line of sight) must not be picked again the moment a short ignore
//! timer runs out: the bot would stand next to it, fighting nothing, for minutes. [`Avoid`] keeps
//! two bounded lists in [`BotState`](super::state::BotState): single units (by object id) and
//! whole kinds (by NPC template, after repeated failures against that kind). The grind goal and
//! the quest goal both read it when they choose a monster.

use smallvec::SmallVec;

/// A unit that could not be reached is left alone this long.
pub const UNREACHABLE_MS: u64 = 600_000;
/// Failures against one kind within [`KIND_WINDOW_MS`] after which the whole kind is avoided.
pub const KIND_STRIKES: u8 = 2;
/// Strikes older than this are forgotten.
pub const KIND_WINDOW_MS: u64 = 600_000;
/// A kind that earned its strikes is avoided this long.
pub const KIND_AVOID_MS: u64 = 600_000;
/// Most units remembered.
const MAX_OBJS: usize = 8;
/// Most kinds remembered.
const MAX_KINDS: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Kind {
    template: u32,
    strikes: u8,
    /// End of the strike window, or of the avoidance once the strikes are in.
    until_ms: u64,
}

/// What a bot avoids. See the module docs.
#[derive(Debug, Default, Clone)]
pub struct Avoid {
    objs: SmallVec<[(u32, u64); MAX_OBJS]>,
    kinds: SmallVec<[Kind; MAX_KINDS]>,
}

impl Avoid {
    /// Leaves unit `obj` alone until `until_ms`.
    pub fn ignore_obj(&mut self, obj: u32, until_ms: u64) {
        if let Some(e) = self.objs.iter_mut().find(|e| e.0 == obj) {
            e.1 = e.1.max(until_ms);
            return;
        }
        if self.objs.len() >= MAX_OBJS {
            // the one that expires first goes first
            if let Some(i) = self
                .objs
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.1)
                .map(|(i, _)| i)
            {
                self.objs.remove(i);
            }
        }
        self.objs.push((obj, until_ms));
    }

    /// Whether unit `obj` is left alone at `now_ms`.
    pub fn ignores(&self, obj: u32, now_ms: u64) -> bool {
        self.objs.iter().any(|e| e.0 == obj && now_ms < e.1)
    }

    /// The bot could not reach a monster of `template`: the second strike inside the window makes
    /// the bot avoid the whole kind. Returns whether the kind is avoided now.
    pub fn strike(&mut self, template: u32, now_ms: u64) -> bool {
        self.kinds.retain(|k| now_ms < k.until_ms);
        let i = match self.kinds.iter().position(|k| k.template == template) {
            Some(i) => i,
            None => {
                if self.kinds.len() >= MAX_KINDS {
                    self.kinds.remove(0);
                }
                self.kinds.push(Kind {
                    template,
                    strikes: 0,
                    until_ms: now_ms + KIND_WINDOW_MS,
                });
                self.kinds.len() - 1
            }
        };
        let k = &mut self.kinds[i];
        k.strikes = k.strikes.saturating_add(1);
        if k.strikes >= KIND_STRIKES {
            k.until_ms = now_ms + KIND_AVOID_MS;
        }
        k.strikes >= KIND_STRIKES
    }

    /// Whether monsters of `template` are avoided at `now_ms`.
    pub fn avoids_kind(&self, template: u32, now_ms: u64) -> bool {
        self.kinds
            .iter()
            .any(|k| k.template == template && k.strikes >= KIND_STRIKES && now_ms < k.until_ms)
    }

    /// Whether a monster `obj` of `template` should not be picked at `now_ms`.
    pub fn skips(&self, obj: u32, template: u32, now_ms: u64) -> bool {
        self.ignores(obj, now_ms) || self.avoids_kind(template, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreachable_unit_is_left_alone_until_its_time_is_up() {
        let mut a = Avoid::default();
        a.ignore_obj(7, 10_000);
        assert!(a.ignores(7, 9_999) && !a.ignores(7, 10_000));
        assert!(!a.ignores(8, 0));
        // a longer ignore wins, a shorter one does not shorten it
        a.ignore_obj(7, 20_000);
        a.ignore_obj(7, 12_000);
        assert!(a.ignores(7, 19_999));
    }

    #[test]
    fn units_are_bounded_and_the_oldest_ignore_goes_first() {
        let mut a = Avoid::default();
        for i in 0..20u32 {
            a.ignore_obj(i, 1_000 + u64::from(i));
        }
        assert_eq!(a.objs.len(), MAX_OBJS);
        assert!(a.ignores(19, 0), "the newest stays");
        assert!(!a.ignores(0, 0), "the one that expired first went");
    }

    #[test]
    fn the_second_strike_avoids_the_whole_kind_for_a_while() {
        let mut a = Avoid::default();
        assert!(!a.strike(55, 1_000), "one failure is bad luck");
        assert!(!a.avoids_kind(55, 1_000));
        assert!(a.strike(55, 5_000), "two are a pattern");
        assert!(a.avoids_kind(55, 5_001));
        assert!(a.skips(1, 55, 5_001), "any unit of that kind is skipped");
        assert!(!a.skips(1, 56, 5_001));
        assert!(
            !a.avoids_kind(55, 5_000 + KIND_AVOID_MS),
            "the avoidance runs out"
        );
    }

    #[test]
    fn strikes_expire_and_kinds_are_bounded() {
        let mut a = Avoid::default();
        a.strike(1, 0);
        assert!(
            !a.strike(1, KIND_WINDOW_MS + 1),
            "an old strike does not count"
        );
        for t in 100..120u32 {
            a.strike(t, 10);
        }
        assert!(a.kinds.len() <= MAX_KINDS);
    }
}
