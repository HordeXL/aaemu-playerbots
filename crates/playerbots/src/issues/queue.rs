//! The operator's issue queue: an append-only text file the team's agents work from.
//!
//! The file is edited by other people and tools (download, edit, upload), so this module only ever
//! *appends* one entry at a time and only ever *reads* the file to look for a marker:
//!
//! * an entry is a header line `[YYYY-MM-DD HH:MM:SS <tz>] <Title>` and a few lines of prose, the
//!   entries are separated by one blank line, and the line breaks are CRLF;
//! * [`append_entry`] opens the file in append mode (creating it), looks at its last bytes to supply
//!   the missing line breaks so that exactly one blank line precedes the new entry, and writes the
//!   entry with a single `write`;
//! * agents delete an entry when its fix is done; [`marker_state`] reads the file (at most
//!   [`MAX_READ`] bytes) to tell whether the `pb:<hex>` marker of an entry is still there.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::book::IssueRecord;
use super::evidence::FILES;
use super::logtail::split_line;
use super::model::IssueReport;
use super::redact::one_line;
use super::timefmt::format_local;

/// The queue file is read for a marker only up to this size.
pub const MAX_READ: u64 = 4 << 20;
/// The suffix of the title in the header.
pub const TITLE_SUFFIX: &str = " (playerbots)";

/// What an entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The first entry of an issue.
    New,
    /// The issue was queued before, its entry is gone and it happened again.
    Recurrence {
        /// When the removed entry was written (unix ms).
        previous_at_ms: u64,
        /// Occurrences since then.
        new_occurrences: u64,
    },
}

/// What rendering an entry needs besides the issue.
#[derive(Debug, Clone, Copy)]
pub struct EntryCtx<'a> {
    /// The time of the entry (unix ms).
    pub now_ms: u64,
    /// Local offset from UTC in seconds.
    pub offset_secs: i32,
    /// The label after the time in the header (`EST`, `UTC-05:00`).
    pub tz: &'a str,
    /// The evidence folder; `None` when it could not be written.
    pub evidence: Option<&'a str>,
}

/// Plural-aware `1 time` / `3 times`.
fn count_of(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `text` with a full stop at the end unless it ends in `.`, `!` or `?`.
fn sentence(text: &str) -> String {
    let t = text.trim();
    if t.ends_with(['.', '!', '?']) {
        t.to_string()
    } else {
        format!("{t}.")
    }
}

/// The server log line the entry quotes: the first warning or error of the samples, else the first
/// line, and how many lines there are in all.
fn server_log_summary(rec: &IssueRecord) -> Option<(String, usize)> {
    let lines: Vec<&String> = rec.samples.iter().flat_map(|s| &s.server_log).collect();
    let first = lines
        .iter()
        .find(|l| split_line(l).level.is_notable())
        .or_else(|| lines.first())?;
    Some((one_line(first, 240), lines.len()))
}

/// The block of prose lines for `sample` (the line "Context: ...").
fn context_line(sample: Option<&IssueReport>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = sample {
        if let Some(b) = &s.bot {
            parts.push(b.describe());
        }
        if !s.facts.is_empty() {
            let facts: Vec<String> = s.facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
            parts.push(facts.join(", "));
        }
    }
    if parts.is_empty() {
        parts.push("no context was recorded".to_string());
    }
    format!("Context: {}.", parts.join("; "))
}

/// The text of one queue entry (CRLF line breaks, no line break at the end).
///
/// ```text
/// [2026-11-02 14:03:09 EST] Quest 251 gives no credit (playerbots)
/// Observed by playerbots: ... Expected: ... Seen 3 times by 2 bots between ... and ....
/// Context: bot Aria (level 12 Elf Archery) doing quest (...) at (x, y, z) in Solzreed; quest=251.
/// Likely server area: rust/crates/aaemu-game/src/systems/quests/ (hint from the bot, not verified).
/// Server log at the time: ... (2 lines in the evidence).
/// Agent follow-up: ... (suspect: server).
/// Evidence: <folder> (report.json, trail.txt, server-log.txt, bot-log.txt). Marker pb:6f3b2e1c.
/// ```
pub fn render_entry(rec: &IssueRecord, kind: EntryKind, ctx: &EntryCtx<'_>) -> String {
    let at = |ms: u64| format_local(ms, ctx.offset_secs);
    let title = one_line(&rec.title, 160);
    let latest = rec.latest_sample();
    let (actual, expected) = latest.map_or((String::new(), String::new()), |s| {
        (one_line(&s.actual, 300), one_line(&s.expected, 300))
    });
    let mut lines: Vec<String> = Vec::with_capacity(8);
    lines.push(format!(
        "[{} {}] {title}{TITLE_SUFFIX}",
        at(ctx.now_ms),
        ctx.tz
    ));

    let mut seen = format!("Seen {}", count_of(rec.count, "time", "times"));
    if rec.bots.total > 0 {
        seen.push_str(&format!(
            " by {}",
            count_of(u64::from(rec.bots.total), "bot", "bots")
        ));
    }
    seen.push_str(&format!(
        " between {} and {}.",
        at(rec.first_seen_ms),
        at(rec.last_seen_ms)
    ));
    let observed = if actual.is_empty() {
        title.clone()
    } else {
        actual
    };
    let mut prose = format!("Observed by playerbots: {}", sentence(&observed));
    if !expected.is_empty() {
        prose.push_str(&format!(" Expected: {}", sentence(&expected)));
    }
    prose.push(' ');
    prose.push_str(&seen);
    match kind {
        EntryKind::New => lines.push(prose),
        EntryKind::Recurrence {
            previous_at_ms,
            new_occurrences,
        } => lines.push(format!(
            "Recurrence after the entry was removed: the entry written on {} was deleted, yet playerbots saw this {} again since. {prose}",
            at(previous_at_ms),
            count_of(new_occurrences, "time", "times"),
        )),
    }

    lines.push(context_line(latest));

    if let Some(area) = rec.area.as_deref().filter(|a| !a.is_empty()) {
        lines.push(format!(
            "Likely server area: {} (hint from the bot, not verified).",
            one_line(area, 160).trim_end_matches('.')
        ));
    }

    lines.push(match server_log_summary(rec) {
        Some((line, n)) => format!(
            "Server log at the time: {} ({} in the evidence).",
            line.trim_end_matches('.'),
            count_of(n as u64, "line", "lines")
        ),
        None => "Server log at the time: none captured.".to_string(),
    });

    lines.push(format!(
        "Agent follow-up: reproduce from the packet trail in the evidence; verify against the server code before changing anything. This report comes from an automated client and may be a bot-side misunderstanding (suspect: {}).",
        rec.suspect.as_str()
    ));

    let files = FILES.join(", ");
    lines.push(match ctx.evidence {
        Some(folder) => format!(
            "Evidence: {} ({files}). Marker {}.",
            one_line(folder, 400),
            rec.fingerprint
        ),
        None => format!(
            "Evidence: none could be written (see the playerbots log). Marker {}.",
            rec.fingerprint
        ),
    });

    lines.join("\r\n")
}

/// The line breaks to put in front of a new entry so that exactly one blank line separates it from
/// what `tail` (the last bytes of the file) ends with: nothing for an empty file or one that
/// already ends in a blank line, `\r\n` after a single line break, `\r\n\r\n` after text.
pub fn separator_for(tail: &[u8]) -> &'static str {
    if tail.is_empty() {
        return "";
    }
    let breaks = tail
        .iter()
        .rev()
        .take_while(|b| matches!(**b, b'\n' | b'\r'))
        .filter(|b| **b == b'\n')
        .count();
    match breaks {
        0 => "\r\n\r\n",
        1 => "\r\n",
        _ => "",
    }
}

/// Appends `entry` to the queue file at `path`, creating the file when it does not exist. The
/// file is opened in append mode, its last bytes decide how many line breaks go in front, and
/// prefix, entry and a closing line break are written with one `write`. Existing content is never
/// touched.
pub fn append_entry(path: &Path, entry: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let len = file.metadata()?.len();
    let n = len.min(4) as usize;
    let mut tail = [0u8; 4];
    if n > 0 {
        file.seek(SeekFrom::End(-(n as i64)))?;
        file.read_exact(&mut tail[..n])?;
    }
    let sep = separator_for(&tail[..n]);
    let mut buf = String::with_capacity(sep.len() + entry.len() + 2);
    buf.push_str(sep);
    buf.push_str(entry);
    buf.push_str("\r\n");
    file.write_all(buf.as_bytes())?;
    file.flush()
}

/// What looking for a marker in the queue file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// The marker is in the file: the entry is still open.
    Present,
    /// The marker is not in the file (or there is no file): the entry was removed.
    Absent,
    /// The file is larger than [`MAX_READ`]; nothing can be said.
    TooLarge,
}

/// Whether the marker `pb:<hex>` occurs in `text` as a whole token.
pub fn contains_marker(text: &str, marker: &str) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(marker) {
        let end = from + i + marker.len();
        if !text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return true;
        }
        from = end;
    }
    false
}

/// Reads the queue file (up to [`MAX_READ`] bytes) and looks for `marker`.
pub fn marker_state(path: &Path, marker: &str) -> io::Result<MarkerState> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(MarkerState::Absent),
        Err(e) => return Err(e),
    };
    if len > MAX_READ {
        return Ok(MarkerState::TooLarge);
    }
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(if contains_marker(&text, marker) {
        MarkerState::Present
    } else {
        MarkerState::Absent
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::book::{IssueBook, Policy};
    use crate::issues::model::{BotRef, IssueKind, Severity, Suspect};
    use crate::testutil::temp_dir;

    /// 2026-11-02 19:03:09 UTC.
    const T: u64 = 1_793_646_189_000;

    fn record_with(server_log: Vec<String>, area: Option<&str>) -> IssueRecord {
        let mut b = IssueBook::new(Policy::default());
        let mut fp = String::new();
        for i in 0..3u32 {
            let mut r = IssueReport::new(
                IssueKind::QuestCreditMissing,
                "quest=251 npc=3475",
                "Quest 251 gives no credit for the kill",
            )
            .expected("kill credit after the boar died")
            .actual("the quest log did not change within 5 s")
            .suspect(Suspect::Server)
            .severity(Severity::Medium)
            .fact("quest", 251)
            .fact("npc", 3475)
            .bot(BotRef {
                id: i % 2 + 1,
                name: "Aria".into(),
                level: 12,
                race: "Elf".into(),
                class: "Archery".into(),
                pos: [15000.0, 14800.0, 120.0],
                zone: "Solzreed".into(),
                goal: "quest".into(),
                detail: "Quest 251: kill boars".into(),
                ..BotRef::default()
            })
            .at(T + u64::from(i) * 60_000);
            if let Some(a) = area {
                r = r.area(a);
            }
            r.server_log = server_log.clone();
            r.normalize(T);
            fp = b.record(r, T).fingerprint;
        }
        b.get(&fp).unwrap().clone()
    }

    fn ctx(evidence: Option<&str>) -> EntryCtx<'_> {
        EntryCtx {
            now_ms: T + 600_000,
            offset_secs: -5 * 3600,
            tz: "EST",
            evidence,
        }
    }

    #[test]
    fn an_entry_has_the_agreed_shape() {
        let rec = record_with(
            vec![
                "19:03:09 [WARN] aaemu_game::q - no credit for 251".into(),
                "19:03:10 [INFO] x - y".into(),
            ],
            Some("rust/crates/aaemu-game/src/systems/quests/"),
        );
        let text = render_entry(
            &rec,
            EntryKind::New,
            &ctx(Some("D:\\evidence\\20261102-140309-bot-x")),
        );
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines.len(), 7, "{text}");
        assert!(
            !text.contains('\n') || text.matches('\n').count() == text.matches("\r\n").count(),
            "CRLF only"
        );
        assert_eq!(
            lines[0],
            "[2026-11-02 14:13:09 EST] Quest 251 gives no credit for the kill (playerbots)"
        );
        assert_eq!(
            lines[1],
            "Observed by playerbots: the quest log did not change within 5 s. Expected: kill credit after the boar died. Seen 3 times by 2 bots between 2026-11-02 14:03:09 and 2026-11-02 14:05:09."
        );
        assert_eq!(
            lines[2],
            "Context: bot Aria (level 12 Elf Archery) doing quest (Quest 251: kill boars) at (15000, 14800, 120) in Solzreed; quest=251, npc=3475."
        );
        assert_eq!(
            lines[3],
            "Likely server area: rust/crates/aaemu-game/src/systems/quests/ (hint from the bot, not verified)."
        );
        assert_eq!(
            lines[4],
            "Server log at the time: 19:03:09 [WARN] aaemu_game::q - no credit for 251 (6 lines in the evidence)."
        );
        assert!(lines[5].starts_with("Agent follow-up: reproduce from the packet trail"));
        assert!(lines[5].ends_with("(suspect: server)."));
        assert_eq!(
            lines[6],
            format!(
                "Evidence: D:\\evidence\\20261102-140309-bot-x (report.json, trail.txt, server-log.txt, bot-log.txt). Marker {}.",
                rec.fingerprint
            )
        );
        // the title is on one line and the header is the first line
        assert!(text.starts_with('['));
    }

    #[test]
    fn missing_parts_leave_their_lines_out_or_say_so() {
        let rec = record_with(vec![], None);
        let text = render_entry(&rec, EntryKind::New, &ctx(None));
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines.len(), 6, "no area line: {text}");
        assert_eq!(lines[3], "Server log at the time: none captured.");
        assert!(lines[5].starts_with("Evidence: none could be written"));
        assert!(lines[5].ends_with(&format!("Marker {}.", rec.fingerprint)));
        // an issue with no expected text, no bot and no facts
        let mut bare = rec.clone();
        for s in &mut bare.samples {
            s.expected.clear();
            s.bot = None;
            s.facts.clear();
        }
        bare.bots.total = 0;
        let lines: Vec<String> = render_entry(&bare, EntryKind::New, &ctx(None))
            .split("\r\n")
            .map(String::from)
            .collect();
        assert!(!lines[1].contains("Expected:"), "{}", lines[1]);
        assert!(!lines[1].contains("Seen 3 times by"), "{}", lines[1]);
        assert_eq!(lines[2], "Context: no context was recorded.");
    }

    #[test]
    fn the_single_occurrence_reads_naturally_and_the_time_label_can_be_numeric() {
        let mut b = IssueBook::new(Policy::default());
        let r = IssueReport::new(IssueKind::Kicked, "k", "Kicked by the server")
            .actual("kicked with reason 3!")
            .severity(Severity::High)
            .bot(BotRef {
                id: 1,
                name: "Aria".into(),
                ..BotRef::default()
            })
            .at(T);
        let fp = b.record(r, T).fingerprint;
        let rec = b.get(&fp).unwrap();
        let c = EntryCtx {
            tz: "UTC-05:00",
            ..ctx(None)
        };
        let text = render_entry(rec, EntryKind::New, &c);
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert!(lines[0]
            .starts_with("[2026-11-02 14:13:09 UTC-05:00] Kicked by the server (playerbots)"));
        assert!(
            lines[1].starts_with(
                "Observed by playerbots: kicked with reason 3! Seen 1 time by 1 bot between"
            ),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_recurrence_entry_says_so_and_keeps_the_format() {
        let rec = record_with(vec![], Some("rust/crates/aaemu-game/src/systems/quests/"));
        let text = render_entry(
            &rec,
            EntryKind::Recurrence {
                previous_at_ms: T - 86_400_000,
                new_occurrences: 4,
            },
            &ctx(Some("ev")),
        );
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines.len(), 7);
        assert!(lines[0].ends_with("(playerbots)"));
        assert!(
            lines[1].starts_with("Recurrence after the entry was removed: the entry written on 2026-11-01 14:03:09 was deleted, yet playerbots saw this 4 times again since. Observed by playerbots:"),
            "{}",
            lines[1]
        );
        assert!(lines[6].contains(&rec.fingerprint));
    }

    #[test]
    fn nothing_in_an_entry_can_break_the_line_structure_or_leak() {
        let mut rec = record_with(vec![], None);
        rec.title = "Evil\r\n[2020-01-01 00:00:00 EST] fake entry".into();
        rec.samples[2].actual = "line\nbreak and 198.51.100.9 token=abc".into();
        rec.area = Some("a\r\nb".into());
        let text = render_entry(&rec, EntryKind::New, &ctx(None));
        // exactly the lines of an entry (the area line is there now), no stray breaks
        assert_eq!(text.split("\r\n").count(), 7, "{text}");
        assert!(!text.replace("\r\n", "").contains(['\r', '\n']));
        assert!(
            !text.contains("198.51.100.9") && !text.contains("token=abc"),
            "{text}"
        );
        assert!(text.split("\r\n").nth(1).unwrap().contains("x.x.x.x"));
    }

    #[test]
    fn the_separator_gives_exactly_one_blank_line() {
        assert_eq!(separator_for(b""), "");
        assert_eq!(separator_for(b"text"), "\r\n\r\n");
        assert_eq!(separator_for(b"text\r\n"), "\r\n");
        assert_eq!(separator_for(b"text\n"), "\r\n");
        assert_eq!(separator_for(b"ext\r\n\r\n"), "");
        assert_eq!(separator_for(b"\r\n\r\n\r\n"), "");
        assert_eq!(separator_for(b"a\n\n"), "");
        assert_eq!(separator_for(b"\r"), "\r\n\r\n");
        assert_eq!(separator_for(b" \r\n"), "\r\n");
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    #[test]
    fn appending_creates_the_file_and_separates_entries_by_one_blank_line() {
        let dir = temp_dir("queue-append");
        let path = dir.join("issues.txt");
        append_entry(&path, "[t] first\r\nbody one").unwrap();
        assert_eq!(read(&path), b"[t] first\r\nbody one\r\n");
        append_entry(&path, "[t] second\r\nbody two").unwrap();
        append_entry(&path, "[t] third").unwrap();
        assert_eq!(
            String::from_utf8(read(&path)).unwrap(),
            "[t] first\r\nbody one\r\n\r\n[t] second\r\nbody two\r\n\r\n[t] third\r\n"
        );
    }

    #[test]
    fn appending_to_a_file_that_ends_without_a_newline_adds_the_missing_breaks() {
        let dir = temp_dir("queue-nonl");
        let path = dir.join("issues.txt");
        std::fs::write(&path, b"[old] entry\r\nlast line without a break").unwrap();
        append_entry(&path, "[new] entry").unwrap();
        assert_eq!(
            String::from_utf8(read(&path)).unwrap(),
            "[old] entry\r\nlast line without a break\r\n\r\n[new] entry\r\n"
        );
        // a file that ends with one CRLF gets one more, one that ends with a blank line none
        std::fs::write(&path, b"[old]\r\ntext\r\n").unwrap();
        append_entry(&path, "[a]").unwrap();
        std::fs::write(&path, b"[old]\r\ntext\r\n\r\n").unwrap();
        append_entry(&path, "[b]").unwrap();
        assert_eq!(
            String::from_utf8(read(&path)).unwrap(),
            "[old]\r\ntext\r\n\r\n[b]\r\n"
        );
    }

    #[test]
    fn appending_never_rewrites_what_is_there() {
        let dir = temp_dir("queue-keep");
        let path = dir.join("issues.txt");
        let original: Vec<u8> = (0..3000u32)
            .flat_map(|i| {
                format!("[2026-01-01 00:00:00 EST] entry {i} \u{e9}\r\nbody\r\n\r\n").into_bytes()
            })
            .collect();
        std::fs::write(&path, &original).unwrap();
        append_entry(&path, "[new] x").unwrap();
        let after = read(&path);
        assert!(
            after.starts_with(&original),
            "the existing content is byte for byte the same"
        );
        assert_eq!(&after[original.len()..], b"[new] x\r\n");
        // a file with a UTF-8 byte order mark or invalid bytes is no obstacle either
        std::fs::write(&path, b"\xef\xbb\xbf[bom] first\r\n\xff\xfe").unwrap();
        append_entry(&path, "[n]").unwrap();
        let after = read(&path);
        assert!(after.starts_with(b"\xef\xbb\xbf[bom] first\r\n\xff\xfe"));
        assert!(after.ends_with(b"\r\n\r\n[n]\r\n"));
        // an empty existing file gets no leading breaks
        std::fs::write(&path, b"").unwrap();
        append_entry(&path, "[e]").unwrap();
        assert_eq!(read(&path), b"[e]\r\n");
    }

    #[test]
    fn appending_to_a_directory_or_an_unwritable_path_is_an_error_not_a_panic() {
        let dir = temp_dir("queue-bad");
        assert!(append_entry(&dir, "x").is_err());
        assert!(append_entry(&dir.join("no-such-dir").join("q.txt"), "x").is_err());
    }

    #[test]
    fn markers_are_found_as_whole_tokens_and_a_missing_file_has_none() {
        let dir = temp_dir("queue-marker");
        let path = dir.join("issues.txt");
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::Absent
        );
        std::fs::write(&path, "[t] x\r\nEvidence: e. Marker pb:6f3b2e1c.\r\n").unwrap();
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::Present
        );
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1d").unwrap(),
            MarkerState::Absent
        );
        // a longer hex string is another marker
        std::fs::write(&path, "Marker pb:6f3b2e1c0.").unwrap();
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::Absent
        );
        std::fs::write(&path, "Marker pb:6f3b2e1c0 and pb:6f3b2e1c").unwrap();
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::Present
        );
        // invalid UTF-8 elsewhere does not matter
        std::fs::write(&path, b"\xff\xfe stuff pb:6f3b2e1c\r\n").unwrap();
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::Present
        );
        // too big to read
        let big = vec![b'x'; (MAX_READ + 1) as usize];
        std::fs::write(&path, big).unwrap();
        assert_eq!(
            marker_state(&path, "pb:6f3b2e1c").unwrap(),
            MarkerState::TooLarge
        );
    }
}
