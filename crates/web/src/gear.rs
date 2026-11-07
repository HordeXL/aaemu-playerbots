//! A bot's gear in the web API (owned by the gear feature): the view in the bot detail.

use serde::{Deserialize, Serialize};

/// What the bot wears and how it stands with its gear.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct GearView {
    /// One line: the tier, the average item level, what the bot is after.
    pub summary: String,
    /// The equipped pieces, in slot order.
    pub lines: Vec<GearLine>,
    /// The level band (level / 10, at most 5).
    pub tier: u8,
    /// The lowest item level the band is content with.
    pub min_item_level: u8,
    /// The average item level of the worn gear.
    pub avg_item_level: f32,
    /// How far the core slots are below the tier, 0 (fine) to 1 (empty).
    pub deficit: f32,
    /// What the gear goal could do now (`buy`, `mint`, `socket`, `regrade`).
    pub wants: Vec<String>,
    /// The last change in words.
    pub last_change: String,
    /// What the bot did with its gear this session.
    pub stats: GearStatsView,
}

/// One equipped piece.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct GearLine {
    /// The slot (`head`, `chest`, `mainhand`, ...).
    pub slot: String,
    /// The item's name, or its template id when the name is not known.
    pub item: String,
    /// The item's level.
    pub level: u8,
    /// The grade (`common`, `uncommon`, ...).
    pub grade: String,
    /// Gems socketed in it.
    pub gems: u8,
}

/// What a bot did with its gear this session.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct GearStatsView {
    /// Pieces put on.
    pub equipped: u32,
    /// Pieces bought.
    pub bought: u32,
    /// Copper spent on gear.
    pub spent: i64,
    /// Pieces minted.
    pub minted: u32,
    /// Gems socketed.
    pub socketed: u32,
    /// Socketings that failed.
    pub socket_fails: u32,
    /// Regrades that raised the grade.
    pub regraded: u32,
    /// Regrades that did not.
    pub regrade_fails: u32,
}
