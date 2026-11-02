//! Local time for the issue reports: the header of a queue entry, evidence folder names and the
//! `HH:MM:SS` of the server log.
//!
//! Everything takes the UTC offset as a number of seconds, so the formatting is deterministic and
//! testable; [`local_offset_secs`] asks the operating system for the current one.

use chrono::{DateTime, FixedOffset, Local, TimeZone, Utc};

/// Seconds the local time of the machine is ahead of UTC at `unix_ms`.
pub fn local_offset_secs(unix_ms: u64) -> i32 {
    Local
        .timestamp_millis_opt(unix_ms.min(i64::MAX as u64) as i64)
        .single()
        .map_or(0, |d| d.offset().local_minus_utc())
}

fn at(unix_ms: u64, offset_secs: i32) -> Option<DateTime<FixedOffset>> {
    let off = FixedOffset::east_opt(offset_secs).or_else(|| FixedOffset::east_opt(0))?;
    DateTime::<Utc>::from_timestamp_millis(unix_ms.min(i64::MAX as u64) as i64)
        .map(|d| d.with_timezone(&off))
}

fn format(unix_ms: u64, offset_secs: i32, pattern: &str) -> String {
    at(unix_ms, offset_secs).map_or_else(
        || "0000-00-00 00:00:00".to_string(),
        |d| d.format(pattern).to_string(),
    )
}

/// `2026-11-02 14:03:09`.
pub fn format_local(unix_ms: u64, offset_secs: i32) -> String {
    format(unix_ms, offset_secs, "%Y-%m-%d %H:%M:%S")
}

/// `20261102-140309`, the start of an evidence folder name.
pub fn format_stamp(unix_ms: u64, offset_secs: i32) -> String {
    format(unix_ms, offset_secs, "%Y%m%d-%H%M%S")
}

/// `14:03:09`.
pub fn format_hms(unix_ms: u64, offset_secs: i32) -> String {
    format(unix_ms, offset_secs, "%H:%M:%S")
}

/// `UTC-05:00`, `UTC+00:00`, `UTC+05:30`.
pub fn offset_label(offset_secs: i32) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", abs / 3600, abs % 3600 / 60)
}

/// The label of the time zone in a queue entry's header: the configured one, else the offset.
pub fn tz_label(configured: Option<&str>, offset_secs: i32) -> String {
    match configured.map(str::trim).filter(|l| !l.is_empty()) {
        Some(l) => l.to_string(),
        None => offset_label(offset_secs),
    }
}

/// The unix time in milliseconds of the log time `hms` (seconds since local midnight) that is
/// closest to `reference_ms`, for a machine at `offset_secs`. A time of day names three possible
/// instants around the reference (yesterday, today, tomorrow); the nearest one wins.
pub fn nearest_time_of_day(reference_ms: u64, offset_secs: i32, hms: u32) -> u64 {
    let local = (reference_ms / 1000) as i64 + i64::from(offset_secs);
    let midnight = local - local.rem_euclid(86_400);
    let best = [-86_400i64, 0, 86_400]
        .into_iter()
        .map(|d| midnight + d + i64::from(hms))
        .min_by_key(|c| (c - local).abs())
        .unwrap_or(local);
    ((best - i64::from(offset_secs)).max(0) as u64) * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-02 19:03:09 UTC.
    const T: u64 = 1_793_646_189_000;

    #[test]
    fn times_are_formatted_for_the_given_offset() {
        assert_eq!(format_local(T, 0), "2026-11-02 19:03:09");
        assert_eq!(format_local(T, -5 * 3600), "2026-11-02 14:03:09");
        assert_eq!(format_stamp(T, -5 * 3600), "20261102-140309");
        assert_eq!(format_hms(T, 3600), "20:03:09");
        // the date follows the offset
        assert_eq!(format_local(T, 6 * 3600), "2026-11-03 01:03:09");
        assert_eq!(format_local(0, 0), "1970-01-01 00:00:00");
    }

    #[test]
    fn offsets_have_a_numeric_label_unless_one_is_configured() {
        assert_eq!(offset_label(-5 * 3600), "UTC-05:00");
        assert_eq!(offset_label(0), "UTC+00:00");
        assert_eq!(offset_label(5 * 3600 + 1800), "UTC+05:30");
        assert_eq!(offset_label(-(9 * 3600 + 1800)), "UTC-09:30");
        assert_eq!(tz_label(Some("EST"), -5 * 3600), "EST");
        assert_eq!(tz_label(Some("  "), -5 * 3600), "UTC-05:00");
        assert_eq!(tz_label(None, 3600), "UTC+01:00");
    }

    #[test]
    fn a_time_of_day_resolves_to_the_nearest_instant() {
        let off = -5 * 3600;
        // local 14:03:09 on 2026-11-02
        let hms = 14 * 3600 + 3 * 60 + 9;
        assert_eq!(nearest_time_of_day(T + 1500, off, hms), T);
        assert_eq!(nearest_time_of_day(T - 3000, off, hms), T);
        // just after local midnight a line stamped 23:59:58 belongs to the day before
        let after_midnight = T + (9 * 3600 + 56 * 60 + 53) * 1000; // local 00:00:02 on the 3rd
        let late = 23 * 3600 + 59 * 60 + 58;
        let got = nearest_time_of_day(after_midnight, off, late);
        assert!(
            got < after_midnight && after_midnight - got < 5_000,
            "{got}"
        );
        // and a line stamped 00:00:01 read just before midnight belongs to the next day
        let before_midnight = after_midnight - 5_000; // local 23:59:57
        let got = nearest_time_of_day(before_midnight, off, 1);
        assert!(
            got > before_midnight && got - before_midnight < 10_000,
            "{got}"
        );
    }

    #[test]
    fn the_local_offset_is_sane() {
        let off = local_offset_secs(T);
        assert!((-14 * 3600..=14 * 3600).contains(&off), "{off}");
    }
}
