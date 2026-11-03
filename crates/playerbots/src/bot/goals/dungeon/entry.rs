//! The server's refusals of a dungeon entry (`SCErrorMsg` codes) and what a bot does about each.
//!
//! The codes are `ErrorMessageType` values of the server's `systems/instances/worlds.rs`.

use crate::bot::party::ENTRY_WINDOW_MS;

/// `INSTANCE_LEVEL`: the level is outside the dungeon's range.
pub const CODE_LEVEL: i16 = 470;
/// `NEED_PARTY`: the dungeon wants a party.
pub const CODE_NEED_PARTY: i16 = 671;
/// `INSTANCE_QUOTA`: the dungeon is full.
pub const CODE_FULL: i16 = 471;
/// `INSTANCE_VISIT_LIMIT`: three entries per zone group per four hours are used up.
pub const CODE_VISIT_LIMIT: i16 = 453;
/// `ENTER_INST_REQ_ITEM`: the entry item is missing.
pub const CODE_REQ_ITEM: i16 = 632;
/// `NO_SERVER_INSTANCE_RESOURCE`: the server cannot make another instance (or the world has no
/// spawn point).
pub const CODE_NO_RESOURCE: i16 = 253;
/// `TRY_LATER_INSTANCE`: the player is already queued for the dungeon.
pub const CODE_TRY_LATER: i16 = 254;
/// `INVALID_RETURN_POS_INSTANCE`.
pub const CODE_BAD_RETURN: i16 = 257;
/// `PROHIBITED_IN_INSTANCE`.
pub const CODE_PROHIBITED: i16 = 258;
/// `ALREADY_UNBOUND_INSTANCE`.
pub const CODE_UNBOUND: i16 = 259;
/// `INSTANCE_LEAVE_PARTY`: the bot is not in the party the dungeon belongs to.
pub const CODE_LEAVE_PARTY: i16 = 472;
/// `CANNOT_USE_PORTAL_IN_TRIAL`.
pub const CODE_TRIAL: i16 = 611;

/// A refusal the server gives to a dungeon entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    /// The level is outside the dungeon's range.
    Level,
    /// The dungeon wants a party.
    NeedParty,
    /// The dungeon is full.
    Full,
    /// The entry limit is used up.
    VisitLimit,
    /// The entry item is missing.
    ReqItem,
    /// The server has no room for another instance, or no return point.
    NoResource,
    /// Queued already: the dungeon is still loading.
    TryLater,
    /// The bot is not part of the party the dungeon belongs to.
    LeaveParty,
    /// Something keeps the character out (a court case, the instance forbids the action).
    Prohibited,
}

impl EntryError {
    /// A short word for logs and issue keys.
    pub fn word(self) -> &'static str {
        match self {
            EntryError::Level => "level",
            EntryError::NeedParty => "need-party",
            EntryError::Full => "full",
            EntryError::VisitLimit => "visit-limit",
            EntryError::ReqItem => "required-item",
            EntryError::NoResource => "no-resource",
            EntryError::TryLater => "try-later",
            EntryError::LeaveParty => "leave-party",
            EntryError::Prohibited => "prohibited",
        }
    }
}

/// The refusal an `SCErrorMsg` code is, `None` for every code that has nothing to do with
/// entering a dungeon.
pub fn classify(code: i16) -> Option<EntryError> {
    match code {
        CODE_LEVEL => Some(EntryError::Level),
        CODE_NEED_PARTY => Some(EntryError::NeedParty),
        CODE_FULL => Some(EntryError::Full),
        CODE_VISIT_LIMIT => Some(EntryError::VisitLimit),
        CODE_REQ_ITEM => Some(EntryError::ReqItem),
        CODE_NO_RESOURCE | CODE_BAD_RETURN | CODE_UNBOUND => Some(EntryError::NoResource),
        CODE_TRY_LATER => Some(EntryError::TryLater),
        CODE_LEAVE_PARTY => Some(EntryError::LeaveParty),
        CODE_PROHIBITED | CODE_TRIAL => Some(EntryError::Prohibited),
        _ => None,
    }
}

/// What to do about a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Wait and cast again (the server is busy).
    Retry,
    /// The level is the problem: raise it (when the bot may) and cast again.
    RaiseLevel,
    /// Give up this dungeon and leave it alone for this long (`u64::MAX`: for good).
    GiveUp {
        /// Back-off in milliseconds.
        back_off_ms: u64,
    },
}

/// Back-off after a refusal that may pass soon.
const SOON_MS: u64 = 5 * 60_000;
/// Back-off after a refusal that needs the server to change.
const LONG_MS: u64 = 15 * 60_000;

/// The reaction to `e`.
pub fn reaction(e: EntryError) -> Reaction {
    match e {
        EntryError::TryLater | EntryError::Full => Reaction::Retry,
        EntryError::Level => Reaction::RaiseLevel,
        EntryError::VisitLimit => Reaction::GiveUp {
            back_off_ms: ENTRY_WINDOW_MS,
        },
        EntryError::ReqItem => Reaction::GiveUp {
            back_off_ms: u64::MAX,
        },
        EntryError::NoResource | EntryError::Prohibited => Reaction::GiveUp {
            back_off_ms: LONG_MS,
        },
        EntryError::NeedParty | EntryError::LeaveParty => Reaction::GiveUp {
            back_off_ms: SOON_MS,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_codes_are_classified() {
        assert_eq!(classify(470), Some(EntryError::Level));
        assert_eq!(classify(671), Some(EntryError::NeedParty));
        assert_eq!(classify(471), Some(EntryError::Full));
        assert_eq!(classify(453), Some(EntryError::VisitLimit));
        assert_eq!(classify(632), Some(EntryError::ReqItem));
        assert_eq!(classify(254), Some(EntryError::TryLater));
        assert_eq!(classify(253), Some(EntryError::NoResource));
        assert_eq!(classify(258), Some(EntryError::Prohibited));
        assert_eq!(classify(472), Some(EntryError::LeaveParty));
        // codes of other things (a refused skill, a full bag, a failed loot) are not entry errors
        for other in [0, 1, 73, 80, 100, 469, 473, 1000] {
            assert_eq!(classify(other), None, "{other}");
        }
    }

    #[test]
    fn each_refusal_has_a_reaction() {
        assert_eq!(reaction(EntryError::TryLater), Reaction::Retry);
        assert_eq!(reaction(EntryError::Full), Reaction::Retry);
        assert_eq!(reaction(EntryError::Level), Reaction::RaiseLevel);
        assert_eq!(
            reaction(EntryError::VisitLimit),
            Reaction::GiveUp {
                back_off_ms: ENTRY_WINDOW_MS
            }
        );
        assert_eq!(
            reaction(EntryError::ReqItem),
            Reaction::GiveUp {
                back_off_ms: u64::MAX
            }
        );
        for e in [
            EntryError::NoResource,
            EntryError::Prohibited,
            EntryError::NeedParty,
            EntryError::LeaveParty,
        ] {
            assert!(matches!(reaction(e), Reaction::GiveUp { .. }), "{e:?}");
        }
        // every refusal has a word for the logs
        for e in [
            EntryError::Level,
            EntryError::NeedParty,
            EntryError::Full,
            EntryError::VisitLimit,
            EntryError::ReqItem,
            EntryError::NoResource,
            EntryError::TryLater,
            EntryError::LeaveParty,
            EntryError::Prohibited,
        ] {
            assert!(!e.word().is_empty());
        }
    }
}
