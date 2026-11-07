//! The vocabulary of the `act` command: start a goal once, with arguments (owned by the LLM and
//! agent feature). It is what an LLM agent outside the process, the operator or the in-process LLM
//! director uses to steer a bot more finely than "pick a goal":
//!
//! ```json
//! {"type": "act", "goal": "explore", "zone": "Solzreed Peninsula", "why": "never been there"}
//! {"type": "act", "goal": "wander", "x": 11380.0, "y": 11900.0}
//! {"type": "act", "goal": "farm", "wild": true}
//! ```
//!
//! The bot runs the goal once and then chooses again as before; an operator's forced goal comes
//! back after it.

use serde::{Deserialize, Serialize};

use crate::types::Command;

/// Longest zone name, character name or reason, in characters.
pub const MAX_ARG_CHARS: usize = 80;

/// What `Command::Act` asks a bot to do. Every field but `goal` is optional; a goal reads only the
/// ones it understands.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ActArgs {
    /// The goal, a [`Command::GOALS`] name other than `auto`.
    pub goal: String,
    /// A point (x and y together, world metres): where to walk, explore or work.
    pub x: Option<f32>,
    pub y: Option<f32>,
    /// A zone by name (as the World view and the bot detail show it) or by zone key.
    pub zone: Option<String>,
    /// An item template the goal is about (a seed, a piece of gear, goods).
    pub item: Option<u32>,
    /// A character the goal is about (a duel partner, a player to follow, a buyer).
    pub target: Option<String>,
    /// Farm on unprotected land (a wild, "illegal" farm).
    pub wild: bool,
    /// Why, for the bot's log.
    pub why: Option<String>,
}

impl ActArgs {
    /// Cheap checks the web layer applies before the hub sees the command.
    pub fn validate(&self) -> Result<(), String> {
        let goal = self.goal.as_str();
        if goal == "auto" || !Command::GOALS.contains(&goal) {
            return Err(format!(
                "act needs a goal: one of {} except auto",
                Command::GOALS.join(", ")
            ));
        }
        match (self.x, self.y) {
            (None, None) => {}
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {}
            _ => return Err("x and y go together and must be numbers".into()),
        }
        for (name, text) in [
            ("zone", self.zone.as_deref()),
            ("target", self.target.as_deref()),
            ("why", self.why.as_deref()),
        ] {
            if let Some(t) = text {
                if t.chars().count() > MAX_ARG_CHARS {
                    return Err(format!("{name} is longer than {MAX_ARG_CHARS} characters"));
                }
                if t.chars().any(char::is_control) {
                    return Err(format!("{name} contains control characters"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_json_shape_and_checks() {
        let cmd: Command = serde_json::from_str(
            r#"{"type":"act","goal":"explore","zone":"Solzreed","why":"new sights"}"#,
        )
        .unwrap();
        let Command::Act(a) = &cmd else {
            panic!("{cmd:?}")
        };
        assert_eq!(a.goal, "explore");
        assert_eq!(a.zone.as_deref(), Some("Solzreed"));
        assert!(a.validate().is_ok());
        for bad in [
            r#"{"type":"act"}"#,
            r#"{"type":"act","goal":"auto"}"#,
            r#"{"type":"act","goal":"fly"}"#,
            r#"{"type":"act","goal":"wander","x":1.0}"#,
        ] {
            let Command::Act(a) = serde_json::from_str::<Command>(bad).unwrap() else {
                panic!("{bad}")
            };
            assert!(a.validate().is_err(), "{bad}");
        }
        let long = ActArgs {
            goal: "quest".into(),
            why: Some("x".repeat(MAX_ARG_CHARS + 1)),
            ..ActArgs::default()
        };
        assert!(long.validate().is_err());
    }
}
