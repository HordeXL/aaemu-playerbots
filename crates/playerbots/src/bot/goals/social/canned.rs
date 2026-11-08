//! Canned chat replies for when the LLM is off (or busy): a small table of short templates per
//! kind of line heard, flavoured by the bot's dominant trait, picked so that a bot does not repeat
//! itself. `{n}` stands for the name of the player spoken to. What a bot says unasked is in
//! [`super::lines`] and [`super::humor`].

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

/// The keys of the last `N` templates used: category in the high byte, template in the low (0
/// marks a free slot; no category is 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recent<const N: usize = 4>([u16; N]);

impl<const N: usize> Default for Recent<N> {
    fn default() -> Self {
        Recent([0; N])
    }
}

impl<const N: usize> Recent<N> {
    /// Whether `key` was used lately.
    pub fn has(&self, key: u16) -> bool {
        key != 0 && self.0.contains(&key)
    }

    /// Notes `key` as used, forgetting the oldest.
    pub fn push(&mut self, key: u16) {
        if N == 0 {
            return;
        }
        self.0.rotate_right(1);
        self.0[0] = key;
    }

    /// The key used last (0 for none).
    pub fn last(&self) -> u16 {
        self.0.first().copied().unwrap_or(0)
    }
}

const GREET: &[&str] = &[
    "嘿！",
    "你好呀",
    "哟",
    "你好 {n}",
    "嘿 {n}，最近怎么样？",
    "嗨",
    "哟，来了！",
    "在吗 {n}",
];
const THANKS: &[&str] = &["不客气！", "没事 {n}", "随时帮忙", "不用谢", "小意思 :)"];
const BYE: &[&str] = &["回见 {n}", "回头见", "保重！", "拜拜", "先走了 {n}"];
const HELP: &[&str] = &[
    "抱歉 {n}，我正忙着自己的事",
    "等会儿吧，手上还有活",
    "现在不行，抱歉！",
    "想帮来着，可我任务没做完，抱歉",
    "过会儿再问我一次？",
];
const PRAISE: &[&str] = &[
    "谢啦！",
    "多谢 {n}！",
    "哈哈，谢了",
    "承让承让 {n}",
    "很感激",
];
const QUESTION: &[&str] = &[
    "说实话不知道",
    "嗯，我也不清楚 {n}",
    "好问题，我也想知道",
    "去城里问问？",
    "不好说，我自己还在琢磨",
    "要知道就好了",
];
const OTHER: &[&str] = &[
    "哈哈是啊",
    "确实",
    "哈，不错",
    "有道理",
    "嗯嗯",
    "哦，有点意思",
    "好 :)",
    "我也这么觉得",
    "真有意思",
];

// ---- flavour per dominant trait: (intent, lines) ---------------------------------------------

const QUESTER: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &["嘿！正要去接几个任务", "你好 {n}，一早上都在做任务"],
    ),
    (Intent::Question, &["去附近的任务发布者那看看，一般有用"]),
    (Intent::Other, &["正追着下一个任务呢，你懂的"]),
];
const DUELIST: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &["嘿 {n}，待会儿来场决斗？", "你好！随时准备大干一场"],
    ),
    (Intent::Question, &["打一架就知道了"]),
    (Intent::Other, &["一整天都想找人切磋"]),
];
const TRADER: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &["你好 {n}，有东西要交易吗？", "嘿！有什么好货？"],
    ),
    (Intent::Question, &["市场上肯定有人知道"]),
    (Intent::Other, &["公平买卖随时欢迎"]),
];
const MERCHANT: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &["嘿 {n}，今天忙着跑商呢", "你好！跑商跑得脚不沾地"],
    ),
    (Intent::Question, &["问问特产商人，他们什么都懂"]),
    (Intent::Other, &["背着货包，路都变长了"]),
];
const HOMEBODY: &[(Intent, &[&str])] = &[
    (
        Intent::Greet,
        &["嘿 {n}，就在这看看风景", "你好！今天适合在这歇着"],
    ),
    (Intent::Question, &["慢慢来，不着急"]),
    (Intent::Other, &["清静的一天，挺好"]),
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

/// What the line is about, from keywords. Order matters: a farewell beats a thank-you beats a
/// request for help beats a greeting ("hi, can you help?" is a request).
pub fn classify(text: &str) -> Intent {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    // Chinese has no spaces: a Chinese keyword matches as a substring of the line, English
    // keywords keep the whole-word rule.
    let any = |set: &[&str]| {
        words.iter().any(|w| set.contains(w))
            || set.iter().any(|k| !k.is_ascii() && lower.contains(k))
    };
    if any(&[
        "bye",
        "cya",
        "goodbye",
        "gtg",
        "laters",
        "farewell",
        "gn",
        "goodnight",
        // 中文
        "拜拜",
        "再见",
        "回见",
        "晚安",
        "下了",
    ]) {
        Intent::Bye
    } else if any(&[
        "thx", "thanks", "thank", "ty", "tyvm", "cheers", "谢谢", "多谢", "感谢", "3q",
    ]) {
        Intent::Thanks
    } else if any(&[
        "lfg",
        "party",
        "group",
        "help",
        "anyone",
        "anybody",
        "invite",
        "join",
        // 中文
        "帮忙",
        "帮个忙",
        "帮我",
        "组队",
        "求组",
        "带上我",
        "救救",
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
        // 中文
        "你好",
        "您好",
        "嗨",
        "哈喽",
        "在吗",
        "在么",
        "嘿嘿",
        "哈喽",
    ]) {
        Intent::Greet
    } else if any(&[
        "gg",
        "nice",
        "wp",
        "grats",
        "congrats",
        "gj",
        "wow",
        "awesome",
        // 中文
        "厉害",
        "牛",
        "牛逼",
        "漂亮",
        "恭喜",
        "666",
        "打得好",
    ]) {
        Intent::Praise
    } else if lower.contains('?')
        || lower.contains('？')
        || words.first().is_some_and(|w| {
            [
                "what", "where", "how", "why", "who", "when", "which", "can", "do", "does", "is",
                "are",
            ]
            .contains(w)
        })
        || ["什么", "哪里", "怎么", "为什么", "多少", "几级"]
            .iter()
            .any(|k| lower.contains(k))
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
        if table.len() > 1 && key(idx) == recent.last() {
            idx = (idx + 1) % table.len();
        }
        idx
    };
    recent.push(key(idx));
    render(table[idx], who)
}

/// Fills `{n}`; with no name the placeholder (and the space before it) disappears.
pub fn render(template: &str, who: &str) -> String {
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
            seen_flavour |=
                line.contains("决斗") || line.contains("切磋") || line.contains("大干一场");
        }
        assert!(seen_flavour);
        // an unknown trait falls back to the generic table
        let line = reply(Intent::Greet, "balanced", "Bob", &mut recent, &mut rng);
        assert!(!line.is_empty());
    }
}
