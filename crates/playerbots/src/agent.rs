//! The agent API: how an LLM (or any program) steers the bots more finely than "pick a goal"
//! (owned by the LLM and agent feature).
//!
//! This file is the hook the hub calls. [`act_command`] turns the web UI's `act` command
//! ([`pb_web::ActArgs`]) into a [`BotCmd::GoalWith`]: the goal runs once with its arguments, then the
//! bot chooses again. A zone is accepted by key only until the feature resolves names.

use crate::bot::cmd::{BotCmd, GoalArgs, GoalKind};

/// The bot command for an `act` (the web layer checked the values).
pub fn act_command(a: &pb_web::ActArgs) -> Result<BotCmd, String> {
    let kind = GoalKind::parse(&a.goal).ok_or_else(|| format!("unknown goal {:?}", a.goal))?;
    let zone = match a.zone.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(z) => Some(
            z.parse::<u16>()
                .map_err(|_| format!("unknown zone {z:?} (give the zone key)"))?,
        ),
    };
    let args = GoalArgs {
        dest: a.x.zip(a.y).map(|(x, y)| [x, y]),
        zone,
        item: a.item,
        target: a
            .target
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(Into::into),
        wild: a.wild,
        why: a
            .why
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| format!("agent: {t}").into()),
    };
    Ok(BotCmd::GoalWith {
        kind,
        args: Box::new(args),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_act_becomes_a_goal_with_its_arguments() {
        let a = pb_web::ActArgs {
            goal: "explore".into(),
            x: Some(10.0),
            y: Some(20.0),
            zone: Some(" 12 ".into()),
            why: Some("new sights".into()),
            ..pb_web::ActArgs::default()
        };
        let BotCmd::GoalWith { kind, args } = act_command(&a).unwrap() else {
            panic!()
        };
        assert_eq!(kind, GoalKind::Explore);
        assert_eq!(args.dest, Some([10.0, 20.0]));
        assert_eq!(args.zone, Some(12));
        assert_eq!(args.why.as_deref(), Some("agent: new sights"));
        assert!(act_command(&pb_web::ActArgs {
            goal: "fly".into(),
            ..pb_web::ActArgs::default()
        })
        .is_err());
        assert!(act_command(&pb_web::ActArgs {
            goal: "explore".into(),
            zone: Some("Nowhere".into()),
            ..pb_web::ActArgs::default()
        })
        .is_err());
    }
}
