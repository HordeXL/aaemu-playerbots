//! The reports the guild features file when the server does not do what the protocol promises
//! (see [`crate::issues`]). Pure builders: the duty says what it saw, this makes the
//! [`IssueReport`] with a stable key (the same problem has the same key whichever bot meets it)
//! and the server area to look at.

use aa_proto::guild::err;

use super::found::Fail;
use crate::issues::{IssueKind, IssueReport, Severity, Suspect};

/// The server area every guild problem points at.
const AREA: &str = "rust/crates/aaemu-game/src/systems/social/expedition.rs";

/// What the guild features saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A bot answered "join" to a valid invitation and was not in the guild 15 s later.
    JoinIgnored {
        /// Seconds waited.
        waited_s: u64,
    },
    /// `CSCreateExpedition` of a full, valid team got neither the guild nor an error message.
    CreateNoAnswer {
        /// The name sent.
        name: String,
    },
    /// The server refused the founding for a reason the bots' own checks should have excluded.
    CreateRefused {
        /// `ErrorMessageType`.
        code: i16,
    },
    /// `CSLeaveExpedition` changed nothing.
    LeaveIgnored,
    /// `CSChangeExpeditionOwner` by the owner to a member changed nothing.
    OwnerChangeIgnored,
    /// `CSChangeExpeditionMemberRole` by the owner changed nothing.
    PromoteIgnored {
        /// The role asked for.
        role: u8,
    },
}

/// The report for `problem`.
pub fn build(problem: &Problem) -> IssueReport {
    let base = |kind: IssueKind, tag: &str, title: &str| {
        IssueReport::new(kind, format!("guild {tag}"), title).area(AREA)
    };
    match problem {
        Problem::JoinIgnored { waited_s } => base(
            IssueKind::NoResponse,
            "join-ignored",
            "a guild invitation was accepted but the character never joined",
        )
        .expected("SCUnitExpeditionChanged and the member list within 15 s of CSReplyExpeditionInvitation")
        .actual(format!("nothing for {waited_s} s after the reply (join = true)"))
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
        Problem::CreateNoAnswer { name } => base(
            IssueKind::NoResponse,
            "create-no-answer",
            "CSCreateExpedition of a valid team got no answer",
        )
        .expected("SCFactionCreated for the founder, or an error message")
        .actual("neither came within 10 s")
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .fact("name", name),
        Problem::CreateRefused { code } => base(
            IssueKind::Other,
            &format!("create-refused code={code}"),
            "the server refused a guild the bots' own checks found valid",
        )
        .expected("a team of five of one faction, level 5 or more, unguilded, and 10000 copper")
        .actual(format!("error message {code} ({})", refusal_name(*code)))
        .suspect(Suspect::Unknown)
        .severity(Severity::Low)
        .fact("code", code),
        Problem::LeaveIgnored => base(
            IssueKind::NoResponse,
            "leave-ignored",
            "CSLeaveExpedition changed nothing",
        )
        .expected("SCUnitExpeditionChanged with no guild within 8 s")
        .actual("the character is still in the guild")
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
        Problem::OwnerChangeIgnored => base(
            IssueKind::NoResponse,
            "owner-change-ignored",
            "CSChangeExpeditionOwner by the owner to a member changed nothing",
        )
        .expected("SCExpeditionOwnerChanged within 8 s")
        .actual("the guild still has the same owner")
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
        Problem::PromoteIgnored { role } => base(
            IssueKind::NoResponse,
            "promote-ignored",
            "CSChangeExpeditionMemberRole by the owner changed nothing",
        )
        .expected("SCExpeditionRoleChanged within 10 s")
        .actual("the member's role did not change")
        .suspect(Suspect::Server)
        .severity(Severity::Low)
        .fact("role", role),
    }
}

/// The report for a failed founding, if the failure is worth one.
pub fn of_failure(fail: Fail, name: Option<&str>) -> Option<IssueReport> {
    match fail {
        Fail::NoAnswer => Some(build(&Problem::CreateNoAnswer {
            name: name.unwrap_or_default().to_string(),
        })),
        Fail::Refused(code) if fail.is_bot_mistake() => {
            Some(build(&Problem::CreateRefused { code }))
        }
        _ => None,
    }
}

fn refusal_name(code: i16) -> &'static str {
    match code {
        err::CREATE_LEVEL => "a team member is below level 5",
        err::CREATE_MEMBER => "the team is too small",
        err::CREATE_MONEY => "not enough money",
        err::NAME_CHARACTER => "the name has characters the pattern does not allow",
        err::NAME_LENGTH => "the name is too long",
        err::NAME_EXISTS => "the name exists",
        err::CREATE_FACTION => "a team member is of another mother faction",
        err::CREATE_MEMBER_GUILDED => "a team member is in a guild",
        err::ALREADY_MEMBER => "the founder is in a guild",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_has_a_stable_key_a_server_area_and_an_expectation() {
        let all = [
            Problem::JoinIgnored { waited_s: 15 },
            Problem::CreateNoAnswer { name: "A B".into() },
            Problem::CreateRefused { code: 330 },
            Problem::LeaveIgnored,
            Problem::OwnerChangeIgnored,
            Problem::PromoteIgnored { role: 2 },
        ];
        let mut keys = Vec::new();
        for p in &all {
            let r = build(p);
            assert!(r.key.starts_with("guild "), "{}", r.key);
            assert_eq!(r.area.as_deref(), Some(AREA));
            assert!(!r.title.is_empty() && !r.expected.is_empty() && !r.actual.is_empty());
            keys.push(r.key);
        }
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), all.len(), "the keys differ");
        // the key does not hold anything that changes from one occurrence to the next
        assert_eq!(
            build(&Problem::JoinIgnored { waited_s: 15 }).key,
            build(&Problem::JoinIgnored { waited_s: 99 }).key
        );
        assert_eq!(
            build(&Problem::CreateNoAnswer { name: "X".into() }).key,
            build(&Problem::CreateNoAnswer { name: "Y".into() }).key
        );
    }

    #[test]
    fn only_some_failures_are_worth_a_report() {
        assert!(of_failure(Fail::NoAnswer, Some("N")).is_some());
        assert!(of_failure(Fail::Refused(err::CREATE_MEMBER_GUILDED), None).is_some());
        assert!(of_failure(Fail::Refused(err::CREATE_FACTION), None).is_some());
        assert!(of_failure(Fail::Refused(err::CREATE_MEMBER), None).is_none());
        assert!(of_failure(Fail::NoMoney, None).is_none());
        assert!(of_failure(Fail::Declined, None).is_none());
        assert!(of_failure(Fail::Timeout, None).is_none());
    }
}
