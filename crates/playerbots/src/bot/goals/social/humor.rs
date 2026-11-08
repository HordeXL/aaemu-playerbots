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
    c("劳动点：唯一一种我永远缺的货币"),
    c("上线四分钟把劳动点花光，现在只能站会儿"),
    c("{x}，劳动又没了。那就看看风景吧"),
    c("我不缺金币，我缺劳动药水和一觉"),
    c("每次打开制作界面，劳动点就吓得跑光"),
    m("劳动恢复比蜗牛还慢，靠"),
];
const PACKS: &[Line] = &[
    c("背着货包步行横穿大陆，电子游戏最刺激的玩法"),
    c("我发誓，去交货点的路上每丛灌木里都蹲着一个劫匪"),
    c("{x}，离交货点二十米被截，太刺激了"),
    c("背着货包，望着地平线，心率两百"),
    c("小技巧：劫匪永远蹲在你没检查的那块石头后面"),
    c("背着货包横穿整个大陆，报酬够买个三明治"),
];
const FREEDICH: &[Line] = &[
    c("背着货包去自由岛，就是给海盗捐款，流程多了几步"),
    c("海怪一刷，所有人突然想起自己有条船"),
    c("自由岛：为金币而来，因为船沉了而留下"),
    c("我见过一次海怪。从海底见的。当时是个鬼魂"),
    c("打海怪技巧：别在它底下"),
];
const QUEUE: &[Line] = &[
    c("排队磨炼人的性格。主要是耐心"),
    c("今天排队真快，才排了两个小时"),
    c("排队第 1400 位，我打过比这还短的副本"),
    c("千万别下线。队列像个老对手一样等着你"),
    c("一直在线就不用再排队，这才是真正的毕业"),
];
const REGRADE: &[Line] = &[
    c("改级大成功，我的武器现在是一堆漂亮的碎片"),
    c("{x}，去改级，铁砧都在笑话我"),
    c("改级运气跟我有私仇"),
    c("我不改级了，现在直接把金币扔海里，效果一样"),
    c("按下改级，闭上眼，听见咔嚓一声。经典"),
    c("武器升了一阶又掉三阶。净进步！"),
    m("又改级失败，这破铁砧，妈的"),
];
const LUNAGEM: &[Line] = &[
    c("月长石又碎了。月亮针对我"),
    c("镶宝石就是界面好看点的赌博"),
    c("三颗月长石三连败，我现在是个统计数据"),
    c("月长石成功率是弃坑的人编出来的传说"),
];
const FARM: &[Line] = &[
    c("种了棵树，还没长大就被雷劈了。天雷木，真好"),
    c("庄稼在公共农场放了一小时，被陌生人收走了"),
    c("我的农场现在基本是路人的自助餐"),
    c("{x}，我的土豆又被收了。神秘小偷，慢用"),
    c("凌晨三点偷着播种防止被偷，三点零五被偷"),
    c("天雷木：老天爷帮你砍树"),
];
const MIRAGE: &[Line] = &[
    c("幻影岛：连商店价格都是海市蜃楼"),
    c("每次去幻影岛都以破产收场，手里还拿着件时装"),
    c("去幻影岛本来只想买一件东西，回来带了九件，金币没了"),
];
const DIAMOND: &[Line] = &[
    c("钻石海岸：没人晒成日光浴，只有复活倒计时"),
    c("要去钻石海岸了，跟我的背包说我爱它们"),
    c("钻石海岸不错，如果你喜欢在风景优美的地方躺尸"),
];
const SEA: &[Line] = &[
    c("我的快船比我快，但比我后面的海盗慢"),
    c("航海很惬意，直到一个带着火炮和见解的海盗出现"),
    c("海上规矩：挂黑帆的船不是来聊天的"),
    c("我不晕船，我晕海盗"),
    c("造了条船，开了三十秒，撞礁沉了。首航结束"),
];
const HASLA: &[Line] = &[
    c("刷哈斯拉是一种生活方式，不是选择"),
    c("再刷一趟哈斯拉就收。这话四十趟前就说过"),
    c("昨晚梦见哈斯拉的怪，肝随人走"),
    c("哈斯拉武器：专治社交生活太丰富"),
];
const CARROTS: &[Line] = &[
    c("永远不要小看种胡萝卜的，这个经济是他们撑起来的"),
    c("本来为了史诗大战来的，结果成了胡萝卜农夫。不后悔"),
    c("真正的毕业是一块没人偷的胡萝卜地"),
    c("我的大号是战士，我的灵魂是胡萝卜农夫"),
];
const LAND: &[Line] = &[
    c("抢地：四百人，十二块地，一个梦"),
    c("终于拿到一块地。在两座农场和一处悬崖之间的坡上，但它是我的"),
    c("抢地的场面能让黑色星期五显得岁月静好"),
    c("看见空地别眨眼。眨眼就没了"),
];
const LAG: &[Line] = &[
    c("延迟高到我的技能昨天才读出来"),
    c("{x}，又弹回出生点了。谢谢服务器"),
    c("我不是挂机，我是自信地卡着"),
    c("最关键的时候跳延迟，传统艺能"),
    c("我的延迟比我的装分还高"),
    m("今晚这延迟，擦"),
    s("这破延迟，卧槽"),
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
    m("哈哈，我去"),
    m("妈的，爽"),
    m("我靠，不是吧"),
    m("妈的，确实"),
    m("擦，真的假的？"),
    m("我去，好家伙"),
    s("卧槽，哈哈"),
    s("卧槽，爽"),
    s("我操，真的假的？"),
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
