//! The tick context: everything a goal or the combat controller may touch during one tick.

use std::sync::Arc;

use aa_proto::cs::{self, Out};
use aa_world::World;
use tokio::time::Instant;

use super::combat::Combat;
use super::coordinator::CoordHandle;
use super::motion::{Motion, MotionEnv, Pace};
use super::state::BotState;
use crate::archetype::Archetype;
use crate::config::Config;
use crate::hub::{Hub, Logger};
use crate::roster::Temperament;

/// Who a bot is: fixed for the life of its task (a rename only changes the name).
#[derive(Debug, Clone)]
pub struct Persona {
    /// Bot id.
    pub id: u32,
    /// Character name.
    pub name: Box<str>,
    /// Class build.
    pub archetype: Arc<Archetype>,
    /// Temperament with the configured spread applied.
    pub temper: Temperament,
}

/// Everything one tick works with. Built by the bot task around its own fields; goals receive
/// it as `&mut Ctx` and must not keep any borrow of it between ticks.
pub struct Ctx<'a> {
    /// The static world, shared by all bots.
    pub world: &'a World,
    /// The configuration.
    pub cfg: &'a Config,
    /// Who the bot is.
    pub persona: &'a Persona,
    /// What the bot knows.
    pub state: &'a mut BotState,
    /// The movement controller.
    pub motion: &'a mut Motion,
    /// The combat controller.
    pub combat: &'a mut Combat,
    /// Packets to send after the tick (the bot task flushes them).
    pub out: &'a mut Vec<Out>,
    /// The pairing service.
    pub coord: &'a CoordHandle,
    /// The hub: map markers, the other bots' character ids.
    pub hub: &'a Hub,
    /// The bot's event log.
    pub log: &'a Logger,
    /// Randomness (per bot, so ticks do not contend).
    pub rng: &'a mut fastrand::Rng,
    /// The tick's wall-clock instant.
    pub now: Instant,
    /// The bot clock in milliseconds (monotonic, starts at the task start); the same value as
    /// `state.now_ms`.
    pub now_ms: u64,
    /// A real player (a character that is not one of our bots) is within 150 m.
    pub real_player_near: bool,
}

impl Ctx<'_> {
    /// The bot's position.
    pub fn pos(&self) -> [f32; 3] {
        self.state.me.pos
    }

    /// Queues a packet.
    pub fn send(&mut self, out: Out) {
        self.out.push(out);
    }

    /// Says a line in local chat. Lines starting with `/` are refused (the server would run them
    /// as GM commands); use [`Ctx::gm`] for those.
    pub fn say(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() || text.starts_with('/') {
            return;
        }
        self.out.push(cs::chat(cs::chat_type::SAY, "", text));
    }

    /// Sends a GM command (`/item add self ...`). Only when `bots.gm_bootstrap` is on; returns
    /// whether it was sent.
    pub fn gm(&mut self, line: &str) -> bool {
        if !self.cfg.bots.gm_bootstrap || !line.starts_with('/') {
            return false;
        }
        self.out.push(cs::chat(cs::chat_type::SAY, "", line));
        true
    }

    /// Whether GM commands may be used.
    pub fn gm_allowed(&self) -> bool {
        self.cfg.bots.gm_bootstrap
    }

    /// Starts a trip to (x, y), stopping `radius` metres short. Long trips use `/move` when the
    /// config allows it.
    pub fn travel(&mut self, x: f32, y: f32, radius: f32, pace: Pace) {
        let teleport = if self.cfg.bots.gm_bootstrap {
            self.cfg.bots.long_travel_teleport_m
        } else {
            0.0
        };
        let mut env = MotionEnv {
            ground: self.world,
            now_ms: self.now_ms,
            out: &mut *self.out,
            rng: &mut *self.rng,
            send_gap_ms: 0,
            combat: false,
        };
        self.motion
            .travel_to(x, y, radius, pace, teleport, &mut env);
    }

    /// Whether `bots.gm_bootstrap` lets this bot use `/move` for a trip of `dist` metres.
    pub fn would_teleport(&self, dist: f32) -> bool {
        self.cfg.bots.gm_bootstrap
            && self.cfg.bots.long_travel_teleport_m > 0.0
            && dist > self.cfg.bots.long_travel_teleport_m
    }
}
