//! Chat data a bot keeps in its hub slot: the system lines goals read and the recent chat lines
//! the web UI shows in the bot's detail panel.

use std::collections::VecDeque;

use pb_web::LogLine;

/// System / notice lines kept per bot.
pub const SYS_LINES: usize = 8;
/// Chat lines kept per bot for the detail panel.
pub const CHAT_LINES: usize = 8;
/// Longest stored line, in characters.
const MAX_CHARS: usize = 200;

/// A system (`-2`) or notice (`-1`) line the server sent to the bot, for example the answer to a
/// GM command such as `/build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysLine {
    /// Bot clock (ms) when it arrived; compare with [`Ctx::now_ms`](crate::bot::ctx::Ctx).
    pub at_ms: u64,
    /// Chat type, `-2` (system) or `-1` (notice).
    pub kind: i16,
    /// The text.
    pub text: Box<str>,
}

/// The chat part of a hub slot.
#[derive(Debug, Default)]
pub struct SlotSocial {
    sys: VecDeque<SysLine>,
    chat: VecDeque<LogLine>,
}

impl SlotSocial {
    /// Keeps a system line, dropping the oldest beyond [`SYS_LINES`].
    pub fn push_system(&mut self, line: SysLine) {
        if self.sys.len() >= SYS_LINES {
            self.sys.pop_front();
        }
        self.sys.push_back(line);
    }

    /// The system lines that arrived at or after `since_ms`, oldest first.
    pub fn system_since(&self, since_ms: u64) -> Vec<SysLine> {
        self.sys
            .iter()
            .filter(|l| l.at_ms >= since_ms)
            .cloned()
            .collect()
    }

    /// Keeps a chat line for the detail panel (`t` is unix milliseconds).
    pub fn push_chat(&mut self, t: u64, text: &str) {
        if self.chat.len() >= CHAT_LINES {
            self.chat.pop_front();
        }
        self.chat.push_back(LogLine {
            t,
            text: text.chars().take(MAX_CHARS).collect(),
        });
    }

    /// The chat lines, oldest first.
    pub fn chat_lines(&self) -> Vec<LogLine> {
        self.chat.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(at_ms: u64, text: &str) -> SysLine {
        SysLine {
            at_ms,
            kind: -2,
            text: text.into(),
        }
    }

    #[test]
    fn system_lines_are_kept_in_a_bounded_ring_and_read_by_time() {
        let mut s = SlotSocial::default();
        for i in 0..20u64 {
            s.push_system(sys(100 + i, &format!("line {i}")));
        }
        let all = s.system_since(0);
        assert_eq!(all.len(), SYS_LINES);
        assert_eq!(&*all[0].text, "line 12");
        assert_eq!(&*all[SYS_LINES - 1].text, "line 19");
        let late = s.system_since(118);
        assert_eq!(late.len(), 2);
        assert!(s.system_since(1000).is_empty());
    }

    #[test]
    fn chat_lines_are_bounded_and_cut() {
        let mut s = SlotSocial::default();
        for i in 0..20u64 {
            s.push_chat(i, &"x".repeat(500));
        }
        let lines = s.chat_lines();
        assert_eq!(lines.len(), CHAT_LINES);
        assert_eq!(lines[0].t, 12);
        assert_eq!(lines[0].text.chars().count(), MAX_CHARS);
    }
}
