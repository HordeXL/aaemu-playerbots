//! Presence: bots log in and out on schedules of their own, so the world looks played (owned by
//! the presence feature).
//!
//! This file is the hook the hub calls. Until the feature fills it in, [`Presence`] does nothing
//! and every bot stays online as before. Already routed here:
//!
//! * [`Presence::tick`], called by `Hub::tick` once a second.
//! * `Hub::with_presence` sets it up at start (from `[presence]`).
//! * The switches to gate on: `cfg.presence` (`enabled`), each bot's
//!   `Profile::{dedication, chronotype}` in the roster, its `sessions` section in the journal
//!   (`hub.journal`).

use crate::hub::Hub;

/// The log-in and log-out schedules. See the module docs.
#[derive(Debug, Default)]
pub struct Presence {}

impl Presence {
    /// Once a second, from `Hub::tick`.
    pub fn tick(&self, _hub: &Hub) {}
}
