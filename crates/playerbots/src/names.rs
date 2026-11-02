//! Pronounceable fantasy character names.
//!
//! The server accepts `[a-zA-Z0-9]{1,18}` and stores names with a capital first letter and the
//! rest in lower case; generated names are 4..=12 ASCII letters in exactly that form, so what the
//! roster stores is what the server stores.

/// Shortest generated name.
pub const MIN_LEN: usize = 4;
/// Longest generated name.
pub const MAX_LEN: usize = 12;

const ONSETS: &[&str] = &[
    "b", "br", "c", "d", "dr", "f", "g", "gr", "h", "j", "k", "kl", "l", "m", "n", "p", "pr", "r",
    "s", "sh", "t", "th", "tr", "v", "w", "z", "kr", "fl", "m", "l", "r", "n",
];
const VOWELS: &[&str] = &[
    "a", "e", "i", "o", "u", "a", "e", "i", "o", "ae", "ia", "ei", "ou",
];
const CODAS: &[&str] = &[
    "", "", "", "n", "r", "l", "s", "th", "nd", "rn", "x", "m", "k", "ld",
];

/// Substrings no generated name may contain (matched case-insensitively): staff-looking names
/// and a few words that would get a bot reported.
const BLOCKED: &[&str] = &[
    "gm", "admin", "sysop", "mod", "staff", "fuck", "shit", "cunt", "kunt", "nigg", "rape", "nazi",
    "hitler", "dick", "cock", "slut", "whore", "anus", "porn", "sex",
];

/// Whether `name` is a well-formed generated name: 4..=12 ASCII letters, first upper case, the
/// rest lower case, containing no blocked word.
pub fn is_acceptable(name: &str) -> bool {
    let n = name.len();
    if !(MIN_LEN..=MAX_LEN).contains(&n) || !name.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    let mut bytes = name.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_uppercase())
        || !bytes.all(|b| b.is_ascii_lowercase())
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !BLOCKED.iter().any(|w| lower.contains(w))
}

/// One pronounceable name: two or three consonant-vowel syllables, an optional closing
/// consonant group, capitalised. Always [`is_acceptable`].
pub fn generate(rng: &mut fastrand::Rng) -> String {
    loop {
        let syllables = 2 + usize::from(rng.u8(0..3) == 0);
        let mut s = String::with_capacity(MAX_LEN + 4);
        for i in 0..syllables {
            s.push_str(ONSETS[rng.usize(..ONSETS.len())]);
            s.push_str(VOWELS[rng.usize(..VOWELS.len())]);
            if i + 1 == syllables || rng.u8(0..5) == 0 {
                s.push_str(CODAS[rng.usize(..CODAS.len())]);
            }
        }
        let mut name = String::with_capacity(s.len());
        let mut chars = s.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
        }
        name.extend(chars);
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
            assert!((MIN_LEN..=MAX_LEN).contains(&n.len()), "{n}");
            seen.insert(n);
        }
        // A large pool: collisions are rare enough that the roster can retry cheaply.
        assert!(seen.len() > 3500, "only {} distinct names", seen.len());
    }

    #[test]
    fn acceptance_rules() {
        assert!(is_acceptable("Kelvarin"));
        assert!(is_acceptable("Brae"));
        assert!(!is_acceptable("Abc")); // too short
        assert!(!is_acceptable("Abcdefghijklm")); // too long
        assert!(!is_acceptable("kelvarin")); // not capitalised
        assert!(!is_acceptable("KELVARIN"));
        assert!(!is_acceptable("Kelvar1n"));
        assert!(!is_acceptable("Kelvar n"));
        assert!(!is_acceptable("Gmaster"));
        assert!(!is_acceptable("Dogmar"));
        assert!(!is_acceptable("Adminton"));
        assert!(!is_acceptable("Éowyn"));
    }

    #[test]
    fn generation_is_deterministic_for_a_seed() {
        let a = generate(&mut fastrand::Rng::with_seed(99));
        let b = generate(&mut fastrand::Rng::with_seed(99));
        assert_eq!(a, b);
    }
}
