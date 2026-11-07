//! The `sessions` section of the journal: when a bot played (owned by the presence feature).

use serde::{Deserialize, Serialize};

/// The bot's play sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sessions {
    /// Sessions so far (one per log-in into the world).
    pub count: u32,
    /// Seconds played in all.
    pub total_secs: u64,
    /// Unix seconds of the last log-in, 0 if never.
    pub last_login_unix: u64,
    /// Unix seconds of the last log-out, 0 if never.
    pub last_logout_unix: u64,
}
