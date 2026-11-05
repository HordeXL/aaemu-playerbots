//! Guild names: plausible ones that the server accepts.
//!
//! The server takes a name that matches `^[a-zA-Z ]{3,32}$` (`Expedition.json`, `NameRegex`; the
//! Cyrillic letters pass as well, the bots never use them) and that no other guild has, compared
//! exactly. So a name is letters and single spaces, 3 to 32 characters, with no digit. The
//! generator joins a few word lists in a handful of shapes; a retry (the name was taken) draws
//! again and, from the third try, ends the name in a Roman numeral, which is made of letters.

/// Longest name the server takes.
pub const MAX_LEN: usize = 32;
/// Shortest name the server takes.
pub const MIN_LEN: usize = 3;

const ADJECTIVES: &[&str] = &[
    "Silent",
    "Crimson",
    "Iron",
    "Golden",
    "Wandering",
    "Shattered",
    "Silver",
    "Ashen",
    "Hollow",
    "Verdant",
    "Frozen",
    "Burning",
    "Radiant",
    "Fallen",
    "Ancient",
    "Wild",
    "Gilded",
    "Hidden",
    "Scarlet",
    "Azure",
    "Restless",
    "Humble",
    "Bold",
    "Veiled",
    "Stormborn",
    "Tireless",
];

const NOUNS: &[&str] = &[
    "Wardens",
    "Blades",
    "Pilgrims",
    "Company",
    "Vanguard",
    "Reavers",
    "Covenant",
    "Brotherhood",
    "Legion",
    "Order",
    "Circle",
    "Guard",
    "Hunters",
    "Wolves",
    "Ravens",
    "Lanterns",
    "Mariners",
    "Harvesters",
    "Seekers",
    "Rangers",
    "Watch",
    "Banner",
    "Fellowship",
    "Wayfarers",
    "Archers",
    "Sentinels",
];

/// Places of the world, for "Wardens of Lilyut".
const PLACES: &[&str] = &[
    "Lilyut",
    "Marianople",
    "Solzreed",
    "Dewstone",
    "Villanelle",
    "Falcorth",
    "Mahadevi",
    "Cinderstone",
    "Hasla",
    "Sunrise Peninsula",
    "Two Crowns",
    "Gweonid",
    "Hellswamp",
    "Rookborne",
    "Ynystere",
    "Silent Forest",
    "White Arden",
    "Halcyona",
    "Arcum Iris",
    "Aegis Island",
];

const NUMERALS: &[&str] = &["II", "III", "IV", "V", "VI", "VII"];

/// Whether `name` is one the server's name pattern lets through, and one that looks like a name:
/// letters and single spaces, 3 to 32 characters, no space at either end.
pub fn valid(name: &str) -> bool {
    let n = name.chars().count();
    (MIN_LEN..=MAX_LEN).contains(&n)
        && name.chars().all(|c| c.is_ascii_alphabetic() || c == ' ')
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && !name.contains("  ")
}

fn pick<'a>(list: &[&'a str], rng: &mut fastrand::Rng) -> &'a str {
    list[rng.usize(..list.len())]
}

/// A name for the `attempt`-th try (0 first). Always [`valid`].
pub fn generate(rng: &mut fastrand::Rng, attempt: u32) -> String {
    // a few draws: some shapes with long words do not fit
    for _ in 0..16 {
        let adj = pick(ADJECTIVES, rng);
        let noun = pick(NOUNS, rng);
        let place = pick(PLACES, rng);
        let mut name = match rng.u8(..5) {
            0 => format!("The {adj} {noun}"),
            1 => format!("{adj} {noun}"),
            2 => format!("Order of the {adj} {noun}"),
            3 => format!("{noun} of {place}"),
            _ => format!("The {place} {noun}"),
        };
        if attempt >= 2 {
            let numeral = NUMERALS[(attempt as usize - 2).min(NUMERALS.len() - 1)];
            name = format!("{name} {numeral}");
        }
        if valid(&name) {
            return name;
        }
    }
    // (cannot happen with the lists above; the fallback is valid by construction)
    format!(
        "Free Company {}",
        NUMERALS[attempt as usize % NUMERALS.len()]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet;

    #[test]
    fn the_server_pattern_is_mirrored() {
        for ok in [
            "Silent Wardens",
            "The Iron Order",
            "Abc",
            "A B",
            &"a".repeat(32),
        ] {
            assert!(valid(ok), "{ok}");
        }
        for bad in [
            "",
            "Ab",
            " Silent",
            "Silent ",
            "Silent  Wardens",
            "Legion 7",
            "Guild-of-Iron",
            "Guild_of",
            "Café Order",
            &"a".repeat(33),
        ] {
            assert!(!valid(bad), "{bad:?}");
        }
    }

    #[test]
    fn every_generated_name_passes_the_server_pattern() {
        let mut rng = fastrand::Rng::with_seed(1);
        for attempt in 0..9 {
            for _ in 0..2_000 {
                let n = generate(&mut rng, attempt);
                assert!(valid(&n), "attempt {attempt}: {n:?}");
            }
        }
    }

    #[test]
    fn names_vary_and_a_retry_changes_them() {
        let mut rng = fastrand::Rng::with_seed(2);
        let first: FxHashSet<String> = (0..300).map(|_| generate(&mut rng, 0)).collect();
        assert!(first.len() > 150, "{}", first.len());
        let third = generate(&mut rng, 2);
        assert!(third.ends_with(" II"), "{third}");
        let fifth = generate(&mut rng, 4);
        assert!(fifth.ends_with(" IV"), "{fifth}");
        // many retries stay valid and end in the last numeral
        let late = generate(&mut rng, 50);
        assert!(late.ends_with(" VII") && valid(&late), "{late}");
    }

    #[test]
    fn the_word_lists_hold_only_letters() {
        for list in [ADJECTIVES, NOUNS, PLACES, NUMERALS] {
            for w in list {
                assert!(
                    w.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                    "{w}"
                );
                assert!(!w.is_empty());
            }
        }
    }
}
