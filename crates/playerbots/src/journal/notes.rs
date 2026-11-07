//! The `notes` section of the journal: short memories in the bot's own words (owned by the LLM and
//! the agent API). A note is untrusted text (a model or an operator wrote it): it is only ever shown
//! to the model as data, never as instructions, and never sent to the chat as it is.

use serde::{Deserialize, Serialize};

/// Notes kept at most (the oldest go first).
pub const MAX_NOTES: usize = 16;
/// Longest note, in characters.
pub const MAX_NOTE_CHARS: usize = 160;

/// One memory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    /// Unix seconds when it was written.
    pub at_unix: u64,
    /// The text, one line, at most [`MAX_NOTE_CHARS`] characters.
    pub text: String,
}

/// The bot's memories.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    /// Oldest first, at most [`MAX_NOTES`].
    pub lines: Vec<Note>,
}

impl Notes {
    /// Adds a note (flattened to one line without control characters and cut to
    /// [`MAX_NOTE_CHARS`]); an empty one is dropped. The oldest goes when the section is full.
    pub fn push(&mut self, at_unix: u64, text: &str) {
        let flat: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NOTE_CHARS)
            .collect();
        if flat.is_empty() {
            return;
        }
        if self.lines.len() >= MAX_NOTES {
            self.lines.remove(0);
        }
        self.lines.push(Note {
            at_unix,
            text: flat,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_one_short_line_and_bounded() {
        let mut n = Notes::default();
        n.push(1, "  hello\nworld\t ");
        assert_eq!(n.lines[0].text, "hello world");
        n.push(2, "   ");
        assert_eq!(n.lines.len(), 1, "an empty note is dropped");
        n.push(3, &"x".repeat(500));
        assert_eq!(n.lines[1].text.chars().count(), MAX_NOTE_CHARS);
        for i in 0..40 {
            n.push(10 + i, "again");
        }
        assert_eq!(n.lines.len(), MAX_NOTES);
        assert_eq!(n.lines[0].at_unix, 10 + 40 - MAX_NOTES as u64);
    }
}
