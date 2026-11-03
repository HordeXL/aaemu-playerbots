//! The issue reports of the auction goal: what the bots tell the team when the auction house does
//! not behave. Keys name ids only (never lots, times or amounts), so two bots meeting one problem
//! file one issue.
//!
//! The suspect is [`Suspect::Server`] only where the bot did what a player's client does and the
//! server's own packets contradict each other (the fee was taken but nothing was posted, the money
//! was taken and no answer came). A refusal the server does not explain ([`silent_post`]) and
//! range or flag mistakes are [`Suspect::Bot`] or `Unknown`: the bot may have asked for something
//! the rules forbid.

use crate::bot::market::auction::mail::Kind;
use crate::bot::market::auction::{Fault, WinWait};
use crate::issues::{IssueKind, IssueReport, Severity, Suspect};

/// Where the auction code is in the server.
const AREA_AUCTION: &str = "rust/crates/aaemu-game/src/systems/economy/";

fn kind_word(k: Kind) -> &'static str {
    match k {
        Kind::Sold => "sale",
        Kind::Cancelled => "cancelled lot",
        Kind::Expired => "expired lot",
        Kind::Refund => "outbid refund",
        Kind::Won => "won lot",
    }
}

/// The post took the item and the fee, and no `SCAuctionPosted` came.
pub fn post_without_answer(item: &str, template: u32, fee: i32) -> IssueReport {
    IssueReport::new(
        IssueKind::NoResponse,
        "auction-post-no-posted-packet",
        "An auction post took the item and the fee but sent no SCAuctionPosted",
    )
    .expected("SCAuctionPosted with the new lot within a few seconds of a post the server accepts")
    .actual(format!(
        "the {item} left the bag and {fee} copper of fee were taken, no SCAuctionPosted came"
    ))
    .suspect(Suspect::Server)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
    .fact("template", template)
}

/// The post changed nothing the bot can see and was not answered: the server refused it silently
/// (a bound item, a price it does not take). Stays local.
pub fn silent_post(template: u32) -> IssueReport {
    IssueReport::new(
        IssueKind::Other,
        "auction-post-refused-silently",
        "An auction post was refused without an error message",
    )
    .expected("SCAuctionPosted, or an error message for a refusal")
    .actual("the server neither listed the item nor said why; the bot may have broken a rule")
    .suspect(Suspect::Bot)
    .severity(Severity::Low)
    .area(AREA_AUCTION)
    .fact("template", template)
}

/// A lot posted a minute ago is not in the list of the character's own lots.
pub fn lot_not_listed(item: &str, template: u32) -> IssueReport {
    IssueReport::new(
        IssueKind::Other,
        "auction-posted-lot-not-listed",
        "A lot the server confirmed is missing from the character's own lots",
    )
    .expected("a search by the seller's character id lists a lot posted minutes ago")
    .actual(format!(
        "SCAuctionPosted confirmed the {item} lot, the own-lots search did not list it"
    ))
    .suspect(Suspect::Server)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
    .fact("template", template)
}

/// A search got no answer although the bot stood next to the auctioneer.
pub fn search_unanswered(what: &str) -> IssueReport {
    IssueReport::new(
        IssueKind::NoResponse,
        format!("auction-search-no-reply {what}"),
        "An auction search next to the auctioneer got no answer",
    )
    .expected("SCAuctionSearched (even an empty page) or an error message")
    .actual("no answer within the wait, twice, and no error message")
    .suspect(Suspect::Server)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
}

/// A buyout took the money and no `SCAuctionBid` came.
pub fn bid_without_answer(item: &str, template: u32, pay: i32) -> IssueReport {
    IssueReport::new(
        IssueKind::NoResponse,
        "auction-bid-no-bid-packet",
        "An auction buyout took the money but sent no SCAuctionBid",
    )
    .expected("SCAuctionBid within a few seconds of a buyout the server accepts")
    .actual(format!(
        "{pay} copper left the purse for the {item} lot, no SCAuctionBid came"
    ))
    .suspect(Suspect::Server)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
    .fact("template", template)
}

/// The item of a buyout never came by mail.
pub fn win_mail_missing(w: WinWait, item: &str) -> IssueReport {
    IssueReport::new(
        IssueKind::PaymentMissing,
        "auction-win-mail-missing",
        "A bought-out auction lot never arrived by mail",
    )
    .expected("a mail with the item (type 16) within a minute of the buyout")
    .actual(format!(
        "the money for the {item} lot was taken (SCAuctionBid), no won-lot mail came in {} s",
        crate::bot::market::auction::WIN_MAIL_WAIT_MS / 1000
    ))
    .suspect(Suspect::Server)
    .severity(Severity::High)
    .area(AREA_AUCTION)
    .fact("template", w.template)
    .fact("lot", w.lot_id)
}

/// Lots of the bot vanished from the house and fewer result mails came.
pub fn sale_mail_missing(missing: u32) -> IssueReport {
    IssueReport::new(
        IssueKind::PaymentMissing,
        "auction-seller-mail-missing",
        "An own auction lot ended and its result mail never came",
    )
    .expected("one mail (sold, unsold or cancelled) for every lot that leaves the house")
    .actual(format!(
        "{missing} lot(s) of the character are gone from its own-lots list and no mail came for them"
    ))
    .suspect(Suspect::Server)
    .severity(Severity::High)
    .area(AREA_AUCTION)
    .fact("missing", missing)
}

/// An auction mail that cannot be collected.
pub fn mail_fault(f: Fault) -> IssueReport {
    let (key, what, kind) = match f {
        Fault::Empty(k) => (
            "auction-mail-empty",
            "has neither money nor the item the header announced",
            k,
        ),
        Fault::Unanswered(k) => (
            "auction-mail-unanswered",
            "was not answered when the bot asked for its body or attachments",
            k,
        ),
    };
    IssueReport::new(
        IssueKind::PaymentMissing,
        format!("{key} {}", kind_word(kind)),
        format!("An auction mail ({}) cannot be collected", kind_word(kind)),
    )
    .expected("the mail body and its attachments can be read and taken")
    .actual(format!("the {} mail {what}", kind_word(kind)))
    .suspect(Suspect::Server)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
}

/// An error message nobody expects while the bot waits for an answer of the auction house.
pub fn unexpected_error(code: i16, request: &str) -> IssueReport {
    IssueReport::new(
        IssueKind::Other,
        format!("auction-error={code} {request}"),
        format!("The auction house answered a {request} with error {code}"),
    )
    .expected(
        "an answer packet, or one of the errors the rules explain (241 cannot pay, 388 too far)",
    )
    .actual(format!("SCErrorMsg {code}"))
    .suspect(Suspect::Unknown)
    .severity(Severity::Low)
    .area(AREA_AUCTION)
    .fact("error", code)
}

/// The auctioneer refused a request although the bot stood within a metre of it.
pub fn refused_in_range(template: u32) -> IssueReport {
    IssueReport::new(
        IssueKind::Other,
        format!("auction-refused-in-range npc={template}"),
        "An auctioneer refused a request from the character standing next to it",
    )
    .expected("the request is answered within 5 m of an NPC with the auctioneer flag")
    .actual("error 388 (auctioneer too far) at about one metre, after interacting with the NPC")
    .suspect(Suspect::Unknown)
    .severity(Severity::Medium)
    .area(AREA_AUCTION)
    .fact("npc", template)
}

/// The auctioneer NPC is not where the data says.
pub fn auctioneer_missing(name: &str, template: u32) -> IssueReport {
    IssueReport::new(
        IssueKind::NpcMissing,
        format!("template={template}"),
        format!("NPC auctioneer {name} is not at its spawn"),
    )
    .expected("the unit of the world data's static spawn is in view when the bot stands there")
    .actual("the bot stood at the spawn point for a while and the server showed none")
    .suspect(Suspect::Data)
    .severity(Severity::Medium)
    .area("rust/crates/aaemu-game/src/systems/npcs/")
    .fact("template", template)
    .fact("role", "auctioneer")
}
