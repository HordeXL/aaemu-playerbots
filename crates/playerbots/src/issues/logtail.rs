//! Following the game server's log file, to put server warnings next to the bots' problems.
//!
//! Two parts, both cheap:
//!
//! * [`Follower`] reads the bytes appended to the file since the last poll (it starts at the end of
//!   the file, notices truncation and rotation, and never returns half a line);
//! * [`ServerLog`] keeps the newest lines in a bounded ring ([`RING_LINES`] lines, [`RING_BYTES`]
//!   bytes) with the instant each was logged, and answers [`ServerLog::lines_near`].
//!
//! [`run`] is the one background task: it polls the follower about once a second, feeds the ring and
//! calls a hook for every new `WARN` / `ERROR` line (the hub uses it to tell a bot that the server
//! complained about it).
//!
//! The server's lines look like `HH:MM:SS [LEVEL] target - message`, possibly with colour codes; other
//! prefixes (ISO timestamps, no level) are tolerated: the instant of a line is its time of day when
//! that is within two minutes of the moment the line was read, else the moment it was read.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::model::{MAX_LOG_LINE_CHARS, MAX_SERVER_LOG};
use super::redact::one_line;
use super::timefmt::{local_offset_secs, nearest_time_of_day};

/// Lines the ring keeps.
pub const RING_LINES: usize = 4000;
/// Bytes the ring keeps (the sum of the line lengths).
pub const RING_BYTES: usize = 1 << 20;
/// Longest stored line, in bytes (the rest is cut).
pub const MAX_LINE_BYTES: usize = 2048;
/// How often the file is polled.
pub const POLL_EVERY: Duration = Duration::from_secs(1);
/// Most bytes one poll reads.
const MAX_READ: u64 = 1 << 20;
/// Behind by more than this: jump ahead (the ring could not hold it anyway).
const SKIP_OVER: u64 = 4 << 20;
/// An unterminated line longer than this is taken as a line.
const MAX_PARTIAL: usize = 64 * 1024;
/// A logged time of day is trusted when it is this close to the moment the line was read.
const TRUST_MS: u64 = 120_000;

/// The level of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    /// `ERROR`.
    Error,
    /// `WARN`.
    Warn,
    /// `INFO`.
    Info,
    /// `DEBUG`.
    Debug,
    /// `TRACE`.
    Trace,
    /// No level found (a continuation line, another format).
    Other,
}

impl LogLevel {
    /// `ERROR`, `WARN`, ...
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
            LogLevel::Other => "-",
        }
    }

    /// Warnings and errors, the lines worth correlating.
    pub fn is_notable(self) -> bool {
        matches!(self, LogLevel::Error | LogLevel::Warn)
    }
}

/// The parts of a log line the issue reports need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts<'a> {
    /// The level.
    pub level: LogLevel,
    /// Seconds since midnight of the logged time of day, if the line has one.
    pub hms: Option<u32>,
    /// The module path the line came from (`aaemu_game::systems::quests::engine`).
    pub target: Option<&'a str>,
    /// The message.
    pub message: &'a str,
}

/// Splits a (colour-free) log line into [`Parts`]. Never fails: a line it does not understand is
/// `Other` with the whole line as its message.
pub fn split_line(line: &str) -> Parts<'_> {
    let b = line.as_bytes();
    let head = &b[..b.len().min(64)];
    let hms = time_of_day(head);
    let Some((level, end)) = find_level(head) else {
        return Parts {
            level: LogLevel::Other,
            hms,
            target: None,
            message: line.trim(),
        };
    };
    let rest = line[end.min(line.len())..].trim_start_matches([']', ' ', '\t']);
    let window = rest.get(..rest.len().min(100)).unwrap_or(rest);
    if let Some(i) = window.find(" - ") {
        let target = rest[..i].trim();
        if !target.contains(' ') {
            return Parts {
                level,
                hms,
                target: (!target.is_empty()).then_some(target),
                message: rest[i + 3..].trim(),
            };
        }
    }
    if let Some((first, after)) = rest.split_once(' ') {
        if let Some(target) = first.strip_suffix(':') {
            if !target.is_empty() {
                return Parts {
                    level,
                    hms,
                    target: Some(target),
                    message: after.trim(),
                };
            }
        }
    }
    Parts {
        level,
        hms,
        target: None,
        message: rest.trim(),
    }
}

/// `hh:mm:ss` somewhere in the first bytes of a line, as seconds since midnight.
fn time_of_day(head: &[u8]) -> Option<u32> {
    let d = |i: usize| {
        head.get(i)
            .filter(|b| b.is_ascii_digit())
            .map(|b| u32::from(b - b'0'))
    };
    for i in 0..head.len().saturating_sub(7) {
        if i > 0 && (head[i - 1].is_ascii_digit() || head[i - 1] == b':') {
            continue;
        }
        let (Some(h1), Some(h2), Some(m1), Some(m2), Some(s1), Some(s2)) =
            (d(i), d(i + 1), d(i + 3), d(i + 4), d(i + 6), d(i + 7))
        else {
            continue;
        };
        if head[i + 2] != b':' || head[i + 5] != b':' {
            continue;
        }
        let (h, m, s) = (h1 * 10 + h2, m1 * 10 + m2, s1 * 10 + s2);
        if h < 24 && m < 60 && s < 61 {
            return Some(h * 3600 + m * 60 + s.min(59));
        }
    }
    None
}

/// The first level word in `head` and the offset just after it.
fn find_level(head: &[u8]) -> Option<(LogLevel, usize)> {
    let mut i = 0;
    while i < head.len() {
        if !head[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < head.len() && head[i].is_ascii_alphabetic() {
            i += 1;
        }
        let word = &head[start..i];
        let level = match word {
            b"ERROR" => Some(LogLevel::Error),
            b"WARN" | b"WARNING" => Some(LogLevel::Warn),
            b"INFO" => Some(LogLevel::Info),
            b"DEBUG" => Some(LogLevel::Debug),
            b"TRACE" => Some(LogLevel::Trace),
            // (lower case only in brackets: `[warn]`, or after `level=`)
            _ if start > 0 && (head[start - 1] == b'[' || head[..start].ends_with(b"level=")) => {
                match word.to_ascii_lowercase().as_slice() {
                    b"error" => Some(LogLevel::Error),
                    b"warn" | b"warning" => Some(LogLevel::Warn),
                    b"info" => Some(LogLevel::Info),
                    b"debug" => Some(LogLevel::Debug),
                    b"trace" => Some(LogLevel::Trace),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(l) = level {
            return Some((l, i));
        }
    }
    None
}

/// The level and time of day of a line (see [`split_line`]).
pub fn parse_prefix(line: &str) -> (LogLevel, Option<u32>) {
    let p = split_line(line);
    (p.level, p.hms)
}

/// A line without colour codes, as a string: `ESC [ ... final` sequences (and lone escapes) are
/// removed, a trailing `\r` too, invalid UTF-8 is replaced, and the line is cut at
/// [`MAX_LINE_BYTES`].
pub fn decode_line(raw: &[u8]) -> String {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let text = if raw.contains(&0x1b) {
        let mut clean = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            if raw[i] != 0x1b {
                clean.push(raw[i]);
                i += 1;
                continue;
            }
            i += 1;
            if raw.get(i) == Some(&b'[') {
                i += 1;
                // parameters and intermediates, then the final byte 0x40..=0x7e
                while i < raw.len() && !(0x40..=0x7e).contains(&raw[i]) {
                    i += 1;
                }
                i += 1;
            }
        }
        String::from_utf8_lossy(&clean).into_owned()
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    if text.len() <= MAX_LINE_BYTES {
        return text;
    }
    let mut cut = MAX_LINE_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

/// Whether `line` mentions the character `name` as a whole word (ASCII case-insensitive).
pub fn mentions(line: &str, name: &str) -> bool {
    let (l, n) = (line.as_bytes(), name.as_bytes());
    if n.is_empty() || n.len() > l.len() {
        return false;
    }
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    (0..=l.len() - n.len()).any(|i| {
        l[i..i + n.len()].eq_ignore_ascii_case(n)
            && (i == 0 || !word(l[i - 1]))
            && (i + n.len() == l.len() || !word(l[i + n.len()]))
    })
}

/// The words of `line` (runs of letters, digits and `_`), for matching character names.
pub fn words(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
}

/// A key that is the same for every bot and every occurrence of the same server message: the
/// message in lower case with digit runs turned into `#`, the bot's own name into `*`, blanks
/// collapsed, at most 100 characters.
pub fn log_key(line: &str, own_name: &str) -> String {
    let message = split_line(line).message;
    let mut out = String::with_capacity(message.len().min(120));
    let mut last_blank = true;
    let mut i = 0;
    while i < message.len() && out.len() < 100 {
        let rest = &message[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if !own_name.is_empty()
            && rest.len() >= own_name.len()
            && rest.is_char_boundary(own_name.len())
            && rest[..own_name.len()].eq_ignore_ascii_case(own_name)
        {
            out.push('*');
            i += own_name.len();
            last_blank = false;
        } else if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
            i += 1;
            last_blank = false;
        } else if c.is_whitespace() {
            if !last_blank {
                out.push(' ');
            }
            last_blank = true;
            i += c.len_utf8();
        } else {
            out.extend(c.to_lowercase());
            last_blank = false;
            i += c.len_utf8();
        }
    }
    out.trim_end().to_string()
}

/// A hint at the source directory from a log target: `aaemu_game::systems::quests::engine` is
/// `rust/crates/aaemu-game/src/systems/quests/` (the last segment is taken for a module file).
/// `None` for targets that are not one of the server's crates.
pub fn area_from_target(target: &str) -> Option<String> {
    let mut parts: Vec<&str> = target.split("::").collect();
    let krate = parts.first().copied().filter(|c| c.starts_with("aaemu_"))?;
    if !krate
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let mut path = format!("rust/crates/{}/src/", krate.replace('_', "-"));
    parts.remove(0);
    if parts.len() > 1 {
        parts.pop();
    }
    for p in parts {
        if !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            break;
        }
        path.push_str(p);
        path.push('/');
    }
    Some(path)
}

/// One stored line.
#[derive(Debug, Clone)]
struct LogEntry {
    at_ms: u64,
    level: LogLevel,
    text: Box<str>,
}

#[derive(Debug, Default)]
struct Ring {
    lines: VecDeque<LogEntry>,
    bytes: usize,
}

impl Ring {
    fn push(&mut self, e: LogEntry) {
        self.bytes += e.text.len();
        self.lines.push_back(e);
        while self.lines.len() > RING_LINES || self.bytes > RING_BYTES {
            match self.lines.pop_front() {
                Some(old) => self.bytes -= old.text.len(),
                None => break,
            }
        }
    }
}

/// The newest lines of the server log.
#[derive(Debug, Default)]
pub struct ServerLog {
    ring: Mutex<Ring>,
}

impl ServerLog {
    /// An empty log.
    pub fn new() -> ServerLog {
        ServerLog::default()
    }

    /// Number of lines held.
    pub fn len(&self) -> usize {
        self.ring.lock().lines.len()
    }

    /// Whether no line is held.
    pub fn is_empty(&self) -> bool {
        self.ring.lock().lines.is_empty()
    }

    /// Bytes held (the sum of the line lengths).
    pub fn bytes(&self) -> usize {
        self.ring.lock().bytes
    }

    /// Adds the complete lines of `chunk` (read at `arrival_ms` on a machine at `offset_secs` from
    /// UTC) and returns the new `WARN` / `ERROR` lines.
    pub fn ingest(
        &self,
        chunk: &[u8],
        arrival_ms: u64,
        offset_secs: i32,
    ) -> Vec<(LogLevel, String)> {
        let mut notable = Vec::new();
        let mut entries = Vec::new();
        for raw in chunk.split(|b| *b == b'\n') {
            let text = decode_line(raw);
            if text.trim().is_empty() {
                continue;
            }
            let (level, hms) = parse_prefix(&text);
            let at_ms = match hms {
                Some(h) => {
                    let t = nearest_time_of_day(arrival_ms, offset_secs, h);
                    if t.abs_diff(arrival_ms) <= TRUST_MS {
                        t
                    } else {
                        arrival_ms
                    }
                }
                None => arrival_ms,
            };
            if level.is_notable() {
                notable.push((level, text.clone()));
            }
            entries.push(LogEntry {
                at_ms,
                level,
                text: text.into_boxed_str(),
            });
        }
        let mut ring = self.ring.lock();
        for e in entries {
            ring.push(e);
        }
        notable
    }

    /// The `WARN` / `ERROR` lines logged within `window_secs` of `at_ms`, plus the lines of that
    /// window that mention the character `mention`, oldest first, at most [`MAX_SERVER_LOG`] (the
    /// ones closest to `at_ms` when there are more). Addresses and credentials are removed.
    pub fn lines_near(&self, at_ms: u64, window_secs: u64, mention: Option<&str>) -> Vec<String> {
        let window_ms = window_secs.saturating_mul(1000);
        let mention = mention.map(str::trim).filter(|m| !m.is_empty());
        let ring = self.ring.lock();
        let mut hits: Vec<(u64, &LogEntry)> = Vec::new();
        for e in &ring.lines {
            if e.at_ms.abs_diff(at_ms) > window_ms {
                continue;
            }
            if e.level.is_notable() || mention.is_some_and(|m| mentions(&e.text, m)) {
                hits.push((e.at_ms.abs_diff(at_ms), e));
            }
        }
        if hits.len() > MAX_SERVER_LOG {
            // the closest in time, then back to the order of the log
            let mut order: Vec<usize> = (0..hits.len()).collect();
            order.sort_by_key(|&i| hits[i].0);
            order.truncate(MAX_SERVER_LOG);
            order.sort_unstable();
            hits = order.into_iter().map(|i| hits[i]).collect();
        }
        hits.into_iter()
            .map(|(_, e)| one_line(&e.text, MAX_LOG_LINE_CHARS))
            .collect()
    }
}

/// The identity of a log file, to notice that it was replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    created: Option<SystemTime>,
    ino: Option<u64>,
}

impl FileId {
    fn of(meta: &std::fs::Metadata) -> FileId {
        #[cfg(unix)]
        let ino = {
            use std::os::unix::fs::MetadataExt;
            Some(meta.ino())
        };
        #[cfg(not(unix))]
        let ino = None;
        FileId {
            created: meta.created().ok(),
            ino,
        }
    }
}

#[derive(Debug)]
enum Position {
    /// Not looked at yet: start at the end of the file.
    Fresh,
    /// The file was not there at the last look: read it from the start when it appears.
    Missing,
    /// Read up to `offset` of the file with this identity.
    At { offset: u64, id: FileId },
}

/// Reads what a growing file gained since the last [`Follower::poll`].
#[derive(Debug)]
pub struct Follower {
    path: PathBuf,
    pos: Position,
    /// The unterminated line at the end of the last read.
    partial: Vec<u8>,
}

impl Follower {
    /// A follower of `path`; the first poll starts at the end of the file.
    pub fn new(path: PathBuf) -> Follower {
        Follower {
            path,
            pos: Position::Fresh,
            partial: Vec::new(),
        }
    }

    /// The complete lines (each ending in `\n`) added since the last call. A file that shrank or
    /// was replaced is read from its start; a missing file is waited for.
    pub fn poll(&mut self) -> io::Result<Vec<u8>> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.pos = Position::Missing;
                self.partial.clear();
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let (len, id) = (meta.len(), FileId::of(&meta));
        let mut resync = false;
        let offset = match &self.pos {
            Position::Fresh => {
                self.pos = Position::At { offset: len, id };
                return Ok(Vec::new());
            }
            Position::Missing => 0,
            Position::At { offset, id: old } => {
                if *old != id || len < *offset {
                    self.partial.clear();
                    0
                } else if len - *offset > SKIP_OVER {
                    self.partial.clear();
                    resync = true;
                    len - MAX_READ
                } else {
                    *offset
                }
            }
        };
        if len == offset {
            self.pos = Position::At { offset, id };
            return Ok(Vec::new());
        }
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::new();
        file.take(MAX_READ).read_to_end(&mut buf)?;
        self.pos = Position::At {
            offset: offset + buf.len() as u64,
            id,
        };
        if resync {
            // (we landed in the middle of a line)
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    buf.drain(..=i);
                }
                None => buf.clear(),
            }
        }
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(&buf);
        match data.iter().rposition(|b| *b == b'\n') {
            Some(i) => {
                self.partial = data.split_off(i + 1);
                Ok(data)
            }
            None if data.len() > MAX_PARTIAL => {
                data.push(b'\n');
                Ok(data)
            }
            None => {
                self.partial = data;
                Ok(Vec::new())
            }
        }
    }
}

/// Called for every new `WARN` / `ERROR` line, from the tail task: it must not block.
pub type LineHook = Arc<dyn Fn(LogLevel, &str) + Send + Sync>;

/// The tail task: polls `path` every [`POLL_EVERY`] until `cancel` fires, feeds `log` and calls
/// `hook` for the new `WARN` / `ERROR` lines.
pub async fn run(
    log: Arc<ServerLog>,
    path: PathBuf,
    hook: Option<LineHook>,
    cancel: CancellationToken,
) {
    let mut follower = Follower::new(path.clone());
    let mut every = tokio::time::interval(POLL_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_error = String::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = every.tick() => {}
        }
        let polled = tokio::task::spawn_blocking(move || {
            let r = follower.poll();
            (follower, r)
        })
        .await;
        let (f, result) = match polled {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("server log tail stopped: {e}");
                return;
            }
        };
        follower = f;
        match result {
            Ok(chunk) => {
                last_error.clear();
                if chunk.is_empty() {
                    continue;
                }
                let now = crate::hub::unix_ms();
                for (level, text) in log.ingest(&chunk, now, local_offset_secs(now)) {
                    if let Some(h) = &hook {
                        h(level, &text);
                    }
                }
            }
            Err(e) => {
                let text = e.to_string();
                if text != last_error {
                    tracing::warn!("reading the server log {}: {text}", path.display());
                    last_error = text;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::temp_dir;
    use std::io::Write;

    #[test]
    fn colour_codes_and_carriage_returns_are_stripped() {
        let raw = b"\x1b[2m12:00:01\x1b[0m \x1b[33m WARN\x1b[0m aaemu_game::x: \x1b[1;31mbad\x1b[0m thing\r";
        assert_eq!(decode_line(raw), "12:00:01  WARN aaemu_game::x: bad thing");
        assert_eq!(decode_line(b"plain"), "plain");
        // a lone escape and an escape at the end do not hang or panic
        assert_eq!(decode_line(b"a\x1bb"), "ab");
        assert_eq!(decode_line(b"a\x1b["), "a");
        assert_eq!(decode_line(b"bad \xff byte"), "bad \u{fffd} byte");
        let long = decode_line(&vec![b'x'; 5000]);
        assert_eq!(long.len(), MAX_LINE_BYTES);
        let wide = decode_line("é".repeat(3000).as_bytes());
        assert!(wide.len() <= MAX_LINE_BYTES && wide.is_char_boundary(wide.len()));
    }

    #[test]
    fn the_server_line_format_and_its_cousins_are_understood() {
        let p = split_line(
            "14:03:09 [WARN] aaemu_game::systems::quests::engine - quest 251 has no step",
        );
        assert_eq!(p.level, LogLevel::Warn);
        assert_eq!(p.hms, Some(14 * 3600 + 3 * 60 + 9));
        assert_eq!(p.target, Some("aaemu_game::systems::quests::engine"));
        assert_eq!(p.message, "quest 251 has no step");

        let p = split_line(
            "2026-11-02T19:03:09.123456Z ERROR aaemu_game::net: connection reset by peer",
        );
        assert_eq!(p.level, LogLevel::Error);
        assert_eq!(p.hms, Some(19 * 3600 + 3 * 60 + 9));
        assert_eq!(p.target, Some("aaemu_game::net"));
        assert_eq!(p.message, "connection reset by peer");

        let p = split_line("09:00:00 [info] aaemu_login - ready");
        assert_eq!(p.level, LogLevel::Info);
        assert_eq!(p.message, "ready");

        // no level, no time: the whole line is the message
        let p = split_line("   at some::stack::frame (file.rs:12)");
        assert_eq!((p.level, p.hms, p.target), (LogLevel::Other, None, None));
        assert_eq!(p.message, "at some::stack::frame (file.rs:12)");
        // a level word deep inside a message does not count
        let p = split_line("12:00:00 something happened but this error is not a level, honestly, really long text here");
        assert_eq!(p.level, LogLevel::Other);
        // impossible times are not times
        assert_eq!(split_line("99:99:99 [WARN] t - m").hms, None);
        assert_eq!(split_line("1234:56:78 x").hms, None);
        assert_eq!(
            parse_prefix("12:00:01 [ERROR] a - b"),
            (LogLevel::Error, Some(43_201))
        );
    }

    #[test]
    fn names_are_matched_as_whole_words_without_regard_to_case() {
        assert!(mentions("player Aria moved", "aria"));
        assert!(mentions("Aria", "Aria"));
        assert!(mentions("(aria)", "ARIA"));
        assert!(mentions("name=Aria,", "Aria"));
        assert!(!mentions("Ariadne moved", "aria"));
        assert!(!mentions("Maria", "aria"));
        assert!(!mentions("x_aria", "aria"));
        assert!(!mentions("nothing", ""));
        assert!(!mentions("ab", "abc"));
        assert_eq!(
            words("Bot Aria_1 (hp=3), é!").collect::<Vec<_>>(),
            vec!["Bot", "Aria_1", "hp", "3", "é"]
        );
    }

    #[test]
    fn keys_ignore_numbers_names_and_spacing() {
        let a = log_key(
            "12:00:01 [WARN] aaemu_game::q - Aria   failed quest 251 (step 3)",
            "Aria",
        );
        let b = log_key(
            "12:09:59 [WARN] aaemu_game::q - BOBBY failed quest 77 (step 12)",
            "Bobby",
        );
        assert_eq!(a, "* failed quest # (step #)");
        assert_eq!(a, b);
        assert!(log_key(&"x".repeat(500), "").len() <= 101);
        assert_eq!(log_key("", "A"), "");
    }

    #[test]
    fn a_log_target_hints_at_a_source_directory() {
        assert_eq!(
            area_from_target("aaemu_game::systems::quests::engine").as_deref(),
            Some("rust/crates/aaemu-game/src/systems/quests/")
        );
        assert_eq!(
            area_from_target("aaemu_game::net").as_deref(),
            Some("rust/crates/aaemu-game/src/net/")
        );
        assert_eq!(
            area_from_target("aaemu_login").as_deref(),
            Some("rust/crates/aaemu-login/src/")
        );
        assert_eq!(area_from_target("tokio::runtime"), None);
        assert_eq!(
            area_from_target("aaemu_game::../etc"),
            Some("rust/crates/aaemu-game/src/".into())
        );
        assert_eq!(area_from_target(""), None);
    }

    /// 2026-11-02 19:03:09 UTC.
    const T: u64 = 1_793_646_189_000;

    fn line(h: u32, m: u32, s: u32, level: &str, msg: &str) -> String {
        format!("{h:02}:{m:02}:{s:02} [{level}] aaemu_game::t - {msg}\n")
    }

    #[test]
    fn lines_near_filters_by_time_level_and_name() {
        let log = ServerLog::new();
        // the machine is at UTC, the lines are logged around 19:03:09 (= T)
        let chunk = [
            line(19, 2, 0, "WARN", "far away warning"),
            line(19, 3, 5, "WARN", "warning four seconds before"),
            line(19, 3, 7, "INFO", "Aria entered the world"),
            line(19, 3, 8, "INFO", "somebody else did something"),
            line(19, 3, 9, "ERROR", "error at the very moment 8.8.8.8"),
            line(19, 3, 12, "DEBUG", "debug mentioning aria"),
            line(19, 3, 13, "ERROR", "error four seconds after"),
            line(19, 3, 40, "ERROR", "error far after"),
        ]
        .concat();
        let notable = log.ingest(chunk.as_bytes(), T + 1_000, 0);
        assert_eq!(
            notable.len(),
            5,
            "warnings and errors are reported to the hook"
        );
        assert!(notable.iter().all(|(l, _)| l.is_notable()));

        let near = log.lines_near(T, 5, None);
        assert_eq!(near.len(), 3, "{near:#?}");
        assert!(near[0].contains("four seconds before"));
        assert!(
            near[1].contains("the very moment x.x.x.x"),
            "addresses are masked: {}",
            near[1]
        );
        assert!(near[2].contains("four seconds after"));

        let named = log.lines_near(T, 5, Some("aria"));
        assert_eq!(named.len(), 5, "{named:#?}");
        assert!(named.iter().any(|l| l.contains("Aria entered")));
        assert!(named.iter().any(|l| l.contains("debug mentioning aria")));
        assert!(!named.iter().any(|l| l.contains("somebody else")));
        // chronological
        let at = |needle: &str| named.iter().position(|l| l.contains(needle)).unwrap();
        assert!(
            at("four seconds before") < at("Aria entered")
                && at("Aria entered") < at("very moment")
        );

        // a narrower window and a different centre
        assert_eq!(log.lines_near(T, 0, None).len(), 1);
        assert_eq!(log.lines_near(T + 31_000, 2, None).len(), 1);
        assert!(log.lines_near(T + 3_600_000, 5, Some("aria")).is_empty());
        assert!(ServerLog::new().lines_near(T, 5, Some("aria")).is_empty());
    }

    #[test]
    fn lines_near_is_capped_and_keeps_the_closest() {
        let log = ServerLog::new();
        let mut chunk = String::new();
        for i in 0..100u32 {
            chunk.push_str(&line(19, 3, 9 + (i % 4), "WARN", &format!("warning {i}")));
        }
        log.ingest(chunk.as_bytes(), T + 500, 0);
        let near = log.lines_near(T, 10, None);
        assert_eq!(near.len(), MAX_SERVER_LOG);
        // the closest ones are the lines stamped 19:03:09 (every fourth), in log order
        assert!(near.iter().filter(|l| l.contains("19:03:09")).count() >= 25);
        let order: Vec<u32> = near
            .iter()
            .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn a_time_of_day_from_another_zone_falls_back_to_the_arrival_time() {
        let log = ServerLog::new();
        // the log says 03:03:09 (some other zone), we read it at 19:03:10 UTC
        log.ingest(
            line(3, 3, 9, "ERROR", "from another zone").as_bytes(),
            T + 1_000,
            0,
        );
        assert_eq!(log.lines_near(T, 5, None).len(), 1);
        // a line without a time is stamped with the moment it was read
        log.ingest(b"   continuation of a trace\n", T + 1_000, 0);
        assert_eq!(log.lines_near(T, 5, Some("continuation")).len(), 2);
    }

    #[test]
    fn local_times_are_resolved_with_the_machines_offset() {
        let log = ServerLog::new();
        // UTC-5: 14:03:09 local is 19:03:09 UTC
        log.ingest(
            line(14, 3, 9, "WARN", "local time").as_bytes(),
            T + 800,
            -5 * 3600,
        );
        assert_eq!(log.lines_near(T, 1, None).len(), 1);
        assert!(log.lines_near(T + 3_600_000, 1, None).is_empty());
    }

    #[test]
    fn the_ring_is_bounded_in_lines_and_bytes() {
        let log = ServerLog::new();
        let mut chunk = String::new();
        for i in 0..(RING_LINES + 500) {
            chunk.push_str(&format!("line {i}\n"));
        }
        log.ingest(chunk.as_bytes(), T, 0);
        assert_eq!(log.len(), RING_LINES);
        assert!(log.lines_near(T, 5, Some("line")).len() <= MAX_SERVER_LOG);

        let log = ServerLog::new();
        let fat = format!("{}\n", "y".repeat(1800));
        log.ingest(fat.repeat(1500).as_bytes(), T, 0);
        assert!(log.bytes() <= RING_BYTES, "{}", log.bytes());
        assert!(log.len() < 1500 && log.len() >= 500, "{}", log.len());
        assert!(!log.is_empty());
    }

    #[test]
    fn the_follower_reads_new_lines_only_and_never_half_a_line() {
        let dir = temp_dir("logtail-follow");
        let path = dir.join("server.log");
        std::fs::write(&path, "old line 1\nold line 2\n").unwrap();
        let mut f = Follower::new(path.clone());
        assert!(
            f.poll().unwrap().is_empty(),
            "the first poll starts at the end"
        );
        assert!(f.poll().unwrap().is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"new 1\nnew 2\npart").unwrap();
        file.flush().unwrap();
        assert_eq!(f.poll().unwrap(), b"new 1\nnew 2\n");
        file.write_all(b"ial line\nnext\n").unwrap();
        file.flush().unwrap();
        assert_eq!(f.poll().unwrap(), b"partial line\nnext\n");
        assert!(f.poll().unwrap().is_empty());
    }

    #[test]
    fn a_truncated_or_rotated_file_is_read_from_the_start() {
        let dir = temp_dir("logtail-rotate");
        let path = dir.join("server.log");
        std::fs::write(
            &path,
            "a long first line that makes the file big\nsecond line of the old file\n",
        )
        .unwrap();
        let mut f = Follower::new(path.clone());
        assert!(f.poll().unwrap().is_empty());

        // truncated in place and refilled with less
        std::fs::write(&path, "fresh 1\n").unwrap();
        assert_eq!(f.poll().unwrap(), b"fresh 1\n");
        // rotated: the old file moves away, a new smaller one appears
        std::fs::rename(&path, dir.join("server.log.1")).unwrap();
        std::fs::write(&path, "x\n").unwrap();
        assert_eq!(f.poll().unwrap(), b"x\n");
        // deleted, then back: the new file is read whole
        std::fs::remove_file(&path).unwrap();
        assert!(f.poll().unwrap().is_empty());
        std::fs::write(&path, "back 1\nback 2\n").unwrap();
        assert_eq!(f.poll().unwrap(), b"back 1\nback 2\n");
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        let dir = temp_dir("logtail-missing");
        let mut f = Follower::new(dir.join("nope.log"));
        assert!(f.poll().unwrap().is_empty());
        assert!(f.poll().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_task_feeds_the_ring_and_the_hook_with_new_warnings() {
        let dir = temp_dir("logtail-task");
        let path = dir.join("server.log");
        std::fs::write(&path, "before\n").unwrap();
        let log = Arc::new(ServerLog::new());
        let seen: Arc<Mutex<Vec<(LogLevel, String)>>> = Arc::default();
        let hook: LineHook = {
            let seen = seen.clone();
            Arc::new(move |l, t| seen.lock().push((l, t.to_string())))
        };
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(log.clone(), path.clone(), Some(hook), cancel.clone()));
        // (the first poll fires at once and positions at the end of the file)
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "10:00:00 [WARN] aaemu_game::x - Aria did a thing").unwrap();
        writeln!(file, "10:00:00 [INFO] aaemu_game::x - quiet").unwrap();
        file.flush().unwrap();
        let mut waited = 0;
        while log.len() < 2 && waited < 40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += 1;
        }
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(log.len(), 2);
        let seen = seen.lock();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, LogLevel::Warn);
        assert!(seen[0].1.contains("Aria did a thing"));
    }
}
