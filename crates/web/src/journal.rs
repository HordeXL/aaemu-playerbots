//! What a bot remembers between runs, in the web API (the journal): the view in the bot detail.
//! The features that own a journal section fill their part.

use serde::{Deserialize, Serialize};

/// A short view of the bot's journal.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct JournalView {
    /// Zones the bot has been in.
    pub zones_seen: u32,
    /// Play sessions so far.
    pub sessions: u32,
    /// Seconds played in all.
    pub played_secs: u64,
    /// Plots it keeps out in the wild.
    pub wild_plots: u32,
    /// Its notes (memories), oldest first.
    pub notes: Vec<String>,
}
