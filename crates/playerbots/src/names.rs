//! Pronounceable Chinese character names.
//!
//! The server accepts `[a-zA-Z0-9\p{Han}]{1,18}` (its `CharacterNameRegex`), so a bot may
//! carry a Chinese name. We pair a family name with a given name the way Chinese names are
//! built, so the result reads like a real name.

/// Shortest generated name.
pub const MIN_LEN: usize = 2;
/// Longest generated name.
pub const MAX_LEN: usize = 3;

/// Family names (one character, or two for the compound ones).
const FAMILY: &[&str] = &[
    "李", "张", "王", "刘", "陈", "赵", "黄", "周", "吴", "徐", "孙", "胡", "朱", "高", "何", "郭",
    "马", "罗", "梁", "宋", "郑", "谢", "韩", "唐", "冯", "于", "董", "萧", "程", "曹", "袁", "邓",
    "许", "傅", "沈", "曾", "彭", "吕", "苏", "卢", "蒋", "蔡", "贾", "丁", "魏", "薛", "叶", "余",
    "潘", "杜", "戴", "夏", "钟", "汪", "田", "任", "姜", "范", "方", "石", "姚", "谭", "廖", "邹",
    "熊", "金", "陆", "郝", "孔", "白", "崔", "康", "毛", "邱", "秦", "江", "史", "顾", "侯", "邵",
    "孟", "万", "段", "雷", "钱", "汤", "尹", "黎", "易", "常", "武", "乔", "贺", "赖", "龚", "文",
    "庞", "樊", "殷", "施", "陶", "洪", "翟", "安", "颜", "倪", "严", "牛", "温", "芦", "季", "俞",
    "章", "鲁", "葛", "奚", "彭", "郎", "韦", "昌", "苗", "凤", "花", "鲍", "费", "廉", "岑", "郁",
    "单", "杭", "包", "诸", "左", "祝", "蓝", "闵", "席", "麻", "路", "娄", "危", "童", "谷", "凌",
    "霍", "虞", "支", "柯", "昝", "管", "莫", "经", "房", "裘", "缪", "干", "解", "应", "宗", "宣",
    "贲", "湛", "祁", "禹", "狄", "米", "贝", "臧", "计", "伏", "成", "谈", "茅", "纪", "舒", "屈",
    "项", "阮", "辜", "羿", "储", "靳", "汲", "糜", "邬", "乐", "时", "皮", "卞", "齐", "伍", "元",
    "卜", "和", "穆", "勾", "敖", "融", "冷", "訾", "辛", "阚", "那", "简", "饶", "空", "毋", "沙",
    "乜", "养", "鞠", "须", "丰", "巢", "关", "蒯", "相", "查", "荆", "红", "游", "竺", "权", "逯",
    "盖", "益", "桓", "欧阳", "司马", "上官", "夏侯", "诸葛", "闻人", "东方", "赫连", "皇甫",
    "尉迟", "公羊", "澹台", "公冶", "宗政", "濮阳", "淳于", "轩辕", "令狐", "钟离", "宇文", "长孙",
    "慕容", "鲜于", "闾丘", "司徒", "司空", "亓官", "司寇", "子车", "颛孙", "端木", "巫马", "公西",
    "漆雕", "乐正", "壤驷", "公良", "拓跋", "夹谷", "宰父", "谷梁", "段干", "百里", "东郭", "南门",
    "呼延", "归海", "羊舌", "微生", "帅", "缑", "亢", "况", "有", "琴", "商", "牟", "佘", "佴",
    "伯", "赏", "墨", "哈", "谯", "笪", "年", "爱", "佟", "第五", "言", "福",
];

/// Given names (one or two characters).
const GIVEN: &[&str] = &[
    "伟", "芳", "娜", "秀英", "丽", "强", "磊", "洋", "超", "波", "杰", "勇", "娟", "涛", "春",
    "静", "兰", "琳", "琴", "云", "峰", "洁", "鑫", "鹏", "飞", "燕", "斌", "龙", "丹", "薇", "倩",
    "雪", "宁", "晨", "阳", "宇", "浩", "轩", "博", "凯", "瑞", "泽", "俊", "辉", "嘉", "志", "松",
    "柏", "林", "森", "枫", "柳", "杨", "竹", "菊", "荷", "莲", "桂", "桃", "杏", "月", "霜", "露",
    "翠", "桐", "梧", "桦", "芸", "茜", "蓉", "菱", "珊", "莹", "蕊", "颖", "琪", "瑶", "淑", "惠",
    "秀", "珍", "凤", "琴", "巧", "美", "淑华", "桂英", "秀兰", "淑芬", "玉兰", "丽华", "桂兰",
    "春梅", "秀珍", "凤英", "玉兰", "桂芳", "春花", "秀梅", "淑华",
];

/// Words no generated name may contain.
const BLOCKED: &[&str] = &[
    "gm", "admin", "sysop", "staff", "fuck", "shit", "cunt", "kunt", "nigg", "rape", "nazi",
    "hitler", "dick", "cock", "slut", "whore", "anus", "porn", "sex", "死", "垃圾", "蠢",
];

/// Whether `c` is a Chinese character (the CJK unified ideographs the server allows).
fn is_han(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
}

/// Whether `name` is a well-formed generated name: 2..=3 characters, all of them Chinese.
pub fn is_acceptable(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    if !(MIN_LEN..=MAX_LEN).contains(&chars.len()) {
        return false;
    }
    if !chars.iter().all(|c| is_han(*c)) {
        return false;
    }
    let lower = name.to_lowercase();
    !BLOCKED.iter().any(|w| lower.contains(w))
}

/// One Chinese name: a family name and a given name, sometimes a second given name.
pub fn generate(rng: &mut fastrand::Rng) -> String {
    loop {
        let mut name = String::with_capacity(12);
        name.push_str(FAMILY[rng.usize(..FAMILY.len())]);
        // A compound family name plus a one-character given name already fills the budget.
        let given = GIVEN[rng.usize(..GIVEN.len())];
        name.push_str(given);
        if name.chars().count() < MAX_LEN && rng.u8(0..3) == 0 {
            name.push_str(GIVEN[rng.usize(..GIVEN.len())]);
        }
        if is_acceptable(&name) {
            return name;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_are_well_formed() {
        let mut rng = fastrand::Rng::with_seed(7);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..5000 {
            let n = generate(&mut rng);
            assert!(is_acceptable(&n), "{n}");
            assert!((MIN_LEN..=MAX_LEN).contains(&n.chars().count()), "{n}");
            seen.insert(n);
        }
        // A large pool: collisions are rare enough that the roster can retry cheaply.
        assert!(seen.len() > 1500, "only {} distinct names", seen.len());
    }

    #[test]
    fn acceptance_rules() {
        assert!(is_acceptable("李白"));
        assert!(is_acceptable("欧阳明"));
        assert!(!is_acceptable("李")); // too short
        assert!(!is_acceptable("欧阳明德")); // too long
        assert!(!is_acceptable("LiBai")); // ASCII is not a Chinese name
        assert!(!is_acceptable("李2")); // digits are not allowed
    }

    #[test]
    fn generation_is_deterministic_for_a_seed() {
        let a = generate(&mut fastrand::Rng::with_seed(99));
        let b = generate(&mut fastrand::Rng::with_seed(99));
        assert_eq!(a, b);
    }
}
