//! World events the bots take part in: the process-wide board of the events that are running.
//!
//! The server announces a tower defence event ("rift") to everybody in the world with
//! `SCTowerDefStart`, `SCTowerDefWaveStart`, `SCTowerDefEnd` and a fresh `SCTowerDefList` after
//! each. Every bot gets the same packets, so the first bot to see one posts it to the
//! [`EventBoard`] and the others find it there already ([`on_server_event`], called by the bot
//! loop). The board is what the event goal ([`crate::bot::goals::event`]) reads: which events run,
//! in which wave, where the defended target stands, and how many bots already took part.
//!
//! * **Flood control.** A bot takes a *slot* before it goes: at most `[events] max_bots_per_event`
//!   per event and, over all events, `max_share` of the online bots that are high enough for the
//!   event ([`Population`]). Before it asks, a bot waits a random time in `join_delay_secs`, drawn
//!   from its id and the event so that it is the same every time ([`EventBoard::ready`]). A slot is
//!   a lease the goal renews every tick; one that is not renewed ([`LEASE_MS`]) is free again, so
//!   a bot that vanished costs nothing. A bot that left hurt, or died three times, is barred from
//!   the event for a while ([`Leave`]).
//! * **Evidence.** The board also counts what it hears: the kill-target NPCs that die (each object
//!   once, however many bots see it), whether a wave NPC or a controller has been seen, how long a
//!   participant stood at the target. [`detect`] turns that into server issue reports: a wave
//!   whose NPCs never showed, a wave that did not advance after its kills, an event the server
//!   never ended.
//!
//! The board is one short mutex, touched a few times a second by the bots that look for an event
//! and by the bots in the goal; with no event running a bot reads one atomic ([`EventBoard::any`]).

pub mod board;
pub mod detect;

#[cfg(test)]
mod tests;

pub use board::{EventBoard, Stats};

use aa_proto::sc::ServerEvent;
use aa_proto::sc_event::EventId;
use aa_proto::sc_world::{unit_type, WorldEvent};
use aa_world::World;
use std::sync::Arc;

use crate::bot::state::{BotState, UnitKind};

/// Where the board gets the static event data from: the world, or a fixture in the tests.
pub trait Source: Send + Sync {
    /// The event definitions.
    fn events(&self) -> &aa_world::Events;
}

impl Source for World {
    fn events(&self) -> &aa_world::Events {
        World::events(self)
    }
}

/// How long a slot lasts without being renewed.
pub const LEASE_MS: u64 = 25_000;
/// A bot that left an event hurt stays away this long.
pub const HURT_BAR_MS: u64 = 5 * 60_000;
/// A bot that gave up on an event with nothing to fight stays away this long.
pub const IDLE_BAR_MS: u64 = 10 * 60_000;
/// Deaths in one event after which a bot is done with it.
pub const MAX_DEATHS: u8 = 3;
/// The most events the board keeps.
pub const MAX_EVENTS: usize = 8;

/// A running event as the bots see it. `Copy`, a snapshot of the board.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// Which event.
    pub id: EventId,
    /// The zone key the event was started in.
    pub zone_key: u32,
    /// The wave (0 before the first).
    pub step: u32,
    /// The board clock (ms) at which the wave began.
    pub step_since_ms: u64,
    /// The board clock (ms) at which the board first heard of the event.
    pub first_seen_ms: u64,
    /// The defended target: `(object id, position)`; `None` while the server names none.
    pub target: Option<(u32, [f32; 3])>,
    /// Bots that hold a slot now.
    pub joined: u32,
}

/// The limits on how many bots join (from `[events]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    /// Most bots in one event.
    pub per_event: u32,
    /// Most share (0 to 1) of the bots that are high enough, over all events.
    pub max_share: f32,
    /// The lowest level the event needs: who counts as eligible for the share.
    pub min_level: u8,
}

/// What came of asking for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    /// The bot has a slot now.
    Joined,
    /// It had one already (the lease is renewed).
    Already,
    /// The event holds `per_event` bots.
    Full,
    /// The events together hold `max_share` of the eligible bots.
    OverShare,
    /// The bot left this event hurt, died in it too often or found nothing to fight.
    Barred,
    /// The event is over (or the board never heard of it).
    Gone,
}

/// Why a bot leaves an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    /// The event ended, or the goal was interrupted for another reason: no hard feelings.
    Done,
    /// The bot's hit points ran low: it stays away for [`HURT_BAR_MS`].
    Hurt,
    /// The bot died in the event: three times and it is done with the event.
    Died,
    /// Nothing to fight for a long time: stays away for [`IDLE_BAR_MS`].
    Idle,
}

/// The online bots by level: the denominator of the share cap. Rebuilt about once a second by the
/// hub while an event runs.
#[derive(Debug, Clone)]
pub struct Population {
    /// Bots online.
    pub online: u32,
    levels: [u16; Population::LEVELS],
}

impl Default for Population {
    fn default() -> Self {
        Population {
            online: 0,
            levels: [0; Population::LEVELS],
        }
    }
}

impl Population {
    /// Levels the histogram tells apart (the last bucket also holds the higher ones).
    pub const LEVELS: usize = 128;

    /// The population of online bots with these levels.
    pub fn from_levels(levels: impl IntoIterator<Item = u8>) -> Population {
        let mut p = Population::default();
        for l in levels {
            let i = usize::from(l).min(Population::LEVELS - 1);
            p.levels[i] = p.levels[i].saturating_add(1);
            p.online += 1;
        }
        p
    }

    /// Online bots of at least `min_level`.
    pub fn eligible(&self, min_level: u8) -> u32 {
        self.levels[usize::from(min_level).min(Population::LEVELS - 1)..]
            .iter()
            .map(|&n| u32::from(n))
            .sum()
    }

    /// How many bots may be in events at once: `share` of the eligible ones, at least one.
    /// `None` while the population is not known (no bot counted yet).
    pub fn share_cap(&self, min_level: u8, share: f32) -> Option<u32> {
        if self.online == 0 {
            return None;
        }
        let share = if share.is_finite() {
            share.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // (the small allowance keeps 0.3 of 20 at 6: 0.3 is a hair above itself in f32)
        let n = (share * self.eligible(min_level) as f32 - 0.001).ceil() as u32;
        Some(n.max(1))
    }
}

/// Hands the packets and unit events that concern events to the board. Called by the bot loop for
/// every server event after the bot's own state has taken it; a cheap match, and a read of one
/// atomic while no event runs.
pub fn on_server_event(board: &EventBoard, world: &Arc<World>, ev: &ServerEvent, state: &BotState) {
    let ServerEvent::World(w) = ev else {
        return;
    };
    match w {
        WorldEvent::TowerDefList(_)
        | WorldEvent::TowerDefStart { .. }
        | WorldEvent::TowerDefWave { .. }
        | WorldEvent::TowerDefEnd { .. } => {
            board.bind(world);
            board.on_event(w, !state.place.in_instance());
        }
        WorldEvent::UnitDeath { obj, .. } if board.any() => {
            if let Some(u) = state.units.get(obj).filter(|u| u.kind == UnitKind::Npc) {
                board.note_death(*obj, u.id);
            }
        }
        WorldEvent::UnitState(info) if info.unit_type == unit_type::NPC && board.any() => {
            board.note_unit(info.id);
        }
        _ => {}
    }
}
