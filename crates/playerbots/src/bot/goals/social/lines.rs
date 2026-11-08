//! The lines a bot says unasked: remarks on what happened to it, ambient lines near a real player,
//! lines on the public channels, and the exclamations that season them. Every template carries
//! the coarsest language it uses ([`Line::level`]); a bot only picks the templates its [`Tongue`]
//! allows, a crude bot leans towards the coarse ones, and every line is filtered once more before
//! it is sent ([`pb_llm::persona::sanitize_chat`]).
//!
//! Slots: `{n}` a name (dropped with the space in front when there is none), `{lv}` the new level,
//! `{z}` the zone (a template with `{z}` is skipped when the zone is unknown) and `{x}` an
//! exclamation the bot's tongue allows ("ugh" for a clean one, "damn" or worse for a crude one).

use aa_proto::cs::chat_type;
use pb_llm::persona::Profanity;

use super::canned::{self, Recent};

/// A template and the coarsest language it uses: the lowest profanity level it may be said at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// The level the template needs.
    pub level: Profanity,
    /// The template (see the module docs for the slots).
    pub text: &'static str,
}

/// A clean template.
pub const fn c(text: &'static str) -> Line {
    Line {
        level: Profanity::Off,
        text,
    }
}

/// A template with mild swearing.
pub const fn m(text: &'static str) -> Line {
    Line {
        level: Profanity::Mild,
        text,
    }
}

/// A template with strong swearing.
pub const fn s(text: &'static str) -> Line {
    Line {
        level: Profanity::Strong,
        text,
    }
}

/// How a bot swears: the coarsest level it may use and how much it likes to (0..=1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tongue {
    /// The coarsest level (its own under the operator's cap).
    pub level: Profanity,
    /// How strongly it leans towards the coarse templates.
    pub crude: f32,
}

impl Tongue {
    /// A clean tongue.
    pub const CLEAN: Tongue = Tongue {
        level: Profanity::Off,
        crude: 0.0,
    };
}

/// What fills the slots of a template.
#[derive(Debug, Clone, Copy, Default)]
pub struct Slots<'a> {
    /// `{n}`: the name of the one spoken to (empty: dropped).
    pub name: &'a str,
    /// `{lv}`: a level (0: templates with the slot are skipped).
    pub level: u8,
    /// `{z}`: the zone the bot is in (`None`: templates with the slot are skipped).
    pub zone: Option<&'a str>,
}

/// The key of template `idx` of category `cat` in a [`Recent`] memory.
pub fn key(cat: u16, idx: usize) -> u16 {
    cat << 8 | idx as u16
}

// The categories (the high byte of a key): 1..=7 and 0x81..=0x87 are the replies of
// [`canned`], 0x20.. the joke topics of [`super::humor`], 0x30.. the quips.
const CAT_AMBIENT: u16 = 0x38;
const CAT_SHOUT: u16 = 0x39;
const CAT_TRADE: u16 = 0x3a;
const CAT_LFG: u16 = 0x3b;
const CAT_CHATTER: u16 = 0x40;

fn fits(line: &Line, slots: &Slots<'_>, tongue: Tongue) -> bool {
    line.level <= tongue.level
        && (slots.zone.is_some() || !line.text.contains("{z}"))
        && (slots.level > 0 || !line.text.contains("{lv}"))
}

/// A crude tongue picks the coarse templates more often: as often as a clean one at 0.43,
/// twice as often at 1.
fn weight(line: &Line, tongue: Tongue) -> f32 {
    if line.level == Profanity::Off {
        1.0
    } else {
        0.25 + 1.75 * tongue.crude.clamp(0.0, 1.0)
    }
}

fn choose(
    table: &[Line],
    ok: impl Fn(usize, &Line) -> bool,
    tongue: Tongue,
    rng: &mut fastrand::Rng,
) -> Option<usize> {
    let total: f32 = table
        .iter()
        .enumerate()
        .filter(|(i, l)| ok(*i, l))
        .map(|(_, l)| weight(l, tongue))
        .sum();
    if total <= 0.0 {
        return None;
    }
    let mut r = rng.f32() * total;
    let mut last = None;
    for (i, l) in table.iter().enumerate() {
        if !ok(i, l) {
            continue;
        }
        last = Some(i);
        r -= weight(l, tongue);
        if r < 0.0 {
            return Some(i);
        }
    }
    last
}

/// A template of `table` that fits the slots and the tongue and was not used lately, neither by
/// this bot (`recent`) nor, as far as `avoid` says, by another one; rendered. `None` when every
/// template that fits was used lately.
pub fn pick_fresh<const N: usize>(
    table: &'static [Line],
    cat: u16,
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent<N>,
    avoid: &[u16],
    rng: &mut fastrand::Rng,
) -> Option<(u16, String)> {
    let i = choose(
        table,
        |i, l| {
            let k = key(cat, i);
            fits(l, slots, tongue) && !recent.has(k) && !avoid.contains(&k)
        },
        tongue,
        rng,
    )?;
    recent.push(key(cat, i));
    Some((key(cat, i), render(table[i].text, slots, tongue, rng)))
}

/// [`pick_fresh`], and when every fitting template was used lately any but the one used last.
/// `None` only when no template fits the slots and the tongue at all.
pub fn pick<const N: usize>(
    table: &'static [Line],
    cat: u16,
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent<N>,
    avoid: &[u16],
    rng: &mut fastrand::Rng,
) -> Option<(u16, String)> {
    if let Some(hit) = pick_fresh(table, cat, slots, tongue, recent, avoid, rng) {
        return Some(hit);
    }
    let last = recent.last();
    let i = choose(
        table,
        |i, l| fits(l, slots, tongue) && key(cat, i) != last,
        tongue,
        rng,
    )
    .or_else(|| choose(table, |_, l| fits(l, slots, tongue), tongue, rng))?;
    recent.push(key(cat, i));
    Some((key(cat, i), render(table[i].text, slots, tongue, rng)))
}

/// Fills the slots of `template`.
pub fn render(
    template: &str,
    slots: &Slots<'_>,
    tongue: Tongue,
    rng: &mut fastrand::Rng,
) -> String {
    let mut text = template.to_string();
    if text.contains("{x}") {
        text = text.replace("{x}", exclamation(tongue, rng));
    }
    if text.contains("{lv}") {
        text = text.replace("{lv}", &slots.level.to_string());
    }
    if text.contains("{z}") {
        text = text.replace("{z}", slots.zone.unwrap_or_default());
    }
    canned::render(&text, slots.name)
}

/// Exclamations, from "ugh" to the strong ones; a bot picks one its tongue allows.
pub const EXCLAMATIONS: &[Line] = &[
    c("ugh"),
    c("argh"),
    c("welp"),
    c("oof"),
    c("sigh"),
    c("jeez"),
    c("oh come on"),
    c("great"),
    c("seriously"),
    c("dang"),
    c("oh no"),
    m("damn"),
    m("ffs"),
    m("wtf"),
    m("crap"),
    m("damn it"),
    m("oh hell"),
    m("holy crap"),
    s("fuck"),
    s("shit"),
    s("fml"),
    s("oh fuck"),
    s("holy shit"),
    s("fucking hell"),
];

/// An exclamation the tongue allows.
pub fn exclamation(tongue: Tongue, rng: &mut fastrand::Rng) -> &'static str {
    choose(EXCLAMATIONS, |_, l| l.level <= tongue.level, tongue, rng)
        .map_or("ugh", |i| EXCLAMATIONS[i].text)
}

// ---- remarks ---------------------------------------------------------------------------------

/// What a bot remarks on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chatter {
    /// After a duel (outcome unknown or a draw).
    DuelDone,
    /// After a trade.
    TradeDone,
    /// Meeting another bot.
    Hello,
    /// After a duel it won.
    DuelWon,
    /// After a duel it lost.
    DuelLost,
    /// A level up ("ding").
    LevelUp,
    /// Back from a death to a monster or a fall.
    Died,
    /// Back from a death to a character.
    Ganked,
    /// A quest turned in.
    QuestDone,
    /// A trade pack sold.
    PackSold,
    /// A crop harvested.
    Harvest,
    /// A house finished.
    HouseDone,
    /// The bag filled up with loot.
    Loot,
}

impl Chatter {
    /// Every kind.
    pub const ALL: [Chatter; 13] = [
        Chatter::DuelDone,
        Chatter::TradeDone,
        Chatter::Hello,
        Chatter::DuelWon,
        Chatter::DuelLost,
        Chatter::LevelUp,
        Chatter::Died,
        Chatter::Ganked,
        Chatter::QuestDone,
        Chatter::PackSold,
        Chatter::Harvest,
        Chatter::HouseDone,
        Chatter::Loot,
    ];

    /// The templates of this kind.
    pub fn table(self) -> &'static [Line] {
        match self {
            Chatter::DuelDone => DUEL_DONE,
            Chatter::TradeDone => TRADE_DONE,
            Chatter::Hello => HELLO,
            Chatter::DuelWon => DUEL_WON,
            Chatter::DuelLost => DUEL_LOST,
            Chatter::LevelUp => LEVEL_UP,
            Chatter::Died => DIED,
            Chatter::Ganked => GANKED,
            Chatter::QuestDone => QUEST_DONE,
            Chatter::PackSold => PACK_SOLD,
            Chatter::Harvest => HARVEST,
            Chatter::HouseDone => HOUSE_DONE,
            Chatter::Loot => LOOT,
        }
    }
}

const DUEL_DONE: &[Line] = &[
    c("gg"),
    c("gg {n}"),
    c("gg, good fight"),
    c("well fought {n}"),
    c("nice one {n}"),
];
const TRADE_DONE: &[Line] = &[
    c("thx {n}"),
    c("thanks for the trade {n}"),
    c("pleasure doing business"),
    c("deal, cheers {n}"),
];
const HELLO: &[Line] = &[
    c("hey {n}"),
    c("o/ {n}"),
    c("hi {n}"),
    c("yo {n}"),
    c("hello {n}"),
];
const DUEL_WON: &[Line] = &[
    c("gg {n}"),
    c("gg {n}, that was close"),
    c("good fight {n}"),
    c("gg, you almost had me {n}"),
    m("gg {n}, damn that was close"),
];
const DUEL_LOST: &[Line] = &[
    c("gg {n}, you got me"),
    c("gg {n}, rematch some day?"),
    c("well played {n}"),
    c("gg, i'll get you next time {n}"),
    m("gg {n}, damn, good fight"),
];
const LEVEL_UP: &[Line] = &[
    c("ding!"),
    c("ding {lv}"),
    c("ding {lv}, finally"),
    c("ding! the grind continues"),
    c("ding {lv}. now i run out of labor at a higher level"),
    m("ding {lv}, about damn time"),
    s("ding {lv}, fuck yeah"),
];
const DIED: &[Line] = &[
    c("rip me"),
    c("well that went great"),
    c("ow"),
    c("and down i go"),
    c("{x}, that hurt"),
    c("note to self: don't do that again"),
    c("back to the respawn point, again"),
    m("well, damn"),
    s("fuck, that hurt"),
];
const GANKED: &[Line] = &[
    c("ganked again, classic archeage"),
    c("{x}, ganked on the road. the circle of life"),
    c("and that's why i don't travel alone"),
    c("got ganked. respect the hustle i guess"),
    c("the road claims another one"),
    c("{x}, didn't even see them coming"),
    m("ganked again, every damn time"),
];
const QUEST_DONE: &[Line] = &[
    c("quest done, next!"),
    c("another one turned in"),
    c("that's one more off the list"),
    c("quest log slightly less scary now"),
    m("finally done with that damn quest"),
];
const PACK_SOLD: &[Line] = &[
    c("pack delivered, no gankers today!"),
    c("made it with the pack, feels like winning the lottery"),
    c("trade pack sold. i'm rich, for about an hour"),
    c("delivered! my back thanks me"),
    m("pack delivered, hell yeah"),
];
const HARVEST: &[Line] = &[
    c("harvest done, nobody stole anything this time"),
    c("crops are in, the farm lives"),
    c("harvest time, my favorite time"),
    c("the crops survived the night. a miracle"),
    m("harvest done and not a single damn thief"),
];
const HOUSE_DONE: &[Line] = &[
    c("house finished! my own little piece of land"),
    c("finally a roof over my head"),
    c("house is done. housewarming party anyone? kidding"),
    c("built it! now to fill it with furniture i can't afford"),
    m("house is done, hell yeah"),
];
const LOOT: &[Line] = &[
    c("my bags are full again, the loot never ends"),
    c("loot rain! mostly junk though"),
    c("i'm a walking junk shop now"),
    c("{x}, bags full again"),
    c("need more bag space, as always"),
];

/// A remark of `kind`, avoiding the templates in `recent`.
pub fn remark(
    kind: Chatter,
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent,
    rng: &mut fastrand::Rng,
) -> String {
    let cat = CAT_CHATTER | kind as u16;
    pick(kind.table(), cat, slots, tongue, recent, &[], rng)
        .map(|(_, text)| text)
        .unwrap_or_default()
}

// ---- ambient lines ---------------------------------------------------------------------------

/// What an ambient line is about, for the LLM.
pub const AMBIENT_HINT: &str = "something small about what you are doing or the place you are in";

const AMBIENT: &[Line] = &[
    c("nice day out here"),
    c("this place is busier than usual"),
    c("one more quest and then bed. said that three hours ago"),
    c("my map is all question marks again"),
    c("the sunset here never gets old"),
    c("{z} is pretty lively today"),
    c("{z} again. my second home at this point"),
    c("running low on potions, should restock"),
    c("my mount deserves a vacation"),
    c("love the music around here"),
    c("bags full, labor empty, the usual"),
    c("anyone else hear that? probably just a wolf"),
    c("i should really repair my gear one of these days"),
    c("{x}, forgot to repair my gear again"),
    m("damn, this view though"),
    m("hell of a day out here"),
];

/// An ambient line (small talk to nobody in particular), avoiding `recent` and `avoid`.
pub fn ambient<const N: usize>(
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent<N>,
    avoid: &[u16],
    rng: &mut fastrand::Rng,
) -> Option<(u16, String)> {
    pick(AMBIENT, CAT_AMBIENT, slots, tongue, recent, avoid, rng)
}

// ---- the public channels ---------------------------------------------------------------------

/// The public channels a bot may speak on.
pub const CHANNELS: [i16; 3] = [chat_type::SHOUT, chat_type::TRADE, chat_type::GROUP_FIND];

/// The channel's name for the log and the detail panel.
pub fn channel_name(kind: i16) -> &'static str {
    match kind {
        chat_type::SHOUT => "shout",
        chat_type::TRADE => "trade",
        chat_type::GROUP_FIND => "group finding",
        _ => "channel",
    }
}

// (no channel line is market talk: the market reads these channels, see the tests)
const SHOUT: &[Line] = &[
    c("is anyone else lagging or is it just me?"),
    c("the kraken better not be up, i'm on a boat"),
    c("good evening, world. my labor is gone already"),
    c("friendly reminder: drink water and don't sail alone"),
    c("shoutout to everyone who survived the queue tonight"),
    c("is the server fast tonight or am i dreaming? jinxed it"),
    c("i regraded my weapon. please send condolences"),
    c("remember: every pack ganker was once a pack carrier"),
    c("carrot farmers of the world, unite"),
    c("ugh, the lag again"),
    c("whoever keeps planting trees on the roads, the gankers thank you"),
    c("the sea is calm tonight. too calm. pirate calm"),
    c("lost my weapon to a regrade and my pack to a ganker. good day overall"),
    m("damn, the queue was brutal today"),
    m("damn, the lag again"),
    m("hell of a night on the roads"),
    s("fucking lag, every single night"),
];
const TRADE: &[Line] = &[
    c("the economy runs on carrots and tears"),
    c("if gold grew on trees, someone would steal the trees"),
    c("my bank account has the same energy as my labor bar"),
    c("fun fact: every trade pack in history had a ganker sighting"),
    c("trade chat poetry: roses are red, packs are heavy, a ganker hides behind every tree"),
    c("financial tip: regrading is not an investment"),
    c("my labor bar is emptier than my wallet, and that's saying something"),
    m("damn, these packs are heavy"),
];
const LFG: &[Line] = &[
    c("is anyone actually running dungeons tonight or are we all farming?"),
    c("pug healers are a myth, prove me wrong"),
    c("my party left me at the first boss, so i guess i'm a solo player now"),
    c("joined a group, said hi, the group disbanded. was it something i said?"),
    c("the real dungeon is the friends we lost in the queue"),
    c("tank says just pull. famous last words"),
    c("anyone else grinding hasla tonight? misery loves company"),
    c("ugh, the healer went afk mid fight"),
    m("party wiped again, damn it"),
];

/// A line for one of the public channels: the channel, the key and the text.
pub fn channel<const N: usize>(
    slots: &Slots<'_>,
    tongue: Tongue,
    recent: &mut Recent<N>,
    avoid: &[u16],
    rng: &mut fastrand::Rng,
) -> Option<(i16, u16, String)> {
    let r = rng.f32();
    let (kind, table, cat) = if r < 0.5 {
        (chat_type::SHOUT, SHOUT, CAT_SHOUT)
    } else if r < 0.75 {
        (chat_type::TRADE, TRADE, CAT_TRADE)
    } else {
        (chat_type::GROUP_FIND, LFG, CAT_LFG)
    };
    let (k, text) = pick(table, cat, slots, tongue, recent, avoid, rng)?;
    Some((kind, k, text))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use pb_llm::persona::{blocked, profanity_of, sanitize_chat};

    /// Slots as long as they get: the longest flat name, a two-digit level, a zone.
    pub(crate) const WORST: Slots<'static> = Slots {
        name: "Abcdefghijklmnopqrstuvwx",
        level: 55,
        zone: Some("Two Crowns"),
    };

    /// Every table of this module and of the humor module.
    pub(crate) fn all_tables() -> Vec<(&'static str, &'static [Line])> {
        let mut t: Vec<(&'static str, &'static [Line])> = vec![
            ("exclamations", EXCLAMATIONS),
            ("ambient", AMBIENT),
            ("shout", SHOUT),
            ("trade", TRADE),
            ("lfg", LFG),
        ];
        for k in Chatter::ALL {
            t.push(("chatter", k.table()));
        }
        t.extend(super::super::humor::tests::tables());
        t
    }

    /// Every way `line` can come out for a tongue at `level`.
    pub(crate) fn renderings(line: &Line, level: Profanity) -> Vec<String> {
        let mut out = Vec::new();
        let tongue = Tongue { level, crude: 1.0 };
        if line.text.contains("{x}") {
            for x in EXCLAMATIONS.iter().filter(|x| x.level <= level) {
                let t = line.text.replace("{x}", x.text);
                out.push(render(&t, &WORST, tongue, &mut fastrand::Rng::with_seed(1)));
            }
        } else {
            out.push(render(
                line.text,
                &WORST,
                tongue,
                &mut fastrand::Rng::with_seed(1),
            ));
        }
        out
    }

    #[test]
    fn every_line_passes_the_filter_at_its_own_level_and_none_is_a_command() {
        let mut n = 0;
        for (name, table) in all_tables() {
            assert!(!table.is_empty(), "{name}");
            for line in table {
                let t = line.text;
                assert!(
                    !t.trim_start().starts_with('/') && !t.trim().is_empty(),
                    "{t}"
                );
                assert!(t.chars().count() <= 100, "{t}");
                let lower = t.to_lowercase();
                assert!(!lower.contains("bot") && !lower.contains(" ai"), "{t}");
                assert!(!blocked(t), "{t}");
                assert_eq!(profanity_of(t), line.level, "{name}: {t} is marked wrongly");
                for level in Profanity::ALL.into_iter().filter(|l| *l >= line.level) {
                    for text in renderings(line, level) {
                        n += 1;
                        let out = sanitize_chat(&text, level);
                        assert_eq!(out.as_deref(), Some(text.as_str()), "{name} at {level:?}");
                        assert!(!text.contains('{'), "{text}");
                        assert!(super::super::wire_text(&text).is_some(), "{text}");
                    }
                }
            }
        }
        assert!(n > 500, "{n} renderings");
    }

    #[test]
    fn every_remark_has_a_clean_template_without_level_or_zone() {
        for k in Chatter::ALL {
            let plain = k
                .table()
                .iter()
                .filter(|l| l.level == Profanity::Off)
                .filter(|l| !l.text.contains("{lv}") && !l.text.contains("{z}"))
                .count();
            assert!(plain >= 2, "{k:?}");
            assert!(k.table().len() >= 4, "{k:?}");
        }
        assert!(AMBIENT.len() >= 12 && SHOUT.len() >= 12);
        assert!(TRADE.len() >= 6 && LFG.len() >= 6);
    }

    #[test]
    fn no_channel_line_is_market_talk() {
        use crate::bot::market::ads::grammar::parse;
        for table in [SHOUT, TRADE, LFG] {
            for line in table {
                for level in Profanity::ALL.into_iter().filter(|l| *l >= line.level) {
                    for text in renderings(line, level) {
                        let p = parse(&text);
                        assert!(!p.is_market() && !p.is_ad(), "{text:?} reads as {p:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_clean_tongue_never_picks_a_coarse_template_and_a_crude_one_leans_coarse() {
        let mut rng = fastrand::Rng::with_seed(3);
        let mut recent = Recent::<4>::default();
        for _ in 0..300 {
            let line = remark(Chatter::Died, &WORST, Tongue::CLEAN, &mut recent, &mut rng);
            assert_eq!(profanity_of(&line), Profanity::Off, "{line}");
            assert!(!line.is_empty());
        }
        let count = |tongue: Tongue, rng: &mut fastrand::Rng| {
            (0..2000)
                .filter(|_| {
                    EXCLAMATIONS
                        [choose(EXCLAMATIONS, |_, l| l.level <= tongue.level, tongue, rng).unwrap()]
                    .level
                        > Profanity::Off
                })
                .count()
        };
        let mild_polite = count(
            Tongue {
                level: Profanity::Mild,
                crude: 0.0,
            },
            &mut rng,
        );
        let mild_crude = count(
            Tongue {
                level: Profanity::Mild,
                crude: 1.0,
            },
            &mut rng,
        );
        assert!(
            mild_crude > mild_polite * 3,
            "{mild_crude} vs {mild_polite}"
        );
        assert_eq!(count(Tongue::CLEAN, &mut rng), 0);
    }

    #[test]
    fn slots_are_filled_and_unfillable_templates_skipped() {
        let mut rng = fastrand::Rng::with_seed(4);
        let mut recent = Recent::<8>::default();
        let none = Slots::default();
        for _ in 0..200 {
            let (_, text) = ambient(&none, Tongue::CLEAN, &mut recent, &[], &mut rng).unwrap();
            assert!(
                !text.contains('{') && !text.contains("Two Crowns"),
                "{text}"
            );
            let line = remark(
                Chatter::LevelUp,
                &none,
                Tongue::CLEAN,
                &mut Recent::default(),
                &mut rng,
            );
            assert!(!line.contains('{') && !line.contains(" 0"), "{line}");
        }
        let lv = Slots {
            level: 31,
            ..Slots::default()
        };
        let seen_level = (0..100).any(|_| {
            remark(
                Chatter::LevelUp,
                &lv,
                Tongue::CLEAN,
                &mut Recent::default(),
                &mut rng,
            )
            .contains("31")
        });
        assert!(seen_level);
        let named = remark(
            Chatter::Hello,
            &Slots {
                name: "Frida",
                ..Slots::default()
            },
            Tongue::CLEAN,
            &mut Recent::default(),
            &mut rng,
        );
        assert!(named.contains("Frida"), "{named}");
    }

    #[test]
    fn picks_do_not_repeat_and_avoid_what_others_said() {
        let mut rng = fastrand::Rng::with_seed(5);
        let mut recent = Recent::<8>::default();
        let mut last: Vec<u16> = Vec::new();
        for _ in 0..100 {
            let (k, _) = ambient(&WORST, Tongue::CLEAN, &mut recent, &[], &mut rng).unwrap();
            assert!(!last.contains(&k), "repeated within eight");
            last.push(k);
            if last.len() > 7 {
                last.remove(0);
            }
        }
        // a template another bot used lately is skipped while there are others
        let avoid: Vec<u16> = (0..AMBIENT.len() - 1)
            .map(|i| key(CAT_AMBIENT, i))
            .collect();
        let mut fresh = Recent::<8>::default();
        let mild = Tongue {
            level: Profanity::Mild,
            crude: 0.5,
        };
        let (k, _) = ambient(&WORST, mild, &mut fresh, &avoid, &mut rng).unwrap();
        assert_eq!(k, key(CAT_AMBIENT, AMBIENT.len() - 1));
        // and when all were used, something still comes out
        let all: Vec<u16> = (0..AMBIENT.len()).map(|i| key(CAT_AMBIENT, i)).collect();
        assert!(ambient(&WORST, Tongue::CLEAN, &mut fresh, &all, &mut rng).is_some());
    }

    #[test]
    fn channel_lines_go_to_the_three_public_channels() {
        let mut rng = fastrand::Rng::with_seed(6);
        let mut recent = Recent::<8>::default();
        let mut seen = [0; 3];
        for _ in 0..400 {
            let (kind, _, text) =
                channel(&WORST, Tongue::CLEAN, &mut recent, &[], &mut rng).unwrap();
            let i = CHANNELS.iter().position(|c| *c == kind).unwrap();
            seen[i] += 1;
            assert!(!text.is_empty());
        }
        assert!(seen.iter().all(|n| *n > 50), "{seen:?}");
        assert_eq!(channel_name(chat_type::SHOUT), "shout");
    }
}
