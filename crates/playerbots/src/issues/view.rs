//! The issue book as the web UI sees it: [`pb_web::IssueSummary`] and [`pb_web::IssueDetail`].

use super::book::{Action, IssueRecord, IssueState};
use super::evidence::trail_lines;
use super::model::IssueReport;

/// One row of the issue list.
pub fn summary(rec: &IssueRecord, reportable: bool) -> pb_web::IssueSummary {
    pb_web::IssueSummary {
        fingerprint: rec.fingerprint.clone(),
        kind: rec.kind.as_str().to_string(),
        title: rec.title.clone(),
        suspect: rec.suspect.as_str().to_string(),
        severity: rec.severity.as_str().to_string(),
        count: rec.count,
        bots: rec.bots.total,
        first_seen: rec.first_seen_ms,
        last_seen: rec.last_seen_ms,
        state: rec.state.name().to_string(),
        reportable,
    }
}

fn sample(s: &IssueReport) -> pb_web::IssueSample {
    pb_web::IssueSample {
        at: s.at_ms,
        bot: s.bot.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
        expected: s.expected.clone(),
        actual: s.actual.clone(),
        context: s.bot.as_ref().map(|b| b.describe()).unwrap_or_default(),
        facts: s.facts.clone(),
        trail: trail_lines(s),
        server_log: s.server_log.clone(),
        bot_log: s.bot_log.clone(),
    }
}

/// The detail of one issue; `entry` is the rendered queue entry.
pub fn detail(rec: &IssueRecord, reportable: bool, entry: String) -> pb_web::IssueDetail {
    pb_web::IssueDetail {
        summary: summary(rec, reportable),
        entry,
        area: rec.area.clone(),
        evidence: rec.evidence.clone(),
        queued_at: match rec.state {
            IssueState::Queued { at_ms, .. } => Some(at_ms),
            _ => None,
        },
        samples: rec.samples.iter().map(sample).collect(),
    }
}

/// The web layer's action as the book's.
pub fn action(a: pb_web::IssueAction) -> Action {
    match a {
        pb_web::IssueAction::Queue => Action::Queue,
        pb_web::IssueAction::Ignore => Action::Ignore,
        pb_web::IssueAction::Resolve => Action::Resolve,
        pb_web::IssueAction::Reopen => Action::Reopen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::book::{IssueBook, Policy};
    use crate::issues::model::{BotRef, Dir, IssueKind, Severity, Suspect, TrailEntry};

    #[test]
    fn a_record_becomes_a_summary_and_a_detail() {
        let mut b = IssueBook::new(Policy::default());
        let r = IssueReport::new(IssueKind::Kicked, "k", "Kicked")
            .expected("stay")
            .actual("kicked")
            .suspect(Suspect::Server)
            .severity(Severity::High)
            .fact("reason", 3)
            .bot(BotRef {
                id: 2,
                name: "Aria".into(),
                level: 5,
                ..BotRef::default()
            })
            .trail(vec![TrailEntry::new(5, Dir::S2C, 1, 1, "SCx")])
            .at(1_000);
        let fp = b.record(r, 1_000).fingerprint;
        b.mark_queued(&fp, 2_000);
        let rec = b.get(&fp).unwrap();
        let s = summary(rec, true);
        assert_eq!(
            (
                s.kind.as_str(),
                s.suspect.as_str(),
                s.severity.as_str(),
                s.state.as_str()
            ),
            ("kicked", "server", "high", "queued")
        );
        assert_eq!(
            (s.count, s.bots, s.first_seen, s.last_seen),
            (1, 1, 1_000, 1_000)
        );
        assert!(s.reportable);
        let d = detail(rec, true, "entry".into());
        assert_eq!(d.queued_at, Some(2_000));
        assert_eq!(d.entry, "entry");
        assert_eq!(d.samples.len(), 1);
        assert_eq!(d.samples[0].bot, "Aria");
        assert_eq!(d.samples[0].context, "bot Aria (level 5)");
        assert_eq!(
            d.samples[0].trail,
            vec!["-     5 ms  S2C L1 0x0001 SCx".to_string()]
        );
        assert_eq!(action(pb_web::IssueAction::Resolve), Action::Resolve);
    }
}
