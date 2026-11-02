//! The issue service: the process-wide owner of the book, the server log ring and the background
//! work (saving, evidence folders, the queue file).
//!
//! [`Issues::report`] is what the hub calls for every observation of a detector. It is cheap and
//! never waits for the disk: it normalizes the report, attaches the server log lines around it,
//! counts it in the book under a mutex (never held across an `await`) and wakes the background
//! tasks. Those do the slow things on blocking threads:
//!
//! * the **saver** writes `issues.json` (atomically) about 5 s after the last change and at the end;
//! * the **sink** ([`Issues::sink_pass`]) writes evidence folders and queue entries, honouring the
//!   policy and the rate limits of [`IssueBook`], and looks for the marker of a queued entry in the
//!   queue file when the issue recurs;
//! * the **tail** ([`logtail::run`]) follows the server log.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::book::{Action, Counts, IssueBook, IssueRecord, IssueState, Policy, QueueJob};
use super::evidence;
use super::logtail::{self, LineHook, ServerLog};
use super::model::IssueReport;
use super::queue::{self, EntryCtx, EntryKind, MarkerState};
use super::timefmt::{local_offset_secs, tz_label};
use super::view;
use crate::config::{IssuesCfg, QueueMode};
use crate::hub::unix_ms;
use crate::roster::write_atomic;

/// The book is written this long after the last change.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// The sink looks for work at least this often (rate limits expire without anybody reporting).
const SINK_POLL: Duration = Duration::from_secs(60);
/// After a report wakes the sink it waits this long, so the server log lines that follow the
/// problem are in the ring when the evidence is written.
const SETTLE: Duration = Duration::from_secs(3);
/// After a failed write to the queue file the sink leaves the file alone this long.
const QUEUE_BACKOFF_MS: u64 = 60_000;
/// Server log lines are looked for this many seconds around a report.
pub const LOG_WINDOW_SECS: u64 = 5;
/// The file name of the book in the issues directory.
pub const BOOK_FILE: &str = "issues.json";

/// What one pass of the sink did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SinkOutcome {
    /// Evidence folders written or refreshed.
    pub evidence: usize,
    /// First entries appended to the queue file.
    pub queued: usize,
    /// Recurrence entries appended.
    pub recurred: usize,
    /// Queue files searched whose marker was still there.
    pub still_open: usize,
    /// Entries that have to wait for a rate limit.
    pub waiting: usize,
    /// Writes and reads that failed (logged).
    pub errors: usize,
}

/// Counters for the log and the hub's statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IssueStats {
    /// Reports received since the start.
    pub reports: u64,
    /// Issues in the book.
    pub issues: usize,
    /// Per state.
    pub counts: Counts,
}

/// The issue service. See the module docs.
pub struct Issues {
    enabled: bool,
    cfg: IssuesCfg,
    book_file: PathBuf,
    evidence_dir: PathBuf,
    queue_file: Option<PathBuf>,
    book: Mutex<IssueBook>,
    log: Option<Arc<ServerLog>>,
    log_path: Option<PathBuf>,
    offset_override: Option<i32>,
    dirty: AtomicBool,
    save_wake: Notify,
    sink_wake: Notify,
    queue_backoff_until_ms: AtomicU64,
    reports: AtomicU64,
}

impl Issues {
    /// A service that does nothing: reports are dropped, the lists are empty.
    pub fn disabled() -> Issues {
        Issues::build(
            false,
            &IssuesCfg::default(),
            PathBuf::new(),
            PathBuf::new(),
            IssueBook::new(Policy::default()),
        )
    }

    /// Opens the service for `cfg`: resolves the directories (next to `roster` by default) and
    /// loads `issues.json`. A book that cannot be read is moved aside (`issues.json.bad`) and an
    /// empty one is started. Reads one small file; call it at start-up.
    pub fn open(cfg: &IssuesCfg, roster: &Path) -> Issues {
        if !cfg.enabled {
            return Issues::disabled();
        }
        let dir = cfg.resolved_dir(roster);
        let evidence_dir = cfg.resolved_evidence_dir(roster);
        let policy = Policy::from_cfg(cfg);
        let book_file = dir.join(BOOK_FILE);
        let book = load_book(&book_file, policy);
        if let Some(parent) = cfg
            .queue_file
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
        {
            if !parent.is_dir() {
                tracing::warn!(
                    "issues.queue_file: the directory {} does not exist; entries cannot be written until it does",
                    parent.display()
                );
            }
        }
        Issues::build(true, cfg, book_file, evidence_dir, book)
    }

    fn build(
        enabled: bool,
        cfg: &IssuesCfg,
        book_file: PathBuf,
        evidence_dir: PathBuf,
        book: IssueBook,
    ) -> Issues {
        let log = (enabled && cfg.server_log.is_some()).then(|| Arc::new(ServerLog::new()));
        Issues {
            enabled,
            cfg: cfg.clone(),
            book_file,
            evidence_dir,
            queue_file: if enabled {
                cfg.queue_file.clone()
            } else {
                None
            },
            book: Mutex::new(book),
            log,
            log_path: if enabled {
                cfg.server_log.clone()
            } else {
                None
            },
            offset_override: None,
            dirty: AtomicBool::new(false),
            save_wake: Notify::new(),
            sink_wake: Notify::new(),
            queue_backoff_until_ms: AtomicU64::new(0),
            reports: AtomicU64::new(0),
        }
    }

    /// Pins the UTC offset used for local times (for tests; the default asks the system).
    pub fn with_utc_offset(mut self, secs: i32) -> Issues {
        self.offset_override = Some(secs);
        self
    }

    /// Whether reports are collected.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// What happens with reportable issues.
    pub fn mode(&self) -> QueueMode {
        self.cfg.queue_mode
    }

    /// The queue file, if configured.
    pub fn queue_file(&self) -> Option<&Path> {
        self.queue_file.as_deref()
    }

    /// The directory the evidence folders go to.
    pub fn evidence_dir(&self) -> &Path {
        &self.evidence_dir
    }

    /// The file the book is saved to.
    pub fn book_file(&self) -> &Path {
        &self.book_file
    }

    /// The server log ring, if `issues.server_log` is set.
    pub fn server_log(&self) -> Option<&Arc<ServerLog>> {
        self.log.as_ref()
    }

    fn offset(&self, now_ms: u64) -> i32 {
        self.offset_override
            .unwrap_or_else(|| local_offset_secs(now_ms))
    }

    /// Counts a report. Cheap: normalizes the report, attaches the server log lines around it,
    /// updates the book and wakes the background tasks. Returns the fingerprint of the issue
    /// (`None` when the service is disabled).
    pub fn report(&self, report: IssueReport) -> Option<String> {
        self.report_at(report, unix_ms())
    }

    /// [`Issues::report`] with the current time given.
    pub fn report_at(&self, mut report: IssueReport, now_ms: u64) -> Option<String> {
        if !self.enabled {
            return None;
        }
        report.normalize(now_ms);
        if report.server_log.is_empty() {
            if let Some(log) = &self.log {
                let name = report.bot.as_ref().map(|b| b.name.as_str());
                report.server_log = log.lines_near(report.at_ms, LOG_WINDOW_SECS, name);
            }
        }
        let recorded = self.book.lock().record(report, now_ms);
        self.reports.fetch_add(1, Ordering::Relaxed);
        self.mark_dirty();
        if recorded.wake {
            self.sink_wake.notify_one();
        }
        Some(recorded.fingerprint)
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        self.save_wake.notify_one();
    }

    /// Counters.
    pub fn stats(&self) -> IssueStats {
        let b = self.book.lock();
        IssueStats {
            reports: self.reports.load(Ordering::Relaxed),
            issues: b.len(),
            counts: b.counts(),
        }
    }

    /// Runs `f` on the book (under its lock: keep it short).
    pub fn with_book<R>(&self, f: impl FnOnce(&IssueBook) -> R) -> R {
        f(&self.book.lock())
    }

    /// The operator's action on an issue (the web UI's buttons).
    pub fn action(&self, fp: &str, action: Action) -> Result<(), String> {
        if !self.enabled {
            return Err("issue reports are switched off ([issues] enabled = false)".into());
        }
        if action == Action::Queue {
            if self.cfg.queue_mode == QueueMode::Off {
                return Err("queue_mode is \"off\": nothing is written to the queue file".into());
            }
            if self.queue_file.is_none() {
                return Err("no queue_file is configured ([issues] queue_file)".into());
            }
        }
        let now = unix_ms();
        self.book.lock().apply_action(fp, action, now)?;
        self.mark_dirty();
        if action == Action::Queue {
            self.sink_wake.notify_one();
        }
        Ok(())
    }

    /// The issue list for the web UI, most recently seen first.
    pub fn summaries(&self) -> Vec<pb_web::IssueSummary> {
        self.with_book(|b| {
            let mut all: Vec<pb_web::IssueSummary> = b
                .iter()
                .map(|r| view::summary(r, b.is_reportable(r)))
                .collect();
            all.sort_by(|a, b| {
                b.last_seen
                    .cmp(&a.last_seen)
                    .then_with(|| a.fingerprint.cmp(&b.fingerprint))
            });
            all
        })
    }

    /// One issue for the web UI (`fingerprint` as `pb:abcd1234` or `abcd1234`).
    pub fn detail(&self, fingerprint: &str) -> Option<pb_web::IssueDetail> {
        let (rec, reportable) =
            self.with_book(|b| b.get(fingerprint).map(|r| (r.clone(), b.is_reportable(r))))?;
        let entry = self.preview(&rec);
        Some(view::detail(&rec, reportable, entry))
    }

    /// The queue entry as it would be (or was) written, for the web UI.
    pub fn preview(&self, rec: &IssueRecord) -> String {
        let now = unix_ms();
        let offset = self.offset(now);
        let tz = tz_label(self.cfg.label(), offset);
        let ctx = EntryCtx {
            now_ms: now,
            offset_secs: offset,
            tz: &tz,
            evidence: rec.evidence.as_deref(),
        };
        queue::render_entry(rec, EntryKind::New, &ctx)
    }

    /// Local UTC offset in seconds at `now_ms`.
    pub fn utc_offset(&self, now_ms: u64) -> i32 {
        self.offset(now_ms)
    }

    // ---- saving ----------------------------------------------------------------------------

    /// Writes the book now if it changed (on a blocking thread).
    pub async fn flush(&self) {
        if !self.enabled || !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let text = self.book.lock().to_json(unix_ms());
        let path = self.book_file.clone();
        let result = tokio::task::spawn_blocking(move || write_atomic(&path, &text))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("the writer thread failed: {e}")));
        if let Err(e) = result {
            tracing::warn!("saving the issue book: {e:#}");
            self.dirty.store(true, Ordering::Release);
        }
    }

    // ---- the sink --------------------------------------------------------------------------

    /// Repeats the server log correlation for the samples of `rec` (the lines may have arrived after
    /// the report) and returns what changed, as `(sample time, lines)`.
    fn recorrelate(&self, rec: &mut IssueRecord) -> Vec<(u64, Vec<String>)> {
        let Some(log) = &self.log else {
            return Vec::new();
        };
        let mut changed = Vec::new();
        for s in &mut rec.samples {
            let name = s.bot.as_ref().map(|b| b.name.as_str());
            let lines = log.lines_near(s.at_ms, LOG_WINDOW_SECS, name);
            if !lines.is_empty() && lines != s.server_log {
                s.server_log.clone_from(&lines);
                changed.push((s.at_ms, lines));
            }
        }
        changed
    }

    /// One pass of the sink at `now_ms`: for each job of [`IssueBook::plan`] writes the evidence
    /// folder and appends the queue entry, or looks for the marker of a recurring issue.
    pub async fn sink_pass(&self, now_ms: u64) -> SinkOutcome {
        let mut out = SinkOutcome::default();
        if !self.enabled {
            return out;
        }
        let queue_ok = self.queue_file.is_some()
            && now_ms >= self.queue_backoff_until_ms.load(Ordering::Relaxed);
        let jobs = self.book.lock().plan(now_ms, self.cfg.queue_mode, queue_ok);
        let offset = self.offset(now_ms);
        let tz = tz_label(self.cfg.label(), offset);
        let mut changed = false;
        for job in jobs {
            let Some(mut rec) = self.book.lock().get(&job.fp).cloned() else {
                continue;
            };
            let logs = self.recorrelate(&mut rec);
            if !logs.is_empty() {
                self.book.lock().set_server_logs(&job.fp, logs);
                changed = true;
            }

            // A recurring issue: is its entry still in the file?
            let mut kind = EntryKind::New;
            if job.queue == Some(QueueJob::RecurrenceCheck) {
                let Some(file) = self.queue_file.clone() else {
                    continue;
                };
                let marker = rec.fingerprint.clone();
                let found = blocking(move || queue::marker_state(&file, &marker)).await;
                match found {
                    Some(Ok(MarkerState::Absent)) => {}
                    Some(Ok(state)) => {
                        if state == MarkerState::TooLarge {
                            tracing::warn!(
                                "the queue file is larger than {} bytes: cannot tell whether {} is still open",
                                queue::MAX_READ,
                                rec.fingerprint
                            );
                        }
                        self.book.lock().mark_checked(&job.fp, now_ms);
                        out.still_open += 1;
                        changed = true;
                        continue;
                    }
                    other => {
                        let why = match other {
                            Some(Err(e)) => e.to_string(),
                            _ => "the reader thread failed".to_string(),
                        };
                        tracing::warn!("reading the queue file: {why}");
                        // (looked at again after the next quiet period, not in a loop)
                        self.book.lock().mark_checked(&job.fp, now_ms);
                        out.errors += 1;
                        changed = true;
                        continue;
                    }
                }
                if self.book.lock().limit_hit(rec.kind, now_ms, &[]).is_some() {
                    // gone from the file, but the entry waits for the rate limit
                    out.waiting += 1;
                    continue;
                }
                if let IssueState::Queued {
                    at_ms, count_at, ..
                } = rec.state
                {
                    kind = EntryKind::Recurrence {
                        previous_at_ms: at_ms,
                        new_occurrences: rec.count.saturating_sub(count_at),
                    };
                }
            }
            // The evidence folder.
            if job.evidence || matches!(kind, EntryKind::Recurrence { .. }) {
                let dir = self.evidence_dir.clone();
                let r = rec.clone();
                match blocking(move || evidence::write(&dir, &r, now_ms, offset)).await {
                    Some(Ok(folder)) => {
                        let folder = folder.to_string_lossy().into_owned();
                        self.book
                            .lock()
                            .mark_evidence(&job.fp, folder.clone(), now_ms);
                        rec.evidence = Some(folder);
                        out.evidence += 1;
                        changed = true;
                    }
                    Some(Err(e)) => {
                        tracing::warn!("writing the evidence of {}: {e}", job.fp);
                        out.errors += 1;
                    }
                    None => out.errors += 1,
                }
            }

            // The queue entry.
            let wants_entry = matches!(job.queue, Some(QueueJob::New | QueueJob::Manual))
                || matches!(kind, EntryKind::Recurrence { .. });
            if wants_entry {
                let Some(file) = self.queue_file.clone() else {
                    continue;
                };
                let ctx = EntryCtx {
                    now_ms,
                    offset_secs: offset,
                    tz: &tz,
                    evidence: rec.evidence.as_deref(),
                };
                let entry = queue::render_entry(&rec, kind, &ctx);
                match blocking(move || queue::append_entry(&file, &entry)).await {
                    Some(Ok(())) => {
                        self.book.lock().mark_queued(&job.fp, now_ms);
                        changed = true;
                        if matches!(kind, EntryKind::Recurrence { .. }) {
                            out.recurred += 1;
                        } else {
                            out.queued += 1;
                        }
                        tracing::info!(
                            issue = %job.fp,
                            kind = rec.kind.as_str(),
                            "queued issue: {}",
                            rec.title
                        );
                    }
                    other => {
                        let why = match other {
                            Some(Err(e)) => e.to_string(),
                            _ => "the writer thread failed".to_string(),
                        };
                        tracing::warn!(
                            "appending to the queue file: {why} (trying again in {} s)",
                            QUEUE_BACKOFF_MS / 1000
                        );
                        self.queue_backoff_until_ms
                            .store(now_ms + QUEUE_BACKOFF_MS, Ordering::Relaxed);
                        out.errors += 1;
                    }
                }
            }
        }
        if changed {
            self.mark_dirty();
        }
        out
    }

    // ---- the background tasks ---------------------------------------------------------------

    /// Starts the background tasks: the saver, the sink and (with `issues.server_log`) the log
    /// tail, whose `WARN` / `ERROR` lines go to `hook`. [`IssueTasks::stop`] ends them.
    pub fn start(self: &Arc<Self>, hook: Option<LineHook>) -> IssueTasks {
        let cancel = CancellationToken::new();
        let mut handles = Vec::new();
        if self.enabled {
            handles.push(tokio::spawn(saver_loop(self.clone(), cancel.clone())));
            handles.push(tokio::spawn(sink_loop(self.clone(), cancel.clone())));
            if let (Some(log), Some(path)) = (&self.log, &self.log_path) {
                handles.push(tokio::spawn(logtail::run(
                    log.clone(),
                    path.clone(),
                    hook,
                    cancel.clone(),
                )));
            }
            // (entries that were waiting when the last run ended)
            self.sink_wake.notify_one();
            let s = self.stats();
            tracing::info!(
                issues = s.issues,
                dir = %self.book_file.parent().map_or_else(String::new, |p| p.display().to_string()),
                queue_file = %self.queue_file.as_deref().map_or_else(|| "none".to_string(), |p| p.display().to_string()),
                queue_mode = ?self.cfg.queue_mode,
                server_log = %self.log_path.as_deref().map_or_else(|| "none".to_string(), |p| p.display().to_string()),
                "issue reports on"
            );
        }
        IssueTasks {
            cancel,
            handles,
            issues: self.clone(),
        }
    }
}

impl std::fmt::Debug for Issues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issues")
            .field("enabled", &self.enabled)
            .field("book_file", &self.book_file)
            .field("queue_file", &self.queue_file)
            .finish()
    }
}

/// The background tasks of an [`Issues`] service.
pub struct IssueTasks {
    cancel: CancellationToken,
    handles: Vec<JoinHandle<()>>,
    issues: Arc<Issues>,
}

impl IssueTasks {
    /// Stops the tasks and writes the book one last time.
    pub async fn stop(self) {
        self.cancel.cancel();
        for h in self.handles {
            if let Err(e) = h.await {
                tracing::warn!("issue task: {e}");
            }
        }
        self.issues.flush().await;
    }
}

async fn saver_loop(me: Arc<Issues>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = me.save_wake.notified() => {}
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(SAVE_INTERVAL) => {}
        }
        me.flush().await;
    }
}

async fn sink_loop(me: Arc<Issues>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = me.sink_wake.notified() => {}
            _ = tokio::time::sleep(SINK_POLL) => {}
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(SETTLE) => {}
        }
        me.sink_pass(unix_ms()).await;
    }
}

/// Runs blocking I/O off the async workers; `None` when the thread failed.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

fn load_book(path: &Path, policy: Policy) -> IssueBook {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return IssueBook::new(policy),
        Err(e) => {
            tracing::warn!(
                "reading {}: {e}; starting with an empty issue book",
                path.display()
            );
            return IssueBook::new(policy);
        }
    };
    match IssueBook::from_json(&text, policy) {
        Ok(b) => b,
        Err(e) => {
            let mut bad = path.as_os_str().to_owned();
            bad.push(".bad");
            let bad = PathBuf::from(bad);
            tracing::warn!(
                "{} is not a usable issue book ({e:#}); moving it to {} and starting empty",
                path.display(),
                bad.display()
            );
            let _ = std::fs::rename(path, &bad);
            IssueBook::new(policy)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::model::{BotRef, IssueKind, Severity, Suspect};
    use crate::testutil::temp_dir;

    /// 2026-11-02 19:03:09 UTC.
    const T: u64 = 1_793_646_189_000;

    fn cfg(dir: &Path) -> IssuesCfg {
        IssuesCfg {
            dir: Some(dir.join("issues")),
            queue_file: Some(dir.join("queue.txt")),
            time_label: Some("EST".into()),
            ..IssuesCfg::default()
        }
    }

    fn open(cfg: &IssuesCfg, dir: &Path) -> Issues {
        Issues::open(cfg, &dir.join("roster.json")).with_utc_offset(-5 * 3600)
    }

    fn kicked(bot: u32) -> IssueReport {
        IssueReport::new(IssueKind::Kicked, "reason=3", "The server kicked a bot")
            .expected("the bot stays online")
            .actual("kicked with reason 3 from 198.51.100.77")
            .suspect(Suspect::Server)
            .severity(Severity::High)
            .area("rust/crates/aaemu-game/src/net/")
            .fact("reason", 3)
            .bot(BotRef {
                id: bot,
                name: format!("Aria{bot}"),
                level: 7,
                race: "Elf".into(),
                class: "Archery".into(),
                ..BotRef::default()
            })
    }

    fn read(path: &Path) -> String {
        String::from_utf8(std::fs::read(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_reportable_issue_gets_evidence_and_one_queue_entry() {
        let dir = temp_dir("svc-auto");
        let c = cfg(&dir);
        let issues = open(&c, &dir);
        assert!(issues.enabled());
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        assert!(fp.starts_with("pb:"));

        let out = issues.sink_pass(T + 5_000).await;
        assert_eq!((out.queued, out.evidence, out.errors), (1, 1, 0), "{out:?}");

        let q = read(&dir.join("queue.txt"));
        assert!(
            q.starts_with("[2026-11-02 14:03:14 EST] The server kicked a bot (playerbots)\r\n"),
            "{q:?}"
        );
        assert!(q.ends_with("\r\n") && !q.ends_with("\r\n\r\n"));
        assert!(q.contains(&format!("Marker {fp}.")));
        assert!(
            !q.contains("198.51.100.77"),
            "addresses never reach the queue"
        );
        assert!(q.contains("Likely server area: rust/crates/aaemu-game/src/net/ (hint"));
        assert!(q.contains("Server log at the time: none captured."));
        assert!(q.contains("(suspect: server)"));

        // the evidence folder exists and is named in the entry
        let rec = issues.with_book(|b| b.get(&fp).unwrap().clone());
        let folder = rec.evidence.clone().expect("evidence folder");
        assert!(
            q.contains(&format!("Evidence: {folder} (report.json")),
            "{q}"
        );
        assert!(Path::new(&folder).join("report.json").is_file());
        assert!(folder.contains("-bot-kicked-reason-3-"), "{folder}");
        assert!(matches!(rec.state, IssueState::Queued { .. }));
        // the same issue again is counted, not queued twice
        issues.report_at(kicked(2).at(T + 10_000), T + 10_000);
        let again = issues.sink_pass(T + 20_000).await;
        assert_eq!((again.queued, again.recurred), (0, 0));
        assert_eq!(
            read(&dir.join("queue.txt")).matches("Marker pb:").count(),
            1
        );
    }

    #[tokio::test]
    async fn the_hourly_and_per_kind_limits_make_later_entries_wait() {
        let dir = temp_dir("svc-limits");
        let mut c = cfg(&dir);
        c.max_queue_per_kind_per_day = 2;
        let issues = open(&c, &dir);
        for i in 0..4 {
            let r = IssueReport::new(
                IssueKind::Kicked,
                format!("reason={i}"),
                format!("Kick {i}"),
            )
            .severity(Severity::High)
            .suspect(Suspect::Server)
            .bot(BotRef {
                id: 1,
                name: "A".into(),
                ..BotRef::default()
            });
            issues.report_at(r, T);
        }
        let out = issues.sink_pass(T + 1).await;
        assert_eq!(out.queued, 2, "{out:?}");
        let q = read(&dir.join("queue.txt"));
        assert_eq!(q.matches("Marker pb:").count(), 2);
        assert_eq!(
            q.matches("\r\n\r\n[").count(),
            1,
            "one blank line between the two entries"
        );
        // an hour later still the daily limit
        assert_eq!(issues.sink_pass(T + 2 * 3_600_000).await.queued, 0);
        // a day later the next two go
        assert_eq!(issues.sink_pass(T + 25 * 3_600_000).await.queued, 2);
        assert_eq!(issues.stats().counts.queued, 4);
    }

    #[tokio::test]
    async fn manual_mode_waits_for_the_action_and_off_never_writes() {
        let dir = temp_dir("svc-manual");
        let mut c = cfg(&dir);
        c.queue_mode = QueueMode::Manual;
        let issues = open(&c, &dir);
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        let out = issues.sink_pass(T + 1).await;
        assert_eq!((out.queued, out.evidence), (0, 1), "evidence only: {out:?}");
        assert!(!dir.join("queue.txt").exists());
        issues.action(&fp, Action::Queue).unwrap();
        let out = issues.sink_pass(unix_ms()).await;
        assert_eq!(out.queued, 1, "{out:?}");
        assert!(read(&dir.join("queue.txt")).contains(&fp));
        assert!(issues
            .action(&fp, Action::Queue)
            .unwrap_err()
            .contains("already queued"));

        let dir2 = temp_dir("svc-off");
        let mut c2 = cfg(&dir2);
        c2.queue_mode = QueueMode::Off;
        let off = open(&c2, &dir2);
        let fp2 = off.report_at(kicked(1).at(T), T).unwrap();
        assert!(off.action(&fp2, Action::Queue).unwrap_err().contains("off"));
        assert_eq!(off.sink_pass(T + 1).await.queued, 0);
        assert!(!dir2.join("queue.txt").exists());
        // no queue file at all
        let dir3 = temp_dir("svc-nofile");
        let mut c3 = cfg(&dir3);
        c3.queue_file = None;
        let nofile = open(&c3, &dir3);
        let fp3 = nofile.report_at(kicked(1).at(T), T).unwrap();
        assert!(nofile
            .action(&fp3, Action::Queue)
            .unwrap_err()
            .contains("queue_file"));
        assert_eq!(nofile.sink_pass(T + 1).await.queued, 0);
    }

    #[tokio::test]
    async fn the_web_view_lists_newest_first_and_serves_the_entry() {
        let dir = temp_dir("svc-view");
        let issues = open(&cfg(&dir), &dir);
        let old = issues.report_at(kicked(1).at(T), T).expect("fingerprint");
        let newer = IssueReport::new(IssueKind::NpcMissing, "npc=9", "NPC 9 is missing")
            .suspect(Suspect::Data)
            .at(T + 5_000);
        let newer = issues.report_at(newer, T + 5_000).unwrap();
        let list = issues.summaries();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].fingerprint.as_str(), list[1].fingerprint.as_str()),
            (newer.as_str(), old.as_str())
        );
        assert_eq!(list[0].kind, "npc_missing");
        assert!(!list[0].reportable && list[1].reportable);
        assert_eq!(
            (
                list[1].state.as_str(),
                list[1].suspect.as_str(),
                list[1].severity.as_str()
            ),
            ("open", "server", "high")
        );

        let d = issues.detail(&old).unwrap();
        assert_eq!(d.summary.fingerprint, old);
        assert!(
            d.entry.contains("(playerbots)\r\n") && d.entry.contains(&format!("Marker {old}."))
        );
        assert!(d.entry.starts_with('['));
        assert_eq!(d.samples.len(), 1);
        assert_eq!(d.samples[0].bot, "Aria1");
        assert!(d.evidence.is_none() && d.queued_at.is_none());
        assert!(issues.detail("pb:00000000").is_none());
        assert!(issues.detail("rubbish").is_none());

        // the actions change what the list shows
        issues.action(&newer, Action::Ignore).unwrap();
        assert_eq!(
            issues
                .summaries()
                .iter()
                .find(|s| s.fingerprint == newer)
                .unwrap()
                .state,
            "ignored"
        );
        issues.action(&newer, Action::Resolve).unwrap();
        issues.action(&newer, Action::Reopen).unwrap();
        assert_eq!(issues.detail(&newer).unwrap().summary.state, "open");
        assert!(issues
            .action("pb:00000000", Action::Ignore)
            .unwrap_err()
            .contains("no issue"));
        // after the sink ran, the detail knows the folder and the time
        issues.sink_pass(unix_ms()).await;
        let d = issues.detail(&old).unwrap();
        assert_eq!(d.summary.state, "queued");
        assert!(d.evidence.is_some() && d.queued_at.is_some());
        let s = issues.stats();
        assert_eq!((s.counts.open, s.counts.queued), (1, 1));
    }

    #[tokio::test]
    async fn bot_suspect_issues_stay_local() {
        let dir = temp_dir("svc-bot");
        let issues = open(&cfg(&dir), &dir);
        for i in 0..10 {
            let r = kicked(i + 1).suspect(Suspect::Bot);
            issues.report_at(r, T + u64::from(i));
        }
        let out = issues.sink_pass(T + 100).await;
        assert_eq!((out.queued, out.evidence), (0, 0));
        assert!(!dir.join("queue.txt").exists());
        let s = issues.stats();
        assert_eq!((s.reports, s.issues, s.counts.open), (10, 1, 1));
    }

    #[tokio::test]
    async fn a_recurrence_is_written_only_when_the_entry_was_removed() {
        let dir = temp_dir("svc-recur");
        let issues = open(&cfg(&dir), &dir);
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        issues.sink_pass(T + 1_000).await;
        let queue_path = dir.join("queue.txt");
        let first = read(&queue_path);
        assert!(first.contains(&fp));

        // it happens again 10 minutes later: too soon to look
        issues.report_at(kicked(1).at(T + 600_000), T + 600_000);
        let out = issues.sink_pass(T + 601_000).await;
        assert_eq!((out.still_open, out.recurred), (0, 0));

        // 45 minutes later: the entry is still in the file, so nothing is written
        let t1 = T + 45 * 60_000;
        issues.report_at(kicked(2).at(t1), t1);
        let out = issues.sink_pass(t1 + 1_000).await;
        assert_eq!(
            (out.still_open, out.recurred, out.queued),
            (1, 0, 0),
            "{out:?}"
        );
        assert_eq!(read(&queue_path), first);

        // an agent works through the queue and deletes the entry (and the file gets other content)
        std::fs::write(
            &queue_path,
            "[2026-11-02 09:00:00 EST] Somebody else's entry\r\nbody\r\n",
        )
        .unwrap();
        // 45 more minutes: it happens again
        let t2 = t1 + 45 * 60_000;
        issues.report_at(kicked(3).at(t2), t2);
        let out = issues.sink_pass(t2 + 1_000).await;
        assert_eq!((out.recurred, out.queued), (1, 0), "{out:?}");
        let after = read(&queue_path);
        assert!(
            after.starts_with("[2026-11-02 09:00:00 EST] Somebody else's entry\r\nbody\r\n\r\n["),
            "{after:?}"
        );
        assert!(after.contains("Recurrence after the entry was removed:"));
        assert!(after.contains(&format!("Marker {fp}.")));
        assert_eq!(after.matches("Marker pb:").count(), 1);
        // it is queued again from that moment on (the clock restarted)
        let rec = issues.with_book(|b| b.get(&fp).unwrap().clone());
        match rec.state {
            IssueState::Queued {
                at_ms, count_at, ..
            } => {
                assert_eq!(at_ms, t2 + 1_000);
                assert_eq!(count_at, rec.count);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_recurrence_also_covers_a_queue_file_that_was_deleted() {
        let dir = temp_dir("svc-recur-gone");
        let issues = open(&cfg(&dir), &dir);
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        issues.sink_pass(T + 1_000).await;
        std::fs::remove_file(dir.join("queue.txt")).unwrap();
        let t1 = T + 40 * 60_000;
        issues.report_at(kicked(1).at(t1), t1);
        let out = issues.sink_pass(t1 + 1_000).await;
        assert_eq!(out.recurred, 1);
        let q = read(&dir.join("queue.txt"));
        assert!(q.starts_with("[") && q.contains(&fp));
    }

    #[tokio::test]
    async fn a_failing_queue_file_is_retried_later_and_changes_nothing() {
        let dir = temp_dir("svc-fail");
        let mut c = cfg(&dir);
        // the "file" is a directory
        std::fs::create_dir_all(dir.join("queue.txt")).unwrap();
        c.queue_file = Some(dir.join("queue.txt"));
        let issues = open(&c, &dir);
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        let out = issues.sink_pass(T + 1).await;
        assert_eq!((out.queued, out.errors), (0, 1), "{out:?}");
        assert!(matches!(
            issues.with_book(|b| b.get(&fp).unwrap().state.clone()),
            IssueState::Open
        ));
        // within the back-off the queue file is left alone ...
        let out = issues.sink_pass(T + 10_000).await;
        assert_eq!((out.queued, out.errors), (0, 0), "{out:?}");
        // ... and after it the entry is tried again, which works once the obstacle is gone
        std::fs::remove_dir_all(dir.join("queue.txt")).unwrap();
        let out = issues.sink_pass(T + 2 * QUEUE_BACKOFF_MS).await;
        assert_eq!(out.queued, 1, "{out:?}");
    }

    #[tokio::test]
    async fn server_log_lines_are_attached_and_quoted() {
        let dir = temp_dir("svc-log");
        let mut c = cfg(&dir);
        c.server_log = Some(dir.join("server.log"));
        let issues = open(&c, &dir);
        let log = issues.server_log().expect("ring").clone();
        // the machine is at UTC-5: the server logs local time
        let chunk = "14:03:08 [WARN] aaemu_game::net - connection of Aria1 reset\n\
                     14:03:09 [INFO] aaemu_game::net - Aria1 left\n\
                     14:03:40 [ERROR] aaemu_game::x - far away\n";
        log.ingest(chunk.as_bytes(), T + 500, -5 * 3600);
        let fp = issues.report_at(kicked(1).at(T), T).unwrap();
        let sample = issues.with_book(|b| b.get(&fp).unwrap().samples[0].clone());
        assert_eq!(sample.server_log.len(), 2, "{:?}", sample.server_log);
        assert!(sample.server_log[0].contains("connection of Aria1 reset"));
        // a line that arrives after the report is picked up when the evidence is written
        let late = "14:03:10 [ERROR] aaemu_game::net - write to Aria1 failed\n";
        log.ingest(late.as_bytes(), T + 1_500, -5 * 3600);
        issues.sink_pass(T + 5_000).await;
        let q = read(&dir.join("queue.txt"));
        assert!(
            q.contains("Server log at the time: 14:03:08 [WARN] aaemu_game::net - connection of Aria1 reset (3 lines in the evidence)."),
            "{q}"
        );
        let rec = issues.with_book(|b| b.get(&fp).unwrap().clone());
        let folder = PathBuf::from(rec.evidence.unwrap());
        let text = std::fs::read_to_string(folder.join("server-log.txt")).unwrap();
        assert!(
            text.contains("write to Aria1 failed") && !text.contains("far away"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn the_book_survives_a_restart_and_keeps_deduplicating() {
        let dir = temp_dir("svc-persist");
        let c = cfg(&dir);
        {
            let issues = Arc::new(open(&c, &dir));
            let tasks = issues.start(None);
            issues.report_at(kicked(1).at(T), T);
            issues.report_at(kicked(2).at(T + 1), T + 1);
            issues.sink_pass(T + 2).await;
            tasks.stop().await;
        }
        assert!(dir.join("issues").join(BOOK_FILE).is_file());
        let again = open(&c, &dir);
        let s = again.stats();
        assert_eq!(s.issues, 1);
        assert_eq!(s.counts.queued, 1);
        // the same problem after the restart is the same issue and does not queue again
        again.report_at(kicked(3).at(T + 10), T + 10);
        let (count, bots) = again.with_book(|b| {
            let r = b.iter().next().unwrap();
            (r.count, r.bots.total)
        });
        assert_eq!((count, bots), (3, 3));
        let out = again.sink_pass(T + 20).await;
        assert_eq!((out.queued, out.recurred), (0, 0));
        // the rate limits survived too: three more kinds of kick fill the daily quota
        for i in 0..3 {
            let r = IssueReport::new(IssueKind::Kicked, format!("other={i}"), "x")
                .severity(Severity::High)
                .suspect(Suspect::Server);
            again.report_at(r, T + 30);
        }
        assert_eq!(
            again.sink_pass(T + 40).await.queued,
            1,
            "one of the two kicked slots was used before the restart"
        );
    }

    #[tokio::test]
    async fn an_unreadable_book_is_moved_aside() {
        let dir = temp_dir("svc-bad");
        let c = cfg(&dir);
        std::fs::create_dir_all(dir.join("issues")).unwrap();
        std::fs::write(dir.join("issues").join(BOOK_FILE), "this is not json").unwrap();
        let issues = open(&c, &dir);
        assert_eq!(issues.stats().issues, 0);
        assert!(dir.join("issues").join("issues.json.bad").is_file());
        assert!(!dir.join("issues").join(BOOK_FILE).exists());
    }

    #[tokio::test]
    async fn a_disabled_service_drops_everything() {
        let issues = Issues::disabled();
        assert!(!issues.enabled());
        assert_eq!(issues.report(kicked(1)), None);
        assert!(issues.action("pb:00000000", Action::Ignore).is_err());
        assert_eq!(issues.sink_pass(T).await, SinkOutcome::default());
        assert_eq!(issues.stats(), IssueStats::default());
        let c = IssuesCfg {
            enabled: false,
            ..IssuesCfg::default()
        };
        let off = Arc::new(Issues::open(&c, Path::new("roster.json")));
        assert!(!off.enabled());
        off.start(None).stop().await;
    }

    #[test]
    fn the_default_directories_sit_next_to_the_roster() {
        let c = IssuesCfg::default();
        assert_eq!(
            c.resolved_dir(Path::new("data/roster.json")),
            PathBuf::from("data/issues")
        );
        assert_eq!(
            c.resolved_dir(Path::new("roster.json")),
            PathBuf::from("issues")
        );
        assert_eq!(
            c.resolved_evidence_dir(Path::new("data/roster.json")),
            PathBuf::from("data/issues/evidence")
        );
        let c = IssuesCfg {
            dir: Some("d".into()),
            evidence_dir: Some("e".into()),
            ..IssuesCfg::default()
        };
        assert_eq!(
            c.resolved_dir(Path::new("x/roster.json")),
            PathBuf::from("d")
        );
        assert_eq!(
            c.resolved_evidence_dir(Path::new("x/roster.json")),
            PathBuf::from("e")
        );
    }

    #[tokio::test]
    async fn the_background_tasks_save_and_stop_cleanly() {
        let dir = temp_dir("svc-tasks");
        let issues = Arc::new(open(&cfg(&dir), &dir));
        let tasks = issues.start(None);
        issues.report_at(kicked(1).at(T), T);
        tasks.stop().await;
        let text = std::fs::read_to_string(dir.join("issues").join(BOOK_FILE)).unwrap();
        assert!(text.contains("\"reason=3\""));
    }
}
