//! JSONC reading: the server's data files use `//` and `/* */` comments and trailing commas
//! (Newtonsoft.Json accepts both); `serde_json` does not.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

/// Removes a UTF-8 BOM, `//` and `/* */` comments (outside strings) and trailing commas before
/// `]` / `}`. Line breaks inside line comments are kept so error positions stay meaningful.
pub fn strip(src: &str) -> String {
    let b = src.strip_prefix('\u{feff}').unwrap_or(src).as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                // Copy the string literal verbatim, honouring escapes.
                out.push(b'"');
                i += 1;
                while i < b.len() {
                    let c = b[i];
                    out.push(c);
                    i += 1;
                    if c == b'\\' {
                        if i < b.len() {
                            out.push(b[i]);
                            i += 1;
                        }
                    } else if c == b'"' {
                        break;
                    }
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(b' ');
            }
            b',' => {
                // Look ahead (without consuming) to the next significant byte.
                let mut j = i + 1;
                loop {
                    match b.get(j) {
                        Some(c) if c.is_ascii_whitespace() => j += 1,
                        Some(b'/') if b.get(j + 1) == Some(&b'/') => {
                            while j < b.len() && b[j] != b'\n' {
                                j += 1;
                            }
                        }
                        Some(b'/') if b.get(j + 1) == Some(&b'*') => {
                            j += 2;
                            while j < b.len() && !(b[j] == b'*' && b.get(j + 1) == Some(&b'/')) {
                                j += 1;
                            }
                            j = (j + 2).min(b.len());
                        }
                        _ => break,
                    }
                }
                if !matches!(b.get(j), Some(b']' | b'}')) {
                    out.push(b',');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Only ASCII delimiters are ever cut, so the output stays valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Deserializes JSONC text.
pub fn from_str<T: DeserializeOwned>(src: &str) -> Result<T> {
    serde_json::from_str(&strip(src)).context("parse JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_and_block_comments() {
        let s = strip("[1, // one\n 2, /* two */ 3]");
        let v: Vec<i32> = serde_json::from_str(&s).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn strips_trailing_commas() {
        let v: serde_json::Value = from_str(r#"{"a": [1, 2, ], "b": {"c": 3,},}"#).unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"]["c"], 3);
    }

    #[test]
    fn trailing_comma_before_comment_then_close() {
        let v: Vec<i32> = from_str("[1, 2, // last\n]").unwrap();
        assert_eq!(v, vec![1, 2]);
        let v: Vec<i32> = from_str("[1, 2, /* x */ ]").unwrap();
        assert_eq!(v, vec![1, 2]);
    }

    #[test]
    fn comment_markers_inside_strings_are_kept() {
        let v: Vec<String> = from_str(r#"["http://x", "a /* b */ c", "q\"//r"]"#).unwrap();
        assert_eq!(v, vec!["http://x", "a /* b */ c", "q\"//r"]);
    }

    #[test]
    fn handles_bom_and_multibyte_comments() {
        let v: Vec<i32> = from_str("\u{feff}[1, // 한국어 comment\n 2]").unwrap();
        assert_eq!(v, vec![1, 2]);
    }

    #[test]
    fn unterminated_block_comment_does_not_hang() {
        let s = strip("[1] /* never closed");
        assert_eq!(s.trim(), "[1]");
    }
}
