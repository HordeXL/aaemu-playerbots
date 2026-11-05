//! The LLM director: now and then a bot asks the model which of its eligible goals to do next.
//!
//! Optional (`[llm] enabled = true` and `director = true`) and advisory: the brain's own utility
//! scoring stays in charge. Every `llm.director_interval_secs` (per bot, jittered so the bots do
//! not ask together) the [`Brain`](super::brain::Brain) hands the model a compact description of
//! the bot and the goals it may pick right now (the same eligibility as its own choice, minus the
//! goals that need GM rights when those are off). The answer arrives on a later tick; if the goal
//! is still eligible it is started, otherwise it is dropped silently. At most one request per bot
//! is in flight, the global limits (requests per minute, concurrency, token budget) are the
//! client's ([`pb_llm::Llm`]); a refused or failed request changes nothing.
//!
//! The functions here are pure (eligibility, situation text, prompt); the brain owns the timing.

use std::fmt::Write as _;
use std::sync::Arc;

use pb_llm::{Choice, Llm, LlmError};
use smallvec::SmallVec;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;

use super::cmd::GoalKind;
use super::goals::social::{temperament_text, Facts};

/// Goals offered to the model.
pub type Goals = SmallVec<[GoalKind; 8]>;

/// A request that has not answered after this long is forgotten.
const INFLIGHT_TTL_MS: u64 = 60_000;
/// A choice that could not start at once waits this long for the next goal change.
const READY_TTL_MS: u64 = 5 * 60_000;
/// Goals remembered for the situation text.
const HISTORY: usize = 3;
/// The first request comes at the earliest this long after the bot started, in milliseconds.
const FIRST_ASK_MIN_MS: u64 = 60_000;
/// Longest reason the log shows, in characters.
const REASON_CHARS: usize = 80;

/// Whether `kind` needs GM rights (`bots.gm_bootstrap`) to get anywhere: trade runs and housing
/// need materials, labor, certificates and gold a fresh character does not have.
pub fn gm_only(kind: GoalKind) -> bool {
    matches!(kind, GoalKind::TradeRun | GoalKind::Housing)
}

/// The goals the director may offer: those the brain's scoring gives a chance (`scores` is 0 for
/// switched-off, cooling-down and disabled goals), without the GM-only ones when GM rights are
/// off.
pub fn eligible_goals(scores: &[f32; GoalKind::COUNT], gm_allowed: bool) -> Goals {
    GoalKind::ALL
        .into_iter()
        .filter(|g| scores[g.index()] > 0.0 && (gm_allowed || !gm_only(*g)))
        .collect()
}

/// One line describing a goal to the model.
pub fn option_text(kind: GoalKind) -> &'static str {
    match kind {
        GoalKind::Quest => "quest: pick up quests and work through them",
        GoalKind::Grind => "grind: hunt monsters nearby for experience and loot",
        GoalKind::Duel => "duel: challenge another player to a friendly duel",
        GoalKind::Trade => "trade: swap items with another player",
        GoalKind::TradeRun => "trade_run: craft a trade pack and carry it to a buyer",
        GoalKind::Housing => "housing: build or tend a house",
        GoalKind::Dungeon => "dungeon: join a party and clear an instance dungeon",
        GoalKind::Sail => "sail: summon a boat and sail across the water",
        GoalKind::Auction => "auction: list spare goods at the auction house",
        GoalKind::Market => "market: advertise spare goods and sell them to buyers",
        GoalKind::Guild => "guild: found, join or run a guild",
        GoalKind::Gank => "gank: ambush players carrying trade packs",
        GoalKind::Warzone => "warzone: fight for the faction in a zone at war",
        GoalKind::Event => "event: join a world event such as a rift",
        GoalKind::Companion => "companion: follow a player's party",
        GoalKind::Farm => "farm: plant and harvest crops",
        GoalKind::Rest => "rest: sit down and recover",
        GoalKind::Wander => "wander: stroll around and see the sights",
    }
}

/// What the model is told about the bot.
pub struct Situation<'a> {
    /// Who the bot is.
    pub facts: Facts<'a>,
    /// Level.
    pub level: u8,
    /// Hit points in percent.
    pub hp_pct: u8,
    /// Mana in percent.
    pub mp_pct: u8,
    /// Money in copper.
    pub money: i64,
    /// The goal running now.
    pub current: Option<GoalKind>,
    /// The last goals and whether they finished well, oldest first.
    pub recent: &'a [(GoalKind, bool)],
    /// A fair monster is near.
    pub monsters_near: bool,
    /// Other characters in view.
    pub characters_near: usize,
    /// A real player is near.
    pub real_player_near: bool,
}

/// The situation as a few short lines.
pub fn describe(s: &Situation<'_>) -> String {
    let mut out = String::with_capacity(400);
    let f = &s.facts;
    let _ = writeln!(
        out,
        "Level {} {} {}. Health {}%, mana {}%. Money: {}.",
        s.level,
        flat(f.race),
        flat(f.class),
        s.hp_pct,
        s.mp_pct,
        money_text(s.money)
    );
    if let Some(zone) = f.zone {
        let _ = writeln!(out, "Zone: {}.", flat(zone));
    }
    match s.current {
        Some(g) => {
            let _ = writeln!(out, "Doing now: {}.", g.name());
        }
        None => out.push_str("Doing nothing right now.\n"),
    }
    if !s.recent.is_empty() {
        out.push_str("Recent goals: ");
        for (i, (g, ok)) in s.recent.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(
                out,
                "{} ({})",
                g.name(),
                if *ok { "done" } else { "failed" }
            );
        }
        out.push_str(".\n");
    }
    let _ = writeln!(
        out,
        "Around: monsters to fight {}; {} other characters in view{}.",
        if s.monsters_near {
            "nearby"
        } else {
            "not nearby"
        },
        s.characters_near,
        if s.real_player_near {
            ", a real player among them"
        } else {
            ""
        }
    );
    out
}

/// The system text for the director's question (the client adds the JSON contract).
pub fn system_prompt(f: &Facts<'_>) -> String {
    format!(
        "You play {}, a {} {} in the online game ArcheAge. Temperament: {}. Choose what this \
character does next, the way a real player with this temperament would. Prefer variety over \
repeating the same activity, and look after the character's health.",
        flat(f.name),
        flat(f.race),
        flat(f.class),
        temperament_text(f.dominant)
    )
}

/// `text` on one line without control characters, at most 40 characters.
fn flat(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect()
}

fn money_text(copper: i64) -> String {
    let c = copper.max(0);
    format!("{}g {}s {}c", c / 10_000, c / 100 % 100, c % 100)
}

/// A choice from the model, mapped to a goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The goal.
    pub kind: GoalKind,
    /// The model's short reason, cut and on one line.
    pub reason: Box<str>,
}

#[derive(Debug)]
struct Inflight {
    rx: oneshot::Receiver<Result<Choice, LlmError>>,
    /// The goals in the order they were offered (the model answers with an index).
    offered: Goals,
    since_ms: u64,
}

#[derive(Debug)]
struct Ready {
    pick: Pick,
    until_ms: u64,
}

/// The director state of one bot.
#[derive(Debug)]
pub struct Director {
    next_ask_ms: u64,
    inflight: Option<Inflight>,
    ready: Option<Ready>,
    history: SmallVec<[(GoalKind, bool); HISTORY]>,
}

impl Director {
    /// A director whose first request comes at a random moment within `interval_secs`.
    pub fn new(rng: &mut fastrand::Rng, interval_secs: u64) -> Director {
        let span = interval_secs.saturating_mul(1000);
        let first = FIRST_ASK_MIN_MS.min(span);
        Director {
            next_ask_ms: first + rng.u64(0..span.saturating_sub(first).max(1)),
            inflight: None,
            ready: None,
            history: SmallVec::new(),
        }
    }

    /// Remembers how a goal ended, for the situation text.
    pub fn note_goal(&mut self, kind: GoalKind, ok: bool) {
        if self.history.len() >= HISTORY {
            self.history.remove(0);
        }
        self.history.push((kind, ok));
    }

    /// The last goals and how they ended, oldest first.
    pub fn recent(&self) -> &[(GoalKind, bool)] {
        &self.history
    }

    /// Whether a new request may be made now.
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_ask_ms && self.inflight.is_none() && self.ready.is_none()
    }

    /// Plans the next request about `interval_secs` away (a random 70..130 % of it).
    pub fn schedule(&mut self, now_ms: u64, rng: &mut fastrand::Rng, interval_secs: u64) {
        let base = interval_secs.saturating_mul(1000);
        self.next_ask_ms = now_ms + base * 7 / 10 + rng.u64(0..(base * 6 / 10).max(1));
    }

    /// Asks the model; the answer is collected by [`Director::poll`]. Must run inside a tokio
    /// runtime. Does nothing (and costs nothing) with fewer than two options.
    pub fn ask(
        &mut self,
        now_ms: u64,
        llm: Arc<Llm>,
        system: String,
        situation: String,
        offered: Goals,
    ) {
        if offered.len() < 2 || self.inflight.is_some() {
            return;
        }
        let options: Vec<String> = offered
            .iter()
            .map(|g| option_text(*g).to_string())
            .collect();
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let _ = tx.send(llm.choose(&system, &situation, &options).await);
        });
        self.inflight = Some(Inflight {
            rx,
            offered,
            since_ms: now_ms,
        });
    }

    /// Collects the answer of the request in flight: `Some` when the model picked a goal. A
    /// refused, failed or late request just ends (silently: the utility choice stands).
    pub fn poll(&mut self, now_ms: u64) -> Option<Pick> {
        let inflight = self.inflight.as_mut()?;
        let result = match inflight.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                if now_ms.saturating_sub(inflight.since_ms) > INFLIGHT_TTL_MS {
                    self.inflight = None;
                }
                return None;
            }
            Err(TryRecvError::Closed) => Err(LlmError::Transport("request task ended".into())),
        };
        let done = self.inflight.take()?;
        let choice = result.ok()?;
        let kind = *done.offered.get(choice.index)?;
        Some(Pick {
            kind,
            reason: flat(&choice.reason).chars().take(REASON_CHARS).collect(),
        })
    }

    /// Keeps a pick that cannot start right now until the next goal change.
    pub fn hold(&mut self, pick: Pick, now_ms: u64) {
        self.ready = Some(Ready {
            pick,
            until_ms: now_ms + READY_TTL_MS,
        });
    }

    /// Whether a held pick is waiting.
    pub fn has_ready(&self) -> bool {
        self.ready.is_some()
    }

    /// The held pick if it has not expired and `kind` is among `eligible`; either way it is used
    /// up.
    pub fn take_ready(&mut self, now_ms: u64, eligible: &[GoalKind]) -> Option<Pick> {
        let r = self.ready.take()?;
        (now_ms < r.until_ms && eligible.contains(&r.pick.kind)).then_some(r.pick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot::brain::{base_weights, scores, ScoreInput};
    use crate::bot::cmd::GoalMask;
    use crate::config::BehaviorCfg;
    use crate::roster::Temperament;

    fn scores_for(blocked: GoalMask) -> [f32; GoalKind::COUNT] {
        let base = base_weights(&BehaviorCfg::default(), &Temperament::NEUTRAL);
        scores(
            &base,
            &ScoreInput {
                hp_pct: 100,
                mp_pct: 100,
                monsters_near: true,
                last: None,
                blocked,
            },
        )
    }

    #[test]
    fn gm_only_goals_are_offered_only_when_gm_rights_are_on() {
        let s = scores_for(0);
        let without = eligible_goals(&s, false);
        assert!(!without.contains(&GoalKind::TradeRun));
        assert!(!without.contains(&GoalKind::Housing));
        assert!(without.contains(&GoalKind::Quest) && without.contains(&GoalKind::Wander));
        let with = eligible_goals(&s, true);
        assert!(with.contains(&GoalKind::TradeRun) && with.contains(&GoalKind::Housing));
        // (the dungeon and sail goals score themselves from the whole context, see
        // `brain::own_scores`: this raw table leaves the sail goal at 0 and has the dungeon's
        // temperament weight, which `own_scores` replaces)
        // (the auction, market, guild, gank, warzone, event, companion and farm goals do too, and
        // are 0 until their features exist)
        assert_eq!(with.len(), GoalKind::COUNT - 9);
        assert!(!with.contains(&GoalKind::Sail));
        assert!(!with.contains(&GoalKind::Auction) && !with.contains(&GoalKind::Market));
        for g in [
            GoalKind::Guild,
            GoalKind::Gank,
            GoalKind::Warzone,
            GoalKind::Event,
            GoalKind::Companion,
            GoalKind::Farm,
        ] {
            assert!(!with.contains(&g), "{g:?}");
        }
    }

    #[test]
    fn disabled_cooling_down_and_switched_off_goals_are_never_offered() {
        let blocked = (1 << GoalKind::Quest.index()) | (1 << GoalKind::Grind.index());
        let s = scores_for(blocked);
        let list = eligible_goals(&s, true);
        assert!(!list.contains(&GoalKind::Quest) && !list.contains(&GoalKind::Grind));
        // a weight of 0 in the config switches a goal off for the scoring, and so for the director
        let base = base_weights(
            &BehaviorCfg {
                duel: 0.0,
                ..BehaviorCfg::default()
            },
            &Temperament::NEUTRAL,
        );
        let s = scores(
            &base,
            &ScoreInput {
                hp_pct: 100,
                mp_pct: 100,
                monsters_near: true,
                last: None,
                blocked: 0,
            },
        );
        assert!(!eligible_goals(&s, true).contains(&GoalKind::Duel));
        assert!(eligible_goals(&[0.0; GoalKind::COUNT], true).is_empty());
    }

    #[test]
    fn the_situation_text_is_short_and_has_the_facts() {
        let recent = [(GoalKind::Quest, true), (GoalKind::Grind, false)];
        let s = Situation {
            facts: Facts {
                name: "Aria",
                race: "Nuian",
                class: "Darkrunner",
                dominant: "quester",
                zone: Some("Solzreed"),
            },
            level: 12,
            hp_pct: 87,
            mp_pct: 60,
            money: 123_456,
            current: Some(GoalKind::Wander),
            recent: &recent,
            monsters_near: true,
            characters_near: 3,
            real_player_near: false,
        };
        let text = describe(&s);
        for want in [
            "Level 12 Nuian Darkrunner",
            "Health 87%",
            "12g 34s 56c",
            "Solzreed",
            "Doing now: wander",
            "quest (done), grind (failed)",
            "3 other characters",
        ] {
            assert!(text.contains(want), "{want:?} missing in {text}");
        }
        assert!(text.len() < 500, "{} bytes", text.len());
        assert!(system_prompt(&s.facts).contains("Aria"));
    }

    #[test]
    fn history_keeps_the_last_three_goals() {
        let mut d = Director::new(&mut fastrand::Rng::with_seed(1), 600);
        for (i, g) in GoalKind::ALL.into_iter().enumerate().take(5) {
            d.note_goal(g, i % 2 == 0);
        }
        assert_eq!(d.recent().len(), HISTORY);
        assert_eq!(d.recent()[2].0, GoalKind::ALL[4]);
    }

    #[test]
    fn the_first_request_is_spread_over_the_interval() {
        let mut seen = Vec::new();
        for seed in 0..50 {
            let d = Director::new(&mut fastrand::Rng::with_seed(seed), 600);
            assert!(
                (60_000..600_000).contains(&d.next_ask_ms),
                "{}",
                d.next_ask_ms
            );
            seen.push(d.next_ask_ms);
        }
        seen.sort_unstable();
        assert!(seen[49] - seen[0] > 200_000, "the bots would ask together");
        // a tiny interval never panics
        let d = Director::new(&mut fastrand::Rng::with_seed(1), 30);
        assert!(d.next_ask_ms <= 30_000);
    }

    #[test]
    fn scheduling_jitters_around_the_interval() {
        let mut d = Director::new(&mut fastrand::Rng::with_seed(1), 600);
        let mut rng = fastrand::Rng::with_seed(2);
        for _ in 0..100 {
            d.schedule(1_000, &mut rng, 600);
            let wait = d.next_ask_ms - 1_000;
            assert!((420_000..=780_000).contains(&wait), "{wait}");
        }
    }

    #[test]
    fn a_held_pick_is_used_once_and_only_while_eligible_and_fresh() {
        let mut d = Director::new(&mut fastrand::Rng::with_seed(1), 600);
        let pick = Pick {
            kind: GoalKind::Duel,
            reason: "why not".into(),
        };
        d.hold(pick.clone(), 1_000);
        assert!(d.has_ready() && !d.due(u64::MAX));
        assert_eq!(d.take_ready(2_000, &[GoalKind::Duel]), Some(pick.clone()));
        assert!(!d.has_ready());
        d.hold(pick.clone(), 1_000);
        assert_eq!(
            d.take_ready(2_000, &[GoalKind::Quest]),
            None,
            "not eligible"
        );
        d.hold(pick.clone(), 1_000);
        assert_eq!(
            d.take_ready(1_000 + READY_TTL_MS, &[GoalKind::Duel]),
            None,
            "expired"
        );
    }
}
