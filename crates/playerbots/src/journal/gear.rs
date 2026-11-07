//! The `gear` section of the journal: what a bot learned about its equipment (owned by the gear
//! feature).

use serde::{Deserialize, Serialize};

/// What the bot learned about its gear.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GearNotes {
    /// Unix seconds of the last time the bot changed its gear, 0 if never.
    pub last_change_unix: u64,
}
