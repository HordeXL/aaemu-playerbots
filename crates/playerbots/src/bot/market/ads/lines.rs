//! What the bots say: the wording of advertisements and of every step of a negotiation.
//!
//! [`Speech`] says *what* (an advertisement, a quote, an agreement, the meeting place, ...), and
//! [`render`] turns it into a chat line with a few templates per kind, so the trade channel does not
//! read like one script. Bots answer each other through these lines, so every template is tested
//! to be read back by [`super::grammar::parse`] as what it meant. Nothing here says what the speaker
//! is: the lines are what a player would write.

use super::grammar::normalize;
use super::price::fmt;

/// Where two parties meet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Meet {
    /// Mirage Isle, by the mailbox.
    Mirage,
    /// By the auctioneer of a town, at this position in the main world.
    Town {
        /// Map x.
        x: f32,
        /// Map y.
        y: f32,
    },
}

/// Why a party backs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// The price is too low (for a seller) or too high (for a buyer).
    Price,
    /// The goods are gone.
    SoldOut,
    /// Not interested after all.
    Pass,
}

/// One thing a bot says in a negotiation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Speech {
    /// Goods for sale: item template, quantity, copper per unit.
    AdSell {
        /// Item template.
        template: u32,
        /// Units.
        qty: u32,
        /// Copper per unit.
        unit: u32,
    },
    /// Goods wanted.
    AdBuy {
        /// Item template.
        template: u32,
        /// Units.
        qty: u32,
        /// Copper per unit.
        unit: u32,
    },
    /// A buyer asks what it costs.
    Ask {
        /// Item template.
        template: u32,
    },
    /// A seller names its price.
    Quote {
        /// Item template.
        template: u32,
        /// Units it has.
        qty: u32,
        /// Copper per unit.
        unit: u32,
    },
    /// A seller answers a wanted-ad with what it has.
    Offer {
        /// Item template.
        template: u32,
        /// Units it has.
        qty: u32,
        /// Copper per unit.
        unit: u32,
    },
    /// A buyer takes the goods, at a price of its own or at the asked one.
    Take {
        /// Units.
        qty: u32,
        /// Copper per unit when the buyer names one.
        unit: Option<u32>,
    },
    /// A price proposed by the buyer or a last offer of the seller.
    Counter {
        /// Copper per unit.
        unit: u32,
        /// The seller speaks (a buyer's counter is a question).
        seller: bool,
    },
    /// The seller will not go lower.
    Lowest {
        /// Copper per unit.
        unit: u32,
    },
    /// Agreed.
    Agree,
    /// Backing out.
    Decline(Decline),
    /// The seller closes the deal and says where to meet.
    Confirm {
        /// Item template.
        template: u32,
        /// Units.
        qty: u32,
        /// Copper for all of them.
        total: i64,
        /// The place.
        meet: Meet,
    },
    /// The place again.
    Place {
        /// The place.
        meet: Meet,
    },
    /// On the way.
    OnMyWay,
    /// At the place.
    Here,
    /// Thanks after the trade.
    Thanks,
    /// Calling the deal off.
    Cancel,
}

impl Speech {
    /// The item the speech is about, if it names one.
    pub fn template(&self) -> Option<u32> {
        match self {
            Speech::AdSell { template, .. }
            | Speech::AdBuy { template, .. }
            | Speech::Ask { template }
            | Speech::Quote { template, .. }
            | Speech::Offer { template, .. }
            | Speech::Confirm { template, .. } => Some(*template),
            _ => None,
        }
    }

    /// The place the speech names, if it names one.
    pub fn meet(&self) -> Option<Meet> {
        match self {
            Speech::Confirm { meet, .. } | Speech::Place { meet } => Some(*meet),
            _ => None,
        }
    }
}

/// The words a [`Speech`] needs from the world.
#[derive(Debug, Clone, Copy)]
pub struct Words<'a> {
    /// The item's name (of the speech's template).
    pub item: &'a str,
    /// The town's name for a [`Meet::Town`].
    pub place: &'a str,
}

fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut s = template.to_owned();
    for (k, v) in pairs {
        s = s.replace(k, v);
    }
    s
}

const AD_SELL: [&str; 4] = [
    "WTS {item} x{qty} - {unit} ea, PST",
    "WTS {item} x{qty}, {unit} each. whisper me",
    "Selling {qty} {item}, {unit} ea - pm me",
    "WTS {qty}x {item} {unit} ea, msg me",
];
const AD_BUY: [&str; 4] = [
    "WTB {item} x{qty}, paying {unit} ea. PST",
    "WTB {qty}x {item} - {unit} each, whisper me",
    "Buying {qty} {item} for {unit} ea, pm me",
    "LF {item} x{qty} - {unit} ea",
];
const ASK: [&str; 4] = [
    "how much for the {item}?",
    "{item}? price?",
    "hi, how much are the {item}?",
    "interested in your {item}, what price?",
];
const QUOTE: [&str; 3] = [
    "{unit} each, i have {qty} {item}",
    "{unit} ea for the {item}, got {qty}",
    "{item} are {unit} ea, x{qty} available",
];
const OFFER: [&str; 3] = [
    "i have {qty} {item}, {unit} ea?",
    "got {qty} {item} for you, {unit} each",
    "can do x{qty} {item} at {unit} ea",
];
const TAKE: [&str; 3] = [
    "ill take {qty}",
    "ok, ill take {qty}",
    "sure, ill take {qty}",
];
const TAKE_AT: [&str; 2] = [
    "ill take {qty} at {unit} ea",
    "ill take {qty} for {unit} ea",
];
const COUNTER_BUYER: [&str; 3] = [
    "{unit} ea?",
    "can you do {unit} ea?",
    "how about {unit} each?",
];
const COUNTER_SELLER: [&str; 3] = [
    "best i can do is {unit} ea",
    "{unit} ea is my best price",
    "i can do {unit} each",
];
const LOWEST: [&str; 2] = ["{unit} ea is my lowest", "thats my lowest, {unit} ea"];
const AGREE: [&str; 4] = ["deal", "ok deal", "sure, deal", "sounds good, deal"];
const DECLINE_PRICE: [&str; 3] = [
    "no thanks, too low for me",
    "sorry, ill pass at that price",
    "nah, thats too low",
];
const DECLINE_SOLD: [&str; 3] = [
    "sorry, already sold",
    "sold out, sorry",
    "sorry, sold out already",
];
const DECLINE_PASS: [&str; 3] = ["no thanks", "nah, ill pass", "not interested, thx"];
const CONFIRM_MIRAGE: [&str; 2] = [
    "deal! {qty} {item} for {total} total. meet me on Mirage Isle by the mailbox (the portal is next to the auctioneer)",
    "ok deal, {total} total for {qty} {item}. lets meet on Mirage Isle at the mailbox",
];
const CONFIRM_TOWN: [&str; 2] = [
    "deal! {qty} {item} for {total} total. meet me at the auctioneer in {place}",
    "sure, {total} total for {qty} {item}. im at the {place} auctioneer, come find me",
];
const PLACE_MIRAGE: [&str; 2] = [
    "ill be at the mailbox on Mirage Isle",
    "Mirage Isle, by the mailbox. the portal is next to any auctioneer",
];
const PLACE_TOWN: [&str; 2] = [
    "at the auctioneer in {place}",
    "{place}, next to the auctioneer",
];
const ON_MY_WAY: [&str; 3] = ["omw", "ok, on my way", "coming, be there in a minute"];
const HERE: [&str; 3] = ["im here", "here! waiting for you", "arrived, waiting"];
const THANKS: [&str; 4] = ["thx, gg", "thanks!", "ty", "thanks, nice doing business"];
const CANCEL: [&str; 3] = [
    "sorry, changed my mind. never mind",
    "sorry, something came up. cancel that",
    "sorry, cancel that",
];

/// How many wordings [`render_nth`] has for `speech`.
pub fn variants(speech: &Speech) -> usize {
    templates(speech).len()
}

fn templates(speech: &Speech) -> &'static [&'static str] {
    match speech {
        Speech::AdSell { .. } => &AD_SELL,
        Speech::AdBuy { .. } => &AD_BUY,
        Speech::Ask { .. } => &ASK,
        Speech::Quote { .. } => &QUOTE,
        Speech::Offer { .. } => &OFFER,
        Speech::Take { unit: None, .. } => &TAKE,
        Speech::Take { unit: Some(_), .. } => &TAKE_AT,
        Speech::Counter { seller: false, .. } => &COUNTER_BUYER,
        Speech::Counter { seller: true, .. } => &COUNTER_SELLER,
        Speech::Lowest { .. } => &LOWEST,
        Speech::Agree => &AGREE,
        Speech::Decline(Decline::Price) => &DECLINE_PRICE,
        Speech::Decline(Decline::SoldOut) => &DECLINE_SOLD,
        Speech::Decline(Decline::Pass) => &DECLINE_PASS,
        Speech::Confirm {
            meet: Meet::Mirage, ..
        } => &CONFIRM_MIRAGE,
        Speech::Confirm { .. } => &CONFIRM_TOWN,
        Speech::Place {
            meet: Meet::Mirage, ..
        } => &PLACE_MIRAGE,
        Speech::Place { .. } => &PLACE_TOWN,
        Speech::OnMyWay => &ON_MY_WAY,
        Speech::Here => &HERE,
        Speech::Thanks => &THANKS,
        Speech::Cancel => &CANCEL,
    }
}

/// The `n`-th wording (modulo the number of wordings) of `speech`.
pub fn render_nth(speech: &Speech, w: &Words<'_>, n: usize) -> String {
    let t = templates(speech);
    let t = t[n % t.len()];
    let item = clean(w.item);
    let place = clean(w.place);
    let (qty, unit, total);
    let line = match *speech {
        Speech::AdSell {
            qty: q, unit: u, ..
        }
        | Speech::AdBuy {
            qty: q, unit: u, ..
        }
        | Speech::Quote {
            qty: q, unit: u, ..
        }
        | Speech::Offer {
            qty: q, unit: u, ..
        } => {
            qty = q.to_string();
            unit = fmt(i64::from(u));
            fill(t, &[("{item}", &item), ("{qty}", &qty), ("{unit}", &unit)])
        }
        Speech::Ask { .. } => fill(t, &[("{item}", &item)]),
        Speech::Take { qty: q, unit: u } => {
            qty = q.to_string();
            unit = fmt(i64::from(u.unwrap_or(0)));
            fill(t, &[("{qty}", &qty), ("{unit}", &unit)])
        }
        Speech::Counter { unit: u, .. } | Speech::Lowest { unit: u } => {
            unit = fmt(i64::from(u));
            fill(t, &[("{unit}", &unit)])
        }
        Speech::Confirm {
            qty: q, total: tot, ..
        } => {
            qty = q.to_string();
            total = fmt(tot);
            fill(
                t,
                &[
                    ("{item}", &item),
                    ("{qty}", &qty),
                    ("{total}", &total),
                    ("{place}", &place),
                ],
            )
        }
        Speech::Place { .. } => fill(t, &[("{place}", &place)]),
        _ => t.to_owned(),
    };
    cut(&line, 190)
}

/// A wording picked at random.
pub fn render(speech: &Speech, w: &Words<'_>, rng: &mut fastrand::Rng) -> String {
    render_nth(speech, w, rng.usize(..variants(speech)))
}

/// `to: text` for a line in a channel, `text` as it is for a whisper.
pub fn addressed(to: Option<&str>, text: &str) -> String {
    match to {
        Some(name) if !name.is_empty() => cut(&format!("{name}: {text}"), 195),
        _ => cut(text, 195),
    }
}

/// An item or place name without anything that could be taken for markup or a command.
fn clean(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | '{' | '}' | '|'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// At most `max` characters.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

/// Whether the chat line `text` starts with `name` as the word it is addressed to (`Bob: ...`,
/// `Bob, ...`, `Bob ...`).
pub fn starts_with_name(text: &str, name: &str) -> bool {
    let n = normalize(name);
    let t = normalize(text);
    let n = n.trim();
    !n.is_empty() && t.split_whitespace().next() == Some(n)
}

#[cfg(test)]
mod tests {
    use super::super::grammar::{parse, Line};
    use super::*;

    const W: Words<'static> = Words {
        item: "Copper Ore",
        place: "Marianople",
    };

    fn every(speech: Speech) -> Vec<(usize, String, Line)> {
        (0..variants(&speech))
            .map(|n| {
                let text = render_nth(&speech, &W, n);
                let line = parse(&text);
                (n, text, line)
            })
            .collect()
    }

    #[test]
    fn selling_ads_read_back_as_selling_ads_with_quantity_and_price() {
        for (n, text, l) in every(Speech::AdSell {
            template: 3411,
            qty: 20,
            unit: 3000,
        }) {
            assert!(l.sell_ad && !l.buy_ad, "{n}: {text}");
            assert_eq!(l.qty, Some(20), "{n}: {text}");
            let p = l.price.unwrap_or_else(|| panic!("{n}: {text}"));
            assert_eq!((p.copper, p.per_unit), (3000, true), "{n}: {text}");
        }
    }

    #[test]
    fn buying_ads_read_back_as_buying_ads_with_quantity_and_price() {
        for (n, text, l) in every(Speech::AdBuy {
            template: 3411,
            qty: 12,
            unit: 45,
        }) {
            assert!(l.buy_ad && !l.sell_ad, "{n}: {text}");
            assert_eq!(l.qty, Some(12), "{n}: {text}");
            let p = l.price.unwrap_or_else(|| panic!("{n}: {text}"));
            assert_eq!((p.copper, p.per_unit), (45, true), "{n}: {text}");
        }
    }

    #[test]
    fn questions_and_quotes() {
        for (n, text, l) in every(Speech::Ask { template: 3411 }) {
            assert!(l.ask && !l.is_ad(), "{n}: {text}");
        }
        for s in [
            Speech::Quote {
                template: 3411,
                qty: 20,
                unit: 3100,
            },
            Speech::Offer {
                template: 3411,
                qty: 20,
                unit: 3100,
            },
        ] {
            for (n, text, l) in every(s) {
                assert!(!l.is_ad(), "{n}: {text}");
                assert_eq!(l.qty, Some(20), "{n}: {text}");
                assert_eq!(
                    l.price.map(|p| (p.copper, p.per_unit)),
                    Some((3100, true)),
                    "{n}: {text}"
                );
            }
        }
    }

    #[test]
    fn purchases_counters_and_the_last_word() {
        for (n, text, l) in every(Speech::Take { qty: 7, unit: None }) {
            assert!(
                l.take && l.qty == Some(7) && l.price.is_none(),
                "{n}: {text}"
            );
        }
        for (n, text, l) in every(Speech::Take {
            qty: 7,
            unit: Some(2500),
        }) {
            assert!(l.take && l.qty == Some(7), "{n}: {text}");
            assert_eq!(l.price.map(|p| p.copper), Some(2500), "{n}: {text}");
        }
        for seller in [false, true] {
            for (n, text, l) in every(Speech::Counter { unit: 2800, seller }) {
                assert_eq!(
                    l.price.map(|p| (p.copper, p.per_unit)),
                    Some((2800, true)),
                    "{n}: {text}"
                );
                assert!(!l.is_ad() && !l.refuse, "{n}: {text}");
            }
        }
        for (n, text, l) in every(Speech::Lowest { unit: 2600 }) {
            assert!(l.haggle, "{n}: {text}");
            assert_eq!(l.price.map(|p| p.copper), Some(2600), "{n}: {text}");
        }
    }

    #[test]
    fn agreements_refusals_and_cancellations() {
        for (n, text, l) in every(Speech::Agree) {
            assert!(l.agree && !l.refuse, "{n}: {text}");
        }
        for why in [Decline::Price, Decline::SoldOut, Decline::Pass] {
            for (n, text, l) in every(Speech::Decline(why)) {
                assert!(l.refuse, "{why:?} {n}: {text}");
                assert!(!l.agree && l.price.is_none(), "{why:?} {n}: {text}");
            }
        }
        for (n, text, l) in every(Speech::Cancel) {
            assert!(l.refuse && !l.agree, "{n}: {text}");
        }
    }

    #[test]
    fn the_confirmation_names_the_goods_the_total_and_the_place() {
        let mirage = Speech::Confirm {
            template: 3411,
            qty: 20,
            total: 61_000,
            meet: Meet::Mirage,
        };
        for (n, text, l) in every(mirage) {
            assert!(l.agree && l.mirage && l.meet && !l.refuse, "{n}: {text}");
            let p = l.price.unwrap_or_else(|| panic!("{n}: {text}"));
            assert_eq!((p.copper, p.per_unit), (61_000, false), "{n}: {text}");
            assert!(
                text.contains("Copper Ore") && text.contains("20"),
                "{n}: {text}"
            );
        }
        let town = Speech::Confirm {
            template: 3411,
            qty: 20,
            total: 61_000,
            meet: Meet::Town { x: 1.0, y: 2.0 },
        };
        for (n, text, l) in every(town) {
            assert!(l.agree && l.town && l.meet && !l.mirage, "{n}: {text}");
            assert_eq!(
                l.price.map(|p| (p.copper, p.per_unit)),
                Some((61_000, false)),
                "{n}: {text}"
            );
            assert!(text.contains("Marianople"), "{n}: {text}");
        }
        for (n, text, l) in every(Speech::Place { meet: Meet::Mirage }) {
            assert!(l.mirage && l.meet, "{n}: {text}");
        }
        for (n, text, l) in every(Speech::Place {
            meet: Meet::Town { x: 0.0, y: 0.0 },
        }) {
            assert!(l.town && !l.mirage, "{n}: {text}");
        }
    }

    #[test]
    fn arrivals_and_thanks() {
        for (n, text, l) in every(Speech::OnMyWay) {
            assert!(l.coming, "{n}: {text}");
        }
        for (n, text, l) in every(Speech::Here) {
            assert!(l.here && !l.coming, "{n}: {text}");
        }
        for (n, text, l) in every(Speech::Thanks) {
            assert!(l.thanks, "{n}: {text}");
        }
    }

    #[test]
    fn lines_are_short_clean_and_never_commands() {
        let long = "x".repeat(300);
        let w = Words {
            item: &long,
            place: &long,
        };
        let all = [
            Speech::AdSell {
                template: 1,
                qty: 5,
                unit: 10,
            },
            Speech::Confirm {
                template: 1,
                qty: 5,
                total: 50,
                meet: Meet::Town { x: 0.0, y: 0.0 },
            },
            Speech::Confirm {
                template: 1,
                qty: 5,
                total: 50,
                meet: Meet::Mirage,
            },
        ];
        for s in all {
            for n in 0..variants(&s) {
                let t = render_nth(&s, &w, n);
                assert!(t.chars().count() <= 190, "{t}");
                assert!(!t.starts_with('/'));
            }
        }
        // an item name cannot smuggle in a command or markup
        let evil = Words {
            item: "/gm {qty} | \\x",
            place: "",
        };
        let t = render_nth(
            &Speech::AdSell {
                template: 1,
                qty: 5,
                unit: 10,
            },
            &evil,
            0,
        );
        assert!(
            !t.contains('/') && !t.contains('{') && !t.contains('|'),
            "{t}"
        );
        assert!(!render_nth(&Speech::Ask { template: 1 }, &evil, 1).starts_with('/'));
    }

    #[test]
    fn channel_lines_are_addressed_by_name() {
        assert_eq!(addressed(Some("Bob"), "deal"), "Bob: deal");
        assert_eq!(addressed(None, "deal"), "deal");
        assert_eq!(addressed(Some(""), "deal"), "deal");
        assert!(starts_with_name("Bob: how much?", "Bob"));
        assert!(starts_with_name("bob, ok", "Bob"));
        assert!(!starts_with_name("Bobby: ok", "Bob"));
        assert!(!starts_with_name("ok bob", "Bob"));
        assert!(!starts_with_name("anything", ""));
    }

    #[test]
    fn a_random_wording_is_one_of_the_templates() {
        let mut rng = fastrand::Rng::with_seed(3);
        let s = Speech::Agree;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            seen.insert(render(&s, &W, &mut rng));
        }
        assert_eq!(seen.len(), variants(&s));
    }
}
