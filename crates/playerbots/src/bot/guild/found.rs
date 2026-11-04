//! Founding a guild: the state machine of a founding team.
//!
//! The coordinator orders five bots of one faction to found a guild together (a
//! [`PartyOrder`](crate::bot::party::PartyOrder) with the purpose `Guild`). The server needs a
//! team of five that is level 5 or more, unguilded and of one mother faction, and a founder who
//! pays 10000 copper, so the steps are:
//!
//! ```text
//! founder: wait for the others to start -> invite one member after the other into a team ->
//!          create the guild with a name -> dismiss the team                       -> done
//! member:  wait for the founder's invitation -> join the team -> wait for the guild
//!          (SCUnitExpeditionChanged) -> leave the team if the founder has not done so   -> done
//! ```
//!
//! [`Machine`] is pure: [`Machine::step`] reads a [`Snap`] (what the bot knows) and returns the
//! packets to send as [`Act`]s and a [`Status`]. Every wait has a deadline and a typed [`Fail`];
//! a failed founding never retries by itself, the bot rests for [`Fail::back_off_ms`] before it
//! wishes again. The goal (`goals::guild`) turns the acts into packets.
//!
//! Server behaviour the steps are built around (`systems/social/expedition.rs`,
//! `team.rs`): an invitation to a character that has one pending is refused with a
//! `SCRejectedTeam`; a pending invitation never expires, so the members answer at once and turn
//! down strays; `CSCreateExpedition` answers refusals with `SCErrorMsg` and success with
//! `SCFactionCreated` for the founder and `SCUnitExpeditionChanged` for every member.

use aa_proto::guild::{self, err};
use smallvec::SmallVec;

/// The founder waits this long for the members to start their goal before it invites (a bot far
/// from every player ticks every few seconds).
pub const START_GRACE_MS: u64 = 6_000;
/// An invitation is given up after this long (the team invitation of the server lasts 60 s).
pub const INVITE_WAIT_MS: u64 = 25_000;
/// A rejected invitation is repeated once after this long.
pub const REINVITE_MS: u64 = 4_000;
/// The server answers a creation within this long or it never will.
pub const CREATE_WAIT_MS: u64 = 10_000;
/// The founder waits this long for the team to go before it leaves the team itself.
pub const DISMISS_WAIT_MS: u64 = 8_000;
/// A member waits this long for the founder's invitation, and for the guild once in the team.
pub const MEMBER_WAIT_MS: u64 = 80_000;
/// A member that has its guild leaves the team itself after this long.
pub const MEMBER_LEAVE_MS: u64 = 12_000;
/// Names tried before the founding gives up.
pub const MAX_NAME_TRIES: u32 = 5;
/// A member answers the same invitation at most this often.
pub const ACCEPT_GAP_MS: u64 = 3_000;

/// Why a founding did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fail {
    /// The bot cannot sign a charter now (in a team already, or in a guild).
    NotReady,
    /// A member declined the invitation (or had another one pending).
    Declined,
    /// A member did not join in time.
    InviteTimeout,
    /// The founder cannot pay.
    NoMoney,
    /// The server refused the creation with this `ErrorMessageType`.
    Refused(i16),
    /// Every name was refused.
    NoName,
    /// The server answered the creation with nothing.
    NoAnswer,
    /// The team broke up.
    TeamLost,
    /// The founder never invited, or the guild never came.
    Timeout,
}

impl Fail {
    /// How long the bot leaves founding alone after this failure.
    pub fn back_off_ms(self) -> u64 {
        const MIN: u64 = 60_000;
        match self {
            Fail::NotReady => MIN,
            Fail::Timeout | Fail::Declined | Fail::InviteTimeout | Fail::TeamLost => 3 * MIN,
            Fail::NoMoney | Fail::NoName | Fail::NoAnswer => 15 * MIN,
            Fail::Refused(err::CREATE_FACTION) => 30 * MIN,
            Fail::Refused(_) => 10 * MIN,
        }
    }

    /// Whether the refusal means the bots' own checks were wrong (a team member was not what the
    /// bot's card said): worth a report.
    pub fn is_bot_mistake(self) -> bool {
        matches!(
            self,
            Fail::Refused(
                err::CREATE_LEVEL
                    | err::CREATE_FACTION
                    | err::CREATE_MEMBER_GUILDED
                    | err::ALREADY_MEMBER
            )
        )
    }

    /// A short phrase for the log.
    pub fn describe(self) -> String {
        match self {
            Fail::NotReady => "not ready for a team".into(),
            Fail::Declined => "a member declined the team".into(),
            Fail::InviteTimeout => "a member did not join the team".into(),
            Fail::NoMoney => "the founder cannot pay".into(),
            Fail::Refused(c) => format!("the server refused the guild (error {c})"),
            Fail::NoName => "every name was refused".into(),
            Fail::NoAnswer => "the server did not answer the creation".into(),
            Fail::TeamLost => "the team broke up".into(),
            Fail::Timeout => "timed out".into(),
        }
    }
}

/// What a step came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Keep going.
    Running,
    /// The guild exists and the team is gone.
    Done,
    /// It did not work out.
    Failed(Fail),
}

/// A member of the founding team as the bot knows it (from the coordinator's order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Character id (0 until the bot's slot knows it).
    pub char_id: u32,
    /// Character name (empty until known).
    pub name: Box<str>,
}

/// A team invitation waiting for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamInvite {
    /// Character id of the inviter.
    pub from_char: u32,
}

/// Everything one step looks at.
#[derive(Clone, Copy)]
pub struct Snap<'a> {
    /// The bot clock in milliseconds.
    pub now_ms: u64,
    /// The members in the order of the coordinator's order, the founder first.
    pub peers: &'a [Peer],
    /// The team the bot is in (0 none).
    pub team_id: u32,
    /// The characters of the team.
    pub team_chars: &'a [u32],
    /// The team invitation waiting for an answer.
    pub invite: Option<&'a TeamInvite>,
    /// Invitations of the bot that were declined, since the goal started.
    pub rejected: u8,
    /// The bot's guild (0 none).
    pub guild: u32,
    /// Copper.
    pub money: i64,
    /// The bot may mint gold (`bots.gm_bootstrap`).
    pub gm: bool,
    /// Whether a guild of this name is known to exist.
    pub name_taken: &'a dyn Fn(&str) -> bool,
    /// The `ErrorMessageType`s the server sent lately, with the bot clock they came at.
    pub errors: &'a [(u64, i16)],
}

/// A packet to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Invite `name` into the team (`team_id` 0 starts one).
    Invite {
        /// The team.
        team_id: u32,
        /// Who.
        name: Box<str>,
    },
    /// Accept the invitation of `snap.invite`.
    Accept,
    /// Decline the invitation of `snap.invite`.
    Reject,
    /// `CSCreateExpedition` with this name.
    Create(String),
    /// Dismiss the team (the founder is its owner).
    Dismiss {
        /// The team.
        team_id: u32,
    },
    /// Leave the team.
    LeaveTeam {
        /// The team.
        team_id: u32,
    },
    /// Give the bot the gold for the charter (`/gold add self`).
    Gold,
}

/// Acts of one step.
pub type Acts = SmallVec<[Act; 2]>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// Founder: wait for the others to start. Member: wait for the invitation.
    Hold,
    /// Founder: invite `peers[rank]` (`sent_ms` 0: not yet, and not before `not_before_ms`).
    Invite {
        rank: usize,
        sent_ms: u64,
        tries: u8,
        not_before_ms: u64,
    },
    /// Founder: the creation of `name` is out.
    Create { name: String, sent_ms: u64 },
    /// Founder: the charter was too dear and gold is being minted; the creation goes out again
    /// once it is there.
    Gold { name: String, since_ms: u64 },
    /// Founder: the guild exists, the team is being dismissed.
    Dismiss { sent_ms: u64, left: bool },
    /// Member: in the team, waiting for the guild.
    Joined { since_ms: u64 },
    /// Member: the guild exists, waiting for the team to go.
    Guilded { since_ms: u64 },
    /// Over.
    Over,
}

/// The state machine of one bot's part in a founding. See the module docs.
#[derive(Debug, Clone)]
pub struct Machine {
    founder: bool,
    started_ms: u64,
    phase: Phase,
    /// Names tried so far.
    tries: u32,
    /// Declined invitations already counted.
    rejected_seen: u8,
    /// The bot was in the team at some point.
    in_team: bool,
    /// The gold was given once.
    gold_given: bool,
    /// When the member last answered an invitation.
    accepted_ms: u64,
    /// The name of the guild, once it was created (the founder).
    pub created: Option<String>,
}

impl Machine {
    /// A machine for the founder (`founder`) or a member, started at `now_ms`.
    pub fn new(founder: bool, now_ms: u64) -> Machine {
        Machine {
            founder,
            started_ms: now_ms.max(1),
            phase: Phase::Hold,
            tries: 0,
            rejected_seen: 0,
            in_team: false,
            gold_given: false,
            accepted_ms: 0,
            created: None,
        }
    }

    /// Whether this is the founder's machine.
    pub fn is_founder(&self) -> bool {
        self.founder
    }

    /// Names tried so far.
    pub fn name_tries(&self) -> u32 {
        self.tries
    }

    /// A one-line description of the phase for the web UI.
    pub fn describe(&self) -> String {
        let what = match &self.phase {
            Phase::Hold if self.founder => "gathering the founding team",
            Phase::Hold => "waiting for the founding team",
            Phase::Invite { .. } => "inviting the founding team",
            Phase::Create { .. } | Phase::Gold { .. } => "founding the guild",
            Phase::Dismiss { .. } => "disbanding the founding team",
            Phase::Joined { .. } | Phase::Guilded { .. } => "in the founding team",
            Phase::Over => "done",
        };
        format!("guild: {what}")
    }

    /// One step.
    pub fn step(&mut self, snap: &Snap<'_>, rng: &mut fastrand::Rng) -> (Status, Acts) {
        let mut acts = Acts::new();
        if snap.team_id != 0 {
            self.in_team = true;
        }
        let status = if self.founder {
            self.founder_step(snap, rng, &mut acts)
        } else {
            self.member_step(snap, &mut acts)
        };
        if status != Status::Running {
            self.phase = Phase::Over;
        }
        (status, acts)
    }

    // ---- the founder -----------------------------------------------------------------------

    fn founder_step(
        &mut self,
        snap: &Snap<'_>,
        rng: &mut fastrand::Rng,
        acts: &mut Acts,
    ) -> Status {
        let now = snap.now_ms;
        if snap.guild != 0
            && !matches!(
                self.phase,
                Phase::Create { .. } | Phase::Gold { .. } | Phase::Dismiss { .. }
            )
        {
            // (a guild we did not found: an invitation was accepted in the meantime)
            return Status::Failed(Fail::NotReady);
        }
        // a team member that was in the team and is gone breaks the founding before the guild
        if self.in_team
            && snap.team_id == 0
            && matches!(
                self.phase,
                Phase::Invite { .. } | Phase::Create { .. } | Phase::Gold { .. }
            )
            && snap.guild == 0
        {
            return Status::Failed(Fail::TeamLost);
        }
        match self.phase.clone() {
            Phase::Hold => {
                if now.saturating_sub(self.started_ms) < START_GRACE_MS {
                    return Status::Running;
                }
                if snap.peers.len() < 2 || snap.peers.iter().skip(1).any(|p| p.char_id == 0) {
                    // the others are not known yet
                    if now.saturating_sub(self.started_ms) > INVITE_WAIT_MS {
                        return Status::Failed(Fail::NotReady);
                    }
                    return Status::Running;
                }
                if snap.money < guild::CREATE_COST {
                    if !snap.gm {
                        return Status::Failed(Fail::NoMoney);
                    }
                    if !self.gold_given {
                        self.gold_given = true;
                        acts.push(Act::Gold);
                    }
                    if now.saturating_sub(self.started_ms) > START_GRACE_MS + INVITE_WAIT_MS {
                        return Status::Failed(Fail::NoMoney);
                    }
                    return Status::Running;
                }
                self.phase = Phase::Invite {
                    rank: 1,
                    sent_ms: 0,
                    tries: 0,
                    not_before_ms: 0,
                };
                Status::Running
            }
            Phase::Invite {
                rank,
                sent_ms,
                tries,
                not_before_ms,
            } => self.invite_step(snap, acts, (rank, sent_ms, tries, not_before_ms)),
            Phase::Gold { name, since_ms } => {
                if snap.money >= guild::CREATE_COST || now.saturating_sub(since_ms) > 4_000 {
                    acts.push(Act::Create(name.clone()));
                    self.phase = Phase::Create {
                        name,
                        sent_ms: now.max(1),
                    };
                }
                Status::Running
            }
            Phase::Create { name, sent_ms } => self.create_step(snap, rng, acts, &name, sent_ms),
            Phase::Dismiss { sent_ms, left } => {
                if snap.team_id == 0 {
                    return Status::Done;
                }
                if sent_ms == 0 {
                    acts.push(Act::Dismiss {
                        team_id: snap.team_id,
                    });
                    self.phase = Phase::Dismiss {
                        sent_ms: now.max(1),
                        left,
                    };
                } else if now.saturating_sub(sent_ms) > DISMISS_WAIT_MS {
                    if left {
                        // (the server keeps the team: the guild exists, that is what counts)
                        return Status::Done;
                    }
                    acts.push(Act::LeaveTeam {
                        team_id: snap.team_id,
                    });
                    self.phase = Phase::Dismiss {
                        sent_ms: now.max(1),
                        left: true,
                    };
                }
                Status::Running
            }
            Phase::Joined { .. } | Phase::Guilded { .. } | Phase::Over => Status::Running,
        }
    }

    fn invite_step(
        &mut self,
        snap: &Snap<'_>,
        acts: &mut Acts,
        (rank, sent_ms, tries, not_before_ms): (usize, u64, u8, u64),
    ) -> Status {
        let now = snap.now_ms;
        let Some(peer) = snap.peers.get(rank) else {
            // everybody is in: choose a name
            return self.start_create();
        };
        if snap.team_chars.contains(&peer.char_id) {
            self.phase = Phase::Invite {
                rank: rank + 1,
                sent_ms: 0,
                tries: 0,
                not_before_ms: 0,
            };
            return Status::Running;
        }
        if sent_ms == 0 {
            if now < not_before_ms {
                return Status::Running;
            }
            if peer.name.is_empty() {
                return Status::Failed(Fail::NotReady);
            }
            // the server's invitations are one per target: the team id of the first one is 0
            acts.push(Act::Invite {
                team_id: snap.team_id,
                name: peer.name.clone(),
            });
            self.rejected_seen = snap.rejected;
            self.phase = Phase::Invite {
                rank,
                sent_ms: now.max(1),
                tries,
                not_before_ms: 0,
            };
            return Status::Running;
        }
        if snap.rejected > self.rejected_seen {
            // declined, or the target had an invitation pending
            self.rejected_seen = snap.rejected;
            if tries >= 1 {
                return Status::Failed(Fail::Declined);
            }
            self.phase = Phase::Invite {
                rank,
                sent_ms: 0,
                tries: tries + 1,
                not_before_ms: now + REINVITE_MS,
            };
            return Status::Running;
        }
        if now.saturating_sub(sent_ms) > INVITE_WAIT_MS {
            return Status::Failed(Fail::InviteTimeout);
        }
        Status::Running
    }

    fn start_create(&mut self) -> Status {
        self.phase = Phase::Create {
            name: String::new(),
            sent_ms: 0,
        };
        Status::Running
    }

    fn create_step(
        &mut self,
        snap: &Snap<'_>,
        rng: &mut fastrand::Rng,
        acts: &mut Acts,
        name: &str,
        sent_ms: u64,
    ) -> Status {
        let now = snap.now_ms;
        if snap.guild != 0 {
            self.created = Some(name.to_string());
            self.phase = Phase::Dismiss {
                sent_ms: 0,
                left: false,
            };
            return Status::Running;
        }
        if sent_ms == 0 {
            return self.send_create(snap, rng, acts);
        }
        // the newest refusal since the creation went out
        let refusal = snap
            .errors
            .iter()
            .rev()
            .find(|(at, code)| *at >= sent_ms && err::is_founding(*code))
            .map(|(_, code)| *code);
        match refusal {
            Some(err::NAME_EXISTS | err::NAME_CHARACTER | err::NAME_LENGTH) => {
                if self.tries >= MAX_NAME_TRIES {
                    return Status::Failed(Fail::NoName);
                }
                self.send_create(snap, rng, acts)
            }
            Some(err::CREATE_MONEY) => {
                if snap.gm && !self.gold_given {
                    self.gold_given = true;
                    acts.push(Act::Gold);
                    // (the same name again once the gold is there)
                    self.phase = Phase::Gold {
                        name: name.to_string(),
                        since_ms: now.max(1),
                    };
                    return Status::Running;
                }
                Status::Failed(Fail::NoMoney)
            }
            Some(code) => Status::Failed(Fail::Refused(code)),
            None => {
                if now.saturating_sub(sent_ms) > CREATE_WAIT_MS {
                    Status::Failed(Fail::NoAnswer)
                } else {
                    Status::Running
                }
            }
        }
    }

    fn send_create(&mut self, snap: &Snap<'_>, rng: &mut fastrand::Rng, acts: &mut Acts) -> Status {
        // a name nobody has: the book knows the guilds the server listed and the names it refused
        let mut name = super::names::generate(rng, self.tries);
        for _ in 0..8 {
            if !(snap.name_taken)(&name) {
                break;
            }
            self.tries += 1;
            name = super::names::generate(rng, self.tries);
        }
        self.tries += 1;
        acts.push(Act::Create(name.clone()));
        self.phase = Phase::Create {
            name,
            sent_ms: snap.now_ms.max(1),
        };
        Status::Running
    }

    // ---- a member --------------------------------------------------------------------------

    fn member_step(&mut self, snap: &Snap<'_>, acts: &mut Acts) -> Status {
        let now = snap.now_ms;
        let leader = snap.peers.first().map_or(0, |p| p.char_id);
        // an invitation: the leader's is answered, anybody else's is turned down
        if let Some(inv) = snap.invite {
            if leader != 0 && inv.from_char == leader {
                if now.saturating_sub(self.accepted_ms) >= ACCEPT_GAP_MS || self.accepted_ms == 0 {
                    self.accepted_ms = now.max(1);
                    acts.push(Act::Accept);
                }
            } else {
                acts.push(Act::Reject);
            }
        }
        match self.phase.clone() {
            Phase::Hold => {
                if snap.guild != 0 {
                    return Status::Failed(Fail::NotReady);
                }
                if snap.team_id != 0 {
                    self.phase = Phase::Joined { since_ms: now };
                } else if now.saturating_sub(self.started_ms) > MEMBER_WAIT_MS {
                    return Status::Failed(Fail::Timeout);
                }
                Status::Running
            }
            Phase::Joined { since_ms } => {
                if snap.guild != 0 {
                    self.phase = Phase::Guilded { since_ms: now };
                    return Status::Running;
                }
                if snap.team_id == 0 {
                    return Status::Failed(Fail::TeamLost);
                }
                if now.saturating_sub(since_ms) > MEMBER_WAIT_MS {
                    acts.push(Act::LeaveTeam {
                        team_id: snap.team_id,
                    });
                    return Status::Failed(Fail::Timeout);
                }
                Status::Running
            }
            Phase::Guilded { since_ms } => {
                if snap.team_id == 0 {
                    return Status::Done;
                }
                if now.saturating_sub(since_ms) > MEMBER_LEAVE_MS {
                    acts.push(Act::LeaveTeam {
                        team_id: snap.team_id,
                    });
                    self.phase = Phase::Guilded { since_ms: now };
                    // (one more wait; the team is the founder's to dismiss)
                    if now.saturating_sub(self.started_ms) > 3 * MEMBER_WAIT_MS {
                        return Status::Done;
                    }
                }
                Status::Running
            }
            Phase::Invite { .. }
            | Phase::Create { .. }
            | Phase::Gold { .. }
            | Phase::Dismiss { .. }
            | Phase::Over => Status::Running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COST: i64 = guild::CREATE_COST;

    fn peers(n: u32) -> Vec<Peer> {
        (0..n)
            .map(|i| Peer {
                char_id: 100 + i,
                name: format!("Bot{i}").into(),
            })
            .collect()
    }

    struct World {
        now: u64,
        peers: Vec<Peer>,
        team_id: u32,
        team: Vec<u32>,
        invite: Option<TeamInvite>,
        rejected: u8,
        guild: u32,
        money: i64,
        gm: bool,
        taken: Vec<String>,
        errors: Vec<(u64, i16)>,
    }

    impl World {
        fn new(n: u32, money: i64) -> World {
            World {
                now: 10_000,
                peers: peers(n),
                team_id: 0,
                team: Vec::new(),
                invite: None,
                rejected: 0,
                guild: 0,
                money,
                gm: false,
                taken: Vec::new(),
                errors: Vec::new(),
            }
        }

        fn step(&mut self, m: &mut Machine, rng: &mut fastrand::Rng) -> (Status, Acts) {
            let taken = self.taken.clone();
            let name_taken = move |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
            let snap = Snap {
                now_ms: self.now,
                peers: &self.peers,
                team_id: self.team_id,
                team_chars: &self.team,
                invite: self.invite.as_ref(),
                rejected: self.rejected,
                guild: self.guild,
                money: self.money,
                gm: self.gm,
                name_taken: &name_taken,
                errors: &self.errors,
            };
            m.step(&snap, rng)
        }

        fn after(&mut self, ms: u64) {
            self.now += ms;
        }
    }

    fn rng() -> fastrand::Rng {
        fastrand::Rng::with_seed(3)
    }

    /// Runs the founder until it sends something; panics when it ends.
    fn until_act(w: &mut World, m: &mut Machine, r: &mut fastrand::Rng) -> Acts {
        for _ in 0..200 {
            let (st, acts) = w.step(m, r);
            assert!(matches!(st, Status::Running), "ended: {st:?}");
            if !acts.is_empty() {
                return acts;
            }
            w.after(500);
        }
        panic!("the founder never acted");
    }

    #[test]
    fn the_founder_invites_in_order_creates_and_dismisses() {
        let mut w = World::new(5, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        // the others get time to start their goal
        assert_eq!(w.step(&mut m, &mut r), (Status::Running, Acts::new()));
        assert!(m.describe().contains("gathering"));
        w.after(START_GRACE_MS);
        // one invitation after the other, the first without a team
        for rank in 1..5usize {
            let acts = until_act(&mut w, &mut m, &mut r);
            assert_eq!(
                acts.as_slice(),
                [Act::Invite {
                    team_id: w.team_id,
                    name: format!("Bot{rank}").into()
                }]
            );
            // the invitation is not repeated while it waits
            w.after(2_000);
            assert!(w.step(&mut m, &mut r).1.is_empty());
            // the member joins
            w.team_id = 77;
            w.team.push(100 + rank as u32);
            w.team.push(100);
            w.after(300);
        }
        // everybody is in: the creation
        let acts = until_act(&mut w, &mut m, &mut r);
        let [Act::Create(name)] = acts.as_slice() else {
            panic!("{acts:?}");
        };
        assert!(super::super::names::valid(name), "{name}");
        // the guild appears: the team is dismissed
        w.after(1_000);
        w.guild = 9;
        let acts = until_act(&mut w, &mut m, &mut r);
        assert_eq!(acts.as_slice(), [Act::Dismiss { team_id: 77 }]);
        assert_eq!(m.created.as_deref(), Some(name.as_str()));
        w.after(500);
        w.team_id = 0;
        w.team.clear();
        assert_eq!(w.step(&mut m, &mut r).0, Status::Done);
    }

    #[test]
    fn the_founder_leaves_a_team_the_server_does_not_dismiss() {
        let mut w = World::new(2, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        until_act(&mut w, &mut m, &mut r); // invite
        w.team_id = 5;
        w.team = vec![100, 101];
        until_act(&mut w, &mut m, &mut r); // create
        w.guild = 3;
        assert!(matches!(
            until_act(&mut w, &mut m, &mut r).as_slice(),
            [Act::Dismiss { .. }]
        ));
        w.after(DISMISS_WAIT_MS + 1);
        assert_eq!(
            w.step(&mut m, &mut r).1.as_slice(),
            [Act::LeaveTeam { team_id: 5 }]
        );
        w.after(DISMISS_WAIT_MS + 1);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Done);
    }

    #[test]
    fn a_declined_invitation_is_repeated_once() {
        let mut w = World::new(3, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        assert!(matches!(
            until_act(&mut w, &mut m, &mut r).as_slice(),
            [Act::Invite { .. }]
        ));
        w.rejected = 1;
        w.after(500);
        // (the invitation goes out again at once: the old one was refused, not pending)
        let acts = until_act(&mut w, &mut m, &mut r);
        assert!(matches!(acts.as_slice(), [Act::Invite { .. }]));
        w.rejected = 2;
        w.after(500);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::Declined));
    }

    #[test]
    fn an_invitation_nobody_answers_times_out() {
        let mut w = World::new(3, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        until_act(&mut w, &mut m, &mut r);
        w.after(INVITE_WAIT_MS + 1);
        assert_eq!(
            w.step(&mut m, &mut r).0,
            Status::Failed(Fail::InviteTimeout)
        );
    }

    #[test]
    fn a_broken_team_ends_the_founding() {
        let mut w = World::new(3, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        until_act(&mut w, &mut m, &mut r);
        w.team_id = 5;
        w.team = vec![100, 101];
        w.after(500);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Running);
        w.team_id = 0;
        w.team.clear();
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::TeamLost));
    }

    #[test]
    fn a_founder_without_money_needs_gm_help() {
        let mut w = World::new(2, COST - 1);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NoMoney));
        // with GM rights the gold is minted once, then the founding goes on when it has arrived
        let mut w = World::new(2, COST - 1);
        w.gm = true;
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        assert_eq!(w.step(&mut m, &mut r).1.as_slice(), [Act::Gold]);
        w.after(500);
        assert!(w.step(&mut m, &mut r).1.is_empty());
        w.money = COST + 5_000;
        w.after(500);
        assert!(matches!(w.step(&mut m, &mut r).0, Status::Running));
        assert!(matches!(
            until_act(&mut w, &mut m, &mut r).as_slice(),
            [Act::Invite { .. }]
        ));
    }

    fn at_creation(taken: Vec<String>, gm: bool) -> (World, Machine, fastrand::Rng) {
        let mut w = World::new(2, COST);
        w.taken = taken;
        w.gm = gm;
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        until_act(&mut w, &mut m, &mut r); // invite
        w.team_id = 5;
        w.team = vec![100, 101];
        (w, m, r)
    }

    #[test]
    fn a_name_the_book_knows_is_not_tried() {
        // every name of the first draws is "taken" except the ones the book does not list
        let mut r = fastrand::Rng::with_seed(3);
        let first = super::super::names::generate(&mut r, 0);
        let (mut w, mut m, mut r) = at_creation(vec![first.clone()], false);
        let acts = until_act(&mut w, &mut m, &mut r);
        let [Act::Create(name)] = acts.as_slice() else {
            panic!("{acts:?}");
        };
        assert_ne!(name, &first);
    }

    #[test]
    fn a_taken_name_is_replaced_until_the_tries_run_out() {
        let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
        let mut names = Vec::new();
        for _ in 0..MAX_NAME_TRIES {
            let acts = until_act(&mut w, &mut m, &mut r);
            let [Act::Create(name)] = acts.as_slice() else {
                panic!("{acts:?}");
            };
            names.push(name.clone());
            w.after(700);
            w.errors.push((w.now, err::NAME_EXISTS));
            w.after(100);
        }
        // all different, the fifth refusal ends it
        names.sort();
        names.dedup();
        assert!(names.len() >= 4, "{names:?}");
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NoName));
    }

    #[test]
    fn an_old_refusal_does_not_count() {
        let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
        w.errors.push((w.now - 5_000, err::NAME_EXISTS));
        until_act(&mut w, &mut m, &mut r); // the creation
        w.after(2_000);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Running);
        w.guild = 4;
        let acts = until_act(&mut w, &mut m, &mut r);
        assert!(matches!(acts.as_slice(), [Act::Dismiss { .. }]));
    }

    #[test]
    fn every_other_refusal_ends_the_founding_with_its_code() {
        for code in [
            err::CREATE_LEVEL,
            err::CREATE_MEMBER,
            err::CREATE_FACTION,
            err::CREATE_MEMBER_GUILDED,
            err::ALREADY_MEMBER,
        ] {
            let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
            until_act(&mut w, &mut m, &mut r);
            w.after(300);
            w.errors.push((w.now, code));
            w.after(100);
            let st = w.step(&mut m, &mut r).0;
            assert_eq!(st, Status::Failed(Fail::Refused(code)));
        }
        // errors that are no founding errors are not blamed on the creation
        let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
        until_act(&mut w, &mut m, &mut r);
        w.after(300);
        w.errors.push((w.now, 999));
        assert_eq!(w.step(&mut m, &mut r).0, Status::Running);
    }

    #[test]
    fn a_refusal_for_money_mints_once_with_gm_rights() {
        let (mut w, mut m, mut r) = at_creation(Vec::new(), true);
        let acts = until_act(&mut w, &mut m, &mut r);
        let [Act::Create(name)] = acts.as_slice() else {
            panic!("{acts:?}");
        };
        let name = name.clone();
        w.after(300);
        w.errors.push((w.now, err::CREATE_MONEY));
        w.after(100);
        assert_eq!(w.step(&mut m, &mut r).1.as_slice(), [Act::Gold]);
        // it asks for the same name again shortly after
        let mut again = None;
        for _ in 0..20 {
            w.after(500);
            let (st, acts) = w.step(&mut m, &mut r);
            assert_eq!(st, Status::Running);
            if let [Act::Create(n)] = acts.as_slice() {
                again = Some(n.clone());
                break;
            }
        }
        assert_eq!(again.as_deref(), Some(name.as_str()), "the same name again");
    }

    #[test]
    fn a_refusal_for_money_without_gm_rights_ends_it() {
        let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
        until_act(&mut w, &mut m, &mut r);
        w.after(300);
        w.errors.push((w.now, err::CREATE_MONEY));
        w.after(100);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NoMoney));
    }

    #[test]
    fn a_creation_the_server_ignores_ends_with_no_answer() {
        let (mut w, mut m, mut r) = at_creation(Vec::new(), false);
        until_act(&mut w, &mut m, &mut r);
        w.after(CREATE_WAIT_MS + 1);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NoAnswer));
    }

    #[test]
    fn a_founder_who_got_a_guild_meanwhile_stops() {
        let mut w = World::new(3, COST);
        let mut r = rng();
        let mut m = Machine::new(true, w.now);
        w.after(START_GRACE_MS);
        until_act(&mut w, &mut m, &mut r);
        w.guild = 4;
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NotReady));
    }

    // ---- members ----------------------------------------------------------------------------

    #[test]
    fn a_member_accepts_the_founders_invitation_once_and_turns_down_strangers() {
        let mut w = World::new(5, 0);
        let mut r = rng();
        let mut m = Machine::new(false, w.now);
        assert!(w.step(&mut m, &mut r).1.is_empty());
        w.invite = Some(TeamInvite { from_char: 555 });
        assert_eq!(w.step(&mut m, &mut r).1.as_slice(), [Act::Reject]);
        w.invite = Some(TeamInvite { from_char: 100 });
        w.after(500);
        assert_eq!(w.step(&mut m, &mut r).1.as_slice(), [Act::Accept]);
        // (the same invitation again within the gap is not answered twice)
        w.after(500);
        assert!(w.step(&mut m, &mut r).1.is_empty());
        w.after(ACCEPT_GAP_MS);
        assert_eq!(w.step(&mut m, &mut r).1.as_slice(), [Act::Accept]);
    }

    #[test]
    fn a_member_waits_for_the_guild_and_leaves_the_team_when_it_is_not_dismissed() {
        let mut w = World::new(5, 0);
        let mut r = rng();
        let mut m = Machine::new(false, w.now);
        w.team_id = 8;
        w.after(100);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Running);
        w.after(2_000);
        w.guild = 3;
        assert_eq!(w.step(&mut m, &mut r).0, Status::Running);
        w.after(MEMBER_LEAVE_MS + 1);
        assert_eq!(
            w.step(&mut m, &mut r).1.as_slice(),
            [Act::LeaveTeam { team_id: 8 }]
        );
        w.team_id = 0;
        w.after(300);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Done);
    }

    #[test]
    fn a_member_is_done_when_the_dismissed_team_and_the_guild_are_there() {
        let mut w = World::new(5, 0);
        let mut r = rng();
        let mut m = Machine::new(false, w.now);
        w.team_id = 8;
        w.step(&mut m, &mut r);
        w.guild = 3;
        w.after(300);
        w.step(&mut m, &mut r);
        w.team_id = 0;
        w.after(300);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Done);
    }

    #[test]
    fn a_member_fails_when_nothing_comes() {
        let mut w = World::new(5, 0);
        let mut r = rng();
        let mut m = Machine::new(false, w.now);
        w.after(MEMBER_WAIT_MS + 1);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::Timeout));
        // a team that goes before the guild came
        let mut w = World::new(5, 0);
        let mut m = Machine::new(false, w.now);
        w.team_id = 8;
        w.step(&mut m, &mut r);
        w.team_id = 0;
        w.after(300);
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::TeamLost));
        // a team that never gets its guild
        let mut w = World::new(5, 0);
        let mut m = Machine::new(false, w.now);
        w.team_id = 8;
        w.step(&mut m, &mut r);
        w.after(MEMBER_WAIT_MS + 1);
        let (st, acts) = w.step(&mut m, &mut r);
        assert_eq!(st, Status::Failed(Fail::Timeout));
        assert_eq!(acts.as_slice(), [Act::LeaveTeam { team_id: 8 }]);
    }

    #[test]
    fn a_member_with_a_guild_already_is_not_ready() {
        let mut w = World::new(5, 0);
        let mut r = rng();
        let mut m = Machine::new(false, w.now);
        w.guild = 2;
        assert_eq!(w.step(&mut m, &mut r).0, Status::Failed(Fail::NotReady));
    }

    #[test]
    fn failures_have_back_offs_and_the_bots_mistakes_are_known() {
        for f in [
            Fail::NotReady,
            Fail::Declined,
            Fail::InviteTimeout,
            Fail::NoMoney,
            Fail::Refused(err::CREATE_MEMBER),
            Fail::NoName,
            Fail::NoAnswer,
            Fail::TeamLost,
            Fail::Timeout,
        ] {
            assert!(f.back_off_ms() >= 60_000, "{f:?}");
            assert!(!f.describe().is_empty());
        }
        assert!(Fail::Refused(err::CREATE_FACTION).back_off_ms() > Fail::NoMoney.back_off_ms());
        assert!(Fail::Refused(err::CREATE_MEMBER_GUILDED).is_bot_mistake());
        assert!(!Fail::Refused(err::CREATE_MEMBER).is_bot_mistake());
        assert!(!Fail::NoMoney.is_bot_mistake());
    }
}
