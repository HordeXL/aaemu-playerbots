//! Server issue reports: the bots tell the team about problems they meet *with the server*.
//!
//! A bot plays like a player, so it notices what a player would: a quest that gives no credit, an
//! NPC that is not where the data says, a character that dies without the server saying so, a
//! connection that drops. A *detector* (code in the bot loop or in a goal that recognises such a
//! symptom) files an [`IssueReport`]; this module does everything after that, process-wide.
//!
//! ```text
//!  detector ──► Ctx::report_issue / Bot::file_issue      attach the bot's identity, position, log
//!                   │
//!                   ▼
//!             Hub::report_issue ──► Issues::report        normalize, add server log lines, count
//!                                      │                  (a mutex, no I/O, never waits)
//!                                      ▼
//!                                  IssueBook              dedup by fingerprint, policy, limits
//!                                      │
//!              ┌───────────────────────┼─────────────────────────┐
//!              ▼                       ▼                         ▼
//!        saver task              sink task                  tail task
//!        issues.json             evidence folder +          follows the server log, keeps the
//!        (debounced)             queue file entry           ring, tells bots about WARN/ERROR
//! ```
//!
//! # Filing a report (for detectors)
//!
//! ```
//! use playerbots::issues::{IssueKind, IssueReport, Severity, Suspect};
//!
//! // `key` is what makes two reports the same issue: ids, never positions or times.
//! let report = IssueReport::new(
//!     IssueKind::QuestCreditMissing,
//!     "quest=251 npc=3475",
//!     "Quest 251: killing NPC 3475 gives no credit",
//! )
//! .expected("the quest log counts the kill within a few seconds")
//! .actual("the quest log did not change within 5 s of the kill")
//! .suspect(Suspect::Server)
//! .severity(Severity::Medium)
//! .area("rust/crates/aaemu-game/src/systems/quests/") // a hint, not verified
//! .fact("quest", 251)
//! .fact("npc", 3475);
//! // In a goal:           ctx.report_issue(report);
//! // In the bot loop:     self.file_issue(report);
//! // Both attach the bot (name, level, class, position, zone, goal, detail), the time and the
//! // bot's last log lines; add the packet trail with `.trail(ring.snapshot(now_ms))` and the
//! // triggering packet with `.packet_head(&packet.body)` before filing, if you have them.
//! # let _ = report;
//! ```
//!
//! Rules of thumb for detectors: report an *episode* once, not on every tick; choose the
//! [`Suspect`] honestly (`Bot` when the bot may simply have misunderstood: it stays in the web UI and
//! never goes to the team's queue by itself); give the [`Severity::High`] only to symptoms that stop
//! the bot or break the server visibly.
//!
//! # What becomes a queue entry
//!
//! See [`book`] for the policy and the limits, [`queue`] for the entry format, [`evidence`] for the
//! folders and [`logtail`] for the server log. `docs/ISSUES.md` has the operator's
//! view.

pub mod book;
pub mod evidence;
pub mod logtail;
pub mod model;
pub mod queue;
pub mod redact;
pub mod service;
pub mod timefmt;
pub mod view;

pub use book::{Action, Counts, IssueBook, IssueRecord, IssueState, Policy};
pub use logtail::{LogLevel, ServerLog};
pub use model::{
    fingerprint, BotRef, Dir, IssueKind, IssueReport, Severity, Suspect, TrailEntry, TrailRing,
};
pub use service::{IssueStats, IssueTasks, Issues};

use crate::hub::{unix_ms, SlotInner};
use model::MAX_BOT_LOG;
use timefmt::{format_hms, local_offset_secs};

/// The identity and the recent log of the bot whose slot data is `inner`, for a report: its name,
/// level, race, class, zone, goal and detail from the published snapshot, `live_pos` (or the
/// snapshot's position), and its last [`MAX_BOT_LOG`] event log lines with their local time.
pub fn bot_context(
    inner: &SlotInner,
    char_id: u32,
    live_pos: Option<[f32; 3]>,
) -> (BotRef, Vec<String>) {
    let s = &inner.snap;
    let bot = BotRef {
        id: s.id,
        name: s.name.clone(),
        char_id,
        level: s.level,
        race: s.race.clone(),
        class: s.class.clone(),
        pos: live_pos.unwrap_or([s.x, s.y, s.z]),
        zone: s.zone.clone().unwrap_or_default(),
        goal: s.goal.clone(),
        detail: s.detail.clone(),
    };
    let offset = local_offset_secs(unix_ms());
    let log = inner
        .log
        .iter()
        .rev()
        .take(MAX_BOT_LOG)
        .rev()
        .map(|l| format!("{} {}", format_hms(l.t, offset), l.text))
        .collect();
    (bot, log)
}

/// Completes `report` with the bot side's context: the bot ([`bot_context`]) and its log unless the
/// detector set them, and the time of the observation when it is not set.
pub fn attach_context(
    mut report: IssueReport,
    inner: &SlotInner,
    char_id: u32,
    live_pos: Option<[f32; 3]>,
) -> IssueReport {
    if report.bot.is_none() || report.bot_log.is_empty() {
        let (bot, log) = bot_context(inner, char_id, live_pos);
        if report.bot.is_none() {
            report.bot = Some(bot);
        }
        if report.bot_log.is_empty() {
            report.bot_log = log;
        }
    }
    if report.at_ms == 0 {
        report.at_ms = unix_ms();
    }
    report
}

/// The report a bot files when the server logged a warning or an error that names its character
/// (`BotCmd::ServerLogLine`): kind [`IssueKind::ServerLogDuringAction`], keyed by the message with
/// its numbers and the bot's own name blanked out, so every bot and every occurrence of the same
/// message is one issue; high severity for an `ERROR`, medium for a `WARN`; the area hint comes
/// from the log line's target. Context (bot, position, log) is attached by the caller.
pub fn server_log_report(level: LogLevel, line: &str, bot_name: &str) -> IssueReport {
    let parts = logtail::split_line(line);
    let key = logtail::log_key(line, bot_name);
    let short: String = parts.message.chars().take(80).collect();
    let (severity, word) = if level == LogLevel::Error {
        (Severity::High, "an error")
    } else {
        (Severity::Medium, "a warning")
    };
    let mut report = IssueReport::new(
        IssueKind::ServerLogDuringAction,
        key,
        format!("Server logged {word} about a bot: {short}"),
    )
    .expected("the server handles the bot's actions without logging a warning or an error")
    .actual(format!("the server logged: {line}"))
    .suspect(Suspect::Server)
    .severity(severity)
    .fact("level", level.as_str());
    if let Some(target) = parts.target {
        report = report.fact("target", target);
        if let Some(area) = logtail::area_from_target(target) {
            report = report.area(area);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::BotSlot;
    use pb_web::BotSnapshot;

    fn slot() -> std::sync::Arc<BotSlot> {
        let slot = BotSlot::new(BotSnapshot {
            id: 7,
            name: "Aria".into(),
            race: "Elf".into(),
            class: "Archery".into(),
            level: 12,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            zone: Some("Solzreed".into()),
            goal: "quest".into(),
            detail: "Quest 251: kill boars".into(),
            ..BotSnapshot::default()
        });
        for i in 0..30 {
            slot.push_log(format!("event {i}"));
        }
        slot
    }

    #[test]
    fn the_bot_side_context_comes_from_the_slot() {
        let slot = slot();
        let (bot, log) = slot.with(|g| bot_context(g, 99, Some([10.0, 20.0, 30.0])));
        assert_eq!(
            (bot.id, bot.name.as_str(), bot.char_id, bot.level),
            (7, "Aria", 99, 12)
        );
        assert_eq!(
            (bot.race.as_str(), bot.class.as_str(), bot.zone.as_str()),
            ("Elf", "Archery", "Solzreed")
        );
        assert_eq!(
            (bot.goal.as_str(), bot.detail.as_str()),
            ("quest", "Quest 251: kill boars")
        );
        assert_eq!(bot.pos, [10.0, 20.0, 30.0]);
        assert_eq!(log.len(), MAX_BOT_LOG);
        assert!(log.last().unwrap().ends_with("event 29"), "{log:?}");
        assert!(log[0].ends_with("event 18"));
        // without a live position the snapshot's is used
        let (bot, _) = slot.with(|g| bot_context(g, 0, None));
        assert_eq!(bot.pos, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn a_server_log_line_becomes_one_issue_for_every_bot_and_occurrence() {
        let a =
            "14:03:09 [ERROR] aaemu_game::systems::quests::engine - Aria failed quest 251 step 3";
        let b =
            "14:09:59 [ERROR] aaemu_game::systems::quests::engine - Bron failed quest 77 step 12";
        let ra = server_log_report(LogLevel::Error, a, "Aria");
        let rb = server_log_report(LogLevel::Error, b, "Bron");
        assert_eq!(ra.kind, IssueKind::ServerLogDuringAction);
        assert_eq!(ra.fingerprint(), rb.fingerprint());
        assert_eq!((ra.severity, ra.suspect), (Severity::High, Suspect::Server));
        assert_eq!(
            ra.area.as_deref(),
            Some("rust/crates/aaemu-game/src/systems/quests/")
        );
        assert!(ra.actual.contains("failed quest 251 step 3"));
        assert!(
            ra.title
                .starts_with("Server logged an error about a bot: Aria failed quest 251"),
            "{}",
            ra.title
        );
        assert!(ra
            .facts
            .contains(&("level".to_string(), "ERROR".to_string())));
        assert!(ra.facts.contains(&(
            "target".to_string(),
            "aaemu_game::systems::quests::engine".to_string()
        )));
        // a warning is medium and is another issue than the same words as an error would be
        let w = server_log_report(
            LogLevel::Warn,
            "14:03:09 [WARN] aaemu_game::x - Aria slow",
            "Aria",
        );
        assert_eq!(w.severity, Severity::Medium);
        assert!(w.title.contains("warning"));
        // a line in an unknown format still makes a report
        let odd = server_log_report(LogLevel::Warn, "something about Aria", "Aria");
        assert!(
            odd.area.is_none() && odd.key.contains("something about *"),
            "{}",
            odd.key
        );
    }

    #[test]
    fn attaching_context_keeps_what_the_detector_set() {
        let slot = slot();
        let plain = IssueReport::new(IssueKind::Other, "k", "t");
        let r = slot.with(|g| attach_context(plain, g, 5, None));
        assert_eq!(r.bot.as_ref().unwrap().name, "Aria");
        assert_eq!(r.bot_log.len(), MAX_BOT_LOG);
        assert!(r.at_ms > 0);

        let own = IssueReport::new(IssueKind::Other, "k", "t")
            .at(42)
            .bot(BotRef {
                name: "Other".into(),
                ..BotRef::default()
            })
            .bot_log(vec!["mine".into()]);
        let r = slot.with(|g| attach_context(own, g, 5, None));
        assert_eq!(r.bot.as_ref().unwrap().name, "Other");
        assert_eq!(r.bot_log, vec!["mine".to_string()]);
        assert_eq!(r.at_ms, 42);
    }
}
