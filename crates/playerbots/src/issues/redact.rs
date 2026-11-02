//! Keeping private things out of what the issue reports write.
//!
//! The reports never carry the bot password (nothing here can reach it), but a server log line may
//! quote an address or a credential. [`scrub`] removes both from any text before it is stored,
//! written to an evidence folder, appended to the queue file or served by the web API:
//!
//! * IPv4 addresses other than 127.x become `x.x.x.x` (a port after it stays);
//! * the value of `password=...`, `token: ...`, `"secret":"..."` and the like becomes `<redacted>`.

use std::borrow::Cow;

/// What replaces an IPv4 address.
pub const IP_MASK: &str = "x.x.x.x";
/// What replaces a credential value.
pub const SECRET_MASK: &str = "<redacted>";

/// Words whose value is never written down.
const SECRET_WORDS: [&str; 10] = [
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "cookie",
    "ticket",
    "authorization",
];

/// Removes IPv4 addresses (other than 127.x) and credential values from `s`.
pub fn scrub(s: &str) -> String {
    let ips = redact_ipv4(s);
    redact_secrets(&ips).into_owned()
}

/// Makes `s` one line of at most `max` characters: control characters become blanks, blank runs
/// shrink to one blank, the text is [`scrub`]bed and cut with `...` when it is too long.
pub fn one_line(s: &str, max: usize) -> String {
    let mut flat = String::with_capacity(s.len().min(max * 4));
    let mut last_blank = true;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            if !last_blank {
                flat.push(' ');
            }
            last_blank = true;
        } else {
            flat.push(c);
            last_blank = false;
        }
    }
    let flat = flat.trim_end().to_string();
    // (scrubbing first: a cut must not leave half of an address that would then survive)
    let clean = scrub(&flat);
    if clean.chars().count() <= max {
        return clean;
    }
    let keep = max.saturating_sub(3);
    let mut cut: String = clean.chars().take(keep).collect();
    cut.push_str("...");
    cut
}

/// Replaces every IPv4 address in `s` whose first octet is not 127 by [`IP_MASK`].
pub fn redact_ipv4(s: &str) -> Cow<'_, str> {
    let b = s.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        if b[i].is_ascii_digit() && starts_number(b, i) {
            if let Some((end, first)) = ipv4_at(b, i) {
                if first != 127 {
                    let o = out.get_or_insert_with(|| String::with_capacity(s.len()));
                    o.push_str(&s[copied..i]);
                    o.push_str(IP_MASK);
                    copied = end;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    match out {
        Some(mut o) => {
            o.push_str(&s[copied..]);
            Cow::Owned(o)
        }
        None => Cow::Borrowed(s),
    }
}

/// Whether a number may start at `b[i]`: not in the middle of a word or of a longer dotted number.
fn starts_number(b: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let p = b[i - 1];
    if p.is_ascii_alphanumeric() || p == b'_' {
        return false;
    }
    !(p == b'.' && i >= 2 && b[i - 2].is_ascii_digit())
}

/// `(end, first octet)` of the IPv4 address that starts at `b[i]`.
fn ipv4_at(b: &[u8], start: usize) -> Option<(usize, u32)> {
    let mut i = start;
    let mut first = 0;
    for part in 0..4 {
        let digits_from = i;
        let mut v = 0u32;
        while i < b.len() && b[i].is_ascii_digit() && i - digits_from < 3 {
            v = v * 10 + u32::from(b[i] - b'0');
            i += 1;
        }
        if i == digits_from || v > 255 {
            return None;
        }
        if part == 0 {
            first = v;
        }
        if part < 3 {
            if i < b.len() && b[i] == b'.' {
                i += 1;
            } else {
                return None;
            }
        }
    }
    // (a fifth group or more digits: a version string or a longer number, not an address)
    if i < b.len() && (b[i].is_ascii_digit() || b[i].is_ascii_alphabetic() || b[i] == b'_') {
        return None;
    }
    if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        return None;
    }
    Some((i, first))
}

/// Replaces the value after a credential word by [`SECRET_MASK`].
pub fn redact_secrets(s: &str) -> Cow<'_, str> {
    let lower = s.to_ascii_lowercase();
    // (to_ascii_lowercase keeps every byte offset)
    let mut out: Option<String> = None;
    let mut copied = 0usize;
    let mut from = 0usize;
    'scan: while from < lower.len() {
        let Some((at, word)) = SECRET_WORDS
            .iter()
            .filter_map(|w| lower[from..].find(w).map(|p| (from + p, *w)))
            .min_by_key(|(p, _)| *p)
        else {
            break;
        };
        let after = at + word.len();
        from = after;
        // (the word must not be the middle of a longer identifier on its left)
        if at > 0 && lower.as_bytes()[at - 1].is_ascii_alphanumeric() {
            continue;
        }
        let b = s.as_bytes();
        let mut i = after;
        while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ') {
            i += 1;
        }
        if i >= b.len() || !matches!(b[i], b'=' | b':') {
            continue 'scan;
        }
        i += 1;
        while i < b.len() && matches!(b[i], b' ' | b'"' | b'\'') {
            i += 1;
        }
        // "Authorization: Bearer abc": the scheme is not the secret
        for scheme in ["bearer ", "basic "] {
            if lower[i..].starts_with(scheme) {
                i += scheme.len();
                break;
            }
        }
        let value_start = i;
        while i < b.len()
            && !matches!(
                b[i],
                b' ' | b'"' | b'\'' | b',' | b';' | b'&' | b'}' | b')' | b'\r' | b'\n' | b'\t'
            )
        {
            i += 1;
        }
        if i == value_start {
            continue;
        }
        let o = out.get_or_insert_with(|| String::with_capacity(s.len()));
        o.push_str(&s[copied..value_start]);
        o.push_str(SECRET_MASK);
        copied = i;
        from = i;
    }
    match out {
        Some(mut o) => {
            o.push_str(&s[copied..]);
            Cow::Owned(o)
        }
        None => Cow::Borrowed(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_other_than_loopback_are_masked() {
        assert_eq!(
            redact_ipv4("from 203.0.113.9:5555 ok"),
            "from x.x.x.x:5555 ok"
        );
        assert_eq!(
            redact_ipv4("127.0.0.1:1237 and 127.1.2.3"),
            "127.0.0.1:1237 and 127.1.2.3"
        );
        assert_eq!(
            redact_ipv4("10.0.0.1,192.168.1.20;8.8.8.8"),
            "x.x.x.x,x.x.x.x;x.x.x.x"
        );
        assert_eq!(redact_ipv4("(1.2.3.4)"), "(x.x.x.x)");
        assert_eq!(redact_ipv4("ends 255.255.255.255"), "ends x.x.x.x");
        assert_eq!(redact_ipv4("ünï 9.9.9.9 çø"), "ünï x.x.x.x çø");
    }

    #[test]
    fn things_that_only_look_like_addresses_are_kept() {
        for keep in [
            "version 1.2.3.4.5 here",
            "x1.2.3.4",
            "1.2.3",
            "1.2.3.456",
            "300.1.1.1",
            "12.34.56.78x",
            "pos 1234.5, 67.8",
            "id 4000000000",
            "",
        ] {
            assert_eq!(redact_ipv4(keep), keep, "{keep:?}");
        }
    }

    #[test]
    fn credential_values_are_masked() {
        assert_eq!(
            redact_secrets("login password=hunter2 ok"),
            "login password=<redacted> ok"
        );
        assert_eq!(
            redact_secrets(r#"{"password":"hunter2","user":"a"}"#),
            r#"{"password":"<redacted>","user":"a"}"#
        );
        assert_eq!(
            redact_secrets("Token: abc123, next"),
            "Token: <redacted>, next"
        );
        assert_eq!(
            redact_secrets("Authorization: Bearer abcdef"),
            "Authorization: Bearer <redacted>"
        );
        assert_eq!(
            redact_secrets("api_key = k1 and secret=s2"),
            "api_key = <redacted> and secret=<redacted>"
        );
        // a word without a value, or inside another word, is left alone
        for keep in [
            "password reset ok",
            "the token was fine",
            "xpassword=1",
            "no secrets here",
        ] {
            assert_eq!(redact_secrets(keep), keep, "{keep:?}");
        }
    }

    #[test]
    fn one_line_flattens_scrubs_and_bounds() {
        assert_eq!(one_line("  a\r\n b \t c  ", 50), "a b c");
        assert_eq!(
            one_line("host 8.8.4.4 password=x", 50),
            "host x.x.x.x password=<redacted>"
        );
        let long = one_line(&"y".repeat(100), 20);
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with("..."));
        // multi-byte characters are cut on a character boundary
        let wide = one_line(&"é".repeat(100), 10);
        assert_eq!(wide.chars().count(), 10);
        // an address is masked before the cut, never half-kept
        let cut = one_line("1234567890123456 192.168.100.200", 22);
        assert!(!cut.contains("192.168"), "{cut}");
        assert_eq!(one_line("", 5), "");
        assert_eq!(one_line("\u{0}\u{7}", 5), "");
    }
}
