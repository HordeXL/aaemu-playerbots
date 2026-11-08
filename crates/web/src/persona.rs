//! A bot's personality in the web API (owned by the personality feature): the view in the bot
//! detail and the `profile` command that changes it.
//!
//! ```json
//! {"type": "profile", "chatty": 0.9, "crude": 0.2, "chronotype": "night_owl"}
//! ```

use serde::{Deserialize, Serialize};

/// The chronotypes a profile may have, as the roster names them.
pub const CHRONOTYPES: [&str; 4] = ["early_bird", "daytime", "evening", "night_owl"];

/// The personality of a bot (each trait 0..=1).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ProfileView {
    pub chatty: f32,
    pub crude: f32,
    pub joker: f32,
    pub explorer: f32,
    pub outlaw: f32,
    pub gearhead: f32,
    pub dedication: f32,
    /// One of [`CHRONOTYPES`].
    pub chronotype: String,
    /// A few words: the traits that stand out.
    pub summary: String,
}

/// What `Command::Profile` changes; a trait left out keeps its value.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ProfilePatch {
    pub chatty: Option<f32>,
    pub crude: Option<f32>,
    pub joker: Option<f32>,
    pub explorer: Option<f32>,
    pub outlaw: Option<f32>,
    pub gearhead: Option<f32>,
    pub dedication: Option<f32>,
    /// One of [`CHRONOTYPES`].
    pub chronotype: Option<String>,
}

impl ProfilePatch {
    /// Cheap checks the web layer applies before the hub sees the command.
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [
            ("chatty", self.chatty),
            ("crude", self.crude),
            ("joker", self.joker),
            ("explorer", self.explorer),
            ("outlaw", self.outlaw),
            ("gearhead", self.gearhead),
            ("dedication", self.dedication),
        ] {
            if let Some(v) = v {
                if !(v.is_finite() && (0.0..=1.0).contains(&v)) {
                    return Err(format!("{name} must be in 0..=1"));
                }
            }
        }
        if let Some(c) = &self.chronotype {
            if !CHRONOTYPES.contains(&c.as_str()) {
                return Err(format!(
                    "chronotype must be one of {}",
                    CHRONOTYPES.join(", ")
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Command;

    #[test]
    fn profile_json_shape_and_checks() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"profile","chatty":0.9,"chronotype":"night_owl"}"#)
                .unwrap();
        let Command::Profile(p) = &cmd else {
            panic!("{cmd:?}")
        };
        assert_eq!(p.chatty, Some(0.9));
        assert_eq!(p.crude, None);
        assert!(p.validate().is_ok());
        for bad in [
            ProfilePatch {
                crude: Some(1.5),
                ..ProfilePatch::default()
            },
            ProfilePatch {
                joker: Some(f32::NAN),
                ..ProfilePatch::default()
            },
            ProfilePatch {
                chronotype: Some("vampire".into()),
                ..ProfilePatch::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
