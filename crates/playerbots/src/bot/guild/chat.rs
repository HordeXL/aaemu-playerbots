//! What the bots say in guild chat, and how they read a player's request for an invitation.
//!
//! The lines are a small table, like the canned replies of the social duty (which supplies the
//! answers to a real player's guild chat line): a welcome for a newcomer, a thank-you for the
//! invitation, a founding cheer, a farewell and small talk. `{n}` stands for a name and `{g}` for
//! the guild's name. A bot does not repeat a line it used lately ([`Recent`]).

/// What a line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An officer welcomes a newcomer.
    Welcome,
    /// The newcomer thanks for the invitation.
    Thanks,
    /// The founder greets the founding team.
    Founded,
    /// A member says goodbye.
    Farewell,
    /// An officer announces an appointment ("Congrats {n}").
    Promoted,
    /// Anything.
    Talk,
}

const WELCOME: &[&str] = &[
    "welcome to {g}, {n}!",
    "hey {n}, welcome aboard!",
    "welcome {n}! glad to have you",
    "{n} joined us, welcome!",
    "good to see you here {n}",
];
const THANKS: &[&str] = &[
    "thanks for the invite!",
    "hi all, thanks for having me",
    "hello everyone!",
    "thx, happy to be here",
    "hey guild!",
];
const FOUNDED: &[&str] = &[
    "welcome to {g}, everyone! we did it",
    "{g} is official, thanks for signing up",
    "our guild is born, let's make it a good one",
    "the charter is signed, welcome to {g}!",
];
const FAREWELL: &[&str] = &[
    "i'm going to look for something different, take care all",
    "time for me to move on, it was fun",
    "bye all, taking a break from the guild",
    "thanks for everything, see you around",
];
const PROMOTED: &[&str] = &[
    "congrats {n}, you're an officer now",
    "{n} is an officer now, nice",
    "{n} made officer, well deserved",
];
const TALK: &[&str] = &[
    "anyone up for a dungeon later?",
    "how is the questing going for everyone?",
    "just finished a long quest chain, finally",
    "need anything crafted? i have some spare materials",
    "the weather is lovely today",
    "has anybody seen a good place to farm?",
    "i keep dying to the same monster, send help",
    "found a nice spot by the lake, very quiet",
    "good morning all",
    "who is around for a duel?",
    "trade packs pay well if you can carry them",
    "this guild is the best thing since the glider",
    "any tips for leveling faster?",
    "back from the shop, the prices are crazy",
    "what are you all working on?",
    "brb, repairing my gear",
];

fn table(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Welcome => WELCOME,
        Kind::Thanks => THANKS,
        Kind::Founded => FOUNDED,
        Kind::Farewell => FAREWELL,
        Kind::Promoted => PROMOTED,
        Kind::Talk => TALK,
    }
}

/// The lines a bot used lately: kind in the high bits, index in the low.
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

/// A line of `kind` for the member `who` (empty: nobody in particular) of the guild `guild`,
/// not one the bot used lately.
pub fn line(
    kind: Kind,
    who: &str,
    guild: &str,
    recent: &mut Recent,
    rng: &mut fastrand::Rng,
) -> String {
    let lines = table(kind);
    let base = (kind as u16) << 8;
    let mut idx = rng.usize(..lines.len());
    for _ in 0..lines.len() {
        if !recent.has(base | idx as u16) {
            break;
        }
        idx = (idx + 1) % lines.len();
    }
    recent.push(base | idx as u16);
    let who = if who.is_empty() { "everyone" } else { who };
    lines[idx].replace("{n}", who).replace("{g}", guild)
}

/// Whether `text` (a whisper) asks for a guild invitation: "ginv", "guild invite", "invite me
/// to the guild", "can i join". A request to join a party or a group is none.
pub fn is_join_request(text: &str) -> bool {
    let t: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() || words.len() > 14 {
        return false;
    }
    let has = |w: &str| words.contains(&w);
    // another kind of joining
    if ["party", "group", "raid", "dungeon", "duel", "trade", "team"]
        .iter()
        .any(|w| has(w))
    {
        return false;
    }
    let joined = format!(" {} ", words.join(" "));
    let phrase = |p: &str| joined.contains(&format!(" {p} "));
    if has("ginv") || has("g") && (has("inv") || has("invite")) {
        return true;
    }
    if has("guild") || has("gld") || has("clan") || has("expedition") {
        return has("invite")
            || has("inv")
            || has("join")
            || has("joining")
            || phrase("can i")
            || phrase("let me in")
            || phrase("take me");
    }
    phrase("can i join") || phrase("could i join") || phrase("can i get an invite")
}

/// A short hash of a player name, to remember who asked.
pub fn name_key(name: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    name.to_lowercase().hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_for_an_invitation_are_understood() {
        for yes in [
            "ginv",
            "ginv pls",
            "Ginv?",
            "g inv",
            "guild invite",
            "guild invite please!",
            "can i get a guild invite",
            "invite me to the guild",
            "Can I join your guild?",
            "can i join?",
            "could I join",
            "i want to join the guild",
            "can i get an invite",
            "guild inv pls",
            "any guild i can join",
        ] {
            assert!(is_join_request(yes), "{yes}");
        }
        for no in [
            "hello",
            "nice day",
            "can i join your party",
            "join my group?",
            "want to join a dungeon",
            "invite me to a duel",
            "how is the guild?",
            "what is a guild",
            "can i trade",
            "",
            "   ",
            "joining the fight now with all my strength and courage and then some more words",
        ] {
            assert!(!is_join_request(no), "{no}");
        }
    }

    #[test]
    fn lines_name_the_member_and_the_guild() {
        let mut rng = fastrand::Rng::with_seed(1);
        let mut recent = Recent::default();
        for _ in 0..30 {
            let l = line(
                Kind::Welcome,
                "Anna",
                "Silent Wardens",
                &mut recent,
                &mut rng,
            );
            assert!(!l.contains('{') && !l.is_empty(), "{l}");
            assert!(l.len() <= 200);
        }
        // a line without a name speaks to everyone
        let l = line(Kind::Welcome, "", "G", &mut recent, &mut rng);
        assert!(!l.contains("  ") && !l.contains('{'), "{l}");
        assert!(l.contains("everyone") || !table(Kind::Welcome).iter().all(|t| t.contains("{n}")));
    }

    #[test]
    fn a_line_is_not_repeated_right_away() {
        let mut rng = fastrand::Rng::with_seed(2);
        let mut recent = Recent::default();
        let mut last = String::new();
        for _ in 0..200 {
            let l = line(Kind::Talk, "", "G", &mut recent, &mut rng);
            assert_ne!(l, last);
            last = l;
        }
    }

    #[test]
    fn every_line_is_safe_to_say() {
        for kind in [
            Kind::Welcome,
            Kind::Thanks,
            Kind::Founded,
            Kind::Farewell,
            Kind::Promoted,
            Kind::Talk,
        ] {
            for l in table(kind) {
                assert!(aa_proto::cs::plain_chat_text(l).is_some(), "{l}");
                assert!(!aa_proto::cs::is_command_like(l), "{l}");
            }
        }
    }

    #[test]
    fn the_name_key_ignores_case() {
        assert_eq!(name_key("Anna"), name_key("ANNA"));
        assert_ne!(name_key("Anna"), name_key("Bo"));
    }
}
