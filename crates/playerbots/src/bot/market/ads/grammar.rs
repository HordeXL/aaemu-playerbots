//! What a chat line says about buying and selling: a small deterministic grammar.
//!
//! [`parse`] reads one line (trade chat, a shout or a whisper) and answers with the flags a
//! negotiation needs: is it an advertisement, a question about the price, a purchase, an
//! agreement, a refusal, a haggle, a question about the meeting place; and the quantity and the
//! price it names. It knows nothing about the conversation (that is [`super::talk`]) or about
//! which item is meant ([`item_hint`], [`super::names`]).
//!
//! The words are the ones players use: `wts`, `wtb`, `how much`, `i'll take`, `x20`, `30s ea`,
//! `1g 20s`, `deal`, `too much`, `where`, `meet`. Prices are gold, silver and copper
//! (`g`, `s`, `c`; 100 copper make a silver, 100 silver a gold); a price is per unit unless the
//! line says `total`, `for all` or `for the lot`. The lines the bots say themselves
//! ([`super::lines`]) are tested to parse back to what they meant, because bots talk to bots.

use super::price::{GOLD, SILVER};

/// A price named in a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    /// Copper.
    pub copper: i64,
    /// Per unit (the default), or for the whole quantity.
    pub per_unit: bool,
}

/// What a line says. See the module docs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// An advertisement of goods for sale (`WTS`, `selling`).
    pub sell_ad: bool,
    /// An advertisement of goods wanted (`WTB`, `buying`, `looking for`).
    pub buy_ad: bool,
    /// The speaker wants the goods (`I'll take`, `I'll buy`, `I want`).
    pub take: bool,
    /// A question about price or availability (`how much`, `price?`, `interested`).
    pub ask: bool,
    /// An agreement (`deal`, `ok`, `sure`).
    pub agree: bool,
    /// A refusal (`no`, `pass`, `not interested`, `sold out`).
    pub refuse: bool,
    /// The price is too high or the speaker wants it lower (`too much`, `cheaper`, `lowest`).
    pub haggle: bool,
    /// Talk about meeting (`meet`, `trade me`).
    pub meet: bool,
    /// A question about where (`where?`).
    pub where_q: bool,
    /// Mirage Isle is named.
    pub mirage: bool,
    /// A town place is named (`auctioneer`, `auction house`, `mailbox`, `town`).
    pub town: bool,
    /// The speaker says it is at the place (`here`, `arrived`, `waiting`).
    pub here: bool,
    /// The speaker says it is on its way (`omw`, `coming`).
    pub coming: bool,
    /// Thanks (`thx`, `ty`).
    pub thanks: bool,
    /// The line has a question mark.
    pub question: bool,
    /// The quantity named (`x20`, `20x`, `take 20`).
    pub qty: Option<u32>,
    /// The price named.
    pub price: Option<Price>,
}

impl Line {
    /// Whether the line has anything to do with buying and selling.
    pub fn is_market(&self) -> bool {
        self.sell_ad
            || self.buy_ad
            || self.take
            || self.ask
            || self.agree
            || self.refuse
            || self.haggle
            || self.meet
            || self.where_q
            || self.mirage
            || self.town
            || self.here
            || self.coming
            || self.qty.is_some()
            || self.price.is_some()
    }

    /// Whether the line is an advertisement.
    pub fn is_ad(&self) -> bool {
        self.sell_ad || self.buy_ad
    }
}

/// The line lowercased, without apostrophes, with punctuation turned into spaces and `?` kept as
/// a word of its own; digits, `.` and `/` stay (amounts such as `1.5g`, `30s/ea`).
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '\'' | '\u{2019}' | '`' => {}
            '?' => out.push_str(" ? "),
            c if c.is_alphanumeric() || c == '.' || c == '/' => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    // a full stop ends a word ("total."), it is a decimal point only between digits
    if out.contains('.') {
        out = out
            .split_whitespace()
            .map(|w| w.trim_end_matches('.'))
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }
    out
}

fn has_word(tokens: &[&str], words: &[&str]) -> bool {
    tokens.iter().any(|t| words.contains(t))
}

/// Whether `phrase` (single spaces) occurs in the tokens as whole words.
fn has_phrase(padded: &str, phrase: &str) -> bool {
    padded.contains(&format!(" {phrase} "))
}

/// Copper per unit of a money word (`g`, `silver`, ...).
fn unit_value(u: &str) -> Option<i64> {
    Some(match u {
        "g" | "gp" | "gold" => GOLD,
        "s" | "sp" | "sil" | "silver" => SILVER,
        "c" | "cp" | "cu" | "copper" => 1,
        _ => return None,
    })
}

/// `12`, `1.5` as hundredths.
fn hundredths(num: &str) -> Option<i64> {
    let (whole, frac) = match num.split_once('.') {
        Some((w, f)) => (w, f),
        None => (num, ""),
    };
    if whole.is_empty() && frac.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let w: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut f = frac.chars().take(2).collect::<String>();
    while f.len() < 2 {
        f.push('0');
    }
    w.checked_mul(100)?.checked_add(f.parse().ok()?)
}

/// One token of the form `12g`, `1g20s`, `1.5g`, `30silver`: the copper it names.
fn attached_amount(tok: &str) -> Option<i64> {
    let mut rest = tok;
    let mut total: i64 = 0;
    let mut any = false;
    while !rest.is_empty() {
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if end == 0 || end == rest.len() {
            return None;
        }
        let (num, tail) = rest.split_at(end);
        let unit_end = tail
            .find(|c: char| c.is_ascii_digit() || c == '.' || c == '/')
            .unwrap_or(tail.len());
        let (unit, after) = tail.split_at(unit_end);
        let value = unit_value(unit)?;
        total = total.checked_add(hundredths(num)?.checked_mul(value)? / 100)?;
        any = true;
        rest = after;
        if rest.starts_with('/') {
            break;
        }
    }
    any.then_some(total)
}

/// Words that may follow an amount of money in a line.
const AFTER_AMOUNT: [&str; 26] = [
    "ea", "each", "per", "apiece", "/ea", "total", "lot", "for", "and", "or", "of", "pst", "pm",
    "msg", "me", "whisper", "pls", "plz", "please", "only", "firm", "obo", "ok", "?", "thx",
    "tyvm",
];

/// Whether `word` is a long money word (`copper`, `silver`, `gold`), which is also the start of
/// item names (`Copper Ore`, `Gold Bar`).
fn is_long_unit(word: &str) -> bool {
    matches!(word, "copper" | "silver" | "gold")
}

/// The money at `tokens[i..]`: `(copper, tokens used)`.
fn amount_at(tokens: &[&str], i: usize) -> Option<(i64, usize)> {
    let mut total: i64 = 0;
    let mut used = 0;
    while let Some(tok) = tokens.get(i + used) {
        if let Some(c) = attached_amount(tok) {
            total = total.checked_add(c)?;
            used += 1;
            continue;
        }
        // "30" "silver"
        let numeric = !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit() || c == '.');
        if numeric {
            let unit_word = tokens.get(i + used + 1).copied();
            if let Some(unit) = unit_word.and_then(unit_value) {
                // "12 copper ore" is twelve of an item, "12 copper each" is twelve copper
                let item_follows = unit_word.is_some_and(is_long_unit)
                    && tokens.get(i + used + 2).is_some_and(|next| {
                        !AFTER_AMOUNT.contains(next)
                            && !next.starts_with(|c: char| c.is_ascii_digit())
                    });
                if !item_follows {
                    total = total.checked_add(hundredths(tok)?.checked_mul(unit)? / 100)?;
                    used += 2;
                    continue;
                }
            }
        }
        break;
    }
    (used > 0).then_some((total, used))
}

/// `x20` or `20x`.
fn marked_qty(tok: &str) -> Option<u32> {
    let digits = tok
        .strip_prefix('x')
        .or_else(|| tok.strip_suffix('x'))
        .filter(|d| !d.is_empty() && d.len() <= 6 && d.chars().all(|c| c.is_ascii_digit()))?;
    digits.parse().ok().filter(|n| *n > 0)
}

/// Reads one chat line. Never fails: a line about something else has no flag set.
pub fn parse(text: &str) -> Line {
    let norm = normalize(text);
    let tokens: Vec<&str> = norm.split_whitespace().collect();
    let padded = format!(" {} ", tokens.join(" "));
    let mut l = Line {
        question: text.contains('?'),
        ..Line::default()
    };
    let first = tokens.first().copied().unwrap_or("");
    l.sell_ad = matches!(first, "wts" | "selling" | "sellin" | "sell" | "wtt")
        || has_phrase(&padded, "for sale");
    l.buy_ad = matches!(first, "wtb" | "buying" | "lf" | "wanted")
        || has_phrase(&padded, "looking for")
        || has_phrase(&padded, "wtb");
    l.take = !l.buy_ad
        && (has_word(
            &tokens,
            &["take", "taking", "buy", "gimme", "want", "wants"],
        ) || has_phrase(&padded, "give me")
            || has_phrase(&padded, "i need"));
    l.ask = has_phrase(&padded, "how much")
        || has_word(
            &tokens,
            &[
                "howmuch",
                "hm",
                "price",
                "prices",
                "cost",
                "quote",
                "interested",
                "avail",
            ],
        )
        || has_phrase(&padded, "still available")
        || has_phrase(&padded, "got any")
        || has_phrase(&padded, "have any")
        || has_phrase(&padded, "what do you want");
    l.agree = has_word(
        &tokens,
        &[
            "deal", "ok", "okay", "k", "kk", "sure", "fine", "yes", "yep", "yeah", "yup", "accept",
            "accepted", "agreed", "done", "alright", "aight",
        ],
    ) || has_phrase(&padded, "sounds good")
        || has_phrase(&padded, "lets do it")
        || has_phrase(&padded, "works for me");
    l.refuse = has_word(
        &tokens,
        &[
            "no",
            "nope",
            "nah",
            "pass",
            "nvm",
            "nevermind",
            "cancel",
            "cancelled",
        ],
    ) || has_phrase(&padded, "not interested")
        || has_phrase(&padded, "forget it")
        || has_phrase(&padded, "never mind")
        || has_phrase(&padded, "sold out")
        || has_phrase(&padded, "already sold")
        || has_phrase(&padded, "changed my mind")
        || has_phrase(&padded, "too late");
    l.haggle = has_word(
        &tokens,
        &[
            "cheaper",
            "lower",
            "less",
            "discount",
            "reduce",
            "lowest",
            "pricey",
            "expensive",
            "steep",
            "overpriced",
            "negotiable",
            "cut",
        ],
    ) || has_phrase(&padded, "too much")
        || has_phrase(&padded, "too high")
        || has_phrase(&padded, "come down")
        || has_phrase(&padded, "best price")
        || has_phrase(&padded, "last price")
        || has_phrase(&padded, "best i can do");
    l.mirage = has_word(&tokens, &["mirage"]);
    l.town = has_word(
        &tokens,
        &[
            "town",
            "city",
            "auctioneer",
            "auction",
            "mailbox",
            "village",
        ],
    );
    l.where_q =
        has_word(&tokens, &["where", "wher", "location"]) || has_phrase(&padded, "which town");
    l.meet = l.mirage
        || l.town
        || l.where_q
        || has_word(&tokens, &["meet", "meetup", "come"])
        || has_phrase(&padded, "trade me")
        || has_phrase(&padded, "find me");
    l.here = has_word(&tokens, &["here", "arrived", "waiting", "ready"]);
    l.coming = has_word(&tokens, &["omw", "otw", "coming", "heading"])
        || has_phrase(&padded, "on my way")
        || has_phrase(&padded, "be there")
        || has_phrase(&padded, "be right there");
    l.thanks = has_word(
        &tokens,
        &["thx", "thanks", "thank", "ty", "tyvm", "thnx", "cheers"],
    );

    // the price and the quantity
    let mut i = 0;
    let mut money: Option<(i64, usize)> = None;
    let mut spans: Vec<(usize, usize)> = Vec::new();
    while i < tokens.len() {
        if let Some((c, used)) = amount_at(&tokens, i) {
            if money.is_none() {
                money = Some((c, i + used));
            }
            spans.push((i, i + used));
            i += used;
        } else {
            i += 1;
        }
    }
    if let Some((copper, end)) = money {
        let near = &tokens[end..tokens.len().min(end + 2)];
        let lead = end.checked_sub(1).and_then(|e| tokens.get(e));
        let total = near.iter().any(|t| matches!(*t, "total" | "lot"))
            || has_phrase(&padded, "for all")
            || has_phrase(&padded, "for the lot")
            || has_phrase(&padded, "all for")
            || has_phrase(&padded, "in total");
        let each = near
            .iter()
            .any(|t| matches!(*t, "ea" | "each" | "per" | "apiece" | "/ea" | "pe"))
            || lead.is_some_and(|t| t.ends_with("/ea"));
        l.price = Some(Price {
            copper,
            per_unit: each || !total,
        });
    }
    let in_money = |idx: usize| spans.iter().any(|(a, b)| idx >= *a && idx < *b);
    for (idx, tok) in tokens.iter().enumerate() {
        if in_money(idx) {
            continue;
        }
        if let Some(n) = marked_qty(tok) {
            l.qty = Some(n);
            break;
        }
        let bare = !tok.is_empty() && tok.len() <= 6 && tok.chars().all(|c| c.is_ascii_digit());
        if bare && l.qty.is_none() {
            let after_verb = idx > 0
                && matches!(
                    tokens[idx - 1],
                    "take"
                        | "buy"
                        | "want"
                        | "need"
                        | "get"
                        | "sell"
                        | "have"
                        | "got"
                        | "wts"
                        | "wtb"
                        | "selling"
                        | "buying"
                        | "of"
                        | "x"
                        | "me"
                );
            if after_verb {
                l.qty = tok.parse().ok().filter(|n| *n > 0);
            }
        }
    }
    l
}

/// Words of an item name that say nothing about which item it is.
const FILLER: [&str; 8] = ["the", "of", "a", "an", "and", "for", "to", "lv"];

/// Whether the (normalized, see [`normalize`]) line seems to talk about the item called `name`:
/// one of the name's own words, or its plural, is a word of the line (`ore` and `copper` for
/// "Copper Ore").
pub fn item_hint(norm_line: &str, name: &str) -> bool {
    let line: Vec<&str> = norm_line.split_whitespace().collect();
    normalize(name)
        .split_whitespace()
        .filter(|w| w.len() >= 3 && !FILLER.contains(w))
        .any(|w| {
            line.iter().any(|t| {
                *t == w
                    || t.strip_suffix('s') == Some(w)
                    || w.strip_suffix('s') == Some(*t) && t.len() >= 3
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(copper: i64, per_unit: bool) -> Option<Price> {
        Some(Price { copper, per_unit })
    }

    #[test]
    fn amounts_in_every_spelling() {
        let p = |t: &str| parse(t).price.map(|p| p.copper);
        assert_eq!(p("30s"), Some(3000));
        assert_eq!(p("WTS ore 1g"), Some(10_000));
        assert_eq!(p("45c ea"), Some(45));
        assert_eq!(p("1g 20s 5c"), Some(12_005));
        assert_eq!(p("1g20s"), Some(12_000));
        assert_eq!(p("1.5g"), Some(15_000));
        assert_eq!(p("2 gold"), Some(20_000));
        assert_eq!(p("50 silver"), Some(5_000));
        assert_eq!(p("500 copper"), Some(500));
        assert_eq!(p("12 copper each"), Some(12));
        assert_eq!(p("2 gold 50 silver pst"), Some(25_000));
        // twelve of an item, not twelve copper
        assert_eq!(p("wtb 12 copper ore"), None);
        assert_eq!(p("selling 3 gold bars 5g ea"), Some(50_000));
        assert_eq!(p("30 s each"), Some(3000));
        assert_eq!(p("nothing to see"), None);
        assert_eq!(p("x20 only"), None);
        // a bare number is no money
        assert_eq!(p("i'll take 20"), None);
        // junk does not panic
        for t in [
            "",
            "g",
            "1.2.3g",
            "..",
            "9999999999999999999g",
            "s s s",
            "x",
            "/ea",
            "1g/ea",
        ] {
            let _ = parse(t);
        }
    }

    #[test]
    fn prices_are_per_unit_unless_the_line_says_total() {
        assert_eq!(parse("30s ea").price, price(3000, true));
        assert_eq!(parse("30s each").price, price(3000, true));
        assert_eq!(parse("30s").price, price(3000, true));
        assert_eq!(parse("5g for all").price, price(50_000, false));
        assert_eq!(parse("5g total").price, price(50_000, false));
        assert_eq!(
            parse("i'll give 5g for the lot").price,
            price(50_000, false)
        );
        assert_eq!(parse("30s/ea").price, price(3000, true));
    }

    #[test]
    fn quantities_are_marked_or_follow_a_verb() {
        assert_eq!(parse("WTS Copper Ore x20 - 30s ea").qty, Some(20));
        assert_eq!(parse("20x copper ore").qty, Some(20));
        assert_eq!(parse("i'll take 5").qty, Some(5));
        assert_eq!(parse("wtb 12 copper ore").qty, Some(12));
        assert_eq!(parse("how much for the ore?").qty, None);
        // the money is not the quantity
        let l = parse("take 10 for 30s ea");
        assert_eq!((l.qty, l.price), (Some(10), price(3000, true)));
        let l = parse("30s for 10");
        assert_eq!(l.price, price(3000, true));
    }

    #[test]
    fn advertisements_and_purchases() {
        let l = parse("WTS Copper Ore x20 - 30s ea, PST");
        assert!(l.sell_ad && !l.buy_ad && !l.take && l.is_ad());
        let l = parse("WTB Hay Bale x10, paying 20c ea");
        assert!(l.buy_ad && !l.sell_ad && !l.take);
        let l = parse("Selling 20 copper ore 30s each");
        assert!(l.sell_ad);
        let l = parse("looking for blue salt");
        assert!(l.buy_ad);
        let l = parse("i'll take 10");
        assert!(l.take && !l.is_ad());
        let l = parse("ill buy all of them");
        assert!(l.take);
        assert!(!parse("see you in town").take);
    }

    #[test]
    fn questions_and_answers() {
        assert!(parse("how much for the ore?").ask);
        assert!(parse("price?").ask);
        assert!(parse("are these still available").ask);
        assert!(parse("deal!").agree);
        assert!(parse("ok").agree);
        assert!(parse("Sounds good").agree);
        assert!(parse("no thanks").refuse);
        assert!(parse("sorry, already sold").refuse);
        assert!(parse("not interested").refuse);
        // "know" and "okra" are not "no" and "ok"
        let l = parse("i know okra");
        assert!(!l.refuse && !l.agree);
        assert!(parse("too much").haggle);
        assert!(parse("can you go cheaper?").haggle);
        assert!(parse("that's my lowest, 26c ea").haggle);
        assert!(parse("thx!").thanks);
    }

    #[test]
    fn meeting_talk() {
        let l = parse("where?");
        assert!(l.where_q && l.meet);
        let l = parse("meet at Mirage Isle");
        assert!(l.mirage && l.meet && !l.town);
        let l = parse("meet me by the auctioneer in Marianople");
        assert!(l.town && l.meet && !l.mirage);
        assert!(parse("omw").coming);
        assert!(parse("ok, on my way").coming);
        assert!(parse("I'm here").here);
        assert!(parse("waiting at the mailbox").here);
        // nothing to do with trading
        let l = parse("nice weather today");
        assert!(!l.is_market());
        let l = parse("anyone want to duel?");
        assert!(
            l.take,
            "'want' is a purchase word; the talk decides if it matters"
        );
    }

    #[test]
    fn items_are_hinted_by_their_words() {
        let n = |t: &str| normalize(t);
        assert!(item_hint(&n("how much for the ore?"), "Copper Ore"));
        assert!(item_hint(&n("copper?"), "Copper Ore"));
        assert!(item_hint(&n("any copper ores left"), "Copper Ore"));
        assert!(!item_hint(&n("how much for the hay"), "Copper Ore"));
        assert!(item_hint(&n("blue salt please"), "Blue Salt"));
        // filler words are no hint
        assert!(!item_hint(&n("the of a"), "Scroll of the Fox"));
        assert!(item_hint(&n("a fox scroll"), "Scroll of the Fox"));
        // short words are no hint
        assert!(!item_hint(&n("it is on"), "Ox On"));
    }
}
