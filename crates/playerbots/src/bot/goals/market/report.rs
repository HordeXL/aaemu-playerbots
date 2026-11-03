//! The reports the market goal files when the server does not do what the data and the protocol
//! promise (see [`crate::issues`]). Pure builders with stable keys: the same problem has the same
//! key whichever bot meets it.

use super::machine::Problem;
use crate::issues::{IssueKind, IssueReport, Severity, Suspect};

/// The server area of the doodad functions that move a character (the island's portals, exits).
const AREA_DOODAD_FUNCS: &str = "rust/crates/aaemu-game/src/systems/npcs/doodad/funcs/";
/// The server area of the spawn data (a doodad the data places that never shows).
const AREA_SPAWNS: &str = "rust/crates/aaemu-game/app/Data/Worlds/main_world/doodad_spawns.json";
/// The server area of player trades.
const AREA_TRADE: &str = "rust/crates/aaemu-game/src/systems/economy/trade.rs";
/// The server area of chat.
const AREA_CHAT: &str = "rust/crates/aaemu-game/src/systems/social/";

/// The server's refusal of a whisper to a hostile character.
pub const CODE_WHISPER_HOSTILE: i16 = 437;
/// The server's refusal of a whisper to an offline character.
pub const CODE_WHISPER_OFFLINE: i16 = 265;

/// What a trade was supposed to move, and what the bot found afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    /// The item traded.
    pub template: u32,
    /// Units the bag should have gained (positive) or lost (negative).
    pub want_items: i64,
    /// Units it did gain or lose.
    pub got_items: i64,
    /// Copper the purse should have gained or lost.
    pub want_money: i64,
    /// Copper it did gain or lose.
    pub got_money: i64,
}

impl Transfer {
    /// Whether the bag and the purse changed as the trade said (money may differ by the fee the
    /// server takes on none of these, so exactly; it may only be more for the receiver).
    pub fn is_complete(&self) -> bool {
        let items = if self.want_items >= 0 {
            self.got_items >= self.want_items
        } else {
            self.got_items <= self.want_items
        };
        let money = if self.want_money >= 0 {
            self.got_money >= self.want_money
        } else {
            self.got_money <= self.want_money
        };
        items && money
    }
}

/// The report for a problem of the island's portal or exit. `seen` names the doodad templates in
/// view when the portal was missing.
pub fn mirage(problem: Problem, skill: u32, doodad: u32, seen: &str) -> IssueReport {
    if problem == Problem::PortalMissing {
        return IssueReport::new(
            IssueKind::DoodadMissing,
            "mirage portal-doodad-missing",
            "Mirage Isle: the portal doodad is not in view at its spawn point",
        )
        .expected(format!(
            "doodad {doodad} (skill {skill}) in view within 20 s of standing 5 m from a spawn point of the data"
        ))
        .actual(format!("doodad templates in view: {seen}"))
        .suspect(Suspect::Data)
        .severity(Severity::Medium)
        .area(AREA_SPAWNS)
        .fact("skill", skill)
        .fact("doodad", doodad);
    }
    let (key, title, expected, actual) = match problem {
        Problem::NeverLoaded { casts } => (
            "mirage enter-no-load",
            "Mirage Isle: the portal cast was accepted but the island never loaded",
            "SCProcessingInstance or SCLoadInstance within 30 s of the accepted cast",
            format!("nothing for 30 s after {casts} cast(s) of skill {skill} on portal doodad {doodad}"),
        ),
        Problem::NeverLeft { casts } => (
            "mirage exit-stuck",
            "Mirage Isle: the exit cast was accepted but the character stayed on the island",
            "a load of the main world within 30 s of the accepted cast",
            format!("still on the island 30 s after {casts} cast(s) of skill {skill} on exit doodad {doodad}"),
        ),
        Problem::PortalMissing => unreachable!("handled above"),
    };
    IssueReport::new(IssueKind::NoResponse, key, title)
        .expected(expected)
        .actual(actual)
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .area(AREA_DOODAD_FUNCS)
        .fact("skill", skill)
        .fact("doodad", doodad)
}

/// The report for a trade both sides confirmed that did not move the goods or the money.
pub fn transfer(t: Transfer, sold: bool) -> IssueReport {
    let side = if sold { "sale" } else { "purchase" };
    IssueReport::new(
        IssueKind::TradeMismatch,
        format!("market trade-no-transfer {side}"),
        format!("A confirmed {side} in a player trade moved nothing (or less than agreed)"),
    )
    .expected(format!(
        "{:+} of item {} and {:+} copper in the bag and purse after both sides confirmed",
        t.want_items, t.template, t.want_money
    ))
    .actual(format!(
        "{:+} of item {} and {:+} copper",
        t.got_items, t.template, t.got_money
    ))
    .suspect(Suspect::Server)
    .severity(Severity::High)
    .area(AREA_TRADE)
    .fact("item", t.template)
}

/// The report for an unexpected chat refusal of the market's own lines. A whisper to a hostile
/// character (437) or to an offline one (265) is the bot's own mistake unless the chat rules
/// changed, so it stays local information.
pub fn chat_error(code: i16, to_bot: bool) -> IssueReport {
    let what = match code {
        CODE_WHISPER_HOSTILE => "a whisper to a hostile character",
        CODE_WHISPER_OFFLINE => "a whisper to an offline character",
        _ => "a chat line",
    };
    IssueReport::new(
        IssueKind::Other,
        format!("market chat-error code={code}"),
        format!("The server refused {what} (error {code})"),
    )
    .expected("market lines go out in the channel or whisper they were heard in")
    .actual(format!(
        "error {code} after a market line to {}",
        if to_bot {
            "one of our bots"
        } else {
            "a player"
        }
    ))
    .suspect(Suspect::Bot)
    .severity(Severity::Low)
    .area(AREA_CHAT)
    .fact("code", code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_island_reports_have_stable_keys_and_point_at_the_server() {
        let a = mirage(Problem::NeverLoaded { casts: 3 }, 17829, 4894, "");
        let b = mirage(Problem::NeverLoaded { casts: 1 }, 17829, 4894, "");
        assert_eq!(a.key, b.key, "the number of casts is no part of the key");
        assert_ne!(
            a.key,
            mirage(Problem::NeverLeft { casts: 1 }, 17838, 4895, "").key
        );
        assert_eq!(a.suspect, Suspect::Server);
        let c = mirage(Problem::PortalMissing, 17829, 4894, "4 x 1373");
        assert_eq!(c.suspect, Suspect::Data);
        assert!(c.actual.contains("1373"));
    }

    #[test]
    fn a_transfer_is_complete_when_goods_and_money_moved_as_agreed() {
        let sold = Transfer {
            template: 3411,
            want_items: -20,
            got_items: -20,
            want_money: 600,
            got_money: 600,
        };
        assert!(sold.is_complete());
        assert!(!Transfer {
            got_items: 0,
            ..sold
        }
        .is_complete());
        assert!(!Transfer {
            got_money: 0,
            ..sold
        }
        .is_complete());
        // more money than agreed is fine for the seller
        assert!(Transfer {
            got_money: 700,
            ..sold
        }
        .is_complete());
        let bought = Transfer {
            template: 3411,
            want_items: 20,
            got_items: 20,
            want_money: -600,
            got_money: -600,
        };
        assert!(bought.is_complete());
        assert!(!Transfer {
            got_items: 5,
            ..bought
        }
        .is_complete());
        assert!(!Transfer {
            got_money: 0,
            ..bought
        }
        .is_complete());
    }

    #[test]
    fn chat_errors_are_local_information() {
        let r = chat_error(CODE_WHISPER_HOSTILE, true);
        assert_eq!(r.suspect, Suspect::Bot);
        assert_eq!(r.severity, Severity::Low);
        assert_ne!(r.key, chat_error(CODE_WHISPER_OFFLINE, true).key);
        assert!(transfer(
            Transfer {
                template: 1,
                want_items: -1,
                got_items: 0,
                want_money: 5,
                got_money: 0
            },
            true
        )
        .key
        .contains("sale"));
    }
}
