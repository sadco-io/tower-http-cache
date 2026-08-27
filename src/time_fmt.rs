//! Minimal UTC timestamp formatting.
//!
//! Replaces the `chrono` dependency, which this crate only ever used to render
//! a `SystemTime` as an RFC 3339 / ISO 8601 string. The output of every
//! function here is byte-identical to the `chrono` call it replaced -- these
//! strings appear in ML training logs and in admin API JSON responses, so a
//! change in shape would be a silent break for consumers.
//!
//! UTC only. No parsing, no local time, no leap seconds.

use std::time::{SystemTime, UNIX_EPOCH};

/// Splits a `SystemTime` into whole seconds since the Unix epoch (negative
/// before 1970) and a nanosecond remainder in `0..1_000_000_000`.
fn epoch_parts(time: SystemTime) -> (i64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            let (secs, nanos) = (d.as_secs() as i64, d.subsec_nanos());
            if nanos == 0 {
                (-secs, 0)
            } else {
                (-secs - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month,
/// day) in the proleptic Gregorian calendar. Correct for negative inputs.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era, [0, 146_096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // March-based month, [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Splits epoch seconds into calendar fields.
fn civil_from_secs(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Renders the year the way chrono's `%Y` / RFC 3339 formatter does: four
/// zero-padded digits inside `0..=9999`, and outside that an explicit sign
/// followed by the magnitude, still zero-padded to at least four digits.
fn push_year(out: &mut String, year: i64) {
    use std::fmt::Write as _;
    if (0..=9999).contains(&year) {
        let _ = write!(out, "{:04}", year);
    } else if year > 9999 {
        let _ = write!(out, "+{:04}", year);
    } else {
        let _ = write!(out, "-{:04}", -year);
    }
}

/// Renders `%Y-%m-%dT%H:%M:%S` (no fraction, no offset).
fn push_datetime_secs(out: &mut String, secs: i64) {
    use std::fmt::Write as _;
    let (y, mo, d, h, mi, s) = civil_from_secs(secs);
    push_year(out, y);
    let _ = write!(out, "-{:02}-{:02}T{:02}:{:02}:{:02}", mo, d, h, mi, s);
}

/// Renders the fractional-second part the way chrono's RFC 3339 formatter
/// does: the shortest of zero, 3, 6 or 9 digits that is lossless.
fn push_fraction(out: &mut String, nanos: u32) {
    use std::fmt::Write as _;
    if nanos == 0 {
        // chrono omits the fraction entirely at whole-second precision.
    } else if nanos % 1_000_000 == 0 {
        let _ = write!(out, ".{:03}", nanos / 1_000_000);
    } else if nanos % 1_000 == 0 {
        let _ = write!(out, ".{:06}", nanos / 1_000);
    } else {
        let _ = write!(out, ".{:09}", nanos);
    }
}

/// Formats a `SystemTime` as RFC 3339 in UTC.
///
/// Byte-identical to `chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()`,
/// including the `+00:00` offset spelling (chrono's `to_rfc3339` does not use
/// `Z`) and the variable fractional-second precision.
#[cfg_attr(not(feature = "admin-api"), allow(dead_code))]
pub(crate) fn format_rfc3339(time: SystemTime) -> String {
    let (secs, nanos) = epoch_parts(time);
    let mut out = String::with_capacity(35);
    push_datetime_secs(&mut out, secs);
    push_fraction(&mut out, nanos);
    out.push_str("+00:00");
    out
}

/// Bounds of `chrono::DateTime::from_timestamp`, in whole seconds.
const CHRONO_MIN_TIMESTAMP: i64 = -8_334_601_228_800; // -262143-01-01T00:00:00Z
const CHRONO_MAX_TIMESTAMP: i64 = 8_210_266_876_799; // +262142-12-31T23:59:59Z

/// Formats whole epoch seconds as RFC 3339 in UTC.
///
/// Byte-identical to
/// `chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH).to_rfc3339()`.
/// Out-of-range inputs fall back to the Unix epoch, as the `unwrap_or` did.
#[cfg_attr(not(feature = "admin-api"), allow(dead_code))]
pub(crate) fn format_rfc3339_from_secs(secs: u64) -> String {
    // The old code did an unchecked `secs as i64`, so a `u64` above
    // `i64::MAX` wrapped to a negative timestamp rather than saturating.
    // Preserved exactly.
    let secs = secs as i64;
    // `chrono::DateTime::from_timestamp` is `None` outside this range (years
    // -262143..=+262142, confirmed by binary search against chrono 0.4.45);
    // the old `unwrap_or(DateTime::UNIX_EPOCH)` mapped that to the epoch.
    let secs = if (CHRONO_MIN_TIMESTAMP..=CHRONO_MAX_TIMESTAMP).contains(&secs) {
        secs
    } else {
        0
    };
    let mut out = String::with_capacity(25);
    push_datetime_secs(&mut out, secs);
    out.push_str("+00:00");
    out
}

/// Formats a `SystemTime` as `%Y-%m-%dT%H:%M:%S.mmmZ`.
///
/// Byte-identical to the ML log timestamp this replaced, which was
/// `format!("{}.{:03}Z", DateTime::<Utc>::from(t).format("%Y-%m-%dT%H:%M:%S"), d.subsec_millis())`
/// where `d` was `t.duration_since(UNIX_EPOCH).unwrap_or_default()`. Note that
/// for pre-1970 inputs the old code rendered the real date but a zero
/// millisecond field; that quirk is preserved deliberately.
#[cfg(feature = "serde")]
pub(crate) fn format_iso8601_millis(time: SystemTime) -> String {
    let (secs, _) = epoch_parts(time);
    let millis = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_millis();
    let mut out = String::with_capacity(28);
    push_datetime_secs(&mut out, secs);
    out.push_str(&format!(".{:03}Z", millis));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: i64, nanos: u32) -> SystemTime {
        if secs >= 0 {
            UNIX_EPOCH + Duration::new(secs as u64, nanos)
        } else {
            UNIX_EPOCH - Duration::new((-secs) as u64, 0) + Duration::new(0, nanos)
        }
    }

    #[test]
    fn civil_from_days_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        // 2000-02-29: a leap day in a century year divisible by 400.
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        // 2024-02-29: an ordinary leap day.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        // 1900-02-28 -> 1900-03-01 on consecutive days: 1900 is NOT a leap
        // year, despite being divisible by 4.
        assert_eq!(civil_from_days(-25509), (1900, 2, 28));
        assert_eq!(civil_from_days(-25508), (1900, 3, 1));
        // Year boundary.
        assert_eq!(civil_from_days(10956), (1999, 12, 31));
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
    }

    #[test]
    fn rfc3339_epoch_and_boundaries() {
        assert_eq!(format_rfc3339(at(0, 0)), "1970-01-01T00:00:00+00:00");
        assert_eq!(format_rfc3339(at(1, 0)), "1970-01-01T00:00:01+00:00");
        assert_eq!(format_rfc3339(at(-1, 0)), "1969-12-31T23:59:59+00:00");
        assert_eq!(format_rfc3339(at(86_399, 0)), "1970-01-01T23:59:59+00:00");
        assert_eq!(format_rfc3339(at(86_400, 0)), "1970-01-02T00:00:00+00:00");
        // 2038 signed-32-bit rollover.
        assert_eq!(
            format_rfc3339(at(2_147_483_647, 0)),
            "2038-01-19T03:14:07+00:00"
        );
    }

    #[test]
    fn rfc3339_leap_day_and_year_boundary() {
        // 2024-02-29T12:00:00Z
        assert_eq!(
            format_rfc3339(at(1_709_208_000, 0)),
            "2024-02-29T12:00:00+00:00"
        );
        // 2000-02-29T00:00:00Z
        assert_eq!(
            format_rfc3339(at(951_782_400, 0)),
            "2000-02-29T00:00:00+00:00"
        );
        // 1900-03-01T00:00:00Z -- proves 1900 was skipped as a leap year.
        assert_eq!(
            format_rfc3339(at(-2_203_891_200, 0)),
            "1900-03-01T00:00:00+00:00"
        );
        // 1999-12-31T23:59:59Z -> 2000-01-01T00:00:00Z
        assert_eq!(
            format_rfc3339(at(946_684_799, 0)),
            "1999-12-31T23:59:59+00:00"
        );
        assert_eq!(
            format_rfc3339(at(946_684_800, 0)),
            "2000-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn rfc3339_fraction_precision_matches_chrono_auto_si() {
        // Whole seconds: no fraction at all.
        assert_eq!(format_rfc3339(at(0, 0)), "1970-01-01T00:00:00+00:00");
        // Millisecond precision: exactly three digits.
        assert_eq!(
            format_rfc3339(at(0, 123_000_000)),
            "1970-01-01T00:00:00.123+00:00"
        );
        // Microsecond precision: exactly six.
        assert_eq!(
            format_rfc3339(at(0, 123_456_000)),
            "1970-01-01T00:00:00.123456+00:00"
        );
        // Nanosecond precision: exactly nine.
        assert_eq!(
            format_rfc3339(at(0, 123_456_789)),
            "1970-01-01T00:00:00.123456789+00:00"
        );
        // A single nanosecond still forces nine digits.
        assert_eq!(
            format_rfc3339(at(0, 1)),
            "1970-01-01T00:00:00.000000001+00:00"
        );
    }

    #[test]
    fn rfc3339_from_secs_drops_the_fraction() {
        assert_eq!(format_rfc3339_from_secs(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(
            format_rfc3339_from_secs(1_709_208_000),
            "2024-02-29T12:00:00+00:00"
        );
        // `u64::MAX as i64` is -1, exactly as the old unchecked cast produced.
        assert_eq!(
            format_rfc3339_from_secs(u64::MAX),
            "1969-12-31T23:59:59+00:00"
        );
        // Genuinely out of chrono's representable range -> the epoch, as the
        // old `unwrap_or(DateTime::UNIX_EPOCH)` did.
        assert_eq!(
            format_rfc3339_from_secs(CHRONO_MAX_TIMESTAMP as u64 + 1),
            "1970-01-01T00:00:00+00:00"
        );
        // The last representable second still formats.
        assert_eq!(
            format_rfc3339_from_secs(CHRONO_MAX_TIMESTAMP as u64),
            "+262142-12-31T23:59:59+00:00"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn iso8601_millis_shape() {
        assert_eq!(format_iso8601_millis(at(0, 0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_iso8601_millis(at(1_709_208_000, 123_456_789)),
            "2024-02-29T12:00:00.123Z"
        );
        // Truncation, not rounding -- chrono's `subsec_millis` truncated too.
        assert_eq!(
            format_iso8601_millis(at(946_684_799, 999_999_999)),
            "1999-12-31T23:59:59.999Z"
        );
        assert_eq!(
            format_iso8601_millis(at(946_684_800, 0)),
            "2000-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn extended_years_match_chrono_padding() {
        // Outside 0..=9999 chrono emits a sign and pads the magnitude to at
        // least four digits -- NOT five. Verified against chrono 0.4.45 over
        // its full representable range.
        assert_eq!(
            format_rfc3339_from_secs(CHRONO_MIN_TIMESTAMP as u64),
            "-262143-01-01T00:00:00+00:00"
        );
        assert_eq!(
            format_rfc3339_from_secs(CHRONO_MAX_TIMESTAMP as u64),
            "+262142-12-31T23:59:59+00:00"
        );
        // Year 0 and year 1 stay in the unsigned four-digit form.
        assert_eq!(
            format_rfc3339_from_secs((-62_167_219_200i64) as u64),
            "0000-01-01T00:00:00+00:00"
        );
        assert_eq!(
            format_rfc3339_from_secs((-62_135_596_800i64) as u64),
            "0001-01-01T00:00:00+00:00"
        );
        // Year -1 pads to four digits: "-0001", not "-00001".
        assert_eq!(
            format_rfc3339_from_secs((-62_198_755_200i64) as u64),
            "-0001-01-01T00:00:00+00:00"
        );
        // Year 10000 is the first to take the "+" form.
        assert_eq!(
            format_rfc3339_from_secs(253_402_300_800),
            "+10000-01-01T00:00:00+00:00"
        );
        assert_eq!(
            format_rfc3339_from_secs(253_402_300_799),
            "9999-12-31T23:59:59+00:00"
        );
    }

    #[test]
    fn round_trip_across_a_full_leap_year() {
        // Every day of 2024 (a leap year) must render a valid, ordered string.
        let mut prev = String::new();
        for day in 0..366 {
            let s = format_rfc3339(at(1_704_067_200 + day * 86_400, 0));
            assert!(s > prev, "not monotonic: {} <= {}", s, prev);
            prev = s;
        }
        // Day 59 (0-indexed) of 2024 is 2024-02-29.
        assert_eq!(
            format_rfc3339(at(1_704_067_200 + 59 * 86_400, 0)),
            "2024-02-29T00:00:00+00:00"
        );
        // The day after day 365 is 2025-01-01.
        assert_eq!(
            format_rfc3339(at(1_704_067_200 + 366 * 86_400, 0)),
            "2025-01-01T00:00:00+00:00"
        );
    }
}
