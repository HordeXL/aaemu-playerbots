//! The reports a dungeon run files when the server does not do what the data and the protocol
//! promise (see [`crate::issues`]). Pure builders: the goal says what it saw, this makes the
//! [`IssueReport`] with a stable key (the same problem always has the same key, whichever bot
//! meets it) and the server area to look at.

use super::entry::EntryError;
use crate::issues::{IssueKind, IssueReport, Severity, Suspect};

/// The server area every dungeon problem points at.
const AREA_INSTANCES: &str = "rust/crates/aaemu-game/src/systems/instances/";

/// The dungeon a report is about.
#[derive(Debug, Clone, Copy)]
pub struct Where<'a> {
    /// English name.
    pub name: &'a str,
    /// Zone group (what the server counts entries by).
    pub zone_group: u32,
}

/// What a run saw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Problem<'a> {
    /// The entrance cast was accepted but neither `SCProcessingInstance` nor `SCLoadInstance`
    /// came.
    NoLoad {
        /// Entrance skill.
        skill: u32,
        /// Entrance doodad template.
        doodad: u32,
        /// Seconds since the first accepted cast.
        waited_s: u64,
    },
    /// The server loaded another world than the dungeon's.
    WrongWorld {
        /// Zone key the dungeon has.
        want_zone_key: u16,
        /// Zone key the load carried.
        got_zone_key: u32,
        /// Name of the world the load matches, if it matches one.
        got_world: Option<&'a str>,
    },
    /// The server refused every cast of the entrance skill.
    CastRefused {
        /// Entrance skill.
        skill: u32,
        /// `SkillResult` of the last refusal.
        result: u8,
    },
    /// The entrance doodad never showed.
    NoEntrance {
        /// Entrance doodad template.
        doodad: u32,
    },
    /// A boss the spawn data places never showed at its stop.
    BossMissing {
        /// Boss template.
        template: u32,
    },
    /// Not a single hostile unit was in view although the spawn data lists many near the route.
    NoMonsters {
        /// Hostile spawns the data lists in the world.
        listed: usize,
        /// NPC units of any kind the bot sees.
        seen_npcs: usize,
        /// Characters (the party) the bot sees.
        seen_characters: usize,
        /// Seconds since the bot arrived.
        waited_s: u64,
    },
    /// A monster stands far above or below the height of its spawn point.
    MonsterHeight {
        /// NPC template of the monster seen.
        template: u32,
        /// Height of its spawn point (metres, rounded).
        spawn_z: i32,
        /// Height it stands at (metres, rounded).
        seen_z: i32,
    },
    /// The exit doodad was cast on and nothing happened.
    ExitStuck {
        /// Exit doodad template.
        doodad: u32,
        /// Exit skill.
        skill: u32,
    },
    /// The bot was moved out of the dungeon without asking.
    Expelled {
        /// Seconds the bot had been inside.
        inside_s: u64,
    },
    /// An error message that is no known refusal of an entry.
    ErrorCode {
        /// `ErrorMessageType`.
        code: i16,
    },
    /// A typed refusal of the entry.
    Refused(EntryError),
}

/// Who is probably at fault for a refusal the bot's own checks should have prevented.
fn refusal_suspect(e: EntryError) -> Suspect {
    match e {
        // the bot checked level, party and entries before it went in
        EntryError::Level
        | EntryError::NeedParty
        | EntryError::LeaveParty
        | EntryError::VisitLimit
        | EntryError::ReqItem => Suspect::Bot,
        EntryError::Full | EntryError::TryLater => Suspect::Unknown,
        EntryError::NoResource | EntryError::Prohibited => Suspect::Server,
    }
}

/// The report for `problem` in dungeon `at`.
pub fn build(at: Where<'_>, problem: Problem<'_>) -> IssueReport {
    let zg = at.zone_group;
    let name = at.name;
    let base = |kind: IssueKind, tag: String, title: String| {
        IssueReport::new(kind, format!("dungeon={zg} {tag}"), title)
            .fact("dungeon", name)
            .fact("zone_group", zg)
            .area(AREA_INSTANCES)
    };
    match problem {
        Problem::NoLoad {
            skill,
            doodad,
            waited_s,
        } => base(
            IssueKind::NoResponse,
            "enter-no-load".into(),
            format!(
                "{name}: the entrance cast was accepted but the server never loaded the dungeon"
            ),
        )
        .expected("SCProcessingInstance or SCLoadInstance within 30 s of the accepted cast")
        .actual(format!(
            "nothing for {waited_s} s after casting skill {skill} on the entrance doodad {doodad}"
        ))
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .fact("skill", skill)
        .fact("doodad", doodad),
        Problem::WrongWorld {
            want_zone_key,
            got_zone_key,
            got_world,
        } => base(
            IssueKind::Other,
            format!("wrong-world got={got_zone_key}"),
            format!("{name}: the server loaded another world than the dungeon's"),
        )
        .expected(format!("a load of zone key {want_zone_key}"))
        .actual(match got_world {
            Some(w) => format!("a load of zone key {got_zone_key} ({w})"),
            None => format!("a load of zone key {got_zone_key}, a world the data does not know"),
        })
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .fact("want_zone_key", want_zone_key)
        .fact("got_zone_key", got_zone_key),
        Problem::CastRefused { skill, result } => base(
            IssueKind::SkillRefusedUnexpectedly,
            format!("entry-cast skill={skill}"),
            format!("{name}: the server refuses the entrance skill"),
        )
        .expected("the cast on the entrance doodad starts")
        .actual(format!("skill {skill} was refused with result {result}"))
        .suspect(Suspect::Unknown)
        .fact("skill", skill)
        .fact("result", result),
        Problem::NoEntrance { doodad } => base(
            IssueKind::DoodadMissing,
            format!("entrance doodad={doodad}"),
            format!("{name}: the entrance doodad {doodad} is not there"),
        )
        .expected("the entrance doodad in view at the place the data gives")
        .actual("the doodad never came into view")
        .suspect(Suspect::Data)
        .fact("doodad", doodad),
        Problem::BossMissing { template } => base(
            IssueKind::NpcMissing,
            format!("boss={template}"),
            format!("{name}: the boss {template} did not spawn"),
        )
        .expected("the boss at its spawn point once the party arrived")
        .actual("no such unit came into view within 10 s")
        .suspect(Suspect::Data)
        .fact("npc", template),
        Problem::NoMonsters {
            listed,
            seen_npcs,
            seen_characters,
            waited_s,
        } => base(
            IssueKind::NpcMissing,
            "no-monsters".into(),
            format!("{name}: no monster at all in view inside the dungeon"),
        )
        .expected(format!("some of the {listed} hostile spawns of the data"))
        .actual(format!(
            "after {waited_s} s inside: {seen_npcs} NPC units and {seen_characters} characters in view, not one hostile"
        ))
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .fact("listed", listed)
        .fact("seen_npcs", seen_npcs)
        .fact("seen_characters", seen_characters),
        Problem::MonsterHeight {
            template,
            spawn_z,
            seen_z,
        } => base(
            IssueKind::NpcMissing,
            "monster-height".into(),
            format!("{name}: the monsters stand far from the height of their spawn points"),
        )
        .expected(format!(
            "monsters on the floor of their spawn points (npc {template}: spawn height {spawn_z} m)"
        ))
        .actual(format!(
            "npc {template} stands at {seen_z} m, {} m {} its spawn point; the party cannot reach it",
            (seen_z - spawn_z).abs(),
            if seen_z < spawn_z { "below" } else { "above" }
        ))
        .suspect(Suspect::Server)
        .fact("npc", template)
        .fact("spawn_z", spawn_z)
        .fact("seen_z", seen_z),
        Problem::ExitStuck { doodad, skill } => base(
            IssueKind::NoResponse,
            format!("exit doodad={doodad}"),
            format!("{name}: the exit doodad {doodad} does nothing"),
        )
        .expected("SCLoadInstance back to the main world after the exit skill")
        .actual("nothing happened; the bot left the team and waited to be moved out")
        .suspect(Suspect::Server)
        .severity(Severity::High)
        .fact("doodad", doodad)
        .fact("skill", skill),
        Problem::Expelled { inside_s } => base(
            IssueKind::Kicked,
            "expelled".into(),
            format!("{name}: the bot was moved out of the dungeon without asking"),
        )
        .expected("to stay inside until the party leaves")
        .actual(format!("back in the main world after {inside_s} s"))
        .suspect(Suspect::Unknown)
        .fact("inside_s", inside_s),
        Problem::ErrorCode { code } => base(
            IssueKind::Other,
            format!("error={code}"),
            format!("{name}: the server answered the entry with error {code}"),
        )
        .expected("a load or one of the known refusals")
        .actual(format!("SCErrorMsg {code}"))
        .suspect(Suspect::Unknown)
        .severity(Severity::Low)
        .fact("code", code),
        Problem::Refused(e) => base(
            IssueKind::Other,
            format!("refused={}", e.word()),
            format!("{name}: the server refused the entry ({})", e.word()),
        )
        .expected("the entry to be accepted: the bot checked level, party and entries first")
        .actual(format!("refusal: {}", e.word()))
        .suspect(refusal_suspect(e))
        .severity(Severity::Low),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Where<'static> = Where {
        name: "Cutting Wind Deadmine",
        zone_group: 50,
    };

    #[test]
    fn keys_name_the_problem_and_not_the_occurrence() {
        let a = build(
            AT,
            Problem::NoLoad {
                skill: 19836,
                doodad: 5075,
                waited_s: 31,
            },
        );
        let b = build(
            AT,
            Problem::NoLoad {
                skill: 19836,
                doodad: 5075,
                waited_s: 44,
            },
        );
        assert_eq!(a.key, b.key);
        assert_eq!(a.key, "dungeon=50 enter-no-load");
        assert_eq!(a.kind, IssueKind::NoResponse);
        assert_eq!(a.suspect, Suspect::Server);
        assert!(a.area.as_deref().unwrap().contains("instances"));
        // another dungeon, another key
        let other = build(
            Where {
                name: "Sal Temple",
                zone_group: 47,
            },
            Problem::NoLoad {
                skill: 17743,
                doodad: 4288,
                waited_s: 31,
            },
        );
        assert_ne!(a.key, other.key);
    }

    #[test]
    fn each_finding_has_a_kind_and_a_suspect() {
        let cases = [
            (
                Problem::WrongWorld {
                    want_zone_key: 262,
                    got_zone_key: 7,
                    got_world: Some("instance_x"),
                },
                IssueKind::Other,
                Suspect::Server,
                "dungeon=50 wrong-world got=7",
            ),
            (
                Problem::BossMissing { template: 12188 },
                IssueKind::NpcMissing,
                Suspect::Data,
                "dungeon=50 boss=12188",
            ),
            (
                Problem::MonsterHeight {
                    template: 11358,
                    spawn_z: 167,
                    seen_z: 110,
                },
                IssueKind::NpcMissing,
                Suspect::Server,
                "dungeon=50 monster-height",
            ),
            (
                Problem::NoMonsters {
                    listed: 300,
                    seen_npcs: 1,
                    seen_characters: 2,
                    waited_s: 30,
                },
                IssueKind::NpcMissing,
                Suspect::Server,
                "dungeon=50 no-monsters",
            ),
            (
                Problem::ExitStuck {
                    doodad: 5723,
                    skill: 19754,
                },
                IssueKind::NoResponse,
                Suspect::Server,
                "dungeon=50 exit doodad=5723",
            ),
            (
                Problem::Expelled { inside_s: 120 },
                IssueKind::Kicked,
                Suspect::Unknown,
                "dungeon=50 expelled",
            ),
            (
                Problem::ErrorCode { code: 999 },
                IssueKind::Other,
                Suspect::Unknown,
                "dungeon=50 error=999",
            ),
            (
                Problem::CastRefused {
                    skill: 19836,
                    result: 15,
                },
                IssueKind::SkillRefusedUnexpectedly,
                Suspect::Unknown,
                "dungeon=50 entry-cast skill=19836",
            ),
            (
                Problem::NoEntrance { doodad: 5075 },
                IssueKind::DoodadMissing,
                Suspect::Data,
                "dungeon=50 entrance doodad=5075",
            ),
        ];
        for (problem, kind, suspect, key) in cases {
            let r = build(AT, problem);
            assert_eq!((r.kind, r.suspect, r.key.as_str()), (kind, suspect, key));
            assert!(r.title.starts_with("Cutting Wind Deadmine"), "{}", r.title);
            assert!(!r.expected.is_empty() && !r.actual.is_empty());
        }
    }

    #[test]
    fn refusals_the_bot_should_have_prevented_are_blamed_on_the_bot() {
        for e in [
            EntryError::Level,
            EntryError::NeedParty,
            EntryError::LeaveParty,
            EntryError::VisitLimit,
        ] {
            let r = build(AT, Problem::Refused(e));
            assert_eq!(r.suspect, Suspect::Bot, "{e:?}");
            assert_eq!(r.key, format!("dungeon=50 refused={}", e.word()));
        }
        assert_eq!(
            build(AT, Problem::Refused(EntryError::NoResource)).suspect,
            Suspect::Server
        );
    }
}
