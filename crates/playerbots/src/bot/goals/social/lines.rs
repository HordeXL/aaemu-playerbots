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
    c("唉"),
    c("啊"),
    c("行吧"),
    c("好家伙"),
    c("无语"),
    c("服了"),
    c("不是吧"),
    c("真是"),
    c("天哪"),
    c("来吧"),
    c("哎呦"),
    c("哦不"),
    m("我去"),
    m("靠"),
    m("擦"),
    m("坑爹"),
    m("妈的"),
    m("见鬼"),
    m("晕死"),
    s("卧槽"),
    s("我操"),
    s("tmd"),
    s("他妈的"),
    s("尼玛"),
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
    c("打得好"),
    c("打得好 {n}"),
    c("精彩，打得漂亮"),
    c("精彩 {n}"),
    c("领教了 {n}"),
];
const TRADE_DONE: &[Line] = &[
    c("谢了 {n}"),
    c("多谢交易 {n}"),
    c("合作愉快"),
    c("成交，回头见 {n}"),
];
const HELLO: &[Line] = &[
    c("嘿 {n}"),
    c("你好 {n}"),
    c("嗨 {n}"),
    c("哟 {n}"),
    c("你好啊 {n}"),
];
const DUEL_WON: &[Line] = &[
    c("打得好 {n}"),
    c("打得好 {n}，就差一点"),
    c("精彩 {n}"),
    c("你差点就赢了我 {n}"),
    m("打得好 {n}，妈的，真险"),
];
const DUEL_LOST: &[Line] = &[
    c("打得好 {n}，我认输了"),
    c("打得好 {n}，改日再战"),
    c("领教了 {n}，下次赢回来"),
    c("打得好，下次是我 {n}"),
    m("打得好 {n}，靠，真险"),
];
const LEVEL_UP: &[Line] = &[
    c("升级！"),
    c("升到 {lv} 级了"),
    c("升到 {lv} 级，总算"),
    c("升级！继续肝"),
    c("升到 {lv} 级，劳动点还是不够用"),
    m("升到 {lv} 级，妈的，总算"),
    s("升到 {lv} 级，卧槽，爽"),
];
const DIED: &[Line] = &[
    c("我阵亡了"),
    c("这下可好"),
    c("哎呦"),
    c("又躺了"),
    c("{x}，真疼"),
    c("下次再也不这么干了"),
    c("又回复活点了"),
    m("真是，靠"),
    s("卧槽，真疼"),
];
const GANKED: &[Line] = &[
    c("又被偷袭了，经典上古世纪"),
    c("{x}，在路上被偷袭，大自然循环"),
    c("所以我从来不单独赶路"),
    c("被劫了，佩服他们的执着"),
    c("大道上又倒下一个"),
    c("{x}，人都没看见就躺了"),
    m("又被偷袭，每次都这样，靠"),
];
const QUEST_DONE: &[Line] = &[
    c("任务完成，下一个！"),
    c("又交了一个"),
    c("清单上又少一项"),
    c("任务列表终于没那么吓人了"),
    m("那个任务总算做完了，坑爹"),
];
const PACK_SOLD: &[Line] = &[
    c("货包出手，今天没遇上劫道的！"),
    c("背着货包一路无恙，像中了彩票"),
    c("特产卖掉了，阔气个把小时"),
    c("送达！我的腰终于谢谢我"),
    m("货包出手，我去，美滋滋"),
];
const HARVEST: &[Line] = &[
    c("收完了，这次没人偷"),
    c("庄稼进仓，农场还活着"),
    c("收获的季节，最喜欢了"),
    c("庄稼熬过了一夜，奇迹"),
    m("收完了，居然一个偷菜的都没有，靠"),
];
const HOUSE_DONE: &[Line] = &[
    c("房子盖好了！属于自己的一小块地"),
    c("头上总算有片屋顶了"),
    c("房子完工。暖房派对有人来吗？开玩笑"),
    c("盖好了！接下来是买不起的家具"),
    m("房子完工，我去，美滋滋"),
];
const LOOT: &[Line] = &[
    c("背包又满了，战利品没完没了"),
    c("装备雨！不过大多是垃圾"),
    c("我现在是个移动杂货铺"),
    c("{x}，背包又满了"),
    c("永远缺背包格子"),
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
    c("今天天气不错"),
    c("这儿比平时热闹"),
    c("再做最后一个任务就睡。这话说了三个小时了"),
    c("地图上又全是问号"),
    c("这里的日落百看不厌"),
    c("{z} 今天挺热闹"),
    c("又是 {z}，都快成第二个家了"),
    c("药水快见底了，该补货了"),
    c("我的坐骑也该放个假了"),
    c("喜欢这附近的音乐"),
    c("背包满，劳动空，老样子"),
    c("你们也听见了吗？大概是只狼"),
    c("哪天得记得修装备"),
    c("{x}，又忘了修装备"),
    m("靠，这风景绝了"),
    m("今天外面真热，我去"),
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
        chat_type::SHOUT => "大喊",
        chat_type::TRADE => "交易",
        chat_type::GROUP_FIND => "组队",
        _ => "频道",
    }
}

// (no channel line is market talk: the market reads these channels, see the tests)
const SHOUT: &[Line] = &[
    c("卡的不只有我一个吧？"),
    c("希望海怪别刷出来，我还在船上"),
    c("晚上好啊各位，劳动点已经见底了"),
    c("友情提醒：多喝水，别一个人航海"),
    c("致敬今晚熬过排队的大伙"),
    c("今晚服务器是变快了还是我在做梦？乌鸦嘴了"),
    c("我刚给武器改级了。节哀顺便发我一份"),
    c("记住：每个劫包的都是曾经背包的"),
    c("全世界的胡萝卜农夫，联合起来"),
    c("唉，又是延迟"),
    c("种树挡路的那些人，劫匪给你们点赞"),
    c("今晚海面很平静。平静得可疑，海盗式的平静"),
    c("改级赔了武器又被抢了货包。总体不错的一天"),
    m("靠，今天排队排得真惨"),
    m("靠，又是延迟"),
    m("今晚的路上，见鬼"),
    s("这破延迟，他妈的，每晚都这样"),
];
const TRADE: &[Line] = &[
    c("经济全靠胡萝卜和眼泪撑着"),
    c("要是钱能从树上长出来，早有人把树偷了"),
    c("我的存款和劳动条一样空"),
    c("冷知识：历史上每件特产都有劫匪目击记录"),
    c("交易频道诗一首：红萝卜，货包沉，棵棵树后藏劫人"),
    c("理财建议：改级不是投资"),
    c("我的劳动条比钱包还空，这话有点扎心"),
    m("靠，这些货包真沉"),
];
const LFG: &[Line] = &[
    c("今晚真有人打副本吗，还是都在刷怪？"),
    c("野队奶妈都是传说，不服来辩"),
    c("队伍在一号 boss 把我甩了，所以我算独狼玩家了"),
    c("进了队，打了个招呼，队散了。是我说错话了？"),
    c("真正的副本是我们在排队里失去的朋友"),
    c("坦克说直接拉怪。经典遗言"),
    c("今晚还有人在哈斯拉刷怪吗？有人陪才不惨"),
    c("唉，奶妈打到一半去挂机了"),
    m("又团灭了，妈的"),
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
        assert_eq!(channel_name(chat_type::SHOUT), "大喊");
    }
}
