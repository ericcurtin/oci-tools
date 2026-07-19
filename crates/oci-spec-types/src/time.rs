//! RFC 3339 UTC timestamp formatting, without a date/time dependency.
//!
//! Originally written for `oci_runtime_core::PersistedState::created`
//! (a `YYYY-MM-DDTHH:MM:SSZ` string for `SystemTime::now()`); moved
//! here so [`image::ImageConfig::created`](crate::image::ImageConfig)
//! and [`image::HistoryEntry::created`](crate::image::HistoryEntry) —
//! both defined in this same crate — can format a real timestamp for
//! themselves too (`oci_dockerfile::commit_layer`'s own future caller
//! needs exactly this), without either duplicating the same hand-
//! rolled math a second time or making `oci-dockerfile` depend on
//! `oci-runtime-core` just for a date string. Pulling in `chrono`/
//! `time` for this one conversion would also be a lot of dependency
//! weight (and a new "date/time" capability group `ci/guards.py`
//! would have to start policing), so this hand-rolls the civil-
//! calendar math instead: Howard Hinnant's `civil_from_days` algorithm
//! (public domain; see
//! <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>),
//! which is a small, well-known, exhaustively-tested-elsewhere formula for
//! days-since-epoch -> (year, month, day) and back.

use std::time::SystemTime;

/// Format `time` as `YYYY-MM-DDTHH:MM:SSZ` (UTC, second precision — the
/// same precision `runc`'s `state.json` uses in practice). Times before
/// the Unix epoch format as the epoch itself (state timestamps are always
/// `SystemTime::now()`, never user input).
pub fn format_rfc3339_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-since-1970-01-01 -> (year, month, day). See module docs for the
/// source algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn epoch_formats_correctly() {
        assert_eq!(
            format_rfc3339_utc(std::time::UNIX_EPOCH),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn known_timestamp_formats_correctly() {
        // 1705329296 = 2024-01-15T14:34:56Z, cross-checked with
        // `date -u -d @1705329296 +%Y-%m-%dT%H:%M:%SZ`.
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_705_329_296);
        assert_eq!(format_rfc3339_utc(t), "2024-01-15T14:34:56Z");
    }

    #[test]
    fn leap_day_formats_correctly() {
        // 2024-02-29T00:00:00Z = 1709164800
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_rfc3339_utc(t), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn end_of_year_formats_correctly() {
        // 1999-12-31T23:59:59Z = 946684799
        let t = std::time::UNIX_EPOCH + Duration::from_secs(946_684_799);
        assert_eq!(format_rfc3339_utc(t), "1999-12-31T23:59:59Z");
    }
}
