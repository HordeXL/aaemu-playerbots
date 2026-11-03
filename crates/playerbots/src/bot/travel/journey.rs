//! A far trip by portal: the state machine any goal (or the party code) can drive.
//!
//! A [`Journey`] either takes the bot to a point far away ([`Journey::new`]): it opens a gate from
//! the Teleport Book to an entry near the destination, steps through and is done (or recalls to the
//! bound point), or follows somebody else's gate ([`Journey::follow_gate`]: the opener's gate NPC has
//! the opener's character id as its owner). Every tick the owner calls [`Journey::tick`]
//! (`bot/travel/drive.rs`), which looks at the bot, runs [`Journey::step`] and does what it asks.
//!
//! [`Journey::step`] is pure: it reads a [`Snap`] (a plain description of the bot and what the
//! server did) and returns the [`Act`]s to perform and a [`JourneyStatus`], so the typed refusals
//! and timeouts are tested without a server. Nothing loops: every wait has a deadline and a typed
//! [`JourneyFail`], and a failed journey leaves walking to the caller.
//!
//! The server side (read from `systems/npcs/portal/` of AAEmu-1.2-Rust):
//!
//! * the cast of the Teleport Book takes 10 s, its cooldown 10 s; the entrance gate NPC 3891
//!   appears where the cast said (within about 5 m of the caster, in 3D), owned by the caster; both
//!   gates vanish after 30 s;
//! * `CSUsePortal` is refused with `SCErrorMsg` 154 beyond the use range, 287 for a closed zone,
//!   610 while the bot carries a trade pack and 611 on trial; success is `SCUnitPortalUsed` and
//!   the teleport (`SCTeleportUnit`, or `SCLoadInstance` into another world).

use smallvec::SmallVec;

use super::plan::Route;

/// The Teleport Book's open skill.
pub const OPEN_SKILL: u32 = aa_world::travel::portal::OPEN_PORTAL_SKILL;
/// The recall skill.
pub const RECALL_SKILL: u32 = aa_world::travel::portal::RECALL_SKILL;

/// How long after the open cast was sent the gate may take (the 10 s cast plus slack).
pub const CAST_WAIT_MS: u64 = 17_000;
/// How long a recall may take (the 10 s cast plus the landing).
pub const RECALL_WAIT_MS: u64 = 18_000;
/// How long the answer to `CSUsePortal` may take.
pub const USE_WAIT_MS: u64 = 5_000;
/// The longest a whole journey may take.
pub const TOTAL_MS: u64 = 90_000;
/// The longest wait for the open skill's cooldown.
pub const COOLDOWN_WAIT_MS: u64 = 15_000;
/// The longest wait for a GM top-up to reach the bag.
pub const SUPPLIES_WAIT_MS: u64 = 2_000;
/// The longest walk to a gate.
pub const WALK_WAIT_MS: u64 = 12_000;
/// How long a follower waits for the opener's gate to show up.
pub const SEEK_WAIT_MS: u64 = 25_000;
/// The bot uses a gate when it is this close (the server allows a little more).
pub const USE_RANGE_M: f32 = 3.5;
/// A move of this many metres means the bot went through.
pub const JUMP_M: f32 = 25.0;
/// `SCErrorMsg` ids the portal code answers with (the ones a journey looks at).
pub const REFUSALS: [i16; 5] = [154, 287, 609, 610, 611];

/// Where a journey goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dest {
    /// A point in the main world.
    Point {
        /// World X.
        x: f32,
        /// World Y.
        y: f32,
    },
    /// Through the gate that character `owner` opens.
    Gate {
        /// Character id of the opener.
        owner: u32,
    },
}

/// Why a journey did not get there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JourneyFail {
    /// No portal route beats walking (nothing near the destination, the trip is short, or the bot
    /// is in an instance).
    NoRoute,
    /// The bot carries a trade pack: the server refuses every gate then (`SCErrorMsg` 610).
    Laden,
    /// The bot has no Teleport Book and may not mint one.
    NoBook,
    /// The bot has no stones for the gate and may not mint them.
    NoReagents,
    /// There is no free spot next to the bot to open the gate on.
    NoSpot,
    /// A fight or death interrupted the journey.
    Interrupted,
    /// The server refused to start the open skill (its `SkillResult`).
    OpenRefused(u8),
    /// An `SCErrorMsg` answered the open cast (closed zone, ...).
    OpenError(i16),
    /// The cast was accepted but no gate of the bot's appeared.
    NoGate,
    /// The gate vanished before the bot reached it (or was never there for a follower).
    GateGone,
    /// `CSUsePortal` was refused with this `SCErrorMsg`.
    UseRefused(i16),
    /// The server accepted `CSUsePortal` but the bot was not moved.
    NoTeleport,
    /// The server said nothing at all to `CSUsePortal`.
    NoReply,
    /// The server refused to start the recall (its `SkillResult`).
    RecallRefused(u8),
    /// A wait ran out.
    Timeout,
}

impl JourneyFail {
    /// A short word for logs and keys.
    pub fn word(self) -> &'static str {
        match self {
            JourneyFail::NoRoute => "no route",
            JourneyFail::Laden => "carries a trade pack",
            JourneyFail::NoBook => "no teleport book",
            JourneyFail::NoReagents => "no stones",
            JourneyFail::NoSpot => "no free spot",
            JourneyFail::Interrupted => "interrupted",
            JourneyFail::OpenRefused(_) => "open refused",
            JourneyFail::OpenError(_) => "open error",
            JourneyFail::NoGate => "no gate appeared",
            JourneyFail::GateGone => "gate gone",
            JourneyFail::UseRefused(_) => "use refused",
            JourneyFail::NoTeleport => "not teleported",
            JourneyFail::NoReply => "no reply",
            JourneyFail::RecallRefused(_) => "recall refused",
            JourneyFail::Timeout => "timeout",
        }
    }
}

/// How a journey stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JourneyStatus {
    /// Still on the way.
    Running,
    /// Through the gate (or recalled): the bot is near the destination; the caller walks the rest.
    Arrived,
    /// It did not work; the caller walks.
    Failed(JourneyFail),
}

/// A symptom the server may be to blame for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The open skill was accepted but no gate appeared. `consumed`: the stones were taken.
    OpenNoGate {
        /// The skill start was confirmed.
        accepted: bool,
        /// The stones left the bag.
        consumed: bool,
    },
    /// The open skill was refused with this `SkillResult`.
    OpenRefused(u8),
    /// `CSUsePortal` was refused with this `SCErrorMsg`.
    UseRefused(i16),
    /// `SCUnitPortalUsed` came but the bot was not moved.
    UseNoTeleport,
    /// `CSUsePortal` got no answer.
    UseNoReply,
    /// The recall was refused with this `SkillResult`.
    RecallRefused(u8),
    /// The recall cast ended without a teleport.
    RecallNoTeleport,
}

/// The server's last answer to one of the bot's portal skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillNote {
    /// Skill id.
    pub skill: u32,
    /// `SkillStarted.result` (0 = started).
    pub result: u8,
    /// Bot clock of the answer.
    pub at_ms: u64,
}

/// An entrance gate (NPC 3891) in view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateView {
    /// Object id.
    pub obj: u32,
    /// Character id of the opener.
    pub owner: u32,
    /// Position.
    pub pos: [f32; 3],
    /// Bot clock at which the bot first saw it.
    pub seen_ms: u64,
}

/// What a journey sees of the bot and the server on one tick.
#[derive(Debug, Clone, Copy)]
pub struct Snap<'a> {
    /// Bot clock.
    pub now_ms: u64,
    /// The bot's position.
    pub pos: [f32; 3],
    /// Instance id of the world the bot is in (0 = the main world).
    pub inst: u32,
    /// The bot is inside an instance dungeon.
    pub in_instance: bool,
    /// The bot's character id.
    pub me_char: u32,
    /// The bot is dead.
    pub dead: bool,
    /// A fight or a threat: casts break.
    pub hostile: bool,
    /// The bot carries a trade pack: the server refuses `CSUsePortal` (610), so no gate is opened
    /// or entered.
    pub laden: bool,
    /// GM commands may be used to top up the book, stones and ink.
    pub gm: bool,
    /// The Teleport Book's item id in the bag.
    pub book: Option<u64>,
    /// How many gate stones the bag holds (all kinds the open skill takes).
    pub stones: i64,
    /// The open skill is off cooldown.
    pub open_ready: bool,
    /// The last answer to one of the bot's portal skills.
    pub skill: Option<SkillNote>,
    /// Entrance gates in view.
    pub gates: &'a [GateView],
    /// The last `SCUnitPortalUsed`: `(gate, bot clock)`.
    pub used: Option<(u32, u64)>,
    /// The newest refusal `SCErrorMsg` of the portal code: `(code, bot clock)`.
    pub refusal: Option<(i16, u64)>,
    /// The route to take (only asked for while a journey plans, see [`Journey::needs_route`]).
    pub route: Option<Route>,
    /// A free spot 1.5 to 2.5 m from the bot to open the gate on.
    pub spot: Option<[f32; 3]>,
}

/// What a journey asks the driver to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Stop walking.
    Stop,
    /// Walk to a point, stopping `radius` metres short.
    Walk {
        /// World X.
        x: f32,
        /// World Y.
        y: f32,
        /// Stop radius.
        radius: f32,
    },
    /// Add `n` of `item` to the bag with a GM command.
    TopUp {
        /// Item template.
        item: u32,
        /// Amount.
        n: u32,
    },
    /// Cast the Teleport Book.
    Open {
        /// The book's item id.
        book: u64,
        /// Entry kind.
        kind: u8,
        /// Entry id.
        entry: u32,
        /// Where the entrance gate appears.
        at: [f32; 3],
    },
    /// Send `CSUsePortal` for this gate.
    Use {
        /// The gate's object id.
        gate: u32,
    },
    /// Cast the recall.
    Recall,
    /// A symptom to report.
    Fault(Fault),
}

/// The acts of one step (rarely more than two).
pub type Acts = SmallVec<[Act; 3]>;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Choosing the route.
    Plan,
    /// A GM top-up is on its way.
    Supplies { asked_ms: u64 },
    /// Waiting for the open skill's cooldown.
    Cooldown,
    /// The open cast was sent.
    Cast { sent_ms: u64, stones: i64 },
    /// A gate is in sight: get close and use it.
    Gate { obj: u32 },
    /// `CSUsePortal` was sent.
    Use {
        obj: u32,
        sent_ms: u64,
        from: [f32; 3],
        inst: u32,
    },
    /// The recall cast was sent.
    Recall {
        sent_ms: u64,
        from: [f32; 3],
        inst: u32,
    },
    /// Waiting for the opener's gate.
    Seek,
}

/// A far trip by portal. See the module docs. About 72 bytes: a goal whose own size is tight
/// keeps it in a `Box`.
#[derive(Debug, Clone, PartialEq)]
pub struct Journey {
    dest: Dest,
    phase: Phase,
    began_ms: u64,
    phase_ms: u64,
    kind: u8,
    entry: u32,
    tries: u8,
    recall: bool,
}

impl Journey {
    /// A journey to the world point `(x, y)`.
    pub fn new(dest: [f32; 2]) -> Journey {
        Journey::with(Dest::Point {
            x: dest[0],
            y: dest[1],
        })
    }

    /// A journey through the gate that character `owner` opened (or opens in the next seconds).
    pub fn follow_gate(owner: u32) -> Journey {
        let mut j = Journey::with(Dest::Gate { owner });
        j.phase = Phase::Seek;
        j
    }

    fn with(dest: Dest) -> Journey {
        Journey {
            dest,
            phase: Phase::Plan,
            began_ms: 0,
            phase_ms: 0,
            kind: 0,
            entry: 0,
            tries: 0,
            recall: false,
        }
    }

    /// Whether the journey chose to recall instead of opening a gate.
    pub fn is_recall(&self) -> bool {
        self.recall
    }

    /// Where the journey goes.
    pub fn dest(&self) -> Dest {
        self.dest
    }

    /// Whether this is a journey to a point within `tol` metres of `(x, y)`.
    pub fn goes_to(&self, x: f32, y: f32, tol: f32) -> bool {
        match self.dest {
            Dest::Point { x: dx, y: dy } => ((dx - x).powi(2) + (dy - y).powi(2)).sqrt() <= tol,
            Dest::Gate { .. } => false,
        }
    }

    /// Whether the next [`Journey::step`] wants a [`Snap::route`].
    pub fn needs_route(&self) -> bool {
        matches!(self.dest, Dest::Point { .. }) && self.phase == Phase::Plan
    }

    /// Whether the next step may open a gate (so the driver finds a free [`Snap::spot`]).
    pub fn needs_spot(&self) -> bool {
        matches!(
            self.phase,
            Phase::Plan | Phase::Supplies { .. } | Phase::Cooldown
        ) && matches!(self.dest, Dest::Point { .. })
    }

    /// Whether the journey has begun to act and should keep the bot's goals waiting (it is past
    /// planning: casting, waiting at a gate, stepping through).
    pub fn holds(&self) -> bool {
        !matches!(self.phase, Phase::Plan)
    }

    /// Whether a cast of the journey may still be under way (the bot must not be walked away).
    pub fn casting(&self) -> bool {
        matches!(self.phase, Phase::Cast { .. } | Phase::Recall { .. })
    }

    /// One step. `s` describes the bot now; the acts to perform are pushed to `acts`. Call it
    /// until it returns something other than [`JourneyStatus::Running`].
    pub fn step(&mut self, s: &Snap<'_>, acts: &mut Acts) -> JourneyStatus {
        if self.began_ms == 0 {
            self.began_ms = s.now_ms;
            self.phase_ms = s.now_ms;
        }
        if s.dead {
            return JourneyStatus::Failed(JourneyFail::Interrupted);
        }
        if s.now_ms.saturating_sub(self.began_ms) > TOTAL_MS {
            return JourneyStatus::Failed(JourneyFail::Timeout);
        }
        let stepping_through = matches!(self.phase, Phase::Use { .. });
        if s.hostile && !stepping_through {
            return JourneyStatus::Failed(JourneyFail::Interrupted);
        }
        // a pack in the back slot makes every gate useless: do not start one, do not wait at one
        let before_the_gate = matches!(
            self.phase,
            Phase::Plan
                | Phase::Supplies { .. }
                | Phase::Cooldown
                | Phase::Gate { .. }
                | Phase::Seek
        );
        if s.laden && before_the_gate {
            return JourneyStatus::Failed(JourneyFail::Laden);
        }
        match self.phase {
            Phase::Plan => self.plan(s, acts),
            Phase::Supplies { asked_ms } => {
                if s.now_ms.saturating_sub(asked_ms) >= SUPPLIES_WAIT_MS {
                    self.go(Phase::Plan, s.now_ms);
                    return self.plan(s, acts);
                }
                JourneyStatus::Running
            }
            Phase::Cooldown => self.cooldown(s, acts),
            Phase::Cast { sent_ms, stones } => self.cast(s, acts, sent_ms, stones),
            Phase::Gate { obj } => self.at_gate(s, acts, obj),
            Phase::Use {
                obj,
                sent_ms,
                from,
                inst,
            } => self.using(s, acts, obj, sent_ms, from, inst),
            Phase::Recall {
                sent_ms,
                from,
                inst,
            } => self.recalling(s, acts, sent_ms, from, inst),
            Phase::Seek => self.seek(s),
        }
    }

    fn go(&mut self, phase: Phase, now_ms: u64) {
        self.phase = phase;
        self.phase_ms = now_ms;
    }

    fn plan(&mut self, s: &Snap<'_>, acts: &mut Acts) -> JourneyStatus {
        if s.in_instance {
            return JourneyStatus::Failed(JourneyFail::NoRoute);
        }
        match s.route {
            Some(Route::Gate(c)) => {
                self.kind = c.target.kind;
                self.entry = c.target.id;
                if s.book.is_none() {
                    if s.gm && self.tries < 2 {
                        self.tries += 1;
                        acts.push(Act::TopUp {
                            item: aa_world::travel::portal::TELEPORT_BOOK_ITEM,
                            n: 1,
                        });
                        self.go(Phase::Supplies { asked_ms: s.now_ms }, s.now_ms);
                        return JourneyStatus::Running;
                    }
                    return JourneyStatus::Failed(JourneyFail::NoBook);
                }
                if !c.afford {
                    if s.gm && self.tries < 2 {
                        self.tries += 1;
                        acts.push(Act::TopUp {
                            item: c.topup.0,
                            n: c.topup.1,
                        });
                        self.go(Phase::Supplies { asked_ms: s.now_ms }, s.now_ms);
                        return JourneyStatus::Running;
                    }
                    return JourneyStatus::Failed(JourneyFail::NoReagents);
                }
                self.go(Phase::Cooldown, s.now_ms);
                self.cooldown(s, acts)
            }
            Some(Route::Recall) => {
                self.recall = true;
                acts.push(Act::Stop);
                acts.push(Act::Recall);
                self.go(
                    Phase::Recall {
                        sent_ms: s.now_ms,
                        from: s.pos,
                        inst: s.inst,
                    },
                    s.now_ms,
                );
                JourneyStatus::Running
            }
            Some(Route::Walk) | None => JourneyStatus::Failed(JourneyFail::NoRoute),
        }
    }

    fn cooldown(&mut self, s: &Snap<'_>, acts: &mut Acts) -> JourneyStatus {
        if !s.open_ready {
            if s.now_ms.saturating_sub(self.phase_ms) > COOLDOWN_WAIT_MS {
                return JourneyStatus::Failed(JourneyFail::Timeout);
            }
            return JourneyStatus::Running;
        }
        let Some(book) = s.book else {
            return JourneyStatus::Failed(JourneyFail::NoBook);
        };
        let Some(at) = s.spot else {
            return JourneyStatus::Failed(JourneyFail::NoSpot);
        };
        acts.push(Act::Stop);
        acts.push(Act::Open {
            book,
            kind: self.kind,
            entry: self.entry,
            at,
        });
        self.go(
            Phase::Cast {
                sent_ms: s.now_ms,
                stones: s.stones,
            },
            s.now_ms,
        );
        JourneyStatus::Running
    }

    fn cast(&mut self, s: &Snap<'_>, acts: &mut Acts, sent_ms: u64, stones: i64) -> JourneyStatus {
        let note = s
            .skill
            .filter(|n| n.skill == OPEN_SKILL && n.at_ms >= sent_ms);
        if let Some(n) = note.filter(|n| n.result != 0) {
            acts.push(Act::Fault(Fault::OpenRefused(n.result)));
            return JourneyStatus::Failed(JourneyFail::OpenRefused(n.result));
        }
        if let Some(g) = s.gates.iter().find(|g| {
            g.owner == s.me_char && g.seen_ms + 1_500 >= sent_ms && dist2d(s.pos, g.pos) <= 8.0
        }) {
            self.go(Phase::Gate { obj: g.obj }, s.now_ms);
            return self.at_gate(s, acts, g.obj);
        }
        if let Some((code, at)) = s.refusal {
            if at >= sent_ms {
                return JourneyStatus::Failed(JourneyFail::OpenError(code));
            }
        }
        if s.now_ms.saturating_sub(sent_ms) > CAST_WAIT_MS {
            acts.push(Act::Fault(Fault::OpenNoGate {
                accepted: note.is_some(),
                consumed: s.stones < stones,
            }));
            return JourneyStatus::Failed(JourneyFail::NoGate);
        }
        JourneyStatus::Running
    }

    fn at_gate(&mut self, s: &Snap<'_>, acts: &mut Acts, obj: u32) -> JourneyStatus {
        let Some(g) = s.gates.iter().find(|g| g.obj == obj) else {
            return JourneyStatus::Failed(JourneyFail::GateGone);
        };
        if s.now_ms.saturating_sub(self.phase_ms) > WALK_WAIT_MS {
            return JourneyStatus::Failed(JourneyFail::Timeout);
        }
        if dist2d(s.pos, g.pos) > USE_RANGE_M {
            acts.push(Act::Walk {
                x: g.pos[0],
                y: g.pos[1],
                radius: 1.8,
            });
            return JourneyStatus::Running;
        }
        acts.push(Act::Stop);
        acts.push(Act::Use { gate: obj });
        self.go(
            Phase::Use {
                obj,
                sent_ms: s.now_ms,
                from: s.pos,
                inst: s.inst,
            },
            s.now_ms,
        );
        JourneyStatus::Running
    }

    fn using(
        &mut self,
        s: &Snap<'_>,
        acts: &mut Acts,
        obj: u32,
        sent_ms: u64,
        from: [f32; 3],
        inst: u32,
    ) -> JourneyStatus {
        if moved(s, from, inst) {
            return JourneyStatus::Arrived;
        }
        if let Some((code, _)) = s.refusal.filter(|r| r.1 >= sent_ms) {
            // too far: the server's idea of the position lags; try once more, closer
            if code == 154 && self.tries < 3 {
                self.tries += 1;
                self.go(Phase::Gate { obj }, s.now_ms);
                return JourneyStatus::Running;
            }
            acts.push(Act::Fault(Fault::UseRefused(code)));
            return JourneyStatus::Failed(JourneyFail::UseRefused(code));
        }
        if s.now_ms.saturating_sub(sent_ms) > USE_WAIT_MS {
            let used = s.used.is_some_and(|(g, at)| g == obj && at >= sent_ms);
            return if used {
                acts.push(Act::Fault(Fault::UseNoTeleport));
                JourneyStatus::Failed(JourneyFail::NoTeleport)
            } else {
                acts.push(Act::Fault(Fault::UseNoReply));
                JourneyStatus::Failed(JourneyFail::NoReply)
            };
        }
        JourneyStatus::Running
    }

    fn recalling(
        &mut self,
        s: &Snap<'_>,
        acts: &mut Acts,
        sent_ms: u64,
        from: [f32; 3],
        inst: u32,
    ) -> JourneyStatus {
        if moved(s, from, inst) {
            return JourneyStatus::Arrived;
        }
        if let Some(n) = s
            .skill
            .filter(|n| n.skill == RECALL_SKILL && n.at_ms >= sent_ms && n.result != 0)
        {
            acts.push(Act::Fault(Fault::RecallRefused(n.result)));
            return JourneyStatus::Failed(JourneyFail::RecallRefused(n.result));
        }
        if s.now_ms.saturating_sub(sent_ms) > RECALL_WAIT_MS {
            acts.push(Act::Fault(Fault::RecallNoTeleport));
            return JourneyStatus::Failed(JourneyFail::NoTeleport);
        }
        JourneyStatus::Running
    }

    fn seek(&mut self, s: &Snap<'_>) -> JourneyStatus {
        let Dest::Gate { owner } = self.dest else {
            return JourneyStatus::Failed(JourneyFail::NoRoute);
        };
        if let Some(g) = s.gates.iter().find(|g| g.owner == owner) {
            self.go(Phase::Gate { obj: g.obj }, s.now_ms);
            return JourneyStatus::Running;
        }
        if s.now_ms.saturating_sub(self.phase_ms) > SEEK_WAIT_MS {
            return JourneyStatus::Failed(JourneyFail::GateGone);
        }
        JourneyStatus::Running
    }
}

fn moved(s: &Snap<'_>, from: [f32; 3], inst: u32) -> bool {
    s.inst != inst || dist3d(s.pos, from) > JUMP_M
}

fn dist2d(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn dist3d(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::travel::book::Target;
    use crate::bot::travel::plan::Cand;

    const ME: u32 = 500;
    const OTHER: u32 = 321;
    const GATE_OBJ: u32 = 0x9000;

    fn target() -> Target {
        Target {
            kind: 2,
            id: 4100,
            zone: 179,
            at: [3000.0, 0.0],
        }
    }

    fn gate_route(afford: bool) -> Route {
        Route::Gate(Cand {
            target: target(),
            afford,
            topup: (4052, 3),
        })
    }

    /// A snapshot of a bot with a book, stones and a free spot at the origin.
    fn snap<'a>(now_ms: u64, gates: &'a [GateView]) -> Snap<'a> {
        Snap {
            now_ms,
            pos: [0.0, 0.0, 100.0],
            inst: 0,
            in_instance: false,
            me_char: ME,
            dead: false,
            hostile: false,
            laden: false,
            gm: false,
            book: Some(77),
            stones: 3,
            open_ready: true,
            skill: None,
            gates,
            used: None,
            refusal: None,
            route: None,
            spot: Some([1.8, 0.0, 100.0]),
        }
    }

    fn gate(owner: u32, seen_ms: u64) -> GateView {
        GateView {
            obj: GATE_OBJ,
            owner,
            pos: [1.8, 0.0, 100.0],
            seen_ms,
        }
    }

    fn step(j: &mut Journey, s: &Snap<'_>) -> (JourneyStatus, Acts) {
        let mut acts = Acts::new();
        let st = j.step(s, &mut acts);
        (st, acts)
    }

    /// Plans the gate route and sends the open cast; returns the journey at its cast.
    fn cast_sent() -> Journey {
        let mut j = Journey::new([3000.0, 0.0]);
        assert!(j.needs_route() && j.needs_spot() && !j.holds());
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(
            acts.as_slice(),
            [
                Act::Stop,
                Act::Open {
                    book: 77,
                    kind: 2,
                    entry: 4100,
                    at: [1.8, 0.0, 100.0]
                }
            ]
        );
        assert!(j.holds() && j.casting() && !j.needs_route());
        j
    }

    #[test]
    fn the_gate_route_opens_walks_up_uses_and_arrives() {
        let mut j = cast_sent();
        // the cast runs; the open skill was accepted
        let mut s = snap(3_000, &[]);
        s.skill = Some(SkillNote {
            skill: OPEN_SKILL,
            result: 0,
            at_ms: 1_100,
        });
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Running);
        // a gate of somebody else is not ours; ours appears 11 s later, 1.8 m away
        let other = [gate(999, 11_000)];
        let mut s = snap(11_500, &other);
        s.skill = Some(SkillNote {
            skill: OPEN_SKILL,
            result: 0,
            at_ms: 1_100,
        });
        assert_eq!(step(&mut j, &s), (JourneyStatus::Running, Acts::new()));
        let mine = [gate(ME, 11_400)];
        let s = snap(11_600, &mine);
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(acts.as_slice(), [Act::Stop, Act::Use { gate: GATE_OBJ }]);
        // accepted: PortalUsed, then the bot is 3 km away
        let mut s = snap(11_900, &mine);
        s.used = Some((GATE_OBJ, 11_700));
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Running);
        s.pos = [2990.0, 5.0, 100.0];
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Arrived);
    }

    #[test]
    fn a_gate_farther_than_the_use_range_is_walked_to() {
        let mut j = cast_sent();
        let far = [GateView {
            obj: GATE_OBJ,
            owner: ME,
            pos: [5.0, 0.0, 100.0],
            seen_ms: 11_000,
        }];
        let s = snap(11_100, &far);
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(
            acts.as_slice(),
            [Act::Walk {
                x: 5.0,
                y: 0.0,
                radius: 1.8
            }]
        );
        let mut near = snap(12_000, &far);
        near.pos = [3.0, 0.0, 100.0];
        let (_, acts) = step(&mut j, &near);
        assert_eq!(acts.as_slice(), [Act::Stop, Act::Use { gate: GATE_OBJ }]);
    }

    #[test]
    fn a_refused_open_cast_fails_typed_and_reports() {
        let mut j = cast_sent();
        let mut s = snap(1_400, &[]);
        s.skill = Some(SkillNote {
            skill: OPEN_SKILL,
            result: 0x0F,
            at_ms: 1_300,
        });
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::OpenRefused(0x0F)));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::OpenRefused(0x0F))]);
        // an answer older than the cast is not about this cast
        let mut j = cast_sent();
        let mut s = snap(1_400, &[]);
        s.skill = Some(SkillNote {
            skill: OPEN_SKILL,
            result: 0x0F,
            at_ms: 200,
        });
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Running);
        // a closed zone arrives as an error message
        let mut s = snap(2_000, &[]);
        s.refusal = Some((287, 1_900));
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::OpenError(287))
        );
    }

    #[test]
    fn no_gate_after_the_cast_reports_what_the_bag_says() {
        // accepted and the stones are gone: the server took the reagent and made no gate
        let mut j = cast_sent();
        let mut s = snap(1_000 + CAST_WAIT_MS + 1, &[]);
        s.stones = 2;
        s.skill = Some(SkillNote {
            skill: OPEN_SKILL,
            result: 0,
            at_ms: 1_100,
        });
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::NoGate));
        assert_eq!(
            acts.as_slice(),
            [Act::Fault(Fault::OpenNoGate {
                accepted: true,
                consumed: true
            })]
        );
        // nothing consumed, nothing confirmed
        let mut j = cast_sent();
        let s = snap(1_000 + CAST_WAIT_MS + 1, &[]);
        let (_, acts) = step(&mut j, &s);
        assert_eq!(
            acts.as_slice(),
            [Act::Fault(Fault::OpenNoGate {
                accepted: false,
                consumed: false
            })]
        );
    }

    #[test]
    fn a_laden_bot_neither_opens_nor_follows_a_gate() {
        // planning: the trade pack in the back slot ends the journey before anything is spent
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.laden = true;
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::Laden));
        assert!(acts.is_empty());
        // a follower waiting for a gate gives up the same way
        let mut j = Journey::follow_gate(OTHER);
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::Laden));
        assert!(acts.is_empty());
        // ... and so does one that walks to a gate that is already open
        let mut j = Journey::follow_gate(OTHER);
        let theirs = [gate(OTHER, 1_000)];
        assert_eq!(
            step(&mut j, &snap(1_000, &theirs)).0,
            JourneyStatus::Running
        );
        let mut s = snap(1_500, &theirs);
        s.laden = true;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::Laden)
        );
        // a bot that is not laden is not affected
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Running);
    }

    #[test]
    fn use_refusals_and_silence_are_typed() {
        let mine = [gate(ME, 11_400)];
        let used = |j: &mut Journey| {
            let s = snap(11_600, &mine);
            assert_eq!(step(j, &s).0, JourneyStatus::Running);
        };
        // a trade pack in the way
        let mut j = cast_sent();
        used(&mut j);
        let mut s = snap(11_800, &mine);
        s.refusal = Some((610, 11_700));
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::UseRefused(610)));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::UseRefused(610))]);
        // accepted (PortalUsed) but not moved
        let mut j = cast_sent();
        used(&mut j);
        let mut s = snap(11_600 + USE_WAIT_MS + 1, &mine);
        s.used = Some((GATE_OBJ, 11_700));
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::NoTeleport));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::UseNoTeleport)]);
        // not a word
        let mut j = cast_sent();
        used(&mut j);
        let s = snap(11_600 + USE_WAIT_MS + 1, &mine);
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::NoReply));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::UseNoReply)]);
        // too far (154) is tried again from closer, not reported, but not forever
        let mut j = cast_sent();
        used(&mut j);
        let mut s = snap(11_800, &mine);
        s.refusal = Some((154, 11_700));
        assert_eq!(step(&mut j, &s), (JourneyStatus::Running, Acts::new()));
        let mut last = JourneyStatus::Running;
        for round in 0..4 {
            let s = snap(12_000 + round * 100, &mine);
            step(&mut j, &s); // back at the gate: use again
            let mut s = snap(12_050 + round * 100, &mine);
            s.refusal = Some((154, 12_040 + round * 100));
            last = step(&mut j, &s).0;
            if last != JourneyStatus::Running {
                break;
            }
        }
        assert_eq!(last, JourneyStatus::Failed(JourneyFail::UseRefused(154)));
    }

    #[test]
    fn a_gate_that_vanishes_ends_the_journey() {
        let mut j = cast_sent();
        let far = [GateView {
            obj: GATE_OBJ,
            owner: ME,
            pos: [5.0, 0.0, 100.0],
            seen_ms: 11_000,
        }];
        step(&mut j, &snap(11_100, &far));
        assert_eq!(
            step(&mut j, &snap(14_000, &[])).0,
            JourneyStatus::Failed(JourneyFail::GateGone)
        );
        // walking to it for too long
        let mut j = cast_sent();
        step(&mut j, &snap(11_100, &far));
        assert_eq!(
            step(&mut j, &snap(11_100 + WALK_WAIT_MS + 1, &far)).0,
            JourneyStatus::Failed(JourneyFail::Timeout)
        );
    }

    #[test]
    fn a_fight_or_a_death_interrupts_everything_but_the_step_through() {
        let mut j = cast_sent();
        let mut s = snap(2_000, &[]);
        s.hostile = true;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::Interrupted)
        );
        let mut j = cast_sent();
        let mut s = snap(2_000, &[]);
        s.dead = true;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::Interrupted)
        );
        // a hit while stepping through does not undo the use that was sent
        let mut j = cast_sent();
        let mine = [gate(ME, 11_400)];
        step(&mut j, &snap(11_600, &mine));
        let mut s = snap(11_800, &mine);
        s.hostile = true;
        s.pos = [3000.0, 0.0, 100.0];
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Arrived);
    }

    #[test]
    fn planning_without_a_route_the_book_or_the_stones() {
        // nothing beats walking
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(Route::Walk);
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::NoRoute)
        );
        // inside an instance there is no portal route
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.in_instance = true;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::NoRoute)
        );
        // no book, no GM
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.book = None;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::NoBook)
        );
        // no stones, no GM
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(false));
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::NoReagents)
        );
        // no free spot
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.spot = None;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::NoSpot)
        );
    }

    #[test]
    fn gm_tops_up_the_stones_once_and_the_journey_plans_again() {
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.gm = true;
        s.route = Some(gate_route(false));
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(acts.as_slice(), [Act::TopUp { item: 4052, n: 3 }]);
        assert!(j.holds() && !j.needs_route());
        // too early to look again
        let mut s2 = snap(1_500, &[]);
        s2.gm = true;
        assert_eq!(step(&mut j, &s2), (JourneyStatus::Running, Acts::new()));
        // after the wait the journey plans afresh; the stones arrived
        let mut s3 = snap(1_000 + SUPPLIES_WAIT_MS, &[]);
        s3.gm = true;
        s3.route = Some(gate_route(true));
        let (_, acts) = step(&mut j, &s3);
        assert!(matches!(acts.last(), Some(Act::Open { .. })), "{acts:?}");
        // a book that does not arrive after two tries ends in NoBook
        let mut j = Journey::new([3000.0, 0.0]);
        let mut now = 1_000;
        let mut out = JourneyStatus::Running;
        for _ in 0..4 {
            let mut s = snap(now, &[]);
            s.gm = true;
            s.book = None;
            s.route = Some(gate_route(true));
            out = step(&mut j, &s).0;
            now += SUPPLIES_WAIT_MS;
            if out != JourneyStatus::Running {
                break;
            }
        }
        assert_eq!(out, JourneyStatus::Failed(JourneyFail::NoBook));
    }

    #[test]
    fn the_open_cooldown_is_waited_out_within_limits() {
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.open_ready = false;
        assert_eq!(step(&mut j, &s), (JourneyStatus::Running, Acts::new()));
        let mut s = snap(6_000, &[]);
        s.open_ready = false;
        assert_eq!(step(&mut j, &s), (JourneyStatus::Running, Acts::new()));
        let s = snap(8_000, &[]);
        let (_, acts) = step(&mut j, &s);
        assert!(matches!(acts.last(), Some(Act::Open { .. })));
        // never ready
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.open_ready = false;
        step(&mut j, &s);
        let mut s = snap(1_000 + COOLDOWN_WAIT_MS + 1, &[]);
        s.open_ready = false;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::Timeout)
        );
    }

    #[test]
    fn a_recall_waits_for_the_teleport_and_names_its_failures() {
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(Route::Recall);
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(acts.as_slice(), [Act::Stop, Act::Recall]);
        let mut s = snap(11_000, &[]);
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Running);
        s.pos = [3000.0, 100.0, 100.0];
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Arrived);
        // an instance change counts as moved, whatever the position
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(Route::Recall);
        step(&mut j, &s);
        let mut s = snap(9_000, &[]);
        s.inst = 4;
        assert_eq!(step(&mut j, &s).0, JourneyStatus::Arrived);
        // refused
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(Route::Recall);
        step(&mut j, &s);
        let mut s = snap(1_400, &[]);
        s.skill = Some(SkillNote {
            skill: RECALL_SKILL,
            result: 0x08,
            at_ms: 1_300,
        });
        let (st, acts) = step(&mut j, &s);
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::RecallRefused(0x08)));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::RecallRefused(0x08))]);
        // accepted and nothing happened
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(Route::Recall);
        step(&mut j, &s);
        let (st, acts) = step(&mut j, &snap(1_000 + RECALL_WAIT_MS + 1, &[]));
        assert_eq!(st, JourneyStatus::Failed(JourneyFail::NoTeleport));
        assert_eq!(acts.as_slice(), [Act::Fault(Fault::RecallNoTeleport)]);
    }

    #[test]
    fn following_a_party_members_gate() {
        let mut j = Journey::follow_gate(321);
        assert!(!j.needs_route() && !j.needs_spot() && j.holds());
        assert!(!j.goes_to(0.0, 0.0, 1e9));
        // nobody's gate yet
        let s = snap(1_000, &[]);
        assert_eq!(step(&mut j, &s), (JourneyStatus::Running, Acts::new()));
        // somebody else's gate is not the opener's
        let wrong = [gate(999, 2_000)];
        assert_eq!(
            step(&mut j, &snap(2_000, &wrong)),
            (JourneyStatus::Running, Acts::new())
        );
        // the opener's: walk or use
        let right = [gate(321, 3_000)];
        step(&mut j, &snap(3_000, &right));
        let (st, acts) = step(&mut j, &snap(3_250, &right));
        assert_eq!(st, JourneyStatus::Running);
        assert_eq!(acts.as_slice(), [Act::Stop, Act::Use { gate: GATE_OBJ }]);
        // never comes
        let mut j = Journey::follow_gate(321);
        step(&mut j, &snap(1_000, &[]));
        assert_eq!(
            step(&mut j, &snap(1_000 + SEEK_WAIT_MS + 1, &[])).0,
            JourneyStatus::Failed(JourneyFail::GateGone)
        );
    }

    #[test]
    fn nothing_lasts_longer_than_the_total_limit() {
        let mut j = Journey::new([3000.0, 0.0]);
        let mut s = snap(1_000, &[]);
        s.route = Some(gate_route(true));
        s.open_ready = false;
        step(&mut j, &s);
        let mut s = snap(1_000 + TOTAL_MS + 1, &[]);
        s.open_ready = false;
        assert_eq!(
            step(&mut j, &s).0,
            JourneyStatus::Failed(JourneyFail::Timeout)
        );
    }

    #[test]
    fn a_journey_is_small_enough_for_a_goal_to_own() {
        assert!(
            std::mem::size_of::<Journey>() <= 80,
            "{}",
            std::mem::size_of::<Journey>()
        );
        assert!(Journey::new([5.0, 6.0]).goes_to(6.0, 6.0, 2.0));
        assert!(!Journey::new([5.0, 6.0]).goes_to(60.0, 6.0, 2.0));
    }
}
