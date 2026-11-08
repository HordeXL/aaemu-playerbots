//! The chat commands a companion obeys, and the short lines it answers with.
//!
//! The set is closed and has no GM path: a player cannot make a bot do anything but follow, stay,
//! come, assist, leave the party, walk to a dungeon, or invite them. [`parse`] reads a line (a
//! whisper, a line of local or party chat) into a [`Command`]; the words around the command that
//! people add ("pls", "me", "here") are ignored, anything else is no command, so a bot that hears
//! a conversation does not act on it.

/// A command a player gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Follow the leader again (ends `stay`).
    Follow,
    /// Stand where you are.
    Stay,
    /// Walk to the speaker.
    Come,
    /// Attack the speaker's target.
    Assist,
    /// Leave the party.
    Leave,
    /// Walk to the entrance of a dungeon (`query` is what the player typed after the word, may be
    /// empty).
    Dungeon(Box<str>),
    /// "inv": invite me to your party.
    Invite,
}

/// Words that may follow a command word without changing it.
const FILLER: [&str; 9] = [
    "me", "please", "pls", "plz", "here", "now", "party", "group", "to",
];

/// The words of a line: lowercase, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Reads a chat line into a command; `None` when it is none of them.
pub fn parse(text: &str) -> Option<Command> {
    let mut w = words(text);
    let head = w.first()?.clone();
    // a very long line is a conversation, not a command
    if w.len() > 6 {
        return None;
    }
    w.remove(0);
    if head == "dungeon" {
        return Some(Command::Dungeon(w.join(" ").into_boxed_str()));
    }
    // the rest must be filler only ("follow me", "come here pls")
    if !w.iter().all(|x| FILLER.contains(&x.as_str())) {
        return None;
    }
    match head.as_str() {
        "follow" => Some(Command::Follow),
        "stay" | "wait" | "hold" => Some(Command::Stay),
        "come" => Some(Command::Come),
        "assist" | "attack" => Some(Command::Assist),
        "leave" => Some(Command::Leave),
        "inv" | "invite" => Some(Command::Invite),
        _ => None,
    }
}

/// How well a dungeon query matches a dungeon name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Exactly one dungeon (its index in the list that was searched).
    One(usize),
    /// More than one fits and none is the name itself.
    Many,
    /// None.
    None,
}

/// Finds the dungeon `query` names among `names` (index, English name): every word of the query
/// must start a word of the name ("hadir" finds "Hadir Farm"); the name itself wins over longer
/// names that contain it.
pub fn find_dungeon<'a>(names: impl Iterator<Item = (usize, &'a str)>, query: &str) -> Match {
    let q = words(query);
    if q.is_empty() {
        return Match::None;
    }
    let mut found: Vec<(usize, bool)> = Vec::new();
    for (i, name) in names {
        let n = words(name);
        let all = q
            .iter()
            .all(|qw| n.iter().any(|nw| nw.starts_with(qw.as_str())));
        if all {
            found.push((i, n == q));
        }
    }
    if let Some(&(i, _)) = found.iter().find(|f| f.1) {
        return Match::One(i);
    }
    match found.as_slice() {
        [] => Match::None,
        [(i, _)] => Match::One(*i),
        _ => Match::Many,
    }
}

/// The kinds of line a companion says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Say {
    /// Obeying `follow`.
    Following,
    /// Obeying `stay`.
    Staying,
    /// Obeying `come`.
    Coming,
    /// Obeying `assist`.
    Assisting,
    /// `assist` and nothing is targeted.
    NoTarget,
    /// Obeying `leave`.
    Leaving,
    /// A dungeon was asked for and the bot goes there (`{}` is the name).
    Heading,
    /// At the entrance of the dungeon that was asked for (`{}` is the name).
    AtDoor,
    /// A dungeon was asked for and the bot's level is too low (`{}` is the name).
    TooLow,
    /// A dungeon query that names none.
    UnknownDungeon,
    /// A dungeon query that names several.
    WhichDungeon,
    /// The bot cannot go into the dungeon the leader entered (`{}` is the name).
    CannotEnter,
    /// The bot sent the invitation the player asked for.
    InviteSent,
    /// The bot cannot invite: busy or the policy says no.
    Sorry,
    /// The bot is in a party it does not lead (`{}` is the leader).
    AskLeader,
    /// A command from someone who may not give it (`leave`, `dungeon`).
    NotYours,
    /// The bot left because the levels are too far apart.
    LevelGap,
    /// The bot is full of party members already.
    Full,
}

/// The line for `say`, picked by `pick` (any number: the caller's randomness) so that the same
/// answer is not always the same words. `arg` replaces `{}`.
pub fn line(say: Say, pick: usize, arg: &str) -> String {
    let options: &[&str] = match say {
        Say::Following => &["跟着你呢", "就在你后面", "马上来"],
        Say::Staying => &["好，就在这待命", "在这等着", "原地待命"],
        Say::Coming => &["来了", "在路上", "马上到"],
        Say::Assisting => &["打你的目标", "这就上", "协助中"],
        Say::NoTarget => &["我没看见目标", "你没有目标"],
        Say::Leaving => &["好，我退队了", "拜拜，祝好运"],
        Say::Heading => &["这就去 {}", "好，这就动身去 {}", "正在去 {} 的路上"],
        Say::AtDoor => &["我在 {} 的入口了", "在 {} 等着"],
        Say::TooLow => &["我等级不够打 {}", "{} 对我来说太难了"],
        Say::UnknownDungeon => &["哪个副本？我不知道这个"],
        Say::WhichDungeon => &["哪一个？说具体点"],
        Say::CannotEnter => &["我进不去 {}", "{} 不让我进"],
        Say::InviteSent => &["邀请已发出", "发了邀请"],
        Say::Sorry => &["抱歉，我正忙着", "抱歉，现在不行", "现在不行，抱歉"],
        Say::AskLeader => &["我已经在队里了，问 {}", "让 {} 邀请你"],
        Say::NotYours => &["只有队长能这么要求我"],
        Say::LevelGap => &["抱歉，我们等级差太多了"],
        Say::Full => &["抱歉，我们人已经够了"],
    };
    options[pick % options.len()].replace("{}", arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_closed_set_is_understood() {
        let cases = [
            ("follow", Command::Follow),
            ("Follow me", Command::Follow),
            ("follow me pls", Command::Follow),
            ("!follow", Command::Follow),
            ("stay", Command::Stay),
            ("stay here", Command::Stay),
            ("wait", Command::Stay),
            ("hold", Command::Stay),
            ("come", Command::Come),
            ("come here", Command::Come),
            ("come to me!", Command::Come),
            ("assist", Command::Assist),
            ("assist me", Command::Assist),
            ("attack", Command::Assist),
            ("leave", Command::Leave),
            ("leave party", Command::Leave),
            ("inv", Command::Invite),
            ("invite me", Command::Invite),
            ("Inv me pls", Command::Invite),
            ("invite", Command::Invite),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text), Some(want), "{text:?}");
        }
        assert_eq!(parse("dungeon"), Some(Command::Dungeon("".into())));
        assert_eq!(
            parse("dungeon Hadir Farm"),
            Some(Command::Dungeon("hadir farm".into()))
        );
        assert_eq!(
            parse("Dungeon: howling abyss!"),
            Some(Command::Dungeon("howling abyss".into()))
        );
    }

    #[test]
    fn conversation_and_other_words_are_no_commands() {
        for text in [
            "",
            "   ",
            "hello",
            "follow the white rabbit",
            "can you follow me",
            "i will stay at home tonight and then we will see",
            "leave it to me",
            "come on guys let us go",
            "item add self 1",
            "teleport",
            "kill",
            "invite him",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn dungeons_are_found_by_the_start_of_their_words() {
        let names = [
            (0, "Hadir Farm"),
            (1, "Howling Abyss"),
            (2, "Howling Abyss Easy"),
            (3, "Library"),
            (4, "Sal Temple"),
        ];
        let find = |q: &str| find_dungeon(names.iter().copied(), q);
        assert_eq!(find("hadir"), Match::One(0));
        assert_eq!(find("Hadir Farm"), Match::One(0));
        assert_eq!(find("lib"), Match::One(3));
        assert_eq!(find("temple"), Match::One(4));
        // the name itself beats the longer names that contain it
        assert_eq!(find("howling abyss"), Match::One(1));
        // two fit and none is the name
        assert_eq!(find("howling"), Match::Many);
        assert_eq!(find("a"), Match::Many);
        assert_eq!(find("nowhere"), Match::None);
        assert_eq!(find(""), Match::None);
        assert_eq!(find("  !! "), Match::None);
        assert_eq!(find_dungeon(std::iter::empty(), "hadir"), Match::None);
    }

    #[test]
    fn every_line_is_sendable_and_fills_the_name() {
        for say in [
            Say::Following,
            Say::Staying,
            Say::Coming,
            Say::Assisting,
            Say::NoTarget,
            Say::Leaving,
            Say::Heading,
            Say::AtDoor,
            Say::TooLow,
            Say::UnknownDungeon,
            Say::WhichDungeon,
            Say::CannotEnter,
            Say::InviteSent,
            Say::Sorry,
            Say::AskLeader,
            Say::NotYours,
            Say::LevelGap,
            Say::Full,
        ] {
            for pick in 0..4 {
                let l = line(say, pick, "Library");
                assert!(!l.is_empty() && !l.contains("{}"), "{say:?}: {l}");
                // none may look like a command
                assert!(aa_proto::cs::plain_chat_text(&l).is_some(), "{l}");
            }
        }
        assert_eq!(line(Say::Heading, 0, "Library"), "这就去 Library");
        assert_eq!(line(Say::Heading, 3, "Library"), "这就去 Library");
    }
}
