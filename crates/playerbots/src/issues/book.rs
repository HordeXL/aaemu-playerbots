//! The book of issues: deduplication, counting, the "is this worth telling the team" policy, the
//! rate limits of the queue sink and the state of each issue.
//!
//! [`IssueBook`] is plain data and does no I/O and no clock reading (every method that needs the time
//! takes it), so it is tested without a runtime. The [`super::Issues`] service owns one behind a
//! mutex and does the writing.
//!
//! # Policy
//!
//! An issue is *reportable* ([`Policy::reportable`]) when its suspect is not [`Suspect::Bot`] and
//!
//! * it is of an [always-report kind](IssueKind::always_report) and was seen once with
//!   [`Severity::High`], or
//! * it was seen [`Policy::min_occurrences`] times, or by [`Policy::min_bots`] different bots.
//!
//! Bot-suspect issues are kept and shown in the web UI, but never queued by themselves.
//!
//! # Limits
//!
//! A queue entry is written for a reportable issue in state [`IssueState::Open`], most severe and
//! most frequent first, as long as fewer than [`Policy::max_per_hour`] entries went out in the last
//! hour and fewer than [`Policy::max_per_kind_per_day`] of the same kind in the last 24 hours. The
//! others wait (and stay visible in the web UI). The entries written are remembered in the book, so
//! a restart does not reset the limits.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::model::{fingerprint, parse_fingerprint, IssueKind, IssueReport, Severity, Suspect};
use crate::config::{IssuesCfg, QueueMode};

/// Version of the `issues.json` format.
pub const BOOK_VERSION: u32 = 1;
/// Most issues the book keeps.
pub const MAX_RECORDS: usize = 500;
/// Samples kept per issue: the first and the latest two.
pub const MAX_SAMPLES: usize = 3;
/// Bot ids listed per issue.
pub const MAX_BOT_IDS: usize = 16;
/// A queued issue that is seen again this long after it was queued (or last looked up in the queue
/// file) makes the sink check whether its entry is still in the file.
pub const RECURRENCE_AFTER_MS: u64 = 30 * 60_000;
/// Evidence of an issue is refreshed at most this often ...
pub const EVIDENCE_REFRESH_MS: u64 = 5 * 60_000;
/// ... and written at most this many times.
pub const MAX_EVIDENCE_WRITES: u8 = 6;
/// An hour in milliseconds.
pub const HOUR_MS: u64 = 3_600_000;
/// A day in milliseconds.
pub const DAY_MS: u64 = 24 * HOUR_MS;
/// An issue not seen for this long is the first to be evicted when the book is full.
const STALE_MS: u64 = 14 * DAY_MS;
/// Most jobs [`IssueBook::plan`] hands out at once.
const MAX_JOBS: usize = 16;

/// The numbers the policy needs, from `[issues]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Occurrences that make an issue reportable.
    pub min_occurrences: u32,
    /// Distinct bots that make an issue reportable.
    pub min_bots: u32,
    /// Queue entries per hour, all kinds together.
    pub max_per_hour: u32,
    /// New queue entries per kind per 24 hours.
    pub max_per_kind_per_day: u32,
    /// Issues kept.
    pub capacity: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            min_occurrences: 3,
            min_bots: 2,
            max_per_hour: 6,
            max_per_kind_per_day: 2,
            capacity: MAX_RECORDS,
        }
    }
}

impl Policy {
    /// The policy of a configuration.
    pub fn from_cfg(cfg: &IssuesCfg) -> Policy {
        Policy {
            min_occurrences: cfg.min_occurrences.max(1),
            min_bots: cfg.min_bots.max(1),
            max_per_hour: cfg.max_queue_per_hour,
            max_per_kind_per_day: cfg.max_queue_per_kind_per_day,
            capacity: MAX_RECORDS,
        }
    }

    /// Whether the team should hear about `rec` (its state is not looked at).
    pub fn reportable(&self, rec: &IssueRecord) -> bool {
        if !rec.suspect.is_reportable() {
            return false;
        }
        if rec.kind.always_report() && rec.severity >= Severity::High {
            return rec.count >= 1;
        }
        rec.count >= u64::from(self.min_occurrences) || rec.bots.total >= self.min_bots
    }
}

/// Where an issue stands. `{"which":"queued","at_ms":...}` in JSON.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(tag = "which", rename_all = "snake_case")]
pub enum IssueState {
    /// Collected, not (yet) told to the team.
    #[default]
    Open,
    /// An entry with the marker was appended to the queue file.
    Queued {
        /// When the entry was written (unix ms).
        at_ms: u64,
        /// The marker in the entry (`pb:...`), what the recurrence check looks for.
        marker: String,
        /// The occurrence count when it was queued.
        #[serde(default)]
        count_at: u64,
        /// When the queue file was last searched for the marker (unix ms).
        #[serde(default)]
        checked_ms: u64,
    },
    /// The operator does not want to hear about it.
    Ignored,
    /// The operator says it is fixed.
    Resolved {
        /// When (unix ms).
        #[serde(default)]
        at_ms: u64,
    },
}

impl IssueState {
    /// `open`, `queued`, `ignored`, `resolved`.
    pub fn name(&self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Queued { .. } => "queued",
            IssueState::Ignored => "ignored",
            IssueState::Resolved { .. } => "resolved",
        }
    }
}

/// The bots that met an issue: the first [`MAX_BOT_IDS`] ids and how many there were. The total is
/// exact up to [`MAX_BOT_IDS`] bots; beyond that a bot that is not in the list counts again each time
/// it reports, so the number can only be too high there.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct BotSet {
    /// The first bots.
    pub ids: Vec<u32>,
    /// How many different bots (see above).
    pub total: u32,
}

impl BotSet {
    fn add(&mut self, id: u32) {
        if self.ids.contains(&id) {
            return;
        }
        if self.ids.len() < MAX_BOT_IDS {
            self.ids.push(id);
        }
        self.total = self.total.saturating_add(1);
    }
}

/// Everything the book knows about one issue.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct IssueRecord {
    /// `pb:...`.
    pub fingerprint: String,
    /// The kind.
    pub kind: IssueKind,
    /// The dedup key.
    pub key: String,
    /// The latest title.
    pub title: String,
    /// The latest suspect.
    pub suspect: Suspect,
    /// The highest severity seen.
    pub severity: Severity,
    /// The latest area hint.
    #[serde(default)]
    pub area: Option<String>,
    /// Occurrences.
    pub count: u64,
    /// The bots that met it.
    pub bots: BotSet,
    /// First occurrence (unix ms).
    pub first_seen_ms: u64,
    /// Latest occurrence (unix ms).
    pub last_seen_ms: u64,
    /// The first report and the latest two.
    pub samples: Vec<IssueReport>,
    /// Where it stands.
    #[serde(default)]
    pub state: IssueState,
    /// The evidence folder, once written.
    #[serde(default)]
    pub evidence: Option<String>,
    /// When the evidence was last written (unix ms).
    #[serde(default)]
    pub evidence_at_ms: u64,
    /// How often the evidence was written.
    #[serde(default)]
    pub evidence_writes: u8,
    /// A sample arrived since the evidence was written.
    #[serde(default)]
    pub evidence_dirty: bool,
    /// The operator asked for a queue entry.
    #[serde(default)]
    pub want_queue: bool,
    /// The sink should look for the marker in the queue file.
    #[serde(default)]
    pub recur_due: bool,
}

impl IssueRecord {
    fn new(fp: &str, r: &IssueReport) -> IssueRecord {
        IssueRecord {
            fingerprint: fp.to_string(),
            kind: r.kind,
            key: r.key.clone(),
            title: r.title.clone(),
            suspect: r.suspect,
            severity: r.severity,
            area: r.area.clone(),
            count: 0,
            bots: BotSet::default(),
            first_seen_ms: r.at_ms,
            last_seen_ms: r.at_ms,
            samples: Vec::new(),
            state: IssueState::Open,
            evidence: None,
            evidence_at_ms: 0,
            evidence_writes: 0,
            evidence_dirty: false,
            want_queue: false,
            recur_due: false,
        }
    }

    fn absorb(&mut self, report: IssueReport) {
        self.count += 1;
        if let Some(b) = &report.bot {
            self.bots.add(b.id);
        }
        self.first_seen_ms = self.first_seen_ms.min(report.at_ms);
        self.last_seen_ms = self.last_seen_ms.max(report.at_ms);
        self.title.clone_from(&report.title);
        self.suspect = report.suspect;
        self.severity = self.severity.max(report.severity);
        if report.area.is_some() {
            self.area.clone_from(&report.area);
        }
        if self.samples.len() >= MAX_SAMPLES {
            // (the first sample stays; of the others the oldest goes)
            self.samples.remove(1);
        }
        self.samples.push(report);
    }

    /// The first sample.
    pub fn first_sample(&self) -> Option<&IssueReport> {
        self.samples.first()
    }

    /// The latest sample.
    pub fn latest_sample(&self) -> Option<&IssueReport> {
        self.samples.last()
    }

    /// Whether the state is [`IssueState::Queued`].
    pub fn is_queued(&self) -> bool {
        matches!(self.state, IssueState::Queued { .. })
    }
}

/// What [`IssueBook::record`] tells the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The fingerprint of the issue.
    pub fingerprint: String,
    /// Occurrences so far.
    pub count: u64,
    /// The issue was not in the book before.
    pub is_new: bool,
    /// The policy says the team should hear about it.
    pub reportable: bool,
    /// The background sink has something to do for it.
    pub wake: bool,
}

/// What the operator can do with an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Write a queue entry now (whatever the policy and the limits say).
    Queue,
    /// Never queue it.
    Ignore,
    /// It is fixed.
    Resolve,
    /// Back to open.
    Reopen,
}

/// Why a queue entry has to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitHit {
    /// The entries per hour are used up.
    Hourly,
    /// The entries of this kind per day are used up.
    Kind,
}

/// What the sink should do for one issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueJob {
    /// Append the first entry of a reportable issue.
    New,
    /// Append an entry because the operator asked.
    Manual,
    /// Look for the marker in the queue file; append a recurrence entry if it is gone.
    RecurrenceCheck,
}

/// One unit of work for the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The issue.
    pub fp: String,
    /// Write (or refresh) the evidence folder.
    pub evidence: bool,
    /// What to do with the queue file.
    pub queue: Option<QueueJob>,
}

/// The counts of issues per state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// In state open.
    pub open: u32,
    /// In state queued.
    pub queued: u32,
    /// In state ignored.
    pub ignored: u32,
    /// In state resolved.
    pub resolved: u32,
}

#[derive(Serialize)]
struct BookView<'a> {
    version: u32,
    saved_at_ms: u64,
    records: Vec<&'a IssueRecord>,
    queue_log: &'a [(u64, IssueKind)],
}

#[derive(Deserialize)]
struct BookFile {
    version: u32,
    #[serde(default)]
    records: Vec<IssueRecord>,
    #[serde(default)]
    queue_log: Vec<(u64, IssueKind)>,
}

/// All issues. See the module docs.
#[derive(Debug)]
pub struct IssueBook {
    policy: Policy,
    records: HashMap<String, IssueRecord>,
    /// When queue entries were written (unix ms), for the limits; entries older than a day go.
    queue_log: Vec<(u64, IssueKind)>,
}

impl IssueBook {
    /// An empty book.
    pub fn new(policy: Policy) -> IssueBook {
        IssueBook {
            policy,
            records: HashMap::new(),
            queue_log: Vec::new(),
        }
    }

    /// The policy in force.
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// Number of issues.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the book is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The issue with this fingerprint (`pb:abcd1234` or just `abcd1234`).
    pub fn get(&self, fp: &str) -> Option<&IssueRecord> {
        self.records.get(&parse_fingerprint(fp)?)
    }

    /// All issues, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &IssueRecord> {
        self.records.values()
    }

    /// Whether the policy makes `rec` reportable.
    pub fn is_reportable(&self, rec: &IssueRecord) -> bool {
        self.policy.reportable(rec)
    }

    /// Counts per state.
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in self.records.values() {
            match r.state {
                IssueState::Open => c.open += 1,
                IssueState::Queued { .. } => c.queued += 1,
                IssueState::Ignored => c.ignored += 1,
                IssueState::Resolved { .. } => c.resolved += 1,
            }
        }
        c
    }

    /// Counts `report` (already [normalized](IssueReport::normalize)) at `now_ms`.
    pub fn record(&mut self, mut report: IssueReport, now_ms: u64) -> Recorded {
        if report.at_ms == 0 {
            report.at_ms = now_ms;
        }
        let fp = report.fingerprint();
        let is_new = !self.records.contains_key(&fp);
        if is_new {
            self.make_room(now_ms);
            self.records
                .insert(fp.clone(), IssueRecord::new(&fp, &report));
        }
        let policy = self.policy;
        let Some(rec) = self.records.get_mut(&fp) else {
            // (cannot happen: it was inserted above)
            return Recorded {
                fingerprint: fp,
                count: 0,
                is_new,
                reportable: false,
                wake: false,
            };
        };
        // A fixed issue that comes back later is a new problem: start counting again.
        if let IssueState::Resolved { at_ms } = rec.state {
            if now_ms >= at_ms.saturating_add(RECURRENCE_AFTER_MS) {
                *rec = IssueRecord::new(&fp, &report);
            }
        }
        rec.absorb(report);
        let reportable = policy.reportable(rec);
        if reportable || rec.is_queued() || rec.want_queue {
            rec.evidence_dirty = true;
        }
        if let IssueState::Queued {
            at_ms, checked_ms, ..
        } = rec.state
        {
            let quiet = at_ms.max(checked_ms).saturating_add(RECURRENCE_AFTER_MS);
            if now_ms >= quiet {
                rec.recur_due = true;
            }
        }
        let wake = (reportable && matches!(rec.state, IssueState::Open))
            || rec.recur_due
            || rec.want_queue
            || (rec.is_queued() && rec.evidence_dirty);
        Recorded {
            fingerprint: fp,
            count: rec.count,
            is_new,
            reportable,
            wake,
        }
    }

    /// Evicts issues until there is room for one more: queued ones last, then the ones not seen
    /// for two weeks, then the ones seen the least, the oldest first.
    fn make_room(&mut self, now_ms: u64) {
        self.evict_down_to(self.policy.capacity.max(1) - 1, now_ms);
    }

    /// Evicts (see [`IssueBook::make_room`]) until at most `keep` issues are left.
    fn evict_down_to(&mut self, keep: usize, now_ms: u64) {
        while self.records.len() > keep {
            let victim = self
                .records
                .values()
                .min_by_key(|r| {
                    (
                        r.is_queued() || r.want_queue,
                        r.last_seen_ms.saturating_add(STALE_MS) > now_ms,
                        r.count,
                        r.last_seen_ms,
                    )
                })
                .map(|r| r.fingerprint.clone());
            match victim {
                Some(v) => {
                    self.records.remove(&v);
                }
                None => break,
            }
        }
    }

    // ---- limits ----------------------------------------------------------------------------

    /// Why an entry of `kind` may not go out at `now_ms`, if it may not. `pending` are entries
    /// that are about to go out and count already.
    pub fn limit_hit(
        &self,
        kind: IssueKind,
        now_ms: u64,
        pending: &[(u64, IssueKind)],
    ) -> Option<LimitHit> {
        let entries = || self.queue_log.iter().chain(pending);
        let hour = entries()
            .filter(|(t, _)| t.saturating_add(HOUR_MS) > now_ms)
            .count();
        if hour >= self.policy.max_per_hour as usize {
            return Some(LimitHit::Hourly);
        }
        let day = entries()
            .filter(|(t, k)| *k == kind && t.saturating_add(DAY_MS) > now_ms)
            .count();
        (day >= self.policy.max_per_kind_per_day as usize).then_some(LimitHit::Kind)
    }

    fn note_queued(&mut self, kind: IssueKind, now_ms: u64) {
        self.queue_log.push((now_ms, kind));
        self.queue_log
            .retain(|(t, _)| t.saturating_add(DAY_MS) > now_ms);
    }

    /// Entries written in the last hour.
    pub fn queued_last_hour(&self, now_ms: u64) -> usize {
        self.queue_log
            .iter()
            .filter(|(t, _)| t.saturating_add(HOUR_MS) > now_ms)
            .count()
    }

    // ---- what the sink does ----------------------------------------------------------------

    /// The work the sink has at `now_ms`: manual queue requests, recurrence checks, new entries for
    /// reportable issues (when `mode` is [`QueueMode::Auto`] and the limits allow) and evidence
    /// folders to write. `queue_file` says whether there is a queue file at all.
    pub fn plan(&self, now_ms: u64, mode: QueueMode, queue_file: bool) -> Vec<Job> {
        let can_queue = queue_file && mode != QueueMode::Off;
        let mut jobs: Vec<Job> = Vec::new();
        let mut pending: Vec<(u64, IssueKind)> = Vec::new();

        let mut manual: Vec<&IssueRecord> = Vec::new();
        let mut recur: Vec<&IssueRecord> = Vec::new();
        let mut auto: Vec<&IssueRecord> = Vec::new();
        let mut evidence_only: Vec<&IssueRecord> = Vec::new();
        for r in self.records.values() {
            if can_queue && r.want_queue && !r.is_queued() {
                manual.push(r);
            } else if can_queue && r.recur_due && r.is_queued() {
                recur.push(r);
            } else if can_queue
                && mode == QueueMode::Auto
                && matches!(r.state, IssueState::Open)
                && self.policy.reportable(r)
            {
                auto.push(r);
            } else if self.evidence_due(r, now_ms) {
                evidence_only.push(r);
            }
        }
        let by_priority = |a: &&IssueRecord, b: &&IssueRecord| {
            b.severity
                .cmp(&a.severity)
                .then(b.count.cmp(&a.count))
                .then(a.first_seen_ms.cmp(&b.first_seen_ms))
                .then(a.fingerprint.cmp(&b.fingerprint))
        };
        manual.sort_by(by_priority);
        recur.sort_by(by_priority);
        auto.sort_by(by_priority);
        evidence_only.sort_by(by_priority);

        for r in manual {
            pending.push((now_ms, r.kind));
            jobs.push(Job {
                fp: r.fingerprint.clone(),
                evidence: r.evidence.is_none() || r.evidence_dirty,
                queue: Some(QueueJob::Manual),
            });
        }
        for r in recur {
            jobs.push(Job {
                fp: r.fingerprint.clone(),
                evidence: false,
                queue: Some(QueueJob::RecurrenceCheck),
            });
        }
        for r in auto {
            if self.limit_hit(r.kind, now_ms, &pending).is_some() {
                // (it waits; the evidence is still worth having)
                if self.evidence_due(r, now_ms) {
                    jobs.push(Job {
                        fp: r.fingerprint.clone(),
                        evidence: true,
                        queue: None,
                    });
                }
                continue;
            }
            pending.push((now_ms, r.kind));
            jobs.push(Job {
                fp: r.fingerprint.clone(),
                evidence: r.evidence.is_none() || r.evidence_dirty,
                queue: Some(QueueJob::New),
            });
        }
        for r in evidence_only {
            jobs.push(Job {
                fp: r.fingerprint.clone(),
                evidence: true,
                queue: None,
            });
        }
        jobs.truncate(MAX_JOBS);
        jobs
    }

    /// Whether the evidence folder of `r` should be written (or refreshed) now.
    fn evidence_due(&self, r: &IssueRecord, now_ms: u64) -> bool {
        let live = matches!(r.state, IssueState::Open | IssueState::Queued { .. });
        let wanted = live && (self.policy.reportable(r) || r.is_queued() || r.want_queue);
        if !wanted || !r.evidence_dirty || r.evidence_writes >= MAX_EVIDENCE_WRITES {
            return false;
        }
        r.evidence.is_none() || now_ms >= r.evidence_at_ms.saturating_add(EVIDENCE_REFRESH_MS)
    }

    /// The evidence of `fp` was written to `folder`.
    pub fn mark_evidence(&mut self, fp: &str, folder: String, now_ms: u64) {
        if let Some(r) = self.records.get_mut(fp) {
            r.evidence = Some(folder);
            r.evidence_at_ms = now_ms;
            r.evidence_writes = r.evidence_writes.saturating_add(1);
            r.evidence_dirty = false;
        }
    }

    /// An entry for `fp` was appended to the queue file.
    pub fn mark_queued(&mut self, fp: &str, now_ms: u64) {
        let Some(r) = self.records.get_mut(fp) else {
            return;
        };
        r.state = IssueState::Queued {
            at_ms: now_ms,
            marker: r.fingerprint.clone(),
            count_at: r.count,
            checked_ms: now_ms,
        };
        r.want_queue = false;
        r.recur_due = false;
        let kind = r.kind;
        self.note_queued(kind, now_ms);
    }

    /// The marker of `fp` is still in the queue file: nothing to do until the next quiet period.
    pub fn mark_checked(&mut self, fp: &str, now_ms: u64) {
        if let Some(r) = self.records.get_mut(fp) {
            if let IssueState::Queued { checked_ms, .. } = &mut r.state {
                *checked_ms = now_ms;
            }
            r.recur_due = false;
        }
    }

    /// Replaces the server log lines of the samples whose time is `at_ms` (the correlation is redone
    /// when the evidence is written, because the lines may have arrived after the report).
    pub fn set_server_logs(&mut self, fp: &str, logs: Vec<(u64, Vec<String>)>) {
        let Some(r) = self.records.get_mut(fp) else {
            return;
        };
        for (at_ms, lines) in logs {
            if lines.is_empty() {
                continue;
            }
            for s in r.samples.iter_mut().filter(|s| s.at_ms == at_ms) {
                s.server_log.clone_from(&lines);
            }
        }
    }

    /// Applies an operator action.
    pub fn apply_action(&mut self, fp: &str, action: Action, now_ms: u64) -> Result<(), String> {
        let Some(r) = parse_fingerprint(fp).and_then(|f| self.records.get_mut(&f)) else {
            return Err(format!("no issue {fp}"));
        };
        match action {
            Action::Queue => {
                if r.is_queued() {
                    return Err(
                        "already queued (it is queued again by itself when it recurs after its entry was removed)"
                            .into(),
                    );
                }
                r.want_queue = true;
                r.evidence_dirty = true;
            }
            Action::Ignore => {
                r.state = IssueState::Ignored;
                r.want_queue = false;
                r.recur_due = false;
            }
            Action::Resolve => {
                r.state = IssueState::Resolved { at_ms: now_ms };
                r.want_queue = false;
                r.recur_due = false;
            }
            Action::Reopen => {
                r.state = IssueState::Open;
                r.want_queue = false;
                r.recur_due = false;
            }
        }
        Ok(())
    }

    // ---- persistence -----------------------------------------------------------------------

    /// The text of `issues.json`.
    pub fn to_json(&self, now_ms: u64) -> String {
        let mut records: Vec<&IssueRecord> = self.records.values().collect();
        records.sort_by(|a, b| {
            a.first_seen_ms
                .cmp(&b.first_seen_ms)
                .then(a.fingerprint.cmp(&b.fingerprint))
        });
        let view = BookView {
            version: BOOK_VERSION,
            saved_at_ms: now_ms,
            records,
            queue_log: &self.queue_log,
        };
        // (plain data: serializing cannot fail; an empty string would fail to load, loudly)
        serde_json::to_string(&view).unwrap_or_default()
    }

    /// Reads the text of `issues.json`.
    pub fn from_json(text: &str, policy: Policy) -> anyhow::Result<IssueBook> {
        let file: BookFile = serde_json::from_str(text)?;
        anyhow::ensure!(
            file.version == BOOK_VERSION,
            "issue book version {} is not supported (expected {BOOK_VERSION})",
            file.version
        );
        let mut book = IssueBook::new(policy);
        for mut r in file.records {
            // (the file may have been edited: keep it within the bounds)
            r.fingerprint = fingerprint(r.kind, &r.key);
            r.samples.truncate(MAX_SAMPLES);
            r.bots.ids.truncate(MAX_BOT_IDS);
            book.records.insert(r.fingerprint.clone(), r);
        }
        let newest = book
            .records
            .values()
            .map(|r| r.last_seen_ms)
            .max()
            .unwrap_or(0);
        book.evict_down_to(policy.capacity.max(1), newest);
        book.queue_log = file.queue_log;
        Ok(book)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::model::BotRef;

    const T0: u64 = 1_800_000_000_000;

    fn report(kind: IssueKind, key: &str, bot: u32) -> IssueReport {
        let mut r = IssueReport::new(kind, key, format!("title of {key}"))
            .expected("e")
            .actual("a")
            .suspect(Suspect::Server)
            .bot(BotRef {
                id: bot,
                name: format!("Bot{bot}"),
                ..BotRef::default()
            });
        r.normalize(T0);
        r
    }

    fn book() -> IssueBook {
        IssueBook::new(Policy::default())
    }

    #[test]
    fn equal_reports_are_one_issue_and_are_counted() {
        let mut b = book();
        let a = b.record(report(IssueKind::NpcMissing, "npc=1", 1).at(T0), T0);
        assert!(a.is_new && a.count == 1 && !a.reportable);
        let again = b.record(
            report(IssueKind::NpcMissing, "npc=1", 1).at(T0 + 5_000),
            T0 + 5_000,
        );
        assert!(!again.is_new && again.count == 2);
        assert_eq!(again.fingerprint, a.fingerprint);
        // another key, another kind: other issues
        b.record(report(IssueKind::NpcMissing, "npc=2", 1), T0);
        b.record(report(IssueKind::DoodadMissing, "npc=1", 1), T0);
        assert_eq!(b.len(), 3);
        let r = b.get(&a.fingerprint).unwrap();
        assert_eq!(
            (r.count, r.first_seen_ms, r.last_seen_ms),
            (2, T0, T0 + 5_000)
        );
        assert_eq!(
            r.bots,
            BotSet {
                ids: vec![1],
                total: 1
            }
        );
        // lookups take the bare hex too
        assert!(b.get(&a.fingerprint[3..]).is_some());
        assert!(b.get("nonsense").is_none());
    }

    #[test]
    fn distinct_bots_are_counted_and_the_list_is_bounded() {
        let mut b = book();
        let mut fp = String::new();
        for bot in 1..=40u32 {
            fp = b
                .record(report(IssueKind::NpcMissing, "npc=1", bot), T0)
                .fingerprint;
        }
        // the same bot again does not add
        b.record(report(IssueKind::NpcMissing, "npc=1", 1), T0);
        let r = b.get(&fp).unwrap();
        assert_eq!(r.bots.ids.len(), MAX_BOT_IDS);
        assert_eq!(r.bots.ids[0], 1);
        assert_eq!(
            r.bots.total, 40,
            "bots 17..=40 are not in the list, so each counts when seen"
        );
        assert_eq!(r.count, 41);
        // a report without a bot counts as an occurrence but not as a bot
        let mut b = book();
        let mut anon = report(IssueKind::Other, "x", 1);
        anon.bot = None;
        let fp = b.record(anon, T0).fingerprint;
        assert_eq!(
            (b.get(&fp).unwrap().count, b.get(&fp).unwrap().bots.total),
            (1, 0)
        );
    }

    #[test]
    fn samples_are_the_first_and_the_latest_two() {
        let mut b = book();
        let mut fp = String::new();
        for i in 0..6u64 {
            let r = report(IssueKind::NpcMissing, "npc=1", 1).at(T0 + i);
            let mut r = r;
            r.actual = format!("occurrence {i}");
            fp = b.record(r, T0 + i).fingerprint;
        }
        let r = b.get(&fp).unwrap();
        let actuals: Vec<&str> = r.samples.iter().map(|s| s.actual.as_str()).collect();
        assert_eq!(
            actuals,
            vec!["occurrence 0", "occurrence 4", "occurrence 5"]
        );
        assert_eq!(r.count, 6);
        assert_eq!(r.latest_sample().unwrap().actual, "occurrence 5");
        assert_eq!(r.first_sample().unwrap().actual, "occurrence 0");
    }

    #[test]
    fn the_book_is_bounded_and_evicts_the_oldest_low_count_unqueued_issue() {
        let mut b = IssueBook::new(Policy {
            capacity: 5,
            ..Policy::default()
        });
        // four issues with growing counts, the last one queued
        for i in 1..=4u32 {
            for n in 0..i {
                b.record(
                    report(IssueKind::Other, &format!("k{i}"), 1),
                    T0 + u64::from(i * 10 + n),
                );
            }
        }
        let queued_fp = fingerprint(IssueKind::Other, "k1");
        b.mark_queued(&queued_fp, T0 + 100);
        b.record(report(IssueKind::Other, "k5", 1), T0 + 200);
        assert_eq!(b.len(), 5);
        // a sixth evicts the lowest-count unqueued one: k1 has count 1 but is queued, so k5 (count 1) goes
        b.record(report(IssueKind::Other, "k6", 1), T0 + 300);
        assert_eq!(b.len(), 5);
        assert!(
            b.get(&queued_fp).is_some(),
            "queued issues are evicted last"
        );
        assert!(b.get(&fingerprint(IssueKind::Other, "k5")).is_none());
        assert!(b.get(&fingerprint(IssueKind::Other, "k4")).is_some());
        // a stale issue goes before a fresher one with a lower count
        let mut b = IssueBook::new(Policy {
            capacity: 2,
            ..Policy::default()
        });
        for _ in 0..5 {
            b.record(report(IssueKind::Other, "old", 1), T0);
        }
        let later = T0 + 20 * DAY_MS;
        b.record(report(IssueKind::Other, "fresh", 1).at(later), later);
        b.record(report(IssueKind::Other, "new", 1).at(later + 1), later + 1);
        assert!(
            b.get(&fingerprint(IssueKind::Other, "old")).is_none(),
            "stale first"
        );
        assert!(b.get(&fingerprint(IssueKind::Other, "fresh")).is_some());
        // the real capacity is the documented one
        assert_eq!(Policy::default().capacity, 500);
        let mut big = book();
        for i in 0..600 {
            big.record(report(IssueKind::Other, &format!("key{i}"), 1), T0 + i);
        }
        assert_eq!(big.len(), 500);
    }

    #[test]
    fn reportable_needs_a_server_side_suspect_and_enough_evidence() {
        let mut b = book();
        let fp = |b: &mut IssueBook, r: IssueReport| b.record(r, T0);
        // an ordinary kind: three occurrences ...
        let r = report(IssueKind::QuestRefused, "q=1", 1);
        assert!(!fp(&mut b, r.clone()).reportable);
        assert!(!fp(&mut b, r.clone()).reportable);
        assert!(fp(&mut b, r).reportable);
        // ... or two different bots
        assert!(!fp(&mut b, report(IssueKind::QuestRefused, "q=2", 1)).reportable);
        assert!(fp(&mut b, report(IssueKind::QuestRefused, "q=2", 2)).reportable);
        // an always-report kind: once, when high
        let high = report(IssueKind::DeathWithoutPacket, "d", 1).severity(Severity::High);
        assert!(fp(&mut b, high).reportable);
        let medium = report(IssueKind::MalformedPacket, "m", 1).severity(Severity::Medium);
        assert!(
            !fp(&mut b, medium).reportable,
            "medium needs the usual evidence"
        );
        let other_high = report(IssueKind::QuestRefused, "h", 1).severity(Severity::High);
        assert!(
            !fp(&mut b, other_high).reportable,
            "high is not enough for an ordinary kind"
        );
        // data and unknown suspects count, a bot suspect never does
        let data = report(IssueKind::NpcMissing, "n", 1).suspect(Suspect::Data);
        for _ in 0..3 {
            fp(&mut b, data.clone());
        }
        assert!(b.is_reportable(b.get(&fingerprint(IssueKind::NpcMissing, "n")).unwrap()));
        let unknown = report(IssueKind::NpcMissing, "u", 1).suspect(Suspect::Unknown);
        let unknown2 = report(IssueKind::NpcMissing, "u", 2).suspect(Suspect::Unknown);
        fp(&mut b, unknown);
        assert!(fp(&mut b, unknown2).reportable);
        let bot_side = report(IssueKind::DeathWithoutPacket, "bs", 1)
            .severity(Severity::High)
            .suspect(Suspect::Bot);
        for bot in 1..=5 {
            let mut r = bot_side.clone();
            r.bot.as_mut().unwrap().id = bot;
            assert!(!fp(&mut b, r).reportable, "bot-suspect issues stay local");
        }
        assert_eq!(
            b.get(&fingerprint(IssueKind::DeathWithoutPacket, "bs"))
                .unwrap()
                .count,
            5
        );
        // the policy numbers come from the config
        let cfg = IssuesCfg {
            min_occurrences: 5,
            min_bots: 4,
            max_queue_per_hour: 1,
            max_queue_per_kind_per_day: 9,
            ..IssuesCfg::default()
        };
        let p = Policy::from_cfg(&cfg);
        assert_eq!(
            (
                p.min_occurrences,
                p.min_bots,
                p.max_per_hour,
                p.max_per_kind_per_day
            ),
            (5, 4, 1, 9)
        );
    }

    #[test]
    fn the_highest_severity_and_the_latest_judgement_win() {
        let mut b = book();
        let low = report(IssueKind::NpcMissing, "n", 1)
            .severity(Severity::Low)
            .suspect(Suspect::Bot);
        let fp = b.record(low, T0).fingerprint;
        let high = report(IssueKind::NpcMissing, "n", 1)
            .severity(Severity::High)
            .suspect(Suspect::Server)
            .area("rust/crates/aaemu-game/src/systems/npcs/");
        b.record(high, T0 + 1);
        let r = b.get(&fp).unwrap();
        assert_eq!((r.severity, r.suspect), (Severity::High, Suspect::Server));
        assert_eq!(
            r.area.as_deref(),
            Some("rust/crates/aaemu-game/src/systems/npcs/")
        );
        let later = report(IssueKind::NpcMissing, "n", 1)
            .severity(Severity::Low)
            .suspect(Suspect::Data);
        b.record(later, T0 + 2);
        let r = b.get(&fp).unwrap();
        assert_eq!((r.severity, r.suspect), (Severity::High, Suspect::Data));
        assert_eq!(
            r.area.as_deref(),
            Some("rust/crates/aaemu-game/src/systems/npcs/"),
            "an area is not forgotten"
        );
    }

    /// Makes `n` distinct always-report issues of `kind` and returns their fingerprints.
    fn reportable_issues(b: &mut IssueBook, kind: IssueKind, n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let r = report(kind, &format!("{}-{i}", kind.as_str()), 1).severity(Severity::High);
                b.record(r, T0).fingerprint
            })
            .collect()
    }

    #[test]
    fn the_queue_limits_are_per_hour_and_per_kind_and_later_entries_wait() {
        let mut b = book();
        let kicked = reportable_issues(&mut b, IssueKind::Kicked, 4);
        let malformed = reportable_issues(&mut b, IssueKind::MalformedPacket, 4);
        let death = reportable_issues(&mut b, IssueKind::DeathWithoutPacket, 4);

        let jobs = b.plan(T0, QueueMode::Auto, true);
        let queue: Vec<&Job> = jobs
            .iter()
            .filter(|j| j.queue == Some(QueueJob::New))
            .collect();
        // 2 per kind, and 6 per hour overall: exactly the three kinds' quotas
        assert_eq!(queue.len(), 6, "{jobs:#?}");
        for kind in [
            IssueKind::Kicked,
            IssueKind::MalformedPacket,
            IssueKind::DeathWithoutPacket,
        ] {
            let n = queue
                .iter()
                .filter(|j| b.get(&j.fp).unwrap().kind == kind)
                .count();
            assert_eq!(n, 2, "{kind}");
        }
        // the waiting ones still get their evidence
        assert!(jobs.iter().any(|j| j.queue.is_none() && j.evidence));
        // the hourly cap: lower it and only one kind's pair goes
        let mut tight = IssueBook::new(Policy {
            max_per_hour: 3,
            ..Policy::default()
        });
        reportable_issues(&mut tight, IssueKind::Kicked, 2);
        reportable_issues(&mut tight, IssueKind::MalformedPacket, 2);
        let n = tight
            .plan(T0, QueueMode::Auto, true)
            .iter()
            .filter(|j| j.queue == Some(QueueJob::New))
            .count();
        assert_eq!(n, 3);

        // write the two kicked entries: no more kicked ones for a day, the others are not affected
        for fp in &kicked[..2] {
            b.mark_queued(fp, T0);
        }
        assert_eq!(
            b.limit_hit(IssueKind::Kicked, T0 + HOUR_MS + 1, &[]),
            Some(LimitHit::Kind)
        );
        assert_eq!(b.limit_hit(IssueKind::Kicked, T0 + DAY_MS + 1, &[]), None);
        assert_eq!(b.limit_hit(IssueKind::Other, T0 + 1, &[]), None);
        let later = b.plan(T0 + HOUR_MS + 1, QueueMode::Auto, true);
        assert!(!later
            .iter()
            .any(|j| j.queue == Some(QueueJob::New) && kicked.contains(&j.fp)));
        assert!(later
            .iter()
            .any(|j| j.queue == Some(QueueJob::New) && malformed.contains(&j.fp)));
        // after a day the waiting kicked entries go out
        let next_day = b.plan(T0 + DAY_MS + 1, QueueMode::Auto, true);
        assert!(next_day
            .iter()
            .any(|j| j.queue == Some(QueueJob::New) && kicked[2..].contains(&j.fp)));
        let _ = death;
        // the hourly window slides
        let mut one = IssueBook::new(Policy {
            max_per_hour: 1,
            ..Policy::default()
        });
        let fps = reportable_issues(&mut one, IssueKind::Kicked, 2);
        one.mark_queued(&fps[0], T0);
        assert_eq!(
            one.limit_hit(IssueKind::Kicked, T0 + HOUR_MS - 1, &[]),
            Some(LimitHit::Hourly)
        );
        assert_eq!(one.limit_hit(IssueKind::Kicked, T0 + HOUR_MS, &[]), None);
        assert_eq!(one.queued_last_hour(T0 + 10), 1);
        assert_eq!(one.queued_last_hour(T0 + HOUR_MS), 0);
    }

    #[test]
    fn the_most_severe_and_most_frequent_issue_is_queued_first() {
        let mut b = IssueBook::new(Policy {
            max_per_hour: 1,
            ..Policy::default()
        });
        let mild = b.record(
            report(IssueKind::QuestRefused, "mild", 1).severity(Severity::Medium),
            T0,
        );
        for bot in 2..=5 {
            b.record(report(IssueKind::QuestRefused, "mild", bot), T0);
        }
        let _ = mild;
        let loud = b
            .record(
                report(IssueKind::Kicked, "loud", 1).severity(Severity::High),
                T0,
            )
            .fingerprint;
        let jobs = b.plan(T0, QueueMode::Auto, true);
        let queued: Vec<&Job> = jobs
            .iter()
            .filter(|j| j.queue == Some(QueueJob::New))
            .collect();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].fp, loud, "high severity first");
    }

    #[test]
    fn the_modes_decide_who_may_queue() {
        let mut b = book();
        let fps = reportable_issues(&mut b, IssueKind::Kicked, 1);
        let fp = &fps[0];
        // auto and a file: queued by itself
        assert!(b
            .plan(T0, QueueMode::Auto, true)
            .iter()
            .any(|j| j.queue == Some(QueueJob::New)));
        // manual, off, or no file: not by itself, but the evidence is written
        for (mode, file) in [
            (QueueMode::Manual, true),
            (QueueMode::Off, true),
            (QueueMode::Auto, false),
        ] {
            let jobs = b.plan(T0, mode, file);
            assert!(jobs.iter().all(|j| j.queue.is_none()), "{mode:?} {file}");
            assert!(jobs.iter().any(|j| j.evidence), "{mode:?} {file}");
        }
        // the operator's request goes through in manual mode, even for a bot-suspect issue
        let mut bot_issue = report(IssueKind::Other, "b", 1).suspect(Suspect::Bot);
        bot_issue.severity = Severity::Low;
        let bfp = b.record(bot_issue, T0).fingerprint;
        b.apply_action(&bfp, Action::Queue, T0).unwrap();
        let jobs = b.plan(T0, QueueMode::Manual, true);
        assert!(jobs
            .iter()
            .any(|j| j.fp == bfp && j.queue == Some(QueueJob::Manual) && j.evidence));
        // ... but not when the queue is off
        assert!(b
            .plan(T0, QueueMode::Off, true)
            .iter()
            .all(|j| j.queue.is_none()));
        // an ignored issue is not queued by itself
        b.apply_action(fp, Action::Ignore, T0).unwrap();
        assert!(!b
            .plan(T0, QueueMode::Auto, true)
            .iter()
            .any(|j| j.fp == *fp && j.queue.is_some()));
        assert!(
            !b.plan(T0, QueueMode::Auto, true)
                .iter()
                .any(|j| j.fp == *fp),
            "no evidence for an issue nobody wants"
        );
        b.apply_action(fp, Action::Reopen, T0).unwrap();
        assert!(b
            .plan(T0, QueueMode::Auto, true)
            .iter()
            .any(|j| j.fp == *fp && j.queue == Some(QueueJob::New)));
    }

    #[test]
    fn actions_change_the_state_and_unknown_issues_are_an_error() {
        let mut b = book();
        let fp = reportable_issues(&mut b, IssueKind::Kicked, 1).remove(0);
        assert!(b.apply_action("pb:00000000", Action::Ignore, T0).is_err());
        assert!(b.apply_action("garbage", Action::Ignore, T0).is_err());
        b.apply_action(&fp, Action::Ignore, T0).unwrap();
        assert_eq!(b.get(&fp).unwrap().state, IssueState::Ignored);
        b.apply_action(&fp, Action::Resolve, T0 + 1).unwrap();
        assert_eq!(
            b.get(&fp).unwrap().state,
            IssueState::Resolved { at_ms: T0 + 1 }
        );
        b.apply_action(&fp, Action::Reopen, T0 + 2).unwrap();
        assert_eq!(b.get(&fp).unwrap().state, IssueState::Open);
        b.apply_action(&fp, Action::Queue, T0 + 3).unwrap();
        assert!(b.get(&fp).unwrap().want_queue);
        b.mark_queued(&fp, T0 + 4);
        let r = b.get(&fp).unwrap();
        assert!(!r.want_queue);
        assert_eq!(
            r.state,
            IssueState::Queued {
                at_ms: T0 + 4,
                marker: fp.clone(),
                count_at: 1,
                checked_ms: T0 + 4
            }
        );
        assert!(b
            .apply_action(&fp, Action::Queue, T0 + 5)
            .unwrap_err()
            .contains("already queued"));
        let c = b.counts();
        assert_eq!((c.open, c.queued, c.ignored, c.resolved), (0, 1, 0, 0));
    }

    #[test]
    fn a_resolved_issue_that_comes_back_starts_again_but_not_at_once() {
        let mut b = book();
        let r = report(IssueKind::Kicked, "k", 1).severity(Severity::High);
        let fp = b.record(r.clone(), T0).fingerprint;
        b.record(r.clone(), T0);
        b.apply_action(&fp, Action::Resolve, T0 + 1_000).unwrap();
        // right after the fix: still counted, stays resolved
        b.record(r.clone(), T0 + 2_000);
        assert!(matches!(
            b.get(&fp).unwrap().state,
            IssueState::Resolved { .. }
        ));
        assert_eq!(b.get(&fp).unwrap().count, 3);
        // an hour later it is a regression: open again, counted from one
        let back = b.record(r.at(T0 + HOUR_MS), T0 + HOUR_MS);
        let rec = b.get(&fp).unwrap();
        assert_eq!(rec.state, IssueState::Open);
        assert_eq!(rec.count, 1);
        assert!(back.reportable && back.wake);
        assert_eq!(rec.first_seen_ms, T0 + HOUR_MS);
        assert!(rec.evidence.is_none());
    }

    #[test]
    fn a_queued_issue_asks_for_a_recurrence_check_only_after_a_quiet_period() {
        let mut b = book();
        let r = report(IssueKind::Kicked, "k", 1).severity(Severity::High);
        let fp = b.record(r.clone(), T0).fingerprint;
        b.mark_queued(&fp, T0 + 1_000);
        // seen again soon: nothing to check
        let soon = b.record(r.clone(), T0 + 10 * 60_000);
        assert!(!b.get(&fp).unwrap().recur_due);
        assert!(soon.wake, "a queued issue still gets fresh evidence");
        // seen again 30 minutes after it was queued: check the file
        let late = b.record(r.clone(), T0 + 1_000 + RECURRENCE_AFTER_MS);
        assert!(b.get(&fp).unwrap().recur_due && late.wake);
        let jobs = b.plan(T0 + 2 * HOUR_MS, QueueMode::Auto, true);
        assert!(jobs
            .iter()
            .any(|j| j.fp == fp && j.queue == Some(QueueJob::RecurrenceCheck)));
        // the marker is still there: nothing more until another quiet period
        b.mark_checked(&fp, T0 + 2 * HOUR_MS);
        assert!(!b.get(&fp).unwrap().recur_due);
        b.record(r.clone(), T0 + 2 * HOUR_MS + 60_000);
        assert!(!b.get(&fp).unwrap().recur_due);
        b.record(r, T0 + 2 * HOUR_MS + RECURRENCE_AFTER_MS);
        assert!(b.get(&fp).unwrap().recur_due);
        // with the queue off nothing is checked
        assert!(b
            .plan(T0 + 3 * HOUR_MS, QueueMode::Off, true)
            .iter()
            .all(|j| j.queue.is_none()));
        // a recurrence entry restarts the clock
        b.mark_queued(&fp, T0 + 3 * HOUR_MS);
        assert!(!b.get(&fp).unwrap().recur_due);
    }

    #[test]
    fn evidence_is_written_when_reportable_and_refreshed_within_bounds() {
        let mut b = book();
        let r = report(IssueKind::Kicked, "k", 1).severity(Severity::High);
        let fp = b.record(r.clone(), T0).fingerprint;
        let due = |b: &IssueBook, t: u64| {
            b.plan(t, QueueMode::Off, false)
                .iter()
                .any(|j| j.fp == fp && j.evidence)
        };
        assert!(due(&b, T0));
        b.mark_evidence(&fp, "folder".into(), T0);
        assert!(!due(&b, T0 + 1), "nothing new since");
        b.record(r.clone(), T0 + 1_000);
        assert!(!due(&b, T0 + 2_000), "refreshed at most every five minutes");
        assert!(due(&b, T0 + EVIDENCE_REFRESH_MS));
        for i in 0..10u64 {
            b.mark_evidence(&fp, "folder".into(), T0 + i);
            b.record(r.clone(), T0 + 10 * HOUR_MS + i);
        }
        assert!(b.get(&fp).unwrap().evidence_writes >= MAX_EVIDENCE_WRITES);
        assert!(!due(&b, T0 + 20 * HOUR_MS), "bounded number of refreshes");
        // an issue nobody would report gets no evidence folder
        let quiet = b
            .record(report(IssueKind::NpcMissing, "q", 1), T0)
            .fingerprint;
        assert!(!b
            .plan(T0, QueueMode::Auto, true)
            .iter()
            .any(|j| j.fp == quiet));
    }

    #[test]
    fn server_log_lines_are_attached_to_the_matching_sample() {
        let mut b = book();
        let fp = b
            .record(report(IssueKind::Kicked, "k", 1).at(T0 + 5), T0)
            .fingerprint;
        b.set_server_logs(
            &fp,
            vec![
                (T0 + 5, vec!["a WARN line".into()]),
                (T0 + 99, vec!["x".into()]),
                (T0 + 5, vec![]),
            ],
        );
        let s = &b.get(&fp).unwrap().samples[0];
        assert_eq!(
            s.server_log,
            vec!["a WARN line".to_string()],
            "empty results keep what is there"
        );
    }

    #[test]
    fn the_book_survives_a_json_roundtrip() {
        let mut b = book();
        let a = b
            .record(
                report(IssueKind::Kicked, "k", 3)
                    .severity(Severity::High)
                    .at(T0 + 7),
                T0,
            )
            .fingerprint;
        let q = reportable_issues(&mut b, IssueKind::MalformedPacket, 1).remove(0);
        b.mark_queued(&q, T0 + 100);
        b.mark_evidence(&a, "some/folder".into(), T0 + 50);
        b.apply_action(&a, Action::Resolve, T0 + 60).unwrap();
        let text = b.to_json(T0 + 200);
        let back = IssueBook::from_json(&text, Policy::default()).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.get(&a), b.get(&a));
        assert_eq!(back.get(&q), b.get(&q));
        assert_eq!(
            back.limit_hit(IssueKind::MalformedPacket, T0 + 300, &[]),
            b.limit_hit(IssueKind::MalformedPacket, T0 + 300, &[])
        );
        assert_eq!(
            back.queued_last_hour(T0 + 300),
            1,
            "the limits survive a restart"
        );
        // wrong versions and garbage are errors, not empty books
        assert!(IssueBook::from_json(r#"{"version":99,"records":[]}"#, Policy::default()).is_err());
        assert!(IssueBook::from_json("not json", Policy::default()).is_err());
        assert!(IssueBook::from_json("", Policy::default()).is_err());
        // a hand-edited file is brought back within the bounds
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let one = v["records"][0].clone();
        let many = vec![one; 3];
        v["records"] = serde_json::Value::Array(many);
        let again = IssueBook::from_json(&v.to_string(), Policy::default()).unwrap();
        assert_eq!(again.len(), 1, "equal fingerprints are one issue");
    }

    #[test]
    fn a_loaded_book_is_cut_down_to_the_capacity() {
        let mut big = IssueBook::new(Policy {
            capacity: 1000,
            ..Policy::default()
        });
        for i in 0..30 {
            big.record(report(IssueKind::Other, &format!("k{i}"), 1), T0 + i);
        }
        let text = big.to_json(T0);
        let small = IssueBook::from_json(
            &text,
            Policy {
                capacity: 10,
                ..Policy::default()
            },
        )
        .unwrap();
        assert_eq!(small.len(), 10);
    }
}
