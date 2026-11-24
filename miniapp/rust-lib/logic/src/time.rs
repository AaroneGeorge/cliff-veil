//! Clock times as people read and type them.
//!
//! LEZ clock times are milliseconds since the Unix epoch (`u64`, SPEC section 4.1). `utc` and `duration` render
//! exactly as the SDK's `time` module does (same vectors in the tests), so the CLI and the mini-app print the same
//! text for the same time. `parse_utc` reads the times typed into the create form.

/// `ms` as ISO-8601 UTC with millisecond precision, e.g. `2026-11-24T18:30:00.000Z`.
#[must_use]
pub fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(secs / 86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// A duration in milliseconds as a short text: `3d 4h`, `5h 2m`, `12m 5s`, `42s`, `250ms`.
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

/// Reads a time typed by a person: milliseconds since the epoch (digits only), or ISO-8601 UTC as
/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH:MM:SS` or with `.mmm`, each with an optional trailing `Z`.
/// A space may replace the `T`. Times before 1970 are refused.
///
/// # Errors
/// A text in none of these forms, or a date that does not exist.
pub fn parse_utc(text: &str) -> Result<u64, String> {
    let t = text.trim();
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse::<u64>().map_err(|_| "the time is too large".to_owned());
    }
    let bad = || format!("`{t}` is not a time: use YYYY-MM-DDTHH:MM:SSZ (UTC) or milliseconds since 1970");
    let t = t.strip_suffix('Z').unwrap_or(t);
    let (date, clock) = match t.split_once(['T', ' ']) {
        Some((d, c)) => (d, c),
        None => (t, "00:00"),
    };
    let num = |s: &str, len: usize| -> Result<u64, String> {
        if s.len() == len && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().map_err(|_| bad())
        } else {
            Err(bad())
        }
    };
    let mut d = date.split('-');
    let (y, mo, da) = match (d.next(), d.next(), d.next(), d.next()) {
        (Some(y), Some(m), Some(dd), None) => (num(y, 4)?, num(m, 2)?, num(dd, 2)?),
        _ => return Err(bad()),
    };
    let (hms, millis) = match clock.split_once('.') {
        Some((h, ms)) => (h, num(ms, 3)?),
        None => (clock, 0),
    };
    let mut c = hms.split(':');
    let (h, mi, s) = match (c.next(), c.next(), c.next(), c.next()) {
        (Some(h), Some(m), None, None) => (num(h, 2)?, num(m, 2)?, 0),
        (Some(h), Some(m), Some(s), None) => (num(h, 2)?, num(m, 2)?, num(s, 2)?),
        _ => return Err(bad()),
    };
    if y < 1970 || !(1..=12).contains(&mo) || da == 0 || da > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return Err(bad());
    }
    let days = days_from_civil(y, mo, da);
    Ok(((days * 86_400 + h * 3600 + mi * 60 + s) * 1000) + millis)
}

const fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

const fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 to a civil date (H. Hinnant's `civil_from_days`, non-negative day counts).
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

/// The inverse: a civil date (year >= 1970) to days since 1970-01-01 (H. Hinnant's `days_from_civil`).
const fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDK's vectors (`sdk/src/time.rs`): both render the same text.
    #[test]
    fn renders_as_the_sdk_does() {
        assert_eq!(utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc(1_767_225_600_000), "2026-01-01T00:00:00.000Z");
        assert_eq!(utc(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
        assert_eq!(duration(250), "250ms");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(725_000), "12m 5s");
        assert_eq!(duration(5 * 3_600_000 + 120_000), "5h 2m");
        assert_eq!(duration(3 * 86_400_000 + 4 * 3_600_000), "3d 4h");
    }

    #[test]
    fn parses_what_it_renders() {
        for ms in [
            0,
            1_767_225_600_000,
            1_709_210_096_789,
            951_782_400_000,
            4_102_444_799_999,
        ] {
            assert_eq!(parse_utc(&utc(ms)), Ok(ms), "{}", utc(ms));
        }
        assert_eq!(parse_utc("2026-01-01"), Ok(1_767_225_600_000));
        assert_eq!(parse_utc("2026-01-01T00:00Z"), Ok(1_767_225_600_000));
        assert_eq!(parse_utc("2026-01-01 00:00:00"), Ok(1_767_225_600_000));
        assert_eq!(parse_utc("1767225600000"), Ok(1_767_225_600_000));
    }

    #[test]
    fn refuses_dates_that_do_not_exist() {
        for bad in [
            "",
            "2026-02-29",
            "2026-13-01",
            "2026-01-01T24:00",
            "1969-12-31",
            "tomorrow",
            "2026-1-1",
        ] {
            assert!(parse_utc(bad).is_err(), "{bad}");
        }
        assert!(parse_utc("2024-02-29").is_ok());
    }
}
