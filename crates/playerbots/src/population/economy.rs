//! The economy board: how scarce each item is on this server right now.
//!
//! Three kinds of evidence land here, all bounded:
//!
//! * **Price notes of the bots.** A bot that priced an item at the auction house remembers the
//!   market quote (median of the cheapest lots, the lowest lot, how many lots it saw, five at
//!   most). Once every few seconds each bot hands its fresh notes to [`Economy::note_price`]; the
//!   board keeps the latest note of [`MAX_OBS`] bots per item and a short price history.
//! * **Lines in chat.** A trade-chat line that offers or asks for an item ([`Economy::hear`]) counts
//!   as a sell or a buy line for [`CHAT_WINDOW_MS`]. All bots in a zone group hear the same line, so a
//!   line is counted once. A real player's line counts in full, a bot's a quarter (bots post
//!   stand-in wishes that must not drive the scarcity they would then react to).
//! * **The server's auction list** (optional, [`super::serverapi`]): the exact number of lots and the
//!   median price per item. When it is fresh it replaces the bots' guess at the supply.
//!
//! [`score`] turns the evidence into a number from 0 (plenty) to 1 (scarce): few lots, a price far
//! above what a vendor asks, a rising price and more people asking than offering all raise it. The
//! farming goal reads it through [`ScarcityView`].

use std::collections::VecDeque;

use aa_proto::cs::chat_type;
use parking_lot::Mutex;
use pb_web::{EconomySources, EconomyView, ItemRow};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;

use crate::bot::market::ads::grammar;
use crate::bot::market::ads::names::NameIndex;
use crate::bot::market::auction::{PriceNote, PRICE_TTL_MS};

/// An item of the server's list with more lots than this starts no price history of its own.
const HISTORY_MAX_LOTS: u32 = 5;
/// The most items the board follows.
pub const MAX_ITEMS: usize = 1500;
/// A price note of a bot is good for this long (milliseconds).
pub const OBS_FRESH_MS: u64 = PRICE_TTL_MS;
/// Buy and sell lines count for this long.
pub const CHAT_WINDOW_MS: u64 = 10 * 60_000;
/// The price trend looks back this far.
pub const TREND_WINDOW_MS: u64 = 30 * 60_000;
/// A new price sample is kept at most this often per item.
const HISTORY_GAP_MS: u64 = 2 * 60_000;
/// The server's list is trusted this long after it was fetched.
pub const SERVER_FRESH_MS: u64 = 10 * 60_000;
/// Bots whose latest note is kept per item.
const MAX_OBS: usize = 6;
/// Price samples kept per item.
const MAX_HISTORY: usize = 8;
/// Chat lines kept per item and side.
const MAX_LINES: usize = 12;
/// A chat line heard twice within this long is one line.
const DUP_WINDOW_MS: u64 = 20_000;
/// Remembered chat lines for the duplicate check.
const MAX_HEARD: usize = 256;
/// The weight of a player's chat line and of a bot's (quarters of a line).
const PLAYER_LINE: u8 = 4;
const BOT_LINE: u8 = 1;
/// The most distinct items of the server's list the board looks at.
const MAX_SERVER_ITEMS: usize = 4000;
/// Rows of the table the web UI gets.
pub const VIEW_ROWS: usize = 80;

/// What the board needs to know about an item.
pub trait ItemInfo {
    /// The English name.
    fn name(&self, template: u32) -> Option<String>;
    /// What a vendor asks for one item, copper; 0 when no vendor sells it.
    fn vendor_price(&self, template: u32) -> u32;
}

impl ItemInfo for aa_world::World {
    fn name(&self, template: u32) -> Option<String> {
        self.item_name(template).map(str::to_owned)
    }
    fn vendor_price(&self, template: u32) -> u32 {
        self.item_facts(template).map_or(0, |f| f.price)
    }
}

/// What the server's auction list says about one item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LotStat {
    /// Lots on the house.
    pub lots: u32,
    /// Items in them.
    pub units: u32,
    /// The lowest price per item (copper, buyout).
    pub lowest_unit: u32,
    /// The median price per item.
    pub median_unit: u32,
}

/// The server's whole auction list at one time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerLots {
    /// Unix milliseconds the list was fetched.
    pub at_ms: u64,
    /// Lots on the house.
    pub total: u32,
    pub items: FxHashMap<u32, LotStat>,
}

#[derive(Debug, Clone, Copy)]
struct Obs {
    bot: u32,
    at_ms: u64,
    unit: u32,
    lots: u8,
    listed: bool,
}

#[derive(Debug, Default)]
struct ItemStat {
    obs: SmallVec<[Obs; MAX_OBS]>,
    /// `(unix ms, price per item)`, oldest first.
    history: SmallVec<[(u64, u32); MAX_HISTORY]>,
    /// `(unix ms, weight)`.
    wtb: SmallVec<[(u64, u8); MAX_LINES]>,
    wts: SmallVec<[(u64, u8); MAX_LINES]>,
    touched_ms: u64,
}

impl ItemStat {
    fn push_history(&mut self, at_ms: u64, unit: u32) {
        if unit == 0 {
            return;
        }
        match self.history.last_mut() {
            Some(last) if at_ms.saturating_sub(last.0) < HISTORY_GAP_MS => last.1 = unit,
            _ => {
                if self.history.len() >= MAX_HISTORY {
                    self.history.remove(0);
                }
                self.history.push((at_ms, unit));
            }
        }
    }

    fn lines(list: &[(u64, u8)], now_ms: u64) -> f32 {
        let quarters: u32 = list
            .iter()
            .filter(|(t, _)| now_ms.saturating_sub(*t) < CHAT_WINDOW_MS)
            .map(|(_, w)| u32::from(*w))
            .sum();
        quarters as f32 / f32::from(PLAYER_LINE)
    }

    fn push_line(list: &mut SmallVec<[(u64, u8); MAX_LINES]>, at_ms: u64, weight: u8) {
        if list.len() >= MAX_LINES {
            list.remove(0);
        }
        list.push((at_ms, weight));
    }

    /// The price change over the trend window, percent; 0 without two samples far enough apart.
    fn trend_pct(&self, now_ms: u64) -> f32 {
        let recent: SmallVec<[&(u64, u32); MAX_HISTORY]> = self
            .history
            .iter()
            .filter(|(t, _)| now_ms.saturating_sub(*t) < TREND_WINDOW_MS)
            .collect();
        let (Some(first), Some(last)) = (recent.first(), recent.last()) else {
            return 0.0;
        };
        if last.0.saturating_sub(first.0) < HISTORY_GAP_MS || first.1 == 0 {
            return 0.0;
        }
        (last.1 as f32 - first.1 as f32) / first.1 as f32 * 100.0
    }
}

#[derive(Debug, Default)]
struct Inner {
    items: FxHashMap<u32, ItemStat>,
    server: Option<ServerLots>,
    /// `(hash of sender and text, unix ms)` of the chat lines counted lately.
    heard: VecDeque<(u64, u64)>,
}

impl Inner {
    /// The stat of `template`, made if there is room (the one touched longest ago makes room).
    fn stat(&mut self, template: u32, now_ms: u64) -> Option<&mut ItemStat> {
        if !self.items.contains_key(&template) && self.items.len() >= MAX_ITEMS {
            let oldest = self
                .items
                .iter()
                .min_by_key(|(_, s)| s.touched_ms)
                .map(|(k, s)| (*k, s.touched_ms));
            match oldest {
                Some((k, t)) if t < now_ms => {
                    self.items.remove(&k);
                }
                _ => return None,
            }
        }
        let s = self.items.entry(template).or_default();
        s.touched_ms = now_ms;
        Some(s)
    }
}

/// The board. See the module docs.
#[derive(Debug, Default)]
pub struct Economy {
    inner: Mutex<Inner>,
}

/// What a market quote of a bot says: `(median per item, lowest per item, lots seen)`.
pub type QuoteNote = (u32, u32, u8);

impl Economy {
    /// A bot priced `template`: `quote` is `None` when no lot of the item was up. `at_ms` is unix
    /// milliseconds.
    pub fn note_price(&self, bot: u32, template: u32, quote: Option<QuoteNote>, at_ms: u64) {
        let mut g = self.inner.lock();
        let Some(s) = g.stat(template, at_ms) else {
            return;
        };
        let obs = Obs {
            bot,
            at_ms,
            unit: quote.map_or(0, |q| q.0),
            lots: quote.map_or(0, |q| q.2),
            listed: quote.is_some(),
        };
        if let Some(o) = s.obs.iter_mut().find(|o| o.bot == bot) {
            if o.at_ms > at_ms {
                return;
            }
            *o = obs;
        } else {
            if s.obs.len() >= MAX_OBS {
                if let Some(i) = s
                    .obs
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, o)| o.at_ms)
                    .map(|(i, _)| i)
                {
                    s.obs.remove(i);
                }
            }
            s.obs.push(obs);
        }
        if quote.is_some() {
            s.push_history(at_ms, obs.unit);
        }
    }

    /// A trade-chat line was heard. `index` finds the item a line names; the line counts when it
    /// offers or asks for an item, once however many bots heard it, and not when it is a whisper.
    /// Returns whether it was counted.
    pub fn hear(
        &self,
        index: &NameIndex,
        from_bot: bool,
        kind: i16,
        sender: &str,
        text: &str,
        now_ms: u64,
    ) -> bool {
        if !matches!(
            kind,
            chat_type::SHOUT | chat_type::TRADE | chat_type::GROUP_FIND
        ) {
            return false;
        }
        let line = grammar::parse(text);
        if !line.is_ad() {
            return false;
        }
        let Some(template) = index.find(text) else {
            return false;
        };
        let key = line_key(sender, text);
        let mut g = self.inner.lock();
        while g
            .heard
            .front()
            .is_some_and(|(_, t)| now_ms.saturating_sub(*t) >= DUP_WINDOW_MS)
        {
            g.heard.pop_front();
        }
        if g.heard.iter().any(|(k, _)| *k == key) {
            return false;
        }
        if g.heard.len() >= MAX_HEARD {
            g.heard.pop_front();
        }
        g.heard.push_back((key, now_ms));
        let Some(s) = g.stat(template, now_ms) else {
            return false;
        };
        let weight = if from_bot { BOT_LINE } else { PLAYER_LINE };
        if line.buy_ad {
            ItemStat::push_line(&mut s.wtb, now_ms, weight);
        }
        if line.sell_ad {
            ItemStat::push_line(&mut s.wts, now_ms, weight);
        }
        true
    }

    /// Replaces the server's auction list (and records the median prices as history).
    pub fn set_server_lots(&self, mut lots: ServerLots) {
        if lots.items.len() > MAX_SERVER_ITEMS {
            let mut keep: Vec<(u32, LotStat)> = lots.items.into_iter().collect();
            keep.sort_by_key(|(t, s)| (s.lots, *t));
            keep.truncate(MAX_SERVER_ITEMS);
            lots.items = keep.into_iter().collect();
        }
        let at = lots.at_ms;
        let mut g = self.inner.lock();
        for (template, stat) in &lots.items {
            // (an item with plenty of lots gets a history only when the board follows it already)
            if stat.median_unit > 0
                && (stat.lots <= HISTORY_MAX_LOTS || g.items.contains_key(template))
            {
                if let Some(s) = g.stat(*template, at) {
                    s.push_history(at, stat.median_unit);
                }
            }
        }
        g.server = Some(lots);
    }

    /// Forgets the server's list (the API stopped answering for good).
    pub fn clear_server_lots(&self) {
        self.inner.lock().server = None;
    }

    /// How scarce `template` is, 0 (plenty or unknown) to 1.
    pub fn scarcity(&self, template: u32, now_ms: u64, items: &dyn ItemInfo) -> f32 {
        let g = self.inner.lock();
        match g.items.get(&template) {
            Some(s) => Self::evaluate(&g, template, s, now_ms, items).0.value,
            None => g
                .server
                .as_ref()
                .filter(|sv| now_ms.saturating_sub(sv.at_ms) < SERVER_FRESH_MS)
                .and_then(|sv| sv.items.get(&template))
                .map_or(0.0, |ls| {
                    let ev = Evidence {
                        lots: Some(ls.lots),
                        unit: ls.median_unit,
                        vendor: items.vendor_price(template),
                        ..Evidence::default()
                    };
                    score(&ev).value
                }),
        }
    }

    /// The `n` scarcest items scoring at least `min`, most scarce first.
    pub fn top_scarce(
        &self,
        n: usize,
        min: f32,
        now_ms: u64,
        items: &dyn ItemInfo,
    ) -> Vec<(u32, f32)> {
        let g = self.inner.lock();
        let mut v: Vec<(u32, f32)> = g
            .items
            .iter()
            .map(|(t, s)| (*t, Self::evaluate(&g, *t, s, now_ms, items).0.value))
            .filter(|(_, v)| *v >= min)
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// The evidence of one item and its score.
    fn evaluate(
        g: &Inner,
        template: u32,
        s: &ItemStat,
        now_ms: u64,
        items: &dyn ItemInfo,
    ) -> (Score, Evidence, &'static str) {
        let server = g
            .server
            .as_ref()
            .filter(|sv| now_ms.saturating_sub(sv.at_ms) < SERVER_FRESH_MS);
        let fresh: SmallVec<[&Obs; MAX_OBS]> = s
            .obs
            .iter()
            .filter(|o| now_ms.saturating_sub(o.at_ms) < OBS_FRESH_MS)
            .collect();
        let (lots, unit, source) = if let Some(sv) = server {
            let ls = sv.items.get(&template).copied().unwrap_or_default();
            let unit = if ls.median_unit > 0 {
                ls.median_unit
            } else {
                median(fresh.iter().filter(|o| o.listed).map(|o| o.unit))
            };
            (Some(ls.lots), unit, "server")
        } else if let Some(latest) = fresh.iter().max_by_key(|o| o.at_ms) {
            let lots = if latest.listed {
                u32::from(latest.lots)
            } else {
                0
            };
            let unit = median(fresh.iter().filter(|o| o.listed).map(|o| o.unit));
            (Some(lots), unit, "bots")
        } else {
            (None, 0, "")
        };
        let ev = Evidence {
            lots,
            unit,
            vendor: items.vendor_price(template),
            trend_pct: s.trend_pct(now_ms),
            wtb: ItemStat::lines(&s.wtb, now_ms),
            wts: ItemStat::lines(&s.wts, now_ms),
        };
        (score(&ev), ev, source)
    }

    /// The table for the web UI: the scarcest items first, and every item in `boosted`.
    pub fn view(
        &self,
        now_ms: u64,
        items: &dyn ItemInfo,
        boosted: &[u32],
        limit: usize,
    ) -> EconomyView {
        let g = self.inner.lock();
        let mut rows: Vec<ItemRow> = Vec::new();
        let row = |template: u32, stat: &ItemStat| -> ItemRow {
            let (sc, ev, source) = Self::evaluate(&g, template, stat, now_ms, items);
            ItemRow {
                template,
                name: items
                    .name(template)
                    .unwrap_or_else(|| format!("item {template}")),
                scarcity: sc.value,
                lots: ev.lots.unwrap_or(0),
                supply_source: source.to_owned(),
                unit_price: ev.unit,
                vendor_price: ev.vendor,
                price_ratio: if ev.unit > 0 && ev.vendor > 0 {
                    ev.unit as f32 / ev.vendor as f32
                } else {
                    0.0
                },
                trend_pct: ev.trend_pct,
                wtb: ev.wtb.round() as u32,
                wts: ev.wts.round() as u32,
                reasons: sc.reasons,
                boosted: boosted.contains(&template),
                growing: 0.0,
            }
        };
        for (t, s) in &g.items {
            rows.push(row(*t, s));
        }
        // scarce items of the server's list that nobody on our side looked at yet
        if let Some(sv) = g
            .server
            .as_ref()
            .filter(|sv| now_ms.saturating_sub(sv.at_ms) < SERVER_FRESH_MS)
        {
            let empty = ItemStat::default();
            for (t, ls) in &sv.items {
                if !g.items.contains_key(t) && ls.lots <= 3 {
                    rows.push(row(*t, &empty));
                }
            }
        }
        for t in boosted {
            if !rows.iter().any(|r| r.template == *t) {
                rows.push(row(*t, &ItemStat::default()));
            }
        }
        rows.sort_by(|a, b| {
            b.boosted
                .cmp(&a.boosted)
                .then(b.scarcity.total_cmp(&a.scarcity))
                .then(a.template.cmp(&b.template))
        });
        // (the boosted ones are shown on top of the limit, which counts the others)
        let mut kept = Vec::with_capacity(rows.len().min(limit + boosted.len()));
        let mut others = 0usize;
        for r in rows {
            if r.boosted {
                kept.push(r);
            } else if others < limit {
                others += 1;
                kept.push(r);
            }
        }
        let bot_notes = g
            .items
            .values()
            .map(|s| {
                s.obs
                    .iter()
                    .filter(|o| now_ms.saturating_sub(o.at_ms) < OBS_FRESH_MS)
                    .count() as u32
            })
            .sum();
        let chat_lines = g
            .items
            .values()
            .map(|s| {
                s.wtb
                    .iter()
                    .chain(s.wts.iter())
                    .filter(|(t, _)| now_ms.saturating_sub(*t) < CHAT_WINDOW_MS)
                    .count() as u32
            })
            .sum();
        EconomyView {
            at_ms: now_ms,
            items: kept,
            sources: EconomySources {
                bot_notes,
                server_lots: g
                    .server
                    .as_ref()
                    .filter(|sv| now_ms.saturating_sub(sv.at_ms) < SERVER_FRESH_MS)
                    .map(|sv| sv.total),
                chat_lines,
                chat_window_secs: (CHAT_WINDOW_MS / 1000) as u32,
            },
        }
    }

    /// Items the board follows.
    pub fn len(&self) -> usize {
        self.inner.lock().items.len()
    }

    /// Whether the board follows nothing.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().items.is_empty()
    }
}

/// What the board reads the scarcity of an item from.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Evidence {
    /// Lots on the auction house; `None` when nobody has looked.
    pub lots: Option<u32>,
    /// The market price per item (copper), 0 when unknown.
    pub unit: u32,
    /// What a vendor asks for one item, 0 when no vendor sells it.
    pub vendor: u32,
    /// Price change over the trend window, percent.
    pub trend_pct: f32,
    /// Lines in chat that ask for the item / offer it (a player's line is 1).
    pub wtb: f32,
    pub wts: f32,
}

/// A scarcity score and the reasons for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// 0 (plenty) to 1 (scarce).
    pub value: f32,
    pub reasons: Vec<String>,
}

/// The scarcity of an item: 0.45 for the supply (1 at no lots, a half at two, a third at four),
/// 0.25 for the price against a vendor's (nothing at parity, everything at four times), 0.15 for a
/// rising price (everything at +50 percent) and 0.15 for more people asking than offering
/// (everything at four more). A part without evidence adds nothing.
pub fn score(e: &Evidence) -> Score {
    let mut value = 0.0f32;
    let mut reasons = Vec::new();
    if let Some(lots) = e.lots {
        value += 0.45 / (1.0 + lots as f32 / 2.0);
        match lots {
            0 => reasons.push("none for sale".to_owned()),
            1 => reasons.push("only 1 lot".to_owned()),
            2..=4 => reasons.push(format!("only {lots} lots")),
            _ => {}
        }
    }
    if e.unit > 0 && e.vendor > 0 {
        let ratio = e.unit as f32 / e.vendor as f32;
        value += 0.25 * ((ratio - 1.0) / 3.0).clamp(0.0, 1.0);
        if ratio >= 1.5 {
            reasons.push(format!("{ratio:.1}x the vendor price"));
        }
    }
    if e.trend_pct > 0.0 {
        value += 0.15 * (e.trend_pct / 50.0).clamp(0.0, 1.0);
        if e.trend_pct >= 10.0 {
            reasons.push(format!("price up {:.0}%", e.trend_pct));
        }
    }
    let net = e.wtb - e.wts;
    if net > 0.0 {
        value += 0.15 * (net / 4.0).clamp(0.0, 1.0);
    }
    if e.wtb >= 1.0 && e.wtb > e.wts {
        reasons.push(format!("{:.0} want to buy", e.wtb.round()));
    }
    Score {
        value: value.clamp(0.0, 1.0),
        reasons,
    }
}

/// The median of some prices, 0 for none (the mean of the two in the middle, rounded up).
fn median(values: impl Iterator<Item = u32>) -> u32 {
    let mut v: SmallVec<[u32; MAX_OBS]> = values.filter(|u| *u > 0).collect();
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (u64::from(v[n / 2 - 1]) + u64::from(v[n / 2])).div_ceil(2) as u32
    }
}

/// A key for the duplicate check of chat lines: FNV-1a of the lowercase sender and the text.
fn line_key(sender: &str, text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sender
        .bytes()
        .map(|b| b.to_ascii_lowercase())
        .chain([0])
        .chain(text.bytes().map(|b| b.to_ascii_lowercase()))
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hands a bot's fresh price notes to the board. `bot_now_ms` is the bot clock the notes are
/// stamped with, `unix_now_ms` the same moment as unix time.
pub fn sample_notes(
    economy: &Economy,
    bot: u32,
    notes: &[PriceNote],
    bot_now_ms: u64,
    unix_now_ms: u64,
) {
    for n in notes {
        let age = bot_now_ms.saturating_sub(n.at_ms);
        if age >= OBS_FRESH_MS {
            continue;
        }
        let quote = n.quote.map(|q| (q.unit, q.lowest, q.lots));
        economy.note_price(bot, n.template, quote, unix_now_ms.saturating_sub(age));
    }
}

/// What the farming goal (and anything else that wants to grow what is missing) reads.
pub trait ScarcityView {
    /// How scarce an item is, 0 (plenty or unknown) to 1.
    fn scarcity(&self, template: u32) -> f32;
    /// The `n` scarcest items scoring at least `min`.
    fn most_scarce(&self, n: usize, min: f32) -> Vec<(u32, f32)>;
    /// The items the operator (or the director) wants more of.
    fn wished(&self) -> Vec<u32>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::market::auction::price::Quote;

    struct Items;
    impl ItemInfo for Items {
        fn name(&self, template: u32) -> Option<String> {
            Some(format!("Item{template}"))
        }
        fn vendor_price(&self, template: u32) -> u32 {
            if template == 99 {
                0
            } else {
                100
            }
        }
    }

    const MIN: u64 = 60_000;

    fn index() -> NameIndex {
        NameIndex::build([(3412, "Copper"), (3411, "Copper Ore"), (3712, "Hay Bale")].into_iter())
    }

    #[test]
    fn scarcity_grows_with_every_kind_of_evidence() {
        let none = score(&Evidence::default());
        assert_eq!(none.value, 0.0);
        assert!(none.reasons.is_empty());

        let plenty = score(&Evidence {
            lots: Some(40),
            unit: 90,
            vendor: 100,
            ..Evidence::default()
        });
        assert!(plenty.value < 0.05, "{plenty:?}");

        let few = score(&Evidence {
            lots: Some(2),
            unit: 100,
            vendor: 100,
            ..Evidence::default()
        });
        let nothing = score(&Evidence {
            lots: Some(0),
            unit: 100,
            vendor: 100,
            ..Evidence::default()
        });
        assert!(plenty.value < few.value && few.value < nothing.value);
        assert!((nothing.value - 0.45).abs() < 1e-6);
        assert_eq!(nothing.reasons, ["none for sale"]);

        let dear = score(&Evidence {
            lots: Some(2),
            unit: 400,
            vendor: 100,
            ..Evidence::default()
        });
        assert!(dear.value > few.value + 0.2, "{dear:?}");
        assert!(dear.reasons.iter().any(|r| r.contains("4.0x")));

        let rising = score(&Evidence {
            lots: Some(2),
            unit: 100,
            vendor: 100,
            trend_pct: 50.0,
            ..Evidence::default()
        });
        assert!((rising.value - few.value - 0.15).abs() < 1e-5);
        assert!(rising.reasons.iter().any(|r| r.contains("up 50%")));
        let falling = score(&Evidence {
            trend_pct: -30.0,
            lots: Some(2),
            unit: 100,
            vendor: 100,
            ..Evidence::default()
        });
        assert_eq!(falling.value, few.value);

        let wanted = score(&Evidence {
            lots: Some(2),
            unit: 100,
            vendor: 100,
            wtb: 4.0,
            ..Evidence::default()
        });
        assert!((wanted.value - few.value - 0.15).abs() < 1e-5);
        let offered = score(&Evidence {
            wtb: 3.0,
            wts: 3.0,
            ..Evidence::default()
        });
        assert_eq!(offered.value, 0.0);

        // everything at once stays within 0..=1
        let all = score(&Evidence {
            lots: Some(0),
            unit: 10_000,
            vendor: 1,
            trend_pct: 900.0,
            wtb: 99.0,
            wts: 0.0,
        });
        assert!((0.99..=1.0).contains(&all.value), "{all:?}");
    }

    #[test]
    fn price_notes_of_several_bots_make_a_quote_and_a_trend() {
        let e = Economy::default();
        // three bots saw the lots of item 3412 (a unit price of 300, 320, 500)
        e.note_price(1, 3412, Some((300, 280, 5)), 10 * MIN);
        e.note_price(2, 3412, Some((320, 300, 4)), 10 * MIN + 1_000);
        e.note_price(3, 3412, Some((500, 450, 2)), 11 * MIN);
        let v = e.view(11 * MIN, &Items, &[], 10);
        assert_eq!(v.items.len(), 1);
        let r = &v.items[0];
        assert_eq!(r.template, 3412);
        assert_eq!(r.name, "Item3412");
        assert_eq!(r.supply_source, "bots");
        assert_eq!(r.lots, 2, "the latest note is the supply");
        assert_eq!(r.unit_price, 320, "the median of the notes");
        assert_eq!(r.vendor_price, 100);
        assert!((r.price_ratio - 3.2).abs() < 1e-5);
        assert_eq!(v.sources.bot_notes, 3);
        assert!(r.scarcity > 0.4, "{r:?}");

        // the price rises over a quarter of an hour
        let e = Economy::default();
        e.note_price(1, 3412, Some((200, 190, 5)), 10 * MIN);
        e.note_price(1, 3412, Some((260, 250, 5)), 25 * MIN);
        let r = &e.view(25 * MIN, &Items, &[], 10).items[0];
        assert!((r.trend_pct - 30.0).abs() < 0.01, "{}", r.trend_pct);
        // a single sample, or two seconds apart, is no trend
        let e = Economy::default();
        e.note_price(1, 3412, Some((200, 190, 5)), 10 * MIN);
        e.note_price(2, 3412, Some((260, 250, 5)), 10 * MIN + 2_000);
        assert_eq!(e.view(11 * MIN, &Items, &[], 10).items[0].trend_pct, 0.0);
    }

    #[test]
    fn a_search_that_found_no_lot_is_the_scarcest_news() {
        let e = Economy::default();
        e.note_price(1, 3712, None, 10 * MIN);
        let r = &e.view(10 * MIN, &Items, &[], 10).items[0];
        assert_eq!(r.lots, 0);
        assert_eq!(r.unit_price, 0);
        assert!(r.reasons.iter().any(|x| x == "none for sale"));
        assert!((r.scarcity - 0.45).abs() < 1e-6);
        // a newer note of the same bot replaces it
        e.note_price(1, 3712, Some((100, 90, 5)), 11 * MIN);
        let r = &e.view(11 * MIN, &Items, &[], 10).items[0];
        assert_eq!(r.lots, 5);
        // an older note never replaces a newer one
        e.note_price(1, 3712, None, 5 * MIN);
        assert_eq!(e.view(11 * MIN, &Items, &[], 10).items[0].lots, 5);
    }

    #[test]
    fn notes_go_stale_after_the_ttl() {
        let e = Economy::default();
        e.note_price(1, 3412, Some((300, 280, 1)), MIN);
        assert_eq!(e.view(2 * MIN, &Items, &[], 10).items[0].lots, 1);
        let r = &e.view(MIN + OBS_FRESH_MS + 1, &Items, &[], 10).items[0];
        assert_eq!((r.lots, r.unit_price, r.supply_source.as_str()), (0, 0, ""));
        assert_eq!(r.scarcity, 0.0, "no evidence, no scarcity");
    }

    #[test]
    fn the_notes_of_a_bot_are_sampled_with_their_age() {
        let e = Economy::default();
        let q = Quote {
            unit: 300,
            lowest: 250,
            lots: 3,
        };
        let notes = [
            PriceNote {
                template: 3412,
                quote: Some(q),
                at_ms: 100_000,
            },
            PriceNote {
                template: 3411,
                quote: None,
                at_ms: 100_000,
            },
            // too old to trust
            PriceNote {
                template: 3712,
                quote: Some(q),
                at_ms: 1_000,
            },
        ];
        // the bot clock reads 160 s, so the first two are a minute old
        sample_notes(&e, 7, &notes, 100_000 + 60_000, 5_000_000);
        // (the third is 159 s old and fresh; make one that is really stale)
        let stale = [PriceNote {
            template: 9,
            quote: Some(q),
            at_ms: 0,
        }];
        sample_notes(&e, 7, &stale, OBS_FRESH_MS + 1, 9_000_000);
        let v = e.view(5_000_001, &Items, &[], 10);
        assert!(v.items.iter().any(|r| r.template == 3412 && r.lots == 3));
        assert!(v.items.iter().any(|r| r.template == 3411 && r.lots == 0));
        assert!(
            !v.items.iter().any(|r| r.template == 9),
            "a stale note is ignored"
        );
        // the sighting was stamped a minute before 'now', not at 'now'
        let g = e.inner.lock();
        assert_eq!(g.items[&3412].obs[0].at_ms, 5_000_000 - 60_000);
    }

    #[test]
    fn chat_lines_count_once_and_a_bots_line_a_quarter() {
        let e = Economy::default();
        let idx = index();
        let t0 = 20 * MIN;
        // the same player line heard by three bots
        for k in 0..3 {
            let counted = e.hear(
                &idx,
                false,
                chat_type::TRADE,
                "Kara",
                "WTB Copper x20 - 30s ea",
                t0 + k,
            );
            assert_eq!(counted, k == 0, "heard by bot {k}");
        }
        // another player asks too, a bot sells
        assert!(e.hear(
            &idx,
            false,
            chat_type::SHOUT,
            "Lee",
            "wtb copper",
            t0 + 1_000
        ));
        assert!(e.hear(
            &idx,
            true,
            chat_type::TRADE,
            "Botty",
            "WTS Copper x10 - 40s ea",
            t0 + 2_000
        ));
        let r = e
            .view(t0 + 3_000, &Items, &[], 10)
            .items
            .into_iter()
            .find(|r| r.template == 3412)
            .unwrap();
        assert_eq!(
            (r.wtb, r.wts),
            (2, 0),
            "the sell line of a bot is a quarter: rounds to 0"
        );
        // not an ad, whispers, an item nobody knows, and the same line later
        assert!(!e.hear(
            &idx,
            false,
            chat_type::TRADE,
            "Kara",
            "how much is copper?",
            t0
        ));
        assert!(!e.hear(&idx, false, chat_type::WHISPER, "Kara", "WTB Copper", t0));
        assert!(!e.hear(&idx, false, chat_type::TRADE, "Kara", "WTB unobtainium", t0));
        assert!(!e.hear(&idx, false, chat_type::SAY, "Kara", "WTB Copper", t0));
        assert!(e.hear(
            &idx,
            false,
            chat_type::TRADE,
            "Kara",
            "WTB Copper x20 - 30s ea",
            t0 + 60_000
        ));
        // the longest name wins: ore is not copper
        assert!(e.hear(
            &idx,
            false,
            chat_type::TRADE,
            "Mo",
            "WTS Copper Ore",
            t0 + 61_000
        ));
        let v = e.view(t0 + 62_000, &Items, &[], 10);
        assert!(v.items.iter().any(|r| r.template == 3411 && r.wts == 1));
        assert_eq!(v.sources.chat_lines, 5);
        // the lines age out of the window
        let v = e.view(t0 + CHAT_WINDOW_MS + 70_000, &Items, &[], 10);
        assert!(v.items.iter().all(|r| r.wtb == 0 && r.wts == 0));
    }

    #[test]
    fn the_servers_list_replaces_the_bots_guess_of_the_supply() {
        let e = Economy::default();
        e.note_price(1, 3412, Some((300, 280, 5)), 10 * MIN);
        let mut items = FxHashMap::default();
        items.insert(
            3412,
            LotStat {
                lots: 1,
                units: 4,
                lowest_unit: 600,
                median_unit: 600,
            },
        );
        items.insert(
            3411,
            LotStat {
                lots: 30,
                units: 900,
                lowest_unit: 50,
                median_unit: 60,
            },
        );
        items.insert(
            3712,
            LotStat {
                lots: 2,
                units: 2,
                lowest_unit: 90,
                median_unit: 90,
            },
        );
        e.set_server_lots(ServerLots {
            at_ms: 10 * MIN,
            total: 33,
            items,
        });
        let v = e.view(10 * MIN + 1_000, &Items, &[], 10);
        assert_eq!(v.sources.server_lots, Some(33));
        let copper = v.items.iter().find(|r| r.template == 3412).unwrap();
        assert_eq!((copper.lots, copper.supply_source.as_str()), (1, "server"));
        assert_eq!(copper.unit_price, 600);
        let hay = v.items.iter().find(|r| r.template == 3712).unwrap();
        assert_eq!(hay.lots, 2, "a scarce item nobody looked at is listed");
        let ore = v.items.iter().find(|r| r.template == 3411);
        assert!(
            ore.is_none(),
            "an item with 30 lots is not shown unless looked at"
        );
        assert!(copper.scarcity > hay.scarcity);
        // the list goes stale: the bots' note is back
        let later = e.view(10 * MIN + SERVER_FRESH_MS + 5_000, &Items, &[], 10);
        assert_eq!(later.sources.server_lots, None);
        let copper = later.items.iter().find(|r| r.template == 3412).unwrap();
        assert_eq!((copper.lots, copper.supply_source.as_str()), (5, "bots"));
        // an item the server does not list has no lots at all, when the list is fresh
        let e2 = Economy::default();
        e2.set_server_lots(ServerLots {
            at_ms: MIN,
            total: 0,
            items: FxHashMap::default(),
        });
        e2.note_price(1, 3412, Some((300, 280, 5)), MIN);
        let r = &e2.view(MIN + 1, &Items, &[], 10).items[0];
        assert_eq!((r.lots, r.supply_source.as_str()), (0, "server"));
    }

    #[test]
    fn scarcity_queries_rank_the_items() {
        let e = Economy::default();
        e.note_price(1, 3412, None, MIN); // none for sale: 0.45
        e.note_price(1, 3411, Some((120, 100, 5)), MIN); // five lots at the vendor price
        e.note_price(1, 3712, Some((400, 380, 1)), MIN); // one lot, four times the price
        let top = e.top_scarce(2, 0.0, MIN, &Items);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, 3712);
        assert_eq!(top[1].0, 3412);
        assert!(top[0].1 > top[1].1);
        assert_eq!(e.top_scarce(10, 0.5, MIN, &Items).len(), 1);
        assert!((e.scarcity(3412, MIN, &Items) - 0.45).abs() < 1e-6);
        assert_eq!(
            e.scarcity(555, MIN, &Items),
            0.0,
            "unknown items are not scarce"
        );
    }

    #[test]
    fn boosted_items_are_always_shown_first() {
        let e = Economy::default();
        for t in 1..=5u32 {
            e.note_price(1, 3000 + t, Some((100, 90, 20)), MIN);
        }
        e.note_price(1, 3412, None, MIN);
        let v = e.view(MIN, &Items, &[3003, 777], 2);
        assert_eq!(v.items[0].template, 3003, "boosted, with some evidence");
        assert!(v.items[0].boosted);
        assert_eq!(v.items[1].template, 777, "boosted, without any evidence");
        assert!(v.items[1].boosted);
        assert_eq!(v.items[1].scarcity, 0.0);
        assert_eq!(
            v.items[2].template, 3412,
            "then the scarcest within the limit"
        );
        assert!(!v.items[2].boosted);
        assert_eq!(
            v.items.len(),
            4,
            "the limit counts the rows that are not boosted"
        );
    }

    #[test]
    fn the_board_is_bounded() {
        let e = Economy::default();
        for t in 0..(MAX_ITEMS as u32 + 200) {
            e.note_price(1, t + 1, Some((10, 9, 1)), 1_000 + u64::from(t));
        }
        assert_eq!(e.len(), MAX_ITEMS);
        // observations per item are bounded too
        for b in 0..20 {
            e.note_price(b + 10, 5000, Some((10, 9, 1)), 1_000_000 + u64::from(b));
        }
        let g = e.inner.lock();
        assert_eq!(g.items[&5000].obs.len(), MAX_OBS);
        assert!(g.heard.len() <= MAX_HEARD);
    }

    #[test]
    fn medians() {
        assert_eq!(median([].into_iter()), 0);
        assert_eq!(median([0, 0].into_iter()), 0);
        assert_eq!(median([5].into_iter()), 5);
        assert_eq!(median([9, 1, 5].into_iter()), 5);
        assert_eq!(median([1, 4].into_iter()), 3, "rounded up");
    }
}
