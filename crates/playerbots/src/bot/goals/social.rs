//! Social behavior: chat.
//!
//! Not a goal the brain chooses but a duty that runs on every brain tick, whatever the bot is
//! doing ([`Social::tick`], called by the [`Brain`](crate::bot::brain::Brain)).
//!
//! # What it does
//!
//! * **Routes the chat inbox.** Whispers and local lines go to the reply logic. System (`-2`) and
//!   notice (`-1`) lines, for example the answer to a GM `/build`, are *not* thrown away: they go
//!   into a small per-bot ring that goals read with [`system_lines`]. Everything else is dropped.
//! * **Answers players.** A whisper to the bot is always a candidate. A local line is a candidate
//!   when it names the bot, or comes from a real player (a character that is not one of ours)
//!   within `social.reply_radius_m` and a dice roll (`social.reply_chance`) says yes; of all the
//!   bots that hear such a line only the first to claim it answers. Whispers are answered with a
//!   whisper, local lines in local chat.
//! * **Replies** come from the LLM when it is on ([`reply`]: a task off the bot's task, polled on
//!   the tick) and from a small table of persona-flavoured lines ([`canned`]) otherwise or when
//!   the LLM is busy. Every line, whoever wrote it, passes [`pb_llm::sanitize_reply`] and the
//!   check in [`say_packet`] / [`whisper_packet`]: **a line that starts with `/` is never sent**.
//! * **Bot-to-bot chatter**: a "gg" after a duel, a thanks after a trade, now and then a hello to
//!   a bot standing close by. Strictly rate limited: per bot (`social.chatter_gap_secs`) and for
//!   all bots together (`social.chatter_per_min`); by default only while a real player is near.
//!
//! # Limits that keep it quiet
//!
//! Per bot: a gap between replies, a burst cap, a cap per conversation ([`memory`]); across bots:
//! replies and chatter per minute and one answer per overheard line ([`gate::ChatGate`] in the
//! hub). The LLM has its own limits (requests per minute, tokens per day) in `pb-llm`.
//!
//! The core is [`Social::step`], free of the tick context so it can be tested with plain values;
//! [`Social::tick`] adapts a [`Ctx`] to it.

pub mod canned;
pub mod gate;
pub mod memory;
pub mod reply;
pub mod ring;

#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use aa_proto::cs::{self, Out};
use aa_world::World;
use rustc_hash::{FxHashMap, FxHasher};
use smallvec::SmallVec;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

use self::canned::{Chatter, Intent, Recent};
use self::memory::Memory;
use self::reply::LlmReply;
pub use self::ring::{SlotSocial, SysLine};
use crate::bot::ctx::{Ctx, Persona};
use crate::bot::state::{ChatIn, Me, Unit, UnitKind};
use crate::config::{race_name, SocialCfg};
use crate::hub::{unix_ms, Hub};

/// Pause between two replies of a bot, in milliseconds (random in the range).
const REPLY_GAP_MS: (u64, u64) = (8_000, 14_000);
/// Replies one bot may send within [`BURST_SPAN_MS`].
const BURST: usize = 4;
/// The window of [`BURST`].
const BURST_SPAN_MS: u64 = 120_000;
/// Think time before a canned reply is sent.
const CANNED_DELAY_MS: (u64, u64) = (1_500, 4_000);
/// Extra think time after an LLM reply arrived.
const LLM_DELAY_MS: (u64, u64) = (200, 1_000);
/// An LLM request that has not answered after this long is forgotten.
const PENDING_TTL_MS: u64 = 25_000;
/// After a failed LLM request the bot answers from the table for this long.
const LLM_BACKOFF_MS: u64 = 30_000;
/// A duel or trade is worth a remark for this long.
const EVENT_TTL_MS: u64 = 25_000;
/// Pause between two looks for a bot to greet.
const SCAN_MS: (u64, u64) = (15_000, 30_000);
/// Pause between two greetings of a bot.
const HELLO_GAP_MS: (u64, u64) = (8 * 60_000, 20 * 60_000);
/// A bot this close (metres) gets a hello.
const HELLO_RADIUS: f32 = 12.0;
/// Longest character name a whisper may target.
const MAX_TARGET_CHARS: usize = 32;

// ---- the send path ---------------------------------------------------------------------------

/// The line as it may go on the wire: trimmed, one line, not empty, at most 200 characters, and
/// never command-like (see [`cs::plain_chat_text`]). `None` means "do not send".
pub fn wire_text(text: &str) -> Option<&str> {
    cs::plain_chat_text(text)
}

/// The packet for a local chat line, or `None` when the line must not be sent (empty, too long,
/// control characters, or a leading `/`). Every local line a bot says goes through here.
pub fn say_packet(text: &str) -> Option<Out> {
    Some(cs::chat(cs::chat_type::SAY, "", wire_text(text)?))
}

/// The packet for a whisper to `to`, or `None` (same rules as [`say_packet`], and `to` must be a
/// plain character name). A leading "GM " of a staff member's display name is removed.
pub fn whisper_packet(to: &str, text: &str) -> Option<Out> {
    let to = whisper_target(to)?;
    Some(cs::chat(cs::chat_type::WHISPER, to, wire_text(text)?))
}

/// The character name behind a sender name: "GM Nick" is the character "Nick".
pub fn strip_gm(name: &str) -> &str {
    let name = name.trim();
    match name.get(..3) {
        Some(prefix) if prefix.eq_ignore_ascii_case("gm ") => name[3..].trim(),
        _ => name,
    }
}

fn whisper_target(to: &str) -> Option<&str> {
    let to = strip_gm(to);
    let ok = !to.is_empty()
        && to.chars().count() <= MAX_TARGET_CHARS
        && to
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    ok.then_some(to)
}

/// A reply or remark that passes the safety filter, or `None`.
fn finalize(text: &str) -> Option<String> {
    let t = pb_llm::sanitize_reply(text)?;
    wire_text(&t).map(str::to_string)
}

// ---- what goals may read ---------------------------------------------------------------------

/// The system and notice lines the bot received at or after `since_ms` (the bot clock, see
/// [`Ctx::now_ms`]), oldest first. A goal that sends a GM command remembers `ctx.now_ms` and reads
/// the server's answer with this on later ticks.
pub fn system_lines(ctx: &Ctx<'_>, since_ms: u64) -> Vec<SysLine> {
    ctx.log.with_slot(|g| g.social.system_since(since_ms))
}

// ---- state and plumbing ----------------------------------------------------------------------

/// What the chat logic needs from the process: who is one of our bots, the LLM, the global gate.
pub trait Services {
    /// Whether character `id` is one of our bots.
    fn is_bot_char(&self, id: u32) -> bool;
    /// Whether a character called `name` is one of our bots.
    fn is_bot_name(&self, name: &str) -> bool;
    /// The LLM add-on, if on.
    fn llm(&self) -> Option<Arc<pb_llm::Llm>>;
    /// The process-wide chat limits and counters.
    fn gate(&self) -> &gate::ChatGate;
}

impl Services for Hub {
    fn is_bot_char(&self, id: u32) -> bool {
        Hub::is_bot_char(self, id)
    }

    fn is_bot_name(&self, name: &str) -> bool {
        self.chat.names.contains(name)
    }

    fn llm(&self) -> Option<Arc<pb_llm::Llm>> {
        Hub::llm(self).cloned()
    }

    fn gate(&self) -> &gate::ChatGate {
        &self.chat.gate
    }
}

/// Facts about the bot for flavour and for the LLM prompt.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// Character name.
    pub name: &'a str,
    /// Race name.
    pub race: &'a str,
    /// Class build name.
    pub class: &'a str,
    /// Dominant temperament trait (`quester`, `duelist`, ...).
    pub dominant: &'static str,
    /// Name of the zone the bot is in.
    pub zone: Option<&'a str>,
}

impl Facts<'_> {
    /// The persona the LLM plays.
    pub fn to_persona(&self) -> pb_llm::Persona {
        pb_llm::Persona {
            name: self.name.to_string(),
            race: self.race.to_string(),
            class: self.class.to_string(),
            temperament: temperament_text(self.dominant).to_string(),
            home_zone: self.zone.unwrap_or_default().to_string(),
        }
    }
}

/// The facts about the bot of this tick.
pub fn facts<'a>(ctx: &'a Ctx<'_>) -> Facts<'a> {
    facts_of(ctx.persona, &ctx.state.me, ctx.world)
}

/// [`facts`] from the parts, so a caller can keep other fields of the context borrowed.
fn facts_of<'a>(persona: &'a Persona, me: &Me, world: &'a World) -> Facts<'a> {
    Facts {
        name: &persona.name,
        race: race_name(me.race),
        class: &persona.archetype.name,
        dominant: persona.temper.dominant(),
        zone: world
            .zone_key_at(me.pos[0], me.pos[1])
            .and_then(|k| world.zone_name(k)),
    }
}

/// The temperament trait as a few words of character.
pub fn temperament_text(dominant: &str) -> &'static str {
    match dominant {
        "quester" => "cheerful quester, always chasing the next quest",
        "duelist" => "competitive and proud, loves a good duel",
        "trader" => "chatty haggler who loves a good deal",
        "merchant" => "practical trade-pack runner, business first",
        "homebody" => "laid-back homebody who likes a quiet corner",
        _ => "friendly and easygoing",
    }
}

/// Everything one step looks at.
pub struct Inputs<'a> {
    /// The bot clock in milliseconds.
    pub now_ms: u64,
    /// Wall-clock instant, for the process-wide limits.
    pub now: Instant,
    /// In the world and alive.
    pub active: bool,
    /// A real player is within the watch radius.
    pub real_near: bool,
    /// Where the bot is.
    pub pos: [f32; 3],
    /// Units in view.
    pub units: &'a FxHashMap<u32, Unit>,
    /// The `[social]` config.
    pub cfg: &'a SocialCfg,
    /// Who the bot is.
    pub facts: Facts<'a>,
    /// Randomness.
    pub rng: &'a mut fastrand::Rng,
}

/// A line to put on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Local chat.
    Say(String),
    /// A whisper to the named character.
    Whisper {
        /// Character name.
        to: String,
        /// The line.
        text: String,
    },
}

/// Something worth a remark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// A duel ended; the partner's name if known.
    DuelDone(Option<Box<str>>),
    /// A trade was completed.
    TradeDone(Option<Box<str>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Llm,
    Canned,
    Chatter,
}

/// A line waiting for its moment to be sent.
#[derive(Debug)]
struct Due {
    at_ms: u64,
    action: Action,
    how: Sent,
    /// The conversation partner (for the memory), empty for chatter.
    who: Box<str>,
}

/// A reply being asked of the LLM.
#[derive(Debug)]
struct Pending {
    rx: oneshot::Receiver<LlmReply>,
    plan: Plan,
    since_ms: u64,
}

/// Who to answer, where, and what they said.
#[derive(Debug, Clone)]
struct Plan {
    who: String,
    whisper: bool,
    addressed: bool,
    text: String,
    intent: Intent,
}

/// The chat state of one bot. See the module docs.
#[derive(Debug, Default)]
pub struct Social {
    next_reply_ms: u64,
    next_chatter_ms: u64,
    next_scan_ms: u64,
    next_hello_ms: u64,
    llm_off_until_ms: u64,
    burst: gate::RateWindow<BURST>,
    recent: Recent,
    pending: Option<Pending>,
    due: Option<Due>,
    event: Option<(ChatEvent, u64)>,
    mem: Memory,
}

fn rand_range(rng: &mut fastrand::Rng, (lo, hi): (u64, u64)) -> u64 {
    if hi <= lo {
        lo
    } else {
        lo + rng.u64(0..hi - lo)
    }
}

impl Social {
    /// A bot that just started: the first look for a bot to greet comes a little later.
    pub fn new(rng: &mut fastrand::Rng) -> Social {
        Social {
            next_scan_ms: rand_range(rng, SCAN_MS),
            ..Social::default()
        }
    }

    /// Something happened that the bot may remark on.
    pub fn note(&mut self, event: ChatEvent, now_ms: u64) {
        self.event = Some((event, now_ms));
    }

    /// Whether a reply is on its way (asked of the LLM or waiting to be sent).
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.due.is_some()
    }

    fn has_work(&self, now_ms: u64) -> bool {
        self.busy() || self.event.is_some() || now_ms >= self.next_scan_ms
    }

    /// One brain tick: files the inbox, answers, chatters, sends.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) {
        let mut heard: SmallVec<[ChatIn; 2]> = SmallVec::new();
        route_inbox(ctx, &mut heard);
        if heard.is_empty() && !self.has_work(ctx.now_ms) {
            return;
        }
        let facts = facts_of(ctx.persona, &ctx.state.me, ctx.world);
        let me = &ctx.state.me;
        let mut inp = Inputs {
            now_ms: ctx.now_ms,
            now: ctx.now.into_std(),
            active: me.spawned && !me.dead,
            real_near: ctx.real_player_near,
            pos: me.pos,
            units: &ctx.state.units,
            cfg: &ctx.cfg.social,
            facts,
            rng: &mut *ctx.rng,
        };
        let mut out: SmallVec<[Action; 2]> = SmallVec::new();
        self.step(&mut inp, ctx.hub, heard, &mut out);
        for action in out {
            send(ctx, action);
        }
    }

    /// The core: polls the LLM, sends what is due, considers what was heard, maybe chatters.
    pub fn step(
        &mut self,
        inp: &mut Inputs<'_>,
        svc: &dyn Services,
        heard: SmallVec<[ChatIn; 2]>,
        out: &mut SmallVec<[Action; 2]>,
    ) {
        self.poll_pending(inp);
        self.flush_due(inp, svc, out);
        if !inp.cfg.enabled || !inp.active {
            // Nothing is said while dead or switched off; what was in flight is forgotten.
            self.pending = None;
            self.due = None;
            self.event = None;
            return;
        }
        for line in &heard {
            if self.busy() {
                break;
            }
            self.consider(inp, svc, line);
        }
        self.chatter(inp, svc);
    }

    // ---- answers in flight -------------------------------------------------------------

    fn poll_pending(&mut self, inp: &mut Inputs<'_>) {
        let Some(p) = self.pending.as_mut() else {
            return;
        };
        let result = match p.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                if inp.now_ms.saturating_sub(p.since_ms) > PENDING_TTL_MS {
                    self.pending = None;
                }
                return;
            }
            Err(TryRecvError::Closed) => LlmReply::Failed,
        };
        let Some(p) = self.pending.take() else {
            return;
        };
        match result {
            LlmReply::Text(text) => {
                let at_ms = inp.now_ms + rand_range(inp.rng, LLM_DELAY_MS);
                let action = reply_action(&p.plan, text);
                self.due = Some(Due {
                    at_ms,
                    action,
                    how: Sent::Llm,
                    who: p.plan.who.clone().into_boxed_str(),
                });
            }
            LlmReply::Busy | LlmReply::Failed => {
                self.llm_off_until_ms = inp.now_ms + LLM_BACKOFF_MS;
                if p.plan.addressed {
                    self.schedule_canned(inp, &p.plan);
                }
            }
        }
    }

    fn flush_due(
        &mut self,
        inp: &mut Inputs<'_>,
        svc: &dyn Services,
        out: &mut SmallVec<[Action; 2]>,
    ) {
        let ready = self
            .due
            .as_ref()
            .is_some_and(|d| inp.now_ms >= d.at_ms && inp.active);
        if !ready {
            return;
        }
        let Some(due) = self.due.take() else {
            return;
        };
        let text = match &due.action {
            Action::Say(t) | Action::Whisper { text: t, .. } => t.clone(),
        };
        // The last look before the wire; a line that fails is dropped, not repaired.
        if wire_text(&text).is_none() {
            return;
        }
        let stats = &svc.gate().stats;
        match due.how {
            Sent::Llm => stats.llm_replies.fetch_add(1, Ordering::Relaxed),
            Sent::Canned => stats.canned_replies.fetch_add(1, Ordering::Relaxed),
            Sent::Chatter => stats.chatter.fetch_add(1, Ordering::Relaxed),
        };
        if due.how != Sent::Chatter {
            self.mem.said(&due.who, &text, inp.now_ms);
        }
        out.push(due.action);
    }

    // ---- hearing -----------------------------------------------------------------------

    /// Decides whether to answer `line` and starts the reply.
    fn consider(&mut self, inp: &mut Inputs<'_>, svc: &dyn Services, line: &ChatIn) {
        let whisper = line.kind == -3;
        if !whisper && line.kind != 0 {
            return;
        }
        let who = strip_gm(&line.name);
        if who.is_empty() || who.eq_ignore_ascii_case(inp.facts.name) {
            return;
        }
        let unit = inp.units.get(&line.sender_obj);
        let from_bot = match unit {
            Some(u) if u.kind == UnitKind::Character => svc.is_bot_char(u.id),
            // an NPC (or anything else that is not a player) talking
            Some(_) => return,
            None => svc.is_bot_name(who),
        };
        if from_bot && !inp.cfg.reply_to_bots {
            return;
        }
        let mentioned = mentions(&line.text, inp.facts.name);
        let addressed = whisper || mentioned;
        if !addressed {
            let near = unit.is_some_and(|u| u.dist2d(inp.pos) <= inp.cfg.reply_radius_m);
            if !near || inp.rng.f32() >= inp.cfg.reply_chance {
                return;
            }
        }
        // The bot's own limits first, so a bot that cannot answer does not claim the line.
        let now_ms = inp.now_ms;
        if now_ms < self.next_reply_ms
            || !self.burst.has_room(now_ms, BURST_SPAN_MS)
            || !self.mem.reply_allowed(who, now_ms)
        {
            return;
        }
        let gate = svc.gate();
        let key = line_key(who, &line.text);
        if addressed {
            gate.mark_claimed(key, inp.now);
        } else if !gate.claim_line(key, inp.now) {
            return;
        }
        if !gate.take_reply(inp.now, inp.cfg.replies_per_min) {
            return;
        }
        let plan = Plan {
            who: who.to_string(),
            whisper,
            addressed,
            text: line.text.to_string(),
            intent: canned::classify(&line.text),
        };
        self.start_reply(inp, svc, plan);
    }

    fn start_reply(&mut self, inp: &mut Inputs<'_>, svc: &dyn Services, plan: Plan) {
        let now_ms = inp.now_ms;
        self.next_reply_ms = now_ms + rand_range(inp.rng, REPLY_GAP_MS);
        let _ = self.burst.try_take(now_ms, BURST_SPAN_MS);
        self.mem.heard(&plan.who, &plan.text, now_ms);
        let llm = svc.llm().filter(|_| now_ms >= self.llm_off_until_ms);
        match llm {
            Some(llm) => {
                // The newest line is the last one remembered: the player's.
                let messages = self.mem.messages(&plan.who, now_ms);
                let request = reply::build_request(
                    &inp.facts.to_persona(),
                    &plan.who,
                    plan.whisper,
                    messages,
                );
                let rx = reply::spawn(llm, request);
                self.pending = Some(Pending {
                    rx,
                    plan,
                    since_ms: now_ms,
                });
            }
            None => self.schedule_canned(inp, &plan),
        }
    }

    fn schedule_canned(&mut self, inp: &mut Inputs<'_>, plan: &Plan) {
        let who = reply::flat_name(&plan.who);
        let who = if who == "someone" { String::new() } else { who };
        let line = canned::reply(
            plan.intent,
            inp.facts.dominant,
            &who,
            &mut self.recent,
            inp.rng,
        );
        let Some(text) = finalize(&line) else {
            return;
        };
        self.due = Some(Due {
            at_ms: inp.now_ms + rand_range(inp.rng, CANNED_DELAY_MS),
            action: reply_action(plan, text),
            how: Sent::Canned,
            who: plan.who.clone().into_boxed_str(),
        });
    }

    // ---- unsolicited lines -------------------------------------------------------------

    fn chatter(&mut self, inp: &mut Inputs<'_>, svc: &dyn Services) {
        let cfg = inp.cfg;
        if !cfg.bot_chatter {
            self.event = None;
            return;
        }
        if cfg.chatter_needs_player && !inp.real_near {
            // Nobody to hear it: the moment passes.
            self.event = None;
            return;
        }
        if self.busy() || inp.now_ms < self.next_chatter_ms {
            return;
        }
        if let Some((event, at_ms)) = self.event.take() {
            if inp.now_ms.saturating_sub(at_ms) > EVENT_TTL_MS || inp.rng.f32() >= 0.7 {
                return;
            }
            let (kind, who) = match &event {
                ChatEvent::DuelDone(w) => (Chatter::DuelDone, w.as_deref()),
                ChatEvent::TradeDone(w) => (Chatter::TradeDone, w.as_deref()),
            };
            let who = who.map(reply::flat_name).unwrap_or_default();
            self.say_chatter(inp, svc, kind, &who);
            return;
        }
        if inp.now_ms < self.next_scan_ms {
            return;
        }
        self.next_scan_ms = inp.now_ms + rand_range(inp.rng, SCAN_MS);
        if inp.now_ms < self.next_hello_ms || inp.rng.f32() >= 0.4 {
            return;
        }
        let near = inp
            .units
            .values()
            .filter(|u| u.kind == UnitKind::Character && svc.is_bot_char(u.id))
            .filter(|u| u.dist2d(inp.pos) <= HELLO_RADIUS)
            .filter_map(|u| u.name.as_deref())
            .next()
            .map(reply::flat_name);
        if let Some(who) = near {
            self.next_hello_ms = inp.now_ms + rand_range(inp.rng, HELLO_GAP_MS);
            self.say_chatter(inp, svc, Chatter::Hello, &who);
        }
    }

    fn say_chatter(&mut self, inp: &mut Inputs<'_>, svc: &dyn Services, kind: Chatter, who: &str) {
        if !svc.gate().take_chatter(inp.now, inp.cfg.chatter_per_min) {
            return;
        }
        let who = if who == "someone" { "" } else { who };
        let line = canned::chatter(kind, who, &mut self.recent, inp.rng);
        let Some(text) = finalize(&line) else {
            return;
        };
        let gap = inp.cfg.chatter_gap_secs.saturating_mul(1000);
        self.next_chatter_ms = inp.now_ms + gap + rand_range(inp.rng, (0, gap / 2 + 1));
        self.due = Some(Due {
            at_ms: inp.now_ms + rand_range(inp.rng, CANNED_DELAY_MS),
            action: Action::Say(text),
            how: Sent::Chatter,
            who: "".into(),
        });
    }
}

/// The action that answers `plan` with `text`: a whisper to the sender or a local line.
fn reply_action(plan: &Plan, text: String) -> Action {
    if plan.whisper {
        Action::Whisper {
            to: plan.who.clone(),
            text,
        }
    } else {
        Action::Say(text)
    }
}

/// Whether `text` contains the bot's name as a word.
pub fn mentions(text: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    if name.is_empty() {
        return false;
    }
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == name)
}

fn line_key(who: &str, text: &str) -> u64 {
    let mut h = FxHasher::default();
    who.to_lowercase().hash(&mut h);
    text.to_lowercase().hash(&mut h);
    h.finish()
}

/// Files the inbox: chat lines go to `heard` (and the detail panel), system lines to the ring
/// that goals read, the rest is dropped; shouts, trade and group-finding lines and whispers are
/// also handed to the market ([`crate::bot::market::forward_chat`]). Empties the inbox.
fn route_inbox(ctx: &mut Ctx<'_>, heard: &mut SmallVec<[ChatIn; 2]>) {
    if ctx.state.inbox.is_empty() {
        return;
    }
    let lines = std::mem::take(&mut ctx.state.inbox);
    // (the market features hear the shout, trade and group-finding channels and the whispers
    // besides us; what they do with them is no business of the routing below)
    let market = &mut ctx.state.market;
    crate::bot::market::forward_chat(&lines, |kind, sender, text, at_ms| {
        market.on_chat(kind, sender, text, at_ms)
    });
    // (the guild feature hears the guild chat and the whispers that ask for an invitation)
    let claimed = crate::bot::guild::forward_chat(ctx, &lines);
    let t = unix_ms();
    ctx.log
        .with_slot(|g| route_lines(lines, &mut g.social, t, heard));
    // a whisper the guild feature answers is not answered a second time
    if !claimed.is_empty() {
        heard.retain(|l| {
            !(l.kind == cs::chat_type::WHISPER
                && claimed
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(strip_gm(&l.name))))
        });
    }
    // a whisper that belongs to a negotiation of the market is the market's to answer
    if ctx.cfg.market.enabled && ctx.cfg.market.chat_ads {
        let market = &ctx.state.market;
        heard.retain(|l| !(l.kind == -3 && market.claims_whisper(&l.name)));
    }
}

/// The routing itself: chat lines to `heard` and the panel, system lines to the ring, the rest
/// dropped. `t` is the unix time in milliseconds for the panel.
fn route_lines(
    lines: SmallVec<[ChatIn; 2]>,
    social: &mut SlotSocial,
    t: u64,
    heard: &mut SmallVec<[ChatIn; 2]>,
) {
    for l in lines {
        match l.kind {
            -2 | -1 => social.push_system(SysLine {
                at_ms: l.at_ms,
                kind: l.kind,
                text: l.text,
            }),
            0 => {
                social.push_chat(t, &format!("{}: {}", l.name, l.text));
                heard.push(l);
            }
            -3 => {
                social.push_chat(t, &format!("{} (whisper): {}", l.name, l.text));
                heard.push(l);
            }
            _ => {}
        }
    }
}

/// Puts an action on the wire and into the log and the detail panel. The packet builders refuse
/// anything that is not a safe chat line.
fn send(ctx: &mut Ctx<'_>, action: Action) {
    let (packet, note) = match &action {
        Action::Say(text) => (say_packet(text), format!("(me) {text}")),
        Action::Whisper { to, text } => (
            whisper_packet(to, text),
            format!("(me to {}) {text}", strip_gm(to)),
        ),
    };
    let Some(packet) = packet else {
        ctx.log.debug("chat: refused to send an unsafe line");
        return;
    };
    ctx.out.push(packet);
    ctx.log.say(format!("chat: {note}"));
    let t = unix_ms();
    ctx.log.with_slot(|g| g.social.push_chat(t, &note));
}
