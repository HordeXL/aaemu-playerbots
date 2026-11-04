//! Guild: sign a charter and found a guild together (the guild feature, see
//! [`crate::bot::guild`]).
//!
//! The goal has two modes.
//!
//! * **Wish** (no order): a bot with no guild that can pay the charter tells the coordinator it
//!   wants a founding team ([`CoordHandle::request_guild`](crate::bot::coordinator::CoordHandle))
//!   and is done; it goes on with whatever it did, and the coordinator keeps the wish for
//!   [`WISH_TTL`](crate::bot::guild::plan::WISH_TTL).
//! * **Founding** (a [`PartyOrder`] with the purpose `Guild`): the coordinator found
//!   `party_size` bots of one faction; the first founds, the others sign. The bot runs the pure
//!   [`found::Machine`](crate::bot::guild::found::Machine) and turns its acts into packets: team
//!   invitations by name (no walking, the server checks no distance), the creation with a
//!   generated name, the dismissal of the team. A failure rests the bot for
//!   [`Fail::back_off_ms`], files a report when the failure points at the server (see
//!   [`report`]), and frees the place the founding held in the guild book.
//!
//! What a bot does in its guild afterwards is the duty's ([`crate::bot::guild::duty`]), whatever
//! goal it runs.
//!
//! * [`score`]: the goal's whole eligibility and weight, read by the brain instead of the usual
//!   `scores` table.
//! * [`base_weight`]: the weight before the situation is looked at (0: the goal has its own).

use aa_proto::cs;
use aa_proto::guild as wire;
use pb_web::Activity;
use smallvec::SmallVec;

use crate::bot::cmd::GoalKind;
use crate::bot::coordinator::Dispatch;
use crate::bot::ctx::Ctx;
use crate::bot::guild::found::{Act, Fail, Machine, Peer, Snap, Status, TeamInvite};
use crate::bot::guild::plan::{GuildWish, WISH_TTL};
use crate::bot::guild::{duty, report};
use crate::bot::party::PartyOrder;
use crate::bot::state::StateError;
use crate::config::BehaviorCfg;
use crate::roster::Temperament;

use super::{FailReason, GoalStatus};

/// The score of the goal for a bot that can found a guild and has not asked yet: above the
/// everyday goals, because the goal takes a single tick.
const WISH_SCORE: f32 = 1.5;
/// A wish the coordinator's inbox did not take is repeated for this long.
const POST_RETRY_MS: u64 = 5_000;
/// The peers (names and character ids of the founding team) are read again this often.
const PEERS_EVERY_MS: u64 = 1_000;
/// Gold minted for the charter with `bots.gm_bootstrap` (the charter costs 10000 copper, one
/// gold; two leave something over).
const MINT_GOLD: &str = "/gold add self 2";

/// Guild: wish for a founding team, or sign the charter of one. See the module docs.
#[derive(Debug, Default)]
pub struct Guild {
    /// The coordinator's order (founding mode), kept for the ids of the peers.
    order: Option<Box<PartyOrder>>,
    /// The founding under way.
    run: Option<Box<Run>>,
    /// Bot clock at which the goal started.
    started_ms: u64,
}

/// The founding under way.
#[derive(Debug)]
struct Run {
    machine: Machine,
    /// The team in the order of the coordinator's order, the founder first.
    peers: SmallVec<[Peer; 5]>,
    /// Bot clock of the last read of the peers.
    peers_ms: u64,
    /// The last creation that went out: when and with which name.
    sent: Option<(u64, String)>,
    /// The coordinator's id of the team (also the reservation in the guild book).
    party_id: u32,
    /// This bot is the founder.
    leader: bool,
}

impl Guild {
    /// A fresh goal: with an order (a founding team formed) or a plain wish.
    pub fn new(order: Option<Box<PartyOrder>>) -> Guild {
        Guild {
            order,
            run: None,
            started_ms: 0,
        }
    }

    /// Called once when the brain picks the goal.
    pub fn start(&mut self, ctx: &mut Ctx<'_>) {
        self.started_ms = ctx.now_ms.max(1);
        let Some(order) = self.order.as_deref() else {
            let _ = ctx.state.guild.mem();
            return;
        };
        let bot = ctx.persona.id;
        let leader = order.is_leader(bot);
        let mem = ctx.state.guild.mem();
        mem.t.founding = true;
        mem.t.wished_ms = 0;
        let party_id = order.party_id;
        let size = order.members.len();
        self.run = Some(Box::new(Run {
            machine: Machine::new(leader, ctx.now_ms),
            peers: SmallVec::new(),
            peers_ms: 0,
            sent: None,
            party_id,
            leader,
        }));
        ctx.log.say(format!(
            "guild: {} a founding team of {size}",
            if leader { "leading" } else { "joining" }
        ));
    }

    /// One tick.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        if self.run.is_none() {
            return self.post_wish(ctx);
        }
        self.found(ctx)
    }

    /// Called when the goal ends, fails or is interrupted (stop motion, close windows).
    pub fn stop(&mut self, ctx: &mut Ctx<'_>) {
        let Some(run) = self.run.take() else {
            return;
        };
        // (the founding is over whichever way: the place it held in the book is free, and a bot
        // that is still in the team leaves it, the founder dismisses it)
        if run.leader {
            ctx.hub.guilds.release(run.party_id);
        }
        if let Some(mem) = ctx.state.guild.get_mut() {
            mem.t.founding = false;
        }
        let me = ctx.state.me.char_id;
        if let Some(team_id) = ctx.state.team().map(|t| t.team_id).filter(|t| *t != 0) {
            let action = if run.leader {
                cs::RISKY_DISMISS
            } else {
                cs::RISKY_LEAVE
            };
            ctx.send(cs::ask_risky_team_action(team_id, me, action));
        }
    }

    /// Whether the goal drives the combat controller itself (see [`super::Goal::owns_combat`]).
    pub fn owns_combat(&self) -> bool {
        false
    }

    /// The activity the web UI shows while the goal runs.
    pub fn activity(&self, moving: bool, fighting: bool) -> Activity {
        if fighting {
            Activity::Fighting
        } else if moving {
            Activity::Traveling
        } else {
            Activity::Idle
        }
    }

    /// Appends a one-line description of what the goal is doing to `out`.
    pub fn describe(&self, out: &mut String) {
        match &self.run {
            Some(run) => out.push_str(&run.machine.describe()),
            None => out.push_str("guild: looking for a founding team"),
        }
    }

    // ---- wish ------------------------------------------------------------------------------

    /// Tells the coordinator the bot wants a founding team, then is done.
    fn post_wish(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let facts = duty::facts(ctx, Some(GoalKind::Guild));
        if !duty::can_found(&facts) {
            return GoalStatus::Failed(FailReason::Other("guild: cannot sign a charter now"));
        }
        // (a wish is good for a long while; asking again would only move the bot back in line)
        let ttl = WISH_TTL.as_millis() as u64;
        let wished =
            ctx.state.guild.get().is_some_and(|m| {
                m.t.wished_ms != 0 && ctx.now_ms.saturating_sub(m.t.wished_ms) < ttl
            });
        if wished {
            return GoalStatus::Done;
        }
        let wish = GuildWish {
            bot: ctx.persona.id,
            mother: duty::mother_of(ctx),
            level: ctx.state.me.level,
            can_pay: facts.can_pay,
        };
        if ctx.coord.request_guild(wish) {
            let now = ctx.now_ms.max(1);
            ctx.state.guild.mem().t.wished_ms = now;
            ctx.log
                .say("guild: wants to found a guild, waiting for a founding team");
            return GoalStatus::Done;
        }
        if ctx.now_ms.saturating_sub(self.started_ms) > POST_RETRY_MS {
            return GoalStatus::Failed(FailReason::Timeout);
        }
        GoalStatus::Running
    }

    // ---- founding --------------------------------------------------------------------------

    fn found(&mut self, ctx: &mut Ctx<'_>) -> GoalStatus {
        let now = ctx.now_ms;
        let Some(order) = self.order.as_deref() else {
            return GoalStatus::Failed(FailReason::Other("guild: no order"));
        };
        let Some(run) = self.run.as_deref_mut() else {
            return GoalStatus::Failed(FailReason::Other("guild: no founding"));
        };
        if now.saturating_sub(run.peers_ms) >= PEERS_EVERY_MS
            || run.peers.len() != order.members.len()
        {
            run.peers.clear();
            for &bot in &order.members {
                let (char_id, name) = Dispatch::who(ctx.hub, bot);
                run.peers.push(Peer {
                    char_id,
                    name: name.into(),
                });
            }
            run.peers_ms = now.max(1);
        }
        // a name the server refused is taken for good: the machine picks another one
        if let Some((sent, name)) = run.sent.clone() {
            let refused = ctx
                .state
                .errors
                .iter()
                .any(|(at, e)| *at >= sent && *e == StateError::Msg(wire::err::NAME_EXISTS));
            if refused {
                ctx.hub.guilds.note_name(&name);
                run.sent = None;
            }
        }
        let (team_id, team_chars, invite, rejected) = match ctx.state.team() {
            Some(t) => (
                t.team_id,
                t.members
                    .iter()
                    .map(|m| m.char_id)
                    .collect::<SmallVec<[u32; 8]>>(),
                t.invite.as_ref().map(|i| TeamInvite {
                    from_char: i.from_char,
                }),
                t.rejected,
            ),
            None => (0, SmallVec::new(), None, 0),
        };
        let errors: SmallVec<[(u64, i16); 8]> = ctx
            .state
            .errors
            .iter()
            .filter_map(|(at, e)| match e {
                StateError::Msg(code) => Some((*at, *code)),
                _ => None,
            })
            .collect();
        let guild = ctx.state.guild.log().map_or(0, |l| l.id);
        let book = &ctx.hub.guilds;
        let taken = |name: &str| book.name_taken(name);
        let snap = Snap {
            now_ms: now,
            peers: &run.peers,
            team_id,
            team_chars: &team_chars,
            invite: invite.as_ref(),
            rejected,
            guild,
            money: ctx.state.me.money,
            gm: ctx.gm_allowed(),
            name_taken: &taken,
            errors: &errors,
        };
        let (status, acts) = run.machine.step(&snap, ctx.rng);
        let created = run.machine.created.clone();
        for act in acts {
            Self::apply(ctx, run, act);
        }
        match status {
            Status::Running => GoalStatus::Running,
            Status::Done => {
                if let Some(name) = created {
                    ctx.hub.guilds.note_name(&name);
                    ctx.log.say(format!("guild: founded {name}"));
                }
                GoalStatus::Done
            }
            Status::Failed(fail) => Self::fail(ctx, run, fail),
        }
    }

    /// Puts one act of the machine on the wire.
    fn apply(ctx: &mut Ctx<'_>, run: &mut Run, act: Act) {
        let me = ctx.state.me.char_id;
        match act {
            Act::Invite { team_id, name } => {
                ctx.send(cs::invite_to_team(team_id, true, &name));
                ctx.log.say(format!("guild: invited {name} into the team"));
            }
            Act::Accept | Act::Reject => {
                let reject = act == Act::Reject;
                let Some(inv) = ctx.state.team().and_then(|t| t.invite.clone()) else {
                    return;
                };
                ctx.send(cs::reply_to_join_team(
                    inv.team_id,
                    inv.is_party,
                    inv.from_char,
                    reject,
                    &inv.from_name,
                    false,
                ));
                if reject {
                    ctx.state.party_mem().team.invite = None;
                }
                ctx.log.say(format!(
                    "guild: {} the team of {}",
                    if reject { "turned down" } else { "joining" },
                    inv.from_name
                ));
            }
            Act::Create(name) => {
                ctx.send(wire::create(&name, 0));
                ctx.log.say(format!("guild: founding {name}"));
                run.sent = Some((ctx.now_ms.max(1), name));
            }
            Act::Dismiss { team_id } => {
                ctx.send(cs::ask_risky_team_action(team_id, me, cs::RISKY_DISMISS));
                ctx.log.say("guild: dismissing the founding team");
            }
            Act::LeaveTeam { team_id } => {
                ctx.send(cs::ask_risky_team_action(team_id, me, cs::RISKY_LEAVE));
                ctx.log.say("guild: leaving the founding team");
            }
            Act::Gold => {
                if ctx.gm(MINT_GOLD) {
                    ctx.log.say("guild: minted gold for the charter");
                }
            }
        }
    }

    /// A founding that did not work: rest, report what points at the server, free the place.
    fn fail(ctx: &mut Ctx<'_>, run: &mut Run, fail: Fail) -> GoalStatus {
        let rest = fail.back_off_ms();
        let now = ctx.now_ms;
        ctx.state.guild.mem().t.found_block_until_ms = now + rest;
        ctx.log.say(format!(
            "guild: founding failed: {} (resting {} min)",
            fail.describe(),
            rest / 60_000
        ));
        let name = run.sent.as_ref().map(|s| s.1.as_str());
        if let Some(r) = report::of_failure(fail, name) {
            ctx.report_issue(r);
        }
        GoalStatus::Failed(FailReason::Other("guild: founding failed"))
    }
}

/// The goal's weight before the situation is looked at, from the configured weights and the
/// bot's temperament (`brain::base_weights` calls this; 0 means "not scored by weight").
pub fn base_weight(_behavior: &BehaviorCfg, _temper: &Temperament) -> f32 {
    0.0
}

/// The score of the goal for this tick, given its `base` weight: [`WISH_SCORE`] for a bot that
/// can sign a charter and has not asked for a team lately, else 0.
pub fn score(ctx: &Ctx<'_>, _base: f32) -> f32 {
    let cfg = &ctx.cfg.guild;
    if !(cfg.enabled && cfg.found) {
        return 0.0;
    }
    let wished = ctx.state.guild.get().is_some_and(|m| {
        m.t.wished_ms != 0 && ctx.now_ms.saturating_sub(m.t.wished_ms) < WISH_TTL.as_millis() as u64
    });
    if wished {
        return 0.0;
    }
    let facts = duty::facts(ctx, None);
    if duty::can_found(&facts) {
        WISH_SCORE
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goal_without_an_order_is_a_wish() {
        let g = Guild::new(None);
        assert_eq!(g.activity(false, false), Activity::Idle);
        assert_eq!(g.activity(true, false), Activity::Traveling);
        assert_eq!(g.activity(true, true), Activity::Fighting);
        assert!(!g.owns_combat());
        let mut text = String::new();
        g.describe(&mut text);
        assert!(text.starts_with("guild"), "{text}");
    }

    #[test]
    fn the_goal_has_no_weight_of_its_own() {
        let b = BehaviorCfg::default();
        assert_eq!(base_weight(&b, &Temperament::NEUTRAL), 0.0);
    }

    #[test]
    fn the_goal_stays_small() {
        assert!(std::mem::size_of::<Guild>() <= 32);
    }
}
