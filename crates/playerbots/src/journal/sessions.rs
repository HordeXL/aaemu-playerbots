//! The `sessions` section of the journal: when a bot played (owned by the presence feature).
//!
//! The bot task writes it: [`Sessions::login`] when the bot enters the world, [`Sessions::logout`]
//! when it leaves it again (a lost connection, a log-out the presence schedules asked for, the end
//! of the process). A session that never reached the world is not counted.

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

impl Sessions {
    /// The bot entered the world at `now_unix`.
    pub fn login(&mut self, now_unix: u64) {
        self.count = self.count.saturating_add(1);
        self.last_login_unix = now_unix;
    }

    /// The bot left the world at `now_unix` after `secs` seconds in it.
    pub fn logout(&mut self, now_unix: u64, secs: u64) {
        self.total_secs = self.total_secs.saturating_add(secs);
        self.last_logout_unix = now_unix;
    }

    /// The average session in seconds, 0 before the first.
    pub fn average_secs(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.total_secs / u64::from(self.count)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_counts_at_login_and_adds_its_time_at_logout() {
        let mut s = Sessions::default();
        assert_eq!(s.average_secs(), 0);
        s.login(1_000);
        assert_eq!((s.count, s.last_login_unix), (1, 1_000));
        s.logout(1_600, 600);
        s.login(5_000);
        s.logout(5_300, 300);
        assert_eq!(s.count, 2);
        assert_eq!(s.total_secs, 900);
        assert_eq!(s.last_logout_unix, 5_300);
        assert_eq!(s.average_secs(), 450);
        // nothing overflows
        s.total_secs = u64::MAX - 1;
        s.logout(6_000, 10);
        assert_eq!(s.total_secs, u64::MAX);
    }
}
