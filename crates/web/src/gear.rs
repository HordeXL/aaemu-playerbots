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
