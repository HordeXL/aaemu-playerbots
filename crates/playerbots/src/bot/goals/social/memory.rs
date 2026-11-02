//! The tiny per-conversation memory of a bot: the last few lines it exchanged with each player it
//! talked to lately, so an LLM reply can follow the thread. Bounded: a few conversations, a few
//! lines each, forgotten after a while.

use std::collections::VecDeque;

use pb_llm::Msg;
use smallvec::SmallVec;

use super::gate::RateWindow;

/// Lines remembered per conversation (both sides together).
pub const LINES: usize = 4;
/// Conversations remembered.
pub const CONVERSATIONS: usize = 3;
/// A conversation is forgotten after this long without a line.
pub const FORGET_MS: u64 = 10 * 60_000;
/// Replies one conversation gets per [`REPLY_SPAN_MS`].
pub const REPLIES: usize = 6;
/// The window of [`REPLIES`].
pub const REPLY_SPAN_MS: u64 = 10 * 60_000;
/// Longest remembered line, in characters.
const MAX_CHARS: usize = 200;

#[derive(Debug)]
struct Conv {
    /// Lower-cased name of the other side.
    key: Box<str>,
    /// `(said by the bot, text)`, oldest first.
    lines: VecDeque<(bool, Box<str>)>,
    last_ms: u64,
    replies: RateWindow<REPLIES>,
}

impl Conv {
    fn push(&mut self, mine: bool, text: &str, now_ms: u64) {
        if self.lines.len() >= LINES {
            self.lines.pop_front();
        }
        self.lines
            .push_back((mine, text.chars().take(MAX_CHARS).collect()));
        self.last_ms = now_ms;
    }
}

/// The conversations of one bot.
#[derive(Debug, Default)]
pub struct Memory {
    convs: SmallVec<[Conv; CONVERSATIONS]>,
}

impl Memory {
    /// The conversation with `who`, created (evicting the stalest) when new.
    fn conv(&mut self, who: &str, now_ms: u64) -> &mut Conv {
        let key = who.to_lowercase();
        self.convs
            .retain(|c| now_ms.saturating_sub(c.last_ms) < FORGET_MS);
        if let Some(i) = self.convs.iter().position(|c| *c.key == *key) {
            return &mut self.convs[i];
        }
        if self.convs.len() >= CONVERSATIONS {
            if let Some(oldest) = (0..self.convs.len()).min_by_key(|i| self.convs[*i].last_ms) {
                self.convs.remove(oldest);
            }
        }
        self.convs.push(Conv {
            key: key.into_boxed_str(),
            lines: VecDeque::new(),
            last_ms: now_ms,
            replies: RateWindow::default(),
        });
        let last = self.convs.len() - 1;
        &mut self.convs[last]
    }

    /// Whether the conversation with `who` may get another reply (nothing is recorded).
    pub fn reply_allowed(&mut self, who: &str, now_ms: u64) -> bool {
        self.conv(who, now_ms)
            .replies
            .has_room(now_ms, REPLY_SPAN_MS)
    }

    /// Records a line `who` said to the bot.
    pub fn heard(&mut self, who: &str, text: &str, now_ms: u64) {
        self.conv(who, now_ms).push(false, text, now_ms);
    }

    /// Records a reply the bot sent to `who` and counts it against the conversation's quota.
    pub fn said(&mut self, who: &str, text: &str, now_ms: u64) {
        let c = self.conv(who, now_ms);
        c.push(true, text, now_ms);
        let _ = c.replies.try_take(now_ms, REPLY_SPAN_MS);
    }

    /// The conversation as chat messages for the model, oldest first, starting with a line of the
    /// other side (the model's turn is next).
    pub fn messages(&mut self, who: &str, now_ms: u64) -> Vec<Msg> {
        let c = self.conv(who, now_ms);
        let mut out: Vec<Msg> = Vec::with_capacity(c.lines.len());
        for (mine, text) in &c.lines {
            if out.is_empty() && *mine {
                continue;
            }
            out.push(if *mine {
                Msg::assistant(text.as_ref())
            } else {
                Msg::user(text.as_ref())
            });
        }
        out
    }

    /// Conversations currently remembered.
    pub fn len(&self) -> usize {
        self.convs.len()
    }

    /// Whether nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.convs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pb_llm::Role;

    #[test]
    fn a_conversation_keeps_the_last_four_lines_in_order() {
        let mut m = Memory::default();
        for i in 0..6u64 {
            m.heard("Bob", &format!("q{i}"), 1000 + i);
            m.said("Bob", &format!("a{i}"), 1000 + i);
        }
        let msgs = m.messages("bob", 2000);
        let text: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            text,
            ["q4", "a4", "q5", "a5"].map(str::to_string).as_slice()
        );
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[test]
    fn the_thread_starts_with_the_other_sides_line() {
        let mut m = Memory::default();
        m.heard("Bob", "one", 1);
        m.said("Bob", "two", 2);
        m.heard("Bob", "three", 3);
        m.said("Bob", "four", 4);
        m.heard("Bob", "five", 5);
        // the oldest line ("one") fell out, "two" is the bot's own and is skipped
        let msgs = m.messages("Bob", 6);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].content, "three");
    }

    #[test]
    fn the_memory_is_bounded_and_forgets() {
        let mut m = Memory::default();
        for (i, who) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            m.heard(who, "hi", 100 + i as u64);
        }
        assert_eq!(m.len(), CONVERSATIONS);
        assert!(m.messages("a", 200).is_empty(), "the stalest was evicted");
        assert_eq!(m.messages("e", 200).len(), 1);
        // after the forget time nothing is left
        assert!(m.messages("e", 200 + FORGET_MS).is_empty());
        assert!(m.len() <= 1);
    }

    #[test]
    fn a_conversation_gets_a_limited_number_of_replies() {
        let mut m = Memory::default();
        for i in 0..REPLIES as u64 {
            assert!(m.reply_allowed("Bob", 1000 + i));
            m.heard("Bob", "hi", 1000 + i);
            m.said("Bob", "hello", 1000 + i);
        }
        assert!(!m.reply_allowed("Bob", 5000));
        assert!(m.reply_allowed("Alice", 5000), "other players are separate");
        // later the quota is back
        m.heard("Bob", "still there?", 1000 + REPLY_SPAN_MS + 10);
        assert!(m.reply_allowed("Bob", 1000 + REPLY_SPAN_MS + 10));
    }

    #[test]
    fn long_lines_are_cut() {
        let mut m = Memory::default();
        m.heard("Bob", &"x".repeat(1000), 1);
        assert_eq!(m.messages("Bob", 2)[0].content.chars().count(), MAX_CHARS);
    }
}
