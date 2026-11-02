//! Commands a bot task receives on its command channel (from the web UI through the hub, and
//! from the coordinator).

/// The goals a bot can pursue, as the web UI names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GoalKind {
    /// Walk around.
    Wander,
    /// Stand still and recover.
    Rest,
    /// Kill monsters.
    Grind,
    /// Do quests.
    Quest,
    /// Duel another bot.
    Duel,
    /// Trade with another bot.
    Trade,
    /// Craft, carry and sell trade packs.
    TradeRun,
    /// Build and use a house.
    Housing,
}

impl GoalKind {
    /// Every goal.
    pub const ALL: [GoalKind; 8] = [
        GoalKind::Wander,
        GoalKind::Rest,
        GoalKind::Grind,
        GoalKind::Quest,
        GoalKind::Duel,
        GoalKind::Trade,
        GoalKind::TradeRun,
        GoalKind::Housing,
    ];

    /// Number of goals.
    pub const COUNT: usize = Self::ALL.len();

    /// Dense index for per-goal arrays.
    pub fn index(self) -> usize {
        self as usize
    }

    /// The short name the web UI shows (`pb_web::Command::GOALS` spelling).
    pub fn name(self) -> &'static str {
        match self {
            GoalKind::Wander => "wander",
            GoalKind::Rest => "rest",
            GoalKind::Grind => "grind",
            GoalKind::Quest => "quest",
            GoalKind::Duel => "duel",
            GoalKind::Trade => "trade",
            GoalKind::TradeRun => "trade_run",
            GoalKind::Housing => "housing",
        }
    }

    /// The goal called `name` (a [`pb_web::Command::GOALS`] entry other than `auto`, or `wander`).
    pub fn parse(name: &str) -> Option<GoalKind> {
        GoalKind::ALL.into_iter().find(|g| g.name() == name)
    }
}

/// What the operator chose with `Command::Goal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalChoice {
    /// The bot chooses (utility scoring).
    Auto,
    /// Keep doing this goal.
    Forced(GoalKind),
}

impl GoalChoice {
    /// Parses a `Command::Goal` value (`auto` or a goal name).
    pub fn parse(name: &str) -> Option<GoalChoice> {
        if name == "auto" {
            Some(GoalChoice::Auto)
        } else {
            GoalKind::parse(name).map(GoalChoice::Forced)
        }
    }
}

/// What a coordinator-paired bot is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairKind {
    /// A duel.
    Duel,
    /// A player trade.
    Trade,
}

/// Which side of a pairing a bot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRole {
    /// Sends the challenge / trade request.
    Initiator,
    /// Waits for it and accepts.
    Responder,
}

/// The coordinator's order to both bots of a pair.
#[derive(Debug, Clone, PartialEq)]
pub struct PairOrder {
    /// What the pair does.
    pub kind: PairKind,
    /// This bot's side.
    pub role: PairRole,
    /// Bot id of the partner.
    pub partner_bot: u32,
    /// Character id of the partner (0 when not known yet).
    pub partner_char: u32,
    /// Character name of the partner.
    pub partner_name: Box<str>,
    /// Where the two meet (x, y).
    pub meet_at: (f32, f32),
}

/// A message to one bot task.
#[derive(Debug, Clone, PartialEq)]
pub enum BotCmd {
    /// `Command::Goal`.
    Goal(GoalChoice),
    /// `Command::Pause`: stop acting (keep the connection alive).
    Pause,
    /// `Command::Resume`.
    Resume,
    /// `Command::Say`: a line of local chat.
    Say(String),
    /// `Command::DuelWith`: duel the bot with this id.
    DuelWith(u32),
    /// `Command::TradeWith`: trade with the bot with this id.
    TradeWith(u32),
    /// The coordinator found a partner.
    Pair(PairOrder),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_names_round_trip_and_match_the_web_ui() {
        for g in GoalKind::ALL {
            assert_eq!(GoalKind::parse(g.name()), Some(g));
            assert_eq!(GoalKind::ALL[g.index()], g);
        }
        // every goal the web UI can force (except "auto") is a goal here
        for name in pb_web::Command::GOALS {
            match GoalChoice::parse(name) {
                Some(GoalChoice::Auto) => assert_eq!(name, "auto"),
                Some(GoalChoice::Forced(g)) => assert_eq!(g.name(), name),
                None => panic!("the web UI offers {name:?}, which the bots do not know"),
            }
        }
        assert_eq!(GoalChoice::parse("fly"), None);
    }
}
