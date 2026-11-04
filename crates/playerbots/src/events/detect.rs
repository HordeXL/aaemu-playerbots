//! The event detectors: what the bots tell the team when a rift does not behave. Pure functions
//! over the evidence the board collected ([`Facts`]), so they are tested without a clock or a
//! server.
//!
//! Every judgement is one-sided: the bots see only part of the world, so each rule fires only when
//! the evidence they *do* have proves the server wrong.
//!
//! * **No wave NPCs.** A wave with spawners and kill targets began, a bot stood at the defended
//!   target for a while, and no monster of the wave was seen anywhere. The wait is a minute plus
//!   the walk of the wave NPCs from their controllers to the target (the controllers stand some
//!   hundreds of metres out); the report says whether the controllers were seen.
//! * **A wave that does not advance.** The kill targets of the wave were seen to die in the
//!   numbers the wave asks for (each object counted once, so this can only undercount what the
//!   server counted), and the wave did not move on for half a minute.
//! * **An event that never ends.** The server force-ends every event after `force_end_time`; this
//!   one is still listed a minute after that. The board dates an event from when a bot first heard
//!   of it, so it can only judge late, never early.

use aa_proto::sc_event::EventId;
use aa_world::event::EventDef;
use aa_world::Vec3;
use smallvec::SmallVec;

use crate::issues::{IssueKind, IssueReport, Severity, Suspect};

/// Where the event code is in the server.
const AREA: &str = "rust/crates/aaemu-game/src/systems/instances/tower_def.rs";

/// "No wave NPC" has been told for this wave.
pub const TOLD_NO_WAVE: u8 = 1;
/// "Stuck wave" has been told for this wave.
pub const TOLD_STUCK: u8 = 2;
/// "Overdue" has been told (kept for the whole event).
pub const TOLD_OVERDUE: u8 = 4;

/// The wave NPCs get this long to show, plus their walk.
pub const WAVE_GRACE_MS: u64 = 60_000;
/// A participant must have stood at the target this long in the wave before its absence counts.
pub const NEAR_MIN_MS: u64 = 30_000;
/// The speed the wave NPCs are assumed to cover the way from a controller to the target at.
pub const WAVE_WALK_MPS: f32 = 4.0;
/// The longest walk allowance.
pub const WAVE_WALK_MAX_MS: u64 = 240_000;
/// A wave whose kills are in gets this long to advance.
pub const STUCK_GRACE_MS: u64 = 30_000;
/// An event is overdue this long after its time cap.
pub const OVERDUE_GRACE_MS: u64 = 60_000;

/// What the board knows about one event at one moment.
pub struct Facts<'a> {
    /// The event's data.
    pub def: &'a EventDef,
    /// Which event.
    pub id: EventId,
    /// The board clock (ms).
    pub now: u64,
    /// When the board first heard of the event.
    pub first_seen: u64,
    /// The wave the event is in (0 before the first).
    pub step: u32,
    /// When that wave began.
    pub step_since: u64,
    /// How long, in this wave, a bot stood at the target.
    pub near_ms: u64,
    /// A wave NPC of this wave was seen.
    pub wave_seen: bool,
    /// A controller of the event was seen.
    pub controller_seen: bool,
    /// Deaths of a kill-target template counted in this wave.
    pub kills: &'a dyn Fn(u32) -> u32,
    /// Since when the kill targets of this wave were all counted.
    pub satisfied_since: Option<u64>,
    /// Findings already told (`TOLD_*`).
    pub told: u8,
    /// Bots in the event.
    pub bots: u32,
}

/// What a detector found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finding {
    /// The wave's NPCs never showed up.
    NoWaveNpc {
        /// Seconds since the wave began.
        waited_s: u64,
    },
    /// The wave's kills are in and it did not advance.
    StuckWave {
        /// Seconds the wave has been satisfied.
        waited_s: u64,
    },
    /// The event outlived its time cap.
    Overdue {
        /// Seconds past the cap.
        over_s: u64,
    },
}

impl Finding {
    /// The `TOLD_*` bit.
    pub fn bit(self) -> u8 {
        match self {
            Finding::NoWaveNpc { .. } => TOLD_NO_WAVE,
            Finding::StuckWave { .. } => TOLD_STUCK,
            Finding::Overdue { .. } => TOLD_OVERDUE,
        }
    }
}

/// How long the wave NPCs of `step` need from their controllers to `target`: the farthest
/// controller's distance at [`WAVE_WALK_MPS`], capped.
pub fn walk_allowance_ms(def: &EventDef, step: u32, zone_group: u16, target: Option<Vec3>) -> u64 {
    let Some(target) = target.or_else(|| def.target_positions(zone_group).next()) else {
        return 0;
    };
    let far = def
        .wave_positions(step, zone_group)
        .map(|p| p.dist2d(target))
        .fold(0.0f32, f32::max);
    ((far / WAVE_WALK_MPS * 1000.0) as u64).min(WAVE_WALK_MAX_MS)
}

/// Judges the evidence. Returns the new findings and the value `satisfied_since` takes from now
/// on.
pub fn judge(f: &Facts<'_>) -> (SmallVec<[Finding; 2]>, Option<u64>) {
    let mut out = SmallVec::new();
    let elapsed = f.now.saturating_sub(f.step_since);
    let step = f.def.step(f.step);

    // the wave NPCs never showed
    if let Some(s) = step {
        let expects_npcs = !s.spawners.is_empty() && !s.kills.is_empty();
        if expects_npcs
            && f.told & TOLD_NO_WAVE == 0
            && !f.wave_seen
            && f.near_ms >= NEAR_MIN_MS
            && elapsed >= WAVE_GRACE_MS + walk_allowance_ms(f.def, f.step, f.id.zone_group, None)
        {
            out.push(Finding::NoWaveNpc {
                waited_s: elapsed / 1000,
            });
        }
    }

    // the kills are in and the wave stays
    let satisfied = match step {
        Some(s) if !s.kills.is_empty() && (f.step as usize) < f.def.steps.len() => s
            .complete(elapsed as f32 / 1000.0, f.kills)
            .then(|| f.satisfied_since.unwrap_or(f.now)),
        _ => None,
    };
    if let Some(since) = satisfied {
        let waited = f.now.saturating_sub(since);
        if f.told & TOLD_STUCK == 0 && waited >= STUCK_GRACE_MS {
            out.push(Finding::StuckWave {
                waited_s: waited / 1000,
            });
        }
    }

    // the event is still there after its cap
    let cap_ms = (f.def.force_end_s * 1000.0) as u64;
    let age = f.now.saturating_sub(f.first_seen);
    if f.told & TOLD_OVERDUE == 0 && cap_ms > 0 && age >= cap_ms + OVERDUE_GRACE_MS {
        out.push(Finding::Overdue {
            over_s: (age - cap_ms) / 1000,
        });
    }
    (out, satisfied)
}

/// The issue report of a finding. Keys name ids only (never times or counts), so two processes
/// that meet one problem file one issue.
pub fn report(f: &Facts<'_>, finding: Finding) -> IssueReport {
    let name = &f.def.name;
    let (def, group) = (f.id.def, f.id.zone_group);
    let r = match finding {
        Finding::NoWaveNpc { waited_s } => IssueReport::new(
            IssueKind::NoResponse,
            format!("event-wave-no-npcs def {def} wave {}", f.step),
            format!("Wave {} of the {name} never showed its monsters", f.step),
        )
        .expected(format!(
            "wave {} summons its monsters from the controllers; some come to the defended target within a few minutes",
            f.step
        ))
        .actual(format!(
            "{waited_s} s into the wave, a bot stood at the target for {} s and saw no monster of the wave{}",
            f.near_ms / 1000,
            if f.controller_seen {
                " (the controllers were seen)"
            } else {
                " and no controller either"
            }
        ))
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
        Finding::StuckWave { waited_s } => IssueReport::new(
            IssueKind::NoResponse,
            format!("event-wave-stuck def {def} wave {}", f.step),
            format!("Wave {} of the {name} did not advance after its kills", f.step),
        )
        .expected("the next wave begins within seconds of the last kill the wave waits for")
        .actual(format!(
            "the kills of wave {} were all seen ({}), {waited_s} s ago, and the event stayed in the wave",
            f.step,
            kill_summary(f)
        ))
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
        Finding::Overdue { over_s } => IssueReport::new(
            IssueKind::Other,
            format!("event-not-ended def {def}"),
            format!("The {name} was not ended at its time cap"),
        )
        .expected(format!(
            "the server ends the event {} s after its start",
            f.def.force_end_s as u64
        ))
        .actual(format!(
            "still running {over_s} s past the cap (wave {})",
            f.step
        ))
        .suspect(Suspect::Server)
        .severity(Severity::Medium),
    };
    r.area(AREA)
        .fact("event", def)
        .fact("zone_group", group)
        .fact("wave", f.step)
        .fact("bots_in_event", f.bots)
}

fn kill_summary(f: &Facts<'_>) -> String {
    let Some(s) = f.def.step(f.step) else {
        return String::new();
    };
    s.kills
        .iter()
        .map(|k| format!("{} {}/{}", k.template, (f.kills)(k.template), k.count))
        .collect::<Vec<_>>()
        .join(", ")
}
