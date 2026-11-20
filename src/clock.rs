//! Calendar helpers without a date/time dependency.
//!
//! The crate only needs two things: a UTC RFC 3339 timestamp for provenance
//! fields (`accessed_at`, `extracted_at`) and day arithmetic for recency
//! windows. Both reduce to Howard Hinnant's `civil_from_days`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch; 0 if the system clock is before it.
pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Days-since-epoch → proleptic-Gregorian (year, month, day).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Proleptic-Gregorian (year, month, day) → days since the Unix epoch; the
/// inverse of [`civil_from_days`] (Hinnant's `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parse a strict `YYYY-MM-DD` calendar date into days since the Unix epoch.
/// Rejects impossible dates (`2026-02-30`) by round-tripping.
pub fn day_from_ymd(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = value[0..4].parse().ok()?;
    let month: u32 = value[5..7].parse().ok()?;
    let day: u32 = value[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let index = days_from_civil(year, month, day);
    (civil_from_days(index) == (year, month, day)).then_some(index)
}

/// `YYYY-MM-DD` for a day index (days since the Unix epoch).
pub fn ymd_from_day(day_index: i64) -> String {
    let (year, month, day) = civil_from_days(day_index);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DD` for the UTC day `days_ago` days before `now_unix`.
pub fn utc_date(now_unix: u64, days_ago: u32) -> String {
    let day_index = (now_unix / 86_400) as i64 - i64::from(days_ago);
    let (year, month, day) = civil_from_days(day_index);
    format!("{year:04}-{month:02}-{day:02}")
}

/// UTC RFC 3339 timestamp (second precision) for `unix_seconds`.
pub fn rfc3339(unix_seconds: u64) -> String {
    let (year, month, day) = civil_from_days((unix_seconds / 86_400) as i64);
    let rem = unix_seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// UTC RFC 3339 timestamp for the current moment.
pub fn now_rfc3339() -> String {
    rfc3339(now_unix_seconds())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        // 2026-11-03T04:05:06Z
        assert_eq!(rfc3339(1_793_678_706), "2026-11-03T04:05:06Z");
    }

    #[test]
    fn utc_date_subtracts_whole_days() {
        assert_eq!(utc_date(1_793_678_706, 0), "2026-11-03");
        assert_eq!(utc_date(1_793_678_706, 3), "2026-10-31");
    }
}
