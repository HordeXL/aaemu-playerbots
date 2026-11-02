//! The coordinator: pairs bots that need a partner (duels, player trade).
//!
//! A goal that needs a partner calls [`CoordHandle::request_partner`]; the coordinator task
//! (one for the whole process) keeps the waiting bots and, when two bots want the same thing,
//! sends both a [`BotCmd::Pair`] with their roles. This is the **interface** the duel and trade
//! workers build on: the pairing here is deliberately minimal (first come, first paired, no
//! distance or level matching) and is the place to make it smarter.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::cmd::{BotCmd, PairKind, PairOrder, PairRole};
use crate::hub::Hub;

/// Where the coordinator's orders go: the hub in production, a recorder in tests.
pub trait Dispatch: Send + Sync {
    /// Queues `cmd` for bot `bot`.
    fn send(&self, bot: u32, cmd: BotCmd) -> Result<(), String>;
    /// Character id and name of a bot (0 and empty when unknown).
    fn who(&self, bot: u32) -> (u32, String);
}

impl Dispatch for Hub {
    fn send(&self, bot: u32, cmd: BotCmd) -> Result<(), String> {
        self.slot(bot)
            .ok_or_else(|| format!("no bot {bot}"))?
            .send(cmd)
    }

    fn who(&self, bot: u32) -> (u32, String) {
        match self.slot(bot) {
            Some(s) => (
                s.char_id.load(std::sync::atomic::Ordering::Relaxed),
                s.with(|g| g.snap.name.clone()),
            ),
            None => (0, String::new()),
        }
    }
}

/// How long a request waits for a partner before it is dropped.
pub const REQUEST_TTL: Duration = Duration::from_secs(120);
/// Capacity of the coordinator's inbox.
const INBOX: usize = 256;

/// A message to the coordinator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CoordMsg {
    /// Bot `bot` at `at` wants a partner for `kind`.
    Request {
        /// What for.
        kind: PairKind,
        /// The asking bot.
        bot: u32,
        /// Where it stands (x, y).
        at: (f32, f32),
    },
    /// The bot no longer wants a partner (it gave up or was paired by hand).
    Cancel {
        /// The bot.
        bot: u32,
    },
}

/// The cheap, cloneable end bots talk to the coordinator with.
#[derive(Debug, Clone)]
pub struct CoordHandle {
    tx: mpsc::Sender<CoordMsg>,
}

impl CoordHandle {
    /// Asks for a partner. False when the coordinator's inbox is full (ask again later).
    pub fn request_partner(&self, kind: PairKind, bot: u32, at: (f32, f32)) -> bool {
        self.tx
            .try_send(CoordMsg::Request { kind, bot, at })
            .is_ok()
    }

    /// Withdraws a request.
    pub fn cancel(&self, bot: u32) {
        let _ = self.tx.try_send(CoordMsg::Cancel { bot });
    }

    /// A handle nobody listens to, for tests and for bots running without a coordinator.
    pub fn detached() -> CoordHandle {
        let (tx, _rx) = mpsc::channel(1);
        CoordHandle { tx }
    }
}

struct Waiting {
    kind: PairKind,
    bot: u32,
    at: (f32, f32),
    since: Instant,
}

/// The coordinator task state.
pub struct Coordinator {
    rx: mpsc::Receiver<CoordMsg>,
    sink: Arc<dyn Dispatch>,
    waiting: Vec<Waiting>,
}

/// Creates the coordinator and the handle bots use.
pub fn channel(sink: Arc<dyn Dispatch>) -> (CoordHandle, Coordinator) {
    let (tx, rx) = mpsc::channel(INBOX);
    (
        CoordHandle { tx },
        Coordinator {
            rx,
            sink,
            waiting: Vec::new(),
        },
    )
}

impl Coordinator {
    /// Runs until `cancel` fires.
    pub async fn run(mut self, cancel: CancellationToken) {
        let mut sweep = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                msg = self.rx.recv() => match msg {
                    Some(m) => self.handle(m, Instant::now()),
                    None => return,
                },
                _ = sweep.tick() => self.expire(Instant::now()),
            }
        }
    }

    /// Handles one message; pairs bots when two want the same thing.
    pub fn handle(&mut self, msg: CoordMsg, now: Instant) {
        match msg {
            CoordMsg::Cancel { bot } => self.waiting.retain(|w| w.bot != bot),
            CoordMsg::Request { kind, bot, at } => {
                self.waiting.retain(|w| w.bot != bot);
                let partner = self
                    .waiting
                    .iter()
                    .position(|w| w.kind == kind && w.bot != bot);
                match partner {
                    Some(i) => {
                        let other = self.waiting.remove(i);
                        let meet = ((other.at.0 + at.0) / 2.0, (other.at.1 + at.1) / 2.0);
                        self.pair(kind, other.bot, bot, meet);
                    }
                    None => self.waiting.push(Waiting {
                        kind,
                        bot,
                        at,
                        since: now,
                    }),
                }
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        self.waiting
            .retain(|w| now.saturating_duration_since(w.since) < REQUEST_TTL);
    }

    /// Number of bots waiting for a partner.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    fn pair(&self, kind: PairKind, first: u32, second: u32, meet_at: (f32, f32)) {
        for (bot, role, partner) in [
            (first, PairRole::Initiator, second),
            (second, PairRole::Responder, first),
        ] {
            let (partner_char, partner_name) = self.sink.who(partner);
            let order = PairOrder {
                kind,
                role,
                partner_bot: partner,
                partner_char,
                partner_name: partner_name.into_boxed_str(),
                meet_at,
            };
            if let Err(e) = self.sink.send(bot, BotCmd::Pair(order)) {
                tracing::debug!("pairing {bot}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<(u32, BotCmd)>>);

    impl Dispatch for Recorder {
        fn send(&self, bot: u32, cmd: BotCmd) -> Result<(), String> {
            self.0.lock().push((bot, cmd));
            Ok(())
        }
        fn who(&self, bot: u32) -> (u32, String) {
            (bot * 100, format!("Bot{bot}"))
        }
    }

    fn coordinator() -> (Arc<Recorder>, CoordHandle, Coordinator) {
        let rec = Arc::new(Recorder::default());
        let (h, c) = channel(rec.clone());
        (rec, h, c)
    }

    fn req(kind: PairKind, bot: u32, at: (f32, f32)) -> CoordMsg {
        CoordMsg::Request { kind, bot, at }
    }

    #[test]
    fn two_requests_of_a_kind_make_a_pair_with_opposite_roles() {
        let (rec, _h, mut c) = coordinator();
        let now = Instant::now();
        c.handle(req(PairKind::Duel, 1, (0.0, 0.0)), now);
        assert_eq!(c.waiting(), 1);
        assert!(rec.0.lock().is_empty());
        c.handle(req(PairKind::Duel, 2, (10.0, 20.0)), now);
        assert_eq!(c.waiting(), 0);
        let sent = rec.0.lock();
        assert_eq!(sent.len(), 2);
        let (bot, BotCmd::Pair(first)) = &sent[0] else {
            panic!()
        };
        assert_eq!(
            (*bot, first.role, first.partner_bot),
            (1, PairRole::Initiator, 2)
        );
        assert_eq!((first.partner_char, &*first.partner_name), (200, "Bot2"));
        assert_eq!(first.meet_at, (5.0, 10.0));
        let (bot, BotCmd::Pair(second)) = &sent[1] else {
            panic!()
        };
        assert_eq!(
            (*bot, second.role, second.partner_bot),
            (2, PairRole::Responder, 1)
        );
    }

    #[test]
    fn different_kinds_do_not_pair_and_cancel_withdraws() {
        let (rec, _h, mut c) = coordinator();
        let now = Instant::now();
        c.handle(req(PairKind::Duel, 1, (0.0, 0.0)), now);
        c.handle(req(PairKind::Trade, 2, (0.0, 0.0)), now);
        assert_eq!(c.waiting(), 2);
        c.handle(CoordMsg::Cancel { bot: 1 }, now);
        assert_eq!(c.waiting(), 1);
        // a bot asking twice waits once and is never paired with itself
        c.handle(req(PairKind::Trade, 2, (1.0, 1.0)), now);
        assert_eq!(c.waiting(), 1);
        assert!(rec.0.lock().is_empty());
    }

    #[test]
    fn old_requests_expire() {
        let (_rec, _h, mut c) = coordinator();
        let t0 = Instant::now();
        c.handle(req(PairKind::Duel, 1, (0.0, 0.0)), t0);
        c.expire(t0 + Duration::from_secs(60));
        assert_eq!(c.waiting(), 1);
        c.expire(t0 + REQUEST_TTL + Duration::from_secs(1));
        assert_eq!(c.waiting(), 0);
    }

    #[test]
    fn handles_report_a_full_inbox() {
        let (_rec, h, _c) = coordinator();
        let mut accepted = 0;
        for i in 0..1000 {
            if h.request_partner(PairKind::Duel, i, (0.0, 0.0)) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, INBOX);
        // a detached handle has nobody listening: every request is refused
        let d = CoordHandle::detached();
        assert!(!d.request_partner(PairKind::Duel, 1, (0.0, 0.0)));
    }
}
