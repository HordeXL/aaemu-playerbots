//! Evidence folders: what an agent needs to reproduce a reported issue.
//!
//! For a reportable issue the sink writes `<evidence_dir>/<YYYYMMDD-HHMMSS>-bot-<slug>/` with
//!
//! * `report.json` - the issue, its counts and all kept samples;
//! * `trail.txt` - the packets before each sample, readable;
//! * `server-log.txt` - the server log lines correlated with each sample;
//! * `bot-log.txt` - the bot's own last log lines at each sample.
//!
//! The folder is rewritten (same name) when later samples arrive. Nothing in it can hold a password
//! (no field of a report does) and every text went through [`super::redact::scrub`].

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::book::{BotSet, IssueRecord};
use super::model::{IssueReport, Severity, Suspect};
use super::redact::scrub;
use super::timefmt::{format_local, format_stamp};

/// The files of an evidence folder, in the order the queue entry lists them.
pub const FILES: [&str; 4] = ["report.json", "trail.txt", "server-log.txt", "bot-log.txt"];

/// Longest slug in a folder name, in characters.
const SLUG_CHARS: usize = 40;

/// The name of a new evidence folder for `rec`: `<YYYYMMDD-HHMMSS>-bot-<slug>`, the stamp in the
/// local time at `offset_secs`. The slug is the kind and the key reduced to `[a-z0-9_-]`, then the
/// fingerprint's hex, so two issues never share a folder.
pub fn folder_name(rec: &IssueRecord, now_ms: u64, offset_secs: i32) -> String {
    format!("{}-bot-{}", format_stamp(now_ms, offset_secs), slug(rec))
}

fn slug(rec: &IssueRecord) -> String {
    let mut s = String::new();
    for c in format!("{} {}", rec.kind.as_str(), rec.key).chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
        if s.len() >= SLUG_CHARS {
            break;
        }
    }
    let s = s.trim_matches('-');
    let hex = rec
        .fingerprint
        .strip_prefix("pb:")
        .unwrap_or(&rec.fingerprint);
    format!("{s}-{hex}")
}

/// What `report.json` holds.
#[derive(Serialize)]
struct ReportFile<'a> {
    fingerprint: &'a str,
    kind: &'a str,
    title: &'a str,
    suspect: Suspect,
    severity: Severity,
    area: Option<&'a str>,
    state: &'a str,
    count: u64,
    bots: &'a BotSet,
    first_seen_ms: u64,
    last_seen_ms: u64,
    first_seen: String,
    last_seen: String,
    written_at: String,
    samples: &'a [IssueReport],
}

/// The text of `report.json`.
pub fn report_json(rec: &IssueRecord, now_ms: u64, offset_secs: i32) -> String {
    let file = ReportFile {
        fingerprint: &rec.fingerprint,
        kind: rec.kind.as_str(),
        title: &rec.title,
        suspect: rec.suspect,
        severity: rec.severity,
        area: rec.area.as_deref(),
        state: rec.state.name(),
        count: rec.count,
        bots: &rec.bots,
        first_seen_ms: rec.first_seen_ms,
        last_seen_ms: rec.last_seen_ms,
        first_seen: format_local(rec.first_seen_ms, offset_secs),
        last_seen: format_local(rec.last_seen_ms, offset_secs),
        written_at: format_local(now_ms, offset_secs),
        samples: &rec.samples,
    };
    scrub(&serde_json::to_string_pretty(&file).unwrap_or_default())
}

/// `== sample 2 of 3, 2026-11-02 14:03:09, bot Aria (id 4) ==`
fn sample_header(i: usize, of: usize, s: &IssueReport, offset_secs: i32) -> String {
    let who = s
        .bot
        .as_ref()
        .map_or(String::new(), |b| format!(", bot {} (id {})", b.name, b.id));
    format!(
        "== sample {} of {of}, {}{who} ==",
        i + 1,
        format_local(s.at_ms, offset_secs)
    )
}

fn per_sample(
    rec: &IssueRecord,
    offset_secs: i32,
    empty: &str,
    lines: impl Fn(&IssueReport) -> Vec<String>,
) -> String {
    let mut out = format!(
        "{} {} - {}\n",
        rec.fingerprint,
        rec.kind.as_str(),
        rec.title
    );
    for (i, s) in rec.samples.iter().enumerate() {
        out.push('\n');
        out.push_str(&sample_header(i, rec.samples.len(), s, offset_secs));
        out.push('\n');
        let l = lines(s);
        if l.is_empty() {
            out.push_str(empty);
            out.push('\n');
        }
        for line in l {
            out.push_str(&line);
            out.push('\n');
        }
    }
    scrub(&out)
}

/// The lines of one sample in `trail.txt`.
pub fn trail_lines(s: &IssueReport) -> Vec<String> {
    let mut l: Vec<String> = s.trail.iter().map(|t| t.render()).collect();
    if let Some(h) = &s.packet_head_hex {
        l.push(format!("packet head ({} bytes): {h}", h.len() / 2));
    }
    l
}

/// The text of `trail.txt`.
pub fn trail_txt(rec: &IssueRecord, offset_secs: i32) -> String {
    per_sample(
        rec,
        offset_secs,
        "(no packet trail was attached)",
        trail_lines,
    )
}

/// The text of `server-log.txt`.
pub fn server_log_txt(rec: &IssueRecord, offset_secs: i32) -> String {
    per_sample(
        rec,
        offset_secs,
        "(no server log lines were captured)",
        |s| s.server_log.clone(),
    )
}

/// The text of `bot-log.txt`.
pub fn bot_log_txt(rec: &IssueRecord, offset_secs: i32) -> String {
    per_sample(rec, offset_secs, "(no bot log lines were attached)", |s| {
        s.bot_log.clone()
    })
}

/// Writes (or rewrites) the evidence folder of `rec` under `dir` and returns its path. A folder
/// the record already has under `dir` is reused, so a refresh keeps the name.
pub fn write(dir: &Path, rec: &IssueRecord, now_ms: u64, offset_secs: i32) -> io::Result<PathBuf> {
    let folder = match rec.evidence.as_deref().map(PathBuf::from) {
        Some(p) if p.starts_with(dir) => p,
        _ => dir.join(folder_name(rec, now_ms, offset_secs)),
    };
    std::fs::create_dir_all(&folder)?;
    let files = [
        report_json(rec, now_ms, offset_secs),
        trail_txt(rec, offset_secs),
        server_log_txt(rec, offset_secs),
        bot_log_txt(rec, offset_secs),
    ];
    for (name, text) in FILES.iter().zip(files) {
        std::fs::write(folder.join(name), text)?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::book::{IssueBook, Policy};
    use crate::issues::model::{BotRef, Dir, IssueKind, TrailEntry};
    use crate::testutil::temp_dir;

    /// 2026-11-02 19:03:09 UTC.
    const T: u64 = 1_793_646_189_000;

    fn record() -> IssueRecord {
        let mut b = IssueBook::new(Policy::default());
        let mut fp = String::new();
        for i in 0..2u32 {
            let mut r = IssueReport::new(
                IssueKind::QuestCreditMissing,
                "quest=251 npc=3475",
                "Quest 251 gives no credit",
            )
            .expected("credit")
            .actual("nothing from 203.0.113.7")
            .suspect(Suspect::Server)
            .fact("quest", 251)
            .bot(BotRef {
                id: i + 1,
                name: format!("Aria{i}"),
                ..BotRef::default()
            })
            .trail(vec![
                TrailEntry::new(900, Dir::C2S, 1, 0x2c, "CSChangeTargetPacket"),
                TrailEntry::new(10, Dir::S2C, 1, 0x123, "SCFooPacket"),
            ])
            .packet_head(&[1, 2, 3])
            .bot_log(vec!["09:00:00 killed a boar".into()])
            .at(T + u64::from(i) * 1000);
            r.server_log = if i == 1 {
                vec!["19:03:10 [WARN] aaemu_game::q - no credit".into()]
            } else {
                vec![]
            };
            r.normalize(T);
            fp = b.record(r, T).fingerprint;
        }
        b.get(&fp).unwrap().clone()
    }

    #[test]
    fn the_folder_name_has_a_stamp_a_slug_and_the_hex() {
        let rec = record();
        let name = folder_name(&rec, T, -5 * 3600);
        assert!(
            name.starts_with("20261102-140309-bot-quest_credit_missing-quest-251-npc-3475-"),
            "{name}"
        );
        assert!(name.ends_with(&rec.fingerprint[3..]), "{name}");
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        // a long, odd key stays short and safe
        let mut odd = rec.clone();
        odd.key = "Quest: <1>/..\\ \"x\" ".repeat(20);
        let n = folder_name(&odd, T, 0);
        assert!(n.len() < 100, "{n}");
        assert!(
            n.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{n}"
        );
    }

    #[test]
    fn the_four_files_are_written_and_hold_the_samples() {
        let dir = temp_dir("evidence-write");
        let rec = record();
        let folder = write(&dir, &rec, T, -5 * 3600).unwrap();
        assert!(folder.starts_with(&*dir));
        for f in FILES {
            assert!(folder.join(f).is_file(), "{f}");
        }
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(json["fingerprint"], rec.fingerprint);
        assert_eq!(json["kind"], "quest_credit_missing");
        assert_eq!(json["count"], 2);
        assert_eq!(json["samples"].as_array().unwrap().len(), 2);
        assert_eq!(json["suspect"], "server");
        assert_eq!(json["first_seen"], "2026-11-02 14:03:09");
        let trail = std::fs::read_to_string(folder.join("trail.txt")).unwrap();
        assert!(
            trail.contains("== sample 1 of 2, 2026-11-02 14:03:09, bot Aria0 (id 1) =="),
            "{trail}"
        );
        assert!(trail.contains("S2C L1 0x0123 SCFooPacket"));
        assert!(trail.contains("packet head (3 bytes): 010203"));
        let log = std::fs::read_to_string(folder.join("server-log.txt")).unwrap();
        assert!(
            log.contains("(no server log lines were captured)"),
            "the first sample had none"
        );
        assert!(log.contains("no credit"));
        let bot = std::fs::read_to_string(folder.join("bot-log.txt")).unwrap();
        assert_eq!(bot.matches("killed a boar").count(), 2);
    }

    #[test]
    fn nothing_private_gets_written() {
        let dir = temp_dir("evidence-redact");
        let mut rec = record();
        rec.samples[0].server_log =
            vec!["login from 198.51.100.4 password=hunter2 ok, local 127.0.0.1".into()];
        rec.samples[0].actual = "x".into();
        let folder = write(&dir, &rec, T, 0).unwrap();
        for f in FILES {
            let text = std::fs::read_to_string(folder.join(f)).unwrap();
            assert!(
                !text.contains("198.51.100.4") && !text.contains("203.0.113.7"),
                "{f}"
            );
            assert!(!text.contains("hunter2"), "{f}");
        }
        let log = std::fs::read_to_string(folder.join("server-log.txt")).unwrap();
        assert!(
            log.contains("x.x.x.x")
                && log.contains("127.0.0.1")
                && log.contains("password=<redacted>"),
            "{log}"
        );
    }

    #[test]
    fn a_refresh_reuses_the_folder() {
        let dir = temp_dir("evidence-refresh");
        let mut rec = record();
        let first = write(&dir, &rec, T, 0).unwrap();
        rec.evidence = Some(first.to_string_lossy().into_owned());
        rec.count = 9;
        let second = write(&dir, &rec, T + 3_600_000, 0).unwrap();
        assert_eq!(first, second);
        let json = std::fs::read_to_string(second.join("report.json")).unwrap();
        assert!(json.contains("\"count\": 9"));
        // a folder outside the configured directory is not reused
        let other = temp_dir("evidence-refresh-other");
        let third = write(&other, &rec, T, 0).unwrap();
        assert!(third.starts_with(&*other));
        assert_eq!(std::fs::read_dir(&*dir).unwrap().count(), 1);
    }
}
