//! Jokes, quips and banter in the spirit of ArcheAge 1.x player culture: labor points that always
//! run out, trade packs and gankers on the road, Freedich Island and the Kraken, the login queue,
//! regrades that break weapons, lunagem luck, farm drama, Mirage Isle, Diamond Shores, boats and
//! pirates, the Hasla grind, carrot farmers, the housing land rush and lag. All written for this
//! project. A jokey bot tells them as ambient lines and quips back at small talk; a joke another
//! bot told lately is skipped (the hub remembers the last few, [`super::gate::ChatGate::told`]).
//!
//! Every template follows the rules of [`super::lines`] (a level per line, the `{x}` slot) and is
//! said in local chat only.

use pb_llm::persona::Profanity;

use super::canned::Recent;
use super::lines::{c, m, pick, pick_fresh, s, Line, Slots, Tongue};

/// What a joke is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// Labor points.
    Labor,
    /// Trade packs and gankers.
    Packs,
    /// Freedich Island and the Kraken.
    Freedich,
    /// The login queue.
    Queue,
    /// Regrading gear.
    Regrade,
    /// Lunagems and sockets.
    Lunagem,
    /// Farms: thunderstruck trees, stolen crops.
    Farm,
    /// Mirage Isle.
    Mirage,
    /// Diamond Shores.
    Diamond,
    /// Boats and pirates.
    Sea,
    /// The Hasla grind.
    Hasla,
    /// Carrot farmers.
    Carrots,
    /// The housing land rush.
    Land,
    /// Lag.
    Lag,
}

/// Category of the first topic in a [`Recent`] key; topic `t` is `CAT_JOKE + t as u16`.
const CAT_JOKE: u16 = 0x20;
const CAT_QUIP: u16 = 0x30;
const CAT_CRUDE: u16 = 0x31;

impl Topic {
    /// Every topic.
    pub const ALL: [Topic; 14] = [
        Topic::Labor,
        Topic::Packs,
        Topic::Freedich,
        Topic::Queue,
        Topic::Regrade,
        Topic::Lunagem,
        Topic::Farm,
        Topic::Mirage,
        Topic::Diamond,
        Topic::Sea,
        Topic::Hasla,
        Topic::Carrots,
        Topic::Land,
        Topic::Lag,
    ];

    /// A few words about the topic for an LLM prompt.
    pub fn hint(self) -> &'static str {
        match self {
            Topic::Labor => "labor points always running out",
            Topic::Packs => "carrying trade packs past the gankers on the road",
            Topic::Freedich => "Freedich Island and the Kraken",
            Topic::Queue => "the endless login queue",
            Topic::Regrade => "regrading gear and the luck that breaks weapons",
            Topic::Lunagem => "lunagems and socketing luck",
            Topic::Farm => "farm drama: thunderstruck trees and crops stolen on public land",
            Topic::Mirage => "Mirage Isle and its shop",
            Topic::Diamond => "dying at Diamond Shores",
            Topic::Sea => "boats and pirates",
            Topic::Hasla => "the Hasla grind",
            Topic::Carrots => "carrot farmers running the economy",
            Topic::Land => "the housing land rush",
            Topic::Lag => "lag",
        }
    }

    /// The jokes about the topic.
    pub fn lines(self) -> &'static [Line] {
        match self {
            Topic::Labor => LABOR,
            Topic::Packs => PACKS,
            Topic::Freedich => FREEDICH,
            Topic::Queue => QUEUE,
            Topic::Regrade => REGRADE,
            Topic::Lunagem => LUNAGEM,
            Topic::Farm => FARM,
            Topic::Mirage => MIRAGE,
            Topic::Diamond => DIAMOND,
            Topic::Sea => SEA,
            Topic::Hasla => HASLA,
            Topic::Carrots => CARROTS,
            Topic::Land => LAND,
            Topic::Lag => LAG,
        }
    }

    fn cat(self) -> u16 {
        CAT_JOKE + self as u16
    }
}

const LABOR: &[Line] = &[
    c("labor points: the only currency i'm always broke in"),
    c("logged in, spent my labor in four minutes, now i just stand here"),
    c("{x}, out of labor again. guess i'll go look at trees"),
    c("i don't need gold, i need labor potions and a nap"),
    c("every time i open the crafting window my labor runs away screaming"),
    m("labor regen is slower than a damn snail"),
];
const PACKS: &[Line] = &[
    c("carrying a trade pack at walking speed, the most thrilling thing in gaming"),
    c("every bush on the road to the trade outlet has a ganker in it, i swear"),
    c("{x}, nothing like a ganker 20 m from the turn-in"),
    c("trade pack on my back, eyes on the horizon, heart rate at 200"),
    c("pro tip: the ganker is always behind the one rock you didn't check"),
    c("walked a pack across the whole continent and the payout bought me a sandwich"),
];
const FREEDICH: &[Line] = &[
    c("going to freedich with a pack is a donation to pirates with extra steps"),
    c("the kraken spawned and suddenly everyone remembered they own a boat"),
    c("freedich island: come for the gold, stay because your boat sank"),
    c("i've seen the kraken once. from the seabed. as a ghost"),
    c("kraken fight tip: be anywhere but under it"),
];
const QUEUE: &[Line] = &[
    c("the login queue builds character. mostly patience"),
    c("got through the queue so fast today, only two hours"),
    c("position 1400 in the queue, i've had shorter careers"),
    c("never log out. the queue is waiting for you like an old enemy"),
    c("staying online so i never have to queue again, the true endgame"),
];
const REGRADE: &[Line] = &[
    c("regrade went great, my weapon is now a nice pile of fragments"),
    c("{x}, tried to regrade and the anvil laughed at me"),
    c("regrade luck has a personal grudge against me"),
    c("i stopped regrading, now i just throw gold into the sea. same result"),
    c("pressed regrade, closed my eyes, heard the crunch. classic"),
    c("my weapon went up one grade and down three. net progress!"),
    m("regrade failed again. damn anvil"),
];
const LUNAGEM: &[Line] = &[
    c("lunagem failed again. the moon hates me personally"),
    c("socketing gems is just gambling with nicer menus"),
    c("three lunagems, three fails. i'm a statistic now"),
    c("the lunagem success rate is a myth spread by people who quit"),
];
const FARM: &[Line] = &[
    c("planted a tree, lightning hit it before it grew. thunderstruck, how nice"),
    c("left my crops on public land for one hour and a stranger harvested them"),
    c("my farm is mostly a buffet for passers-by at this point"),
    c("{x}, someone harvested my potatoes again. enjoy them, mystery thief"),
    c("planted my seeds at 3am so nobody would steal them. stolen at 3:05"),
    c("thunderstruck trees: the weather helping with your woodcutting"),
];
const MIRAGE: &[Line] = &[
    c("mirage isle: where the shop prices are a mirage too"),
    c("every trip to mirage isle ends with me broke and holding a costume"),
    c("went to mirage isle for one thing, came back with nine things and no gold"),
];
const DIAMOND: &[Line] = &[
    c("diamond shores: the beach where nobody gets a tan, only a respawn timer"),
    c("heading to diamond shores, tell my bags i loved them"),
    c("diamond shores is great if you like dying somewhere scenic"),
];
const SEA: &[Line] = &[
    c("my clipper is faster than me but slower than the pirate behind me"),
    c("sailing is relaxing until a pirate shows up with cannons and opinions"),
    c("rule of the sea: if it has a black sail, it's not here to chat"),
    c("i don't get seasick, i get pirate-sick"),
    c("built a boat, sailed it for 30 seconds, sank it on a rock. maiden voyage done"),
];
const HASLA: &[Line] = &[
    c("the hasla grind is a lifestyle, not a choice"),
    c("one more hasla run and i'm done. said that 40 runs ago"),
    c("dreamt about hasla mobs last night, the grind follows me"),
    c("hasla weapons: for when you want your social life to disappear"),
];
const CARROTS: &[Line] = &[
    c("never underestimate a carrot farmer, they run this economy"),
    c("came for epic battles, became a carrot farmer. no regrets"),
    c("the real endgame is a field of carrots nobody steals"),
    c("my main is a warrior, my soul is a carrot farmer"),
];
const LAND: &[Line] = &[
    c("land rush: 400 people, 12 plots, one dream"),
    c("finally got a housing plot. it's on a slope between two farms and a cliff, but it's mine"),
    c("the housing rush makes a black friday sale look peaceful"),
    c("if you see an empty plot, don't blink. it's gone"),
];
const LAG: &[Line] = &[
    c("the lag is so bad my skills cast yesterday"),
    c("{x}, rubberbanded right back where i started. thanks server"),
    c("i'm not afk, i'm lagging with confidence"),
    c("lag spike at the worst moment, as tradition demands"),
    c("my ping is higher than my gear score"),
    m("this lag is crap tonight"),
    s("fucking lag"),
];

/// Quips for small talk from a jokey bot.
const QUIPS: &[Line] = &[
    c("that's archeage for you"),
    c("lol, classic archeage"),
    c("blame the lag"),
    c("the kraken knows"),
    c("ask the regrade gods"),
    c("sounds like a labor problem"),
    c("same thing happened to my pack"),
    c("have you tried farming carrots about it?"),
    c("the answer is always more labor potions"),
    c("probably the gankers' fault"),
    c("i'd tell you, but my labor ran out"),
];
/// Coarse answers to small talk from a crude bot.
const CRUDE: &[Line] = &[
    m("haha damn"),
    m("hell yeah"),
    m("lol wtf"),
    m("damn, true"),
    m("crap, really?"),
    m("holy crap lol"),
    s("holy shit lol"),
    s("fuck yeah"),
    s("shit, really?"),
];

/// A joke on a random topic that neither this bot (`recent`) nor, as far as `told` says, another
/// one told lately; when all were told, any. The key, the text and the topic.
pub fn joke<const N: usize>(
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent<N>,
    told: &[u16],
    rng: &mut fastrand::Rng,
) -> Option<(u16, String, Topic)> {
    let n = Topic::ALL.len();
    let start = rng.usize(..n);
    for k in 0..n {
        let t = Topic::ALL[(start + k) % n];
        if let Some((key, text)) = pick_fresh(t.lines(), t.cat(), slots, tongue, recent, told, rng)
        {
            return Some((key, text, t));
        }
    }
    let t = Topic::ALL[start];
    pick(t.lines(), t.cat(), slots, tongue, recent, told, rng).map(|(k, text)| (k, text, t))
}

/// A quip for small talk.
pub fn quip(tongue: Tongue, recent: &mut Recent, rng: &mut fastrand::Rng) -> Option<String> {
    pick(QUIPS, CAT_QUIP, &Slots::default(), tongue, recent, &[], rng).map(|(_, t)| t)
}

/// A coarse answer to small talk; `None` for a clean tongue.
pub fn crude_reply(tongue: Tongue, recent: &mut Recent, rng: &mut fastrand::Rng) -> Option<String> {
    if tongue.level == Profanity::Off {
        return None;
    }
    pick(
        CRUDE,
        CAT_CRUDE,
        &Slots::default(),
        tongue,
        recent,
        &[],
        rng,
    )
    .map(|(_, t)| t)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use pb_llm::persona::profanity_of;

    /// The tables of this module, for the checks in [`super::super::lines`].
    pub(crate) fn tables() -> Vec<(&'static str, &'static [Line])> {
        let mut t: Vec<(&'static str, &'static [Line])> =
            Topic::ALL.iter().map(|t| ("joke", t.lines())).collect();
        t.push(("quips", QUIPS));
        t.push(("crude", CRUDE));
        t
    }

    #[test]
    fn there_are_several_dozen_jokes_on_every_topic() {
        let total: usize = Topic::ALL.iter().map(|t| t.lines().len()).sum();
        assert!(total >= 60, "{total} jokes");
        for t in Topic::ALL {
            let clean = t
                .lines()
                .iter()
                .filter(|l| l.level == Profanity::Off)
                .count();
            assert!(clean >= 3, "{t:?}");
            assert!(!t.hint().is_empty() && !t.hint().contains('\n'));
        }
        // the categories of the topics do not run into the quips
        assert!(Topic::Lag.cat() < CAT_QUIP);
    }

    #[test]
    fn a_bot_tells_no_joke_twice_in_a_row_and_skips_what_others_told() {
        let mut rng = fastrand::Rng::with_seed(7);
        let mut recent = Recent::<8>::default();
        let mut told: Vec<u16> = Vec::new();
        let mut topics = std::collections::HashSet::new();
        for _ in 0..60 {
            let (k, text, topic) = joke(
                &Slots::default(),
                Tongue::CLEAN,
                &mut recent,
                &told,
                &mut rng,
            )
            .unwrap();
            assert!(!told.contains(&k), "{text} was told lately");
            assert_eq!(profanity_of(&text), Profanity::Off);
            told.push(k);
            if told.len() > 16 {
                told.remove(0);
            }
            topics.insert(topic as u8);
        }
        assert!(topics.len() >= 10, "{} topics", topics.len());
    }

    #[test]
    fn quips_and_coarse_answers_follow_the_tongue() {
        let mut rng = fastrand::Rng::with_seed(8);
        let mut recent = Recent::default();
        assert!(crude_reply(Tongue::CLEAN, &mut recent, &mut rng).is_none());
        let mild = Tongue {
            level: Profanity::Mild,
            crude: 1.0,
        };
        for _ in 0..50 {
            let line = crude_reply(mild, &mut recent, &mut rng).unwrap();
            assert_eq!(profanity_of(&line), Profanity::Mild, "{line}");
            let q = quip(Tongue::CLEAN, &mut recent, &mut rng).unwrap();
            assert_eq!(profanity_of(&q), Profanity::Off, "{q}");
        }
    }
}
