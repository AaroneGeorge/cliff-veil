//! Clock times as people read them.
//!
//! LEZ clock times are milliseconds since the Unix epoch (`u64`, SPEC section 4.1). Every time the SDK shows a person
//! is rendered here as ISO-8601 UTC, with the raw value kept next to it where an exact number matters.

/// `ms` (milliseconds since the Unix epoch) as ISO-8601 UTC with millisecond precision, e.g.
/// `2026-11-24T18:30:00.000Z`.
#[must_use]
pub fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// A duration in milliseconds as a short human text: `3d 4h`, `5h 2m`, `12m 5s`, `42s`, `250ms`.
#[must_use]
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {}s", s % 60)
    } else if s > 0 {
        format!("{s}s")
    } else {
        format!("{ms}ms")
    }
}

/// Days since 1970-01-01 to a (year, month, day) civil date in the proleptic Gregorian calendar
/// (H. Hinnant's `civil_from_days`, restricted to non-negative day counts).
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u7_epoch_and_known_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc(1_767_225_600_000), "2026-01-01T00:00:00.000Z");
        // A leap day and a millisecond part.
        assert_eq!(utc(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(250), "250ms");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(725_000), "12m 5s");
        assert_eq!(duration(5 * 3_600_000 + 120_000), "5h 2m");
        assert_eq!(duration(3 * 86_400_000 + 4 * 3_600_000), "3d 4h");
    }
}
