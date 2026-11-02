//! Canned chat lines for when the LLM is off (or busy): a small table of short templates per
//! kind of remark, flavoured by the bot's dominant trait, picked so that a bot does not repeat
//! itself. `{n}` stands for the name of the player spoken to.

/// What a heard line is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// "hi", "hello", "yo".
    Greet,
    /// "thanks", "ty".
    Thanks,
    /// "bye", "cya".
    Bye,
    /// "lfg", "anyone want to party", "help".
    Help,
    /// "gg", "nice", "grats".
    Praise,
    /// A question.
    Question,
    /// Anything else.
    Other,
}

/// What a bot says on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chatter {
    /// After a duel.
    DuelDone,
    /// After a trade.
    TradeDone,
    /// Meeting another bot.
    Hello,
}

/// Index of a template that was used lately: category in the high bits, template in the low.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Recent([u16; 4]);

impl Recent {
    fn has(&self, key: u16) -> bool {
        self.0.contains(&key)
    }

    fn push(&mut self, key: u16) {
        self.0.rotate_right(1);
        self.0[0] = key;
    }
}

const GREET: &[&str] = &[
    "hey!",
    "hello there",
    "yo",
    "hi {n}",
    "hey {n}, how's it going?",
    "heya",
    "oh hi!",
    "sup {n}",
];
const THANKS: &[&str] = &[
    "np!",
    "no problem {n}",
    "anytime",
    "you're welcome",
    "all good :)",
];
const BYE: &[&str] = &[
    "cya {n}",
    "see you around",
    "take care!",
    "bye bye",
    "later {n}",
];
const HELP: &[&str] = &[
    "sorry {n}, kinda busy with my own stuff",
    "maybe later, i'm in the middle of something",
    "can't right now, sorry!",
    "i'd help but i'm on a mission, sorry",
    "ask me again in a bit?",
];
const PRAISE: &[&str] = &[
    "thx!",
    "thanks {n}!",
    "haha thanks",
    "cheers {n}",
    "appreciate it",
];
const QUESTION: &[&str] = &[
    "no idea honestly",
    "hmm, not sure {n}",
    "good question, i wonder too",
    "try asking in town?",
    "hard to say, still figuring it out myself",
    "wish i knew",
];
const OTHER: &[&str] = &[
    "haha yeah",
    "lol true",
    "ha, nice one",
    "fair enough",
    "mhm",
    "hmm interesting",
    "ok :)",
    "yeah same",
    "fun times",
];

// ---- flavour per dominant trait: (intent, lines) ---------------------------------------------

const QUESTER: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &[
            "hey! off to grab a few more quests",
            "hi {n}, been questing all morning",
        ],
    ),
    (
        Intent::Question,
        &["check the quest givers around here, that usually helps"],
    ),
    (
        Intent::Other,
        &["just chasing the next quest, you know how it goes"],
    ),
];
const DUELIST: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &[
            "hey {n}, up for a duel later?",
            "hi! always ready for a good fight",
        ],
    ),
    (Intent::Question, &["i'd just fight it and see"]),
    (Intent::Other, &["been itching for a duel all day"]),
];
const TRADER: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &[
            "hi {n}, looking to trade anything?",
            "hey! got any good deals?",
        ],
    ),
    (Intent::Question, &["someone at the market probably knows"]),
    (Intent::Other, &["always up for a fair swap"]),
];
const MERCHANT: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &[
            "hey {n}, busy hauling trade packs today",
            "hello! trade runs keep me busy",
        ],
    ),
    (
        Intent::Question,
        &["ask the specialty vendors, they know everything"],
    ),
    (
        Intent::Other,
        &["the roads are long with a pack on your back"],
    ),
];
const HOMEBODY: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &[
            "hey {n}, just enjoying the view",
            "hi! nice day to relax around here",
        ],
    ),
    (Intent::Question, &["i'd take it slow, no rush"]),
    (Intent::Other, &["quiet day, i like it"]),
];

fn flavour(dominant: &str, intent: Intent) -> &'static [&'static str] {
    let table = match dominant {
        "quester" => QUESTER,
        "duelist" => DUELIST,
        "trader" => TRADER,
        "merchant" => MERCHANT,
        "homebody" => HOMEBODY,
        _ => return &[],
    };
    table
        .iter()
        .find(|(i, _)| *i == intent)
        .map_or(&[][..], |(_, lines)| *lines)
}

fn generic(intent: Intent) -> &'static [&'static str] {
    match intent {
        Intent::Greet => GREET,
        Intent::Thanks => THANKS,
        Intent::Bye => BYE,
        Intent::Help => HELP,
        Intent::Praise => PRAISE,
        Intent::Question => QUESTION,
        Intent::Other => OTHER,
    }
}

const DUEL_DONE: &[&str] = &[
    "gg",
    "gg {n}",
    "gg, good fight",
    "well fought {n}",
    "nice one {n}",
];
const TRADE_DONE: &[&str] = &[
    "thx {n}",
    "thanks for the trade {n}",
    "pleasure doing business",
    "deal, cheers {n}",
];
const HELLO: &[&str] = &["hey {n}", "o/ {n}", "hi {n}", "yo {n}", "hello {n}"];

fn chatter_table(kind: Chatter) -> &'static [&'static str] {
    match kind {
        Chatter::DuelDone => DUEL_DONE,
        Chatter::TradeDone => TRADE_DONE,
        Chatter::Hello => HELLO,
    }
}

/// What the line is about, from keywords. Order matters: a farewell beats a thank-you beats a
/// request for help beats a greeting ("hi, can you help?" is a request).
pub fn classify(text: &str) -> Intent {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    let any = |set: &[&str]| words.iter().any(|w| set.contains(w));
    if any(&[
        "bye",
        "cya",
        "goodbye",
        "gtg",
        "laters",
        "farewell",
        "gn",
        "goodnight",
    ]) {
        Intent::Bye
    } else if any(&["thx", "thanks", "thank", "ty", "tyvm", "cheers"]) {
        Intent::Thanks
    } else if any(&[
        "lfg", "party", "group", "help", "anyone", "anybody", "invite", "join",
    ]) {
        Intent::Help
    } else if any(&[
        "hi",
        "hello",
        "hey",
        "heya",
        "hiya",
        "yo",
        "sup",
        "hail",
        "greetings",
        "howdy",
    ]) {
        Intent::Greet
    } else if any(&[
        "gg", "nice", "wp", "grats", "congrats", "gj", "wow", "awesome",
    ]) {
        Intent::Praise
    } else if lower.contains('?')
        || words.first().is_some_and(|w| {
            [
                "what", "where", "how", "why", "who", "when", "which", "can", "do", "does", "is",
                "are",
            ]
            .contains(w)
        })
    {
        Intent::Question
    } else {
        Intent::Other
    }
}

/// A reply to a line about `intent` from a bot whose dominant trait is `dominant`, speaking to
/// `who` (may be empty). Avoids the templates in `recent`.
pub fn reply(
    intent: Intent,
    dominant: &str,
    who: &str,
    recent: &mut Recent,
    rng: &mut fastrand::Rng,
) -> String {
    let flavoured = flavour(dominant, intent);
    // category 1.. for replies, bit 7 tells flavoured from generic tables apart
    let flavour_cat = (intent as u16 + 1) | 0x80;
    let generic_cat = intent as u16 + 1;
    let wants_flavour = !flavoured.is_empty() && rng.f32() < 0.4;
    if wants_flavour && fresh(flavoured, flavour_cat, recent) > 0 {
        return fill(flavoured, flavour_cat, who, recent, rng);
    }
    fill(generic(intent), generic_cat, who, recent, rng)
}

/// A line a bot says on its own.
pub fn chatter(kind: Chatter, who: &str, recent: &mut Recent, rng: &mut fastrand::Rng) -> String {
    let cat = 0x40 | kind as u16;
    fill(chatter_table(kind), cat, who, recent, rng)
}

/// How many lines of `table` were not used lately.
fn fresh(table: &[&str], cat: u16, recent: &Recent) -> usize {
    (0..table.len())
        .filter(|i| !recent.has(cat << 8 | *i as u16))
        .count()
}

/// A line of `table` that was not used lately; when all were, any but the last one used.
fn fill(
    table: &'static [&'static str],
    cat: u16,
    who: &str,
    recent: &mut Recent,
    rng: &mut fastrand::Rng,
) -> String {
    debug_assert!(!table.is_empty());
    let key = |i: usize| cat << 8 | i as u16;
    let fresh_count = fresh(table, cat, recent);
    let idx = if fresh_count > 0 {
        (0..table.len())
            .filter(|i| !recent.has(key(*i)))
            .nth(rng.usize(..fresh_count))
            .unwrap_or(0)
    } else {
        let mut idx = rng.usize(..table.len());
        if table.len() > 1 && key(idx) == recent.0[0] {
            idx = (idx + 1) % table.len();
        }
        idx
    };
    recent.push(key(idx));
    render(table[idx], who)
}

/// Fills `{n}`; with no name the placeholder (and the space before it) disappears.
fn render(template: &str, who: &str) -> String {
    if template.contains("{n}") {
        if who.is_empty() {
            return template
                .replace(" {n}", "")
                .replace("{n}", "")
                .trim()
                .to_string();
        }
        return template.replace("{n}", who);
    }
    template.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_classified_by_keyword() {
        assert_eq!(classify("Hello there!"), Intent::Greet);
        assert_eq!(classify("hey aria"), Intent::Greet);
        assert_eq!(classify("thx a lot"), Intent::Thanks);
        assert_eq!(classify("ok bye"), Intent::Bye);
        assert_eq!(classify("lfg anyone want to party"), Intent::Help);
        assert_eq!(classify("hi, can you help me?"), Intent::Help);
        assert_eq!(classify("gg"), Intent::Praise);
        assert_eq!(classify("where is the vendor"), Intent::Question);
        // keywords come before the question mark
        assert_eq!(classify("nice weather?"), Intent::Praise);
        assert_eq!(classify("the wolves are everywhere"), Intent::Other);
    }

    #[test]
    fn every_table_has_lines_and_none_starts_with_a_slash_or_names_a_bot() {
        let intents = [
            Intent::Greet,
            Intent::Thanks,
            Intent::Bye,
            Intent::Help,
            Intent::Praise,
            Intent::Question,
            Intent::Other,
        ];
        for i in intents {
            let mut lines: Vec<&str> = generic(i).to_vec();
            for t in ["quester", "duelist", "trader", "merchant", "homebody"] {
                lines.extend(flavour(t, i));
            }
            assert!(lines.len() >= 4, "{i:?}");
            for l in lines {
                assert!(!l.starts_with('/') && !l.trim().is_empty(), "{l}");
                let lower = l.to_lowercase();
                assert!(!lower.contains("bot") && !lower.contains(" ai"), "{l}");
                assert!(l.chars().count() <= 60, "{l}");
            }
        }
        for k in [Chatter::DuelDone, Chatter::TradeDone, Chatter::Hello] {
            assert!(chatter_table(k).len() >= 4);
        }
    }

    #[test]
    fn the_name_is_filled_or_dropped_cleanly() {
        assert_eq!(render("hi {n}", "Aria"), "hi Aria");
        assert_eq!(render("hi {n}", ""), "hi");
        assert_eq!(render("{n}, hello", ""), ", hello");
        assert_eq!(render("gg", "Aria"), "gg");
    }

    #[test]
    fn a_bot_does_not_repeat_itself_back_to_back() {
        let mut rng = fastrand::Rng::with_seed(5);
        let mut recent = Recent::default();
        let mut last = String::new();
        for _ in 0..200 {
            let line = reply(Intent::Other, "balanced", "Bob", &mut recent, &mut rng);
            assert_ne!(line, last, "same line twice in a row");
            last = line;
        }
    }

    #[test]
    fn flavour_shows_up_for_the_dominant_trait() {
        let mut rng = fastrand::Rng::with_seed(9);
        let mut recent = Recent::default();
        let mut seen_flavour = false;
        for _ in 0..80 {
            let line = reply(Intent::Greet, "duelist", "Bob", &mut recent, &mut rng);
            seen_flavour |= line.contains("duel") || line.contains("fight");
        }
        assert!(seen_flavour);
        // an unknown trait falls back to the generic table
        let line = reply(Intent::Greet, "balanced", "Bob", &mut recent, &mut rng);
        assert!(!line.is_empty());
    }

    #[test]
    fn chatter_lines_come_from_their_own_tables() {
        let mut rng = fastrand::Rng::with_seed(1);
        let mut recent = Recent::default();
        for _ in 0..20 {
            let l = chatter(Chatter::DuelDone, "Bob", &mut recent, &mut rng);
            assert!(l.contains("gg") || l.contains("well fought") || l.contains("nice one"));
        }
    }
}
