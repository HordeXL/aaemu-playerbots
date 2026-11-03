//! Item names to templates: which item a chat line talks about.
//!
//! Players write item names (`Copper Ore`), the market works with templates. The index holds the
//! normalized English names of the items a bot may trade at all and finds the longest name that
//! occurs in a line.

use std::sync::OnceLock;

use aa_world::World;
use rustc_hash::FxHashMap;

use super::grammar::normalize;

/// The longest name, in words, the index looks for.
const MAX_WORDS: usize = 5;

/// Normalized item name to template.
#[derive(Debug, Default)]
pub struct NameIndex {
    by_name: FxHashMap<Box<str>, u32>,
}

impl NameIndex {
    /// An index of `(template, name)` pairs; of two items with one name the lower template wins.
    pub fn build<'a>(items: impl Iterator<Item = (u32, &'a str)>) -> NameIndex {
        let mut by_name: FxHashMap<Box<str>, u32> = FxHashMap::default();
        for (template, name) in items {
            let key = normalize(name)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if key.chars().count() < 3 || key.split(' ').count() > MAX_WORDS {
                continue;
            }
            let slot = by_name.entry(key.into()).or_insert(template);
            if template < *slot {
                *slot = template;
            }
        }
        NameIndex { by_name }
    }

    /// The template of the longest item name in `text` (the leftmost of equally long ones).
    pub fn find(&self, text: &str) -> Option<u32> {
        let norm = normalize(text);
        let words: Vec<&str> = norm.split_whitespace().collect();
        for len in (1..=MAX_WORDS.min(words.len())).rev() {
            for window in words.windows(len) {
                let key = window.join(" ");
                if let Some(t) = self.by_name.get(key.as_str()) {
                    return Some(*t);
                }
            }
        }
        None
    }

    /// Number of names.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// The index of the tradable items of `world`, built on first use and shared by all bots (a
/// process loads one world).
pub fn global(world: &World) -> &'static NameIndex {
    static INDEX: OnceLock<NameIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        NameIndex::build(
            world
                .item_table()
                .iter()
                .filter(|(_, f)| f.tradable())
                .filter_map(|(t, _)| world.item_name(t).map(|n| (t, n))),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> NameIndex {
        NameIndex::build(
            [
                (3411, "Copper Ore"),
                (3412, "Copper"),
                (3712, "Hay Bale"),
                (7765, "Blue Salt"),
                (9000, "Ore"),
                (9001, "Ox"),
                (3999, "Copper Ore"),
                (5000, "Scroll of the Fox"),
            ]
            .into_iter(),
        )
    }

    #[test]
    fn the_longest_name_in_the_line_wins() {
        let i = index();
        assert_eq!(i.find("WTS Copper Ore x20 - 30s ea"), Some(3411));
        assert_eq!(i.find("wtb copper"), Some(3412));
        assert_eq!(i.find("how much for the ore?"), Some(9000));
        assert_eq!(i.find("Hay Bale!!"), Some(3712));
        assert_eq!(i.find("selling blue salt, 20 of them"), Some(7765));
        assert_eq!(i.find("scroll of the fox 1g"), Some(5000));
        assert_eq!(i.find("nothing here"), None);
        assert_eq!(i.find(""), None);
    }

    #[test]
    fn two_items_with_one_name_keep_the_lower_template() {
        let i = index();
        assert_eq!(i.find("copper ore"), Some(3411));
    }

    #[test]
    fn short_names_are_left_out() {
        let i = index();
        assert_eq!(i.find("ox"), None);
        assert!(!i.is_empty());
        assert!(i.len() >= 6);
    }
}
