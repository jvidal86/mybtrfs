//! Snapshot/backup naming: `<basename>.<timestamp>[_N]`.
//!
//! Pure parse + format for the `short` / `long` / `long-iso` timestamp formats;
//! `short`/`long` are local-time, `long-iso` carries an absolute offset.
//! Parallels btrbk's name regex. Names not matching the scheme parse to `None`
//! and are left untouched by mybtrfs.
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use regex::Regex;
use std::sync::OnceLock;

/// strftime patterns for the three timestamp formats.
const SHORT_FORMAT: &str = "%Y%m%d";
const LONG_FORMAT: &str = "%Y%m%dT%H%M";
const LONG_ISO_FORMAT: &str = "%Y%m%dT%H%M%S%z";

const SECONDS_PER_HOUR: i32 = 3600;
const SECONDS_PER_MINUTE: i32 = 60;

/// Timestamp granularity used as the name postfix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimestampFormat {
    /// `YYYYMMDD`
    Short,
    /// `YYYYMMDDThhmm`
    #[default]
    Long,
    /// `YYYYMMDDThhmmss±hhmm`
    LongIso,
}

/// A parsed btrbk-style name.
///
/// `naive` holds the wall-clock components from the name (time-of-day is `00:00`
/// for `short`). `offset` is `Some` only for `long-iso` (an absolute instant);
/// for `short`/`long` it is `None` and the instant is resolved later against the
/// injected timezone (see the retention scheduler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub basename: String,
    pub naive: NaiveDateTime,
    pub has_exact_time: bool,
    pub offset: Option<FixedOffset>,
    pub nn: u32,
}

/// Format the timestamp postfix for `dt` in the requested format.
pub fn format_timestamp(dt: DateTime<FixedOffset>, fmt: TimestampFormat) -> String {
    match fmt {
        TimestampFormat::Short => dt.format(SHORT_FORMAT).to_string(),
        TimestampFormat::Long => dt.format(LONG_FORMAT).to_string(),
        TimestampFormat::LongIso => dt.format(LONG_ISO_FORMAT).to_string(),
    }
}

/// Build `<basename>.<timestamp>` (a collision counter is added via
/// [`with_counter`]).
pub fn make_name(basename: &str, dt: DateTime<FixedOffset>, fmt: TimestampFormat) -> String {
    format!("{basename}.{}", format_timestamp(dt, fmt))
}

/// Append a collision counter to a generated name: `<name>_<counter>`.
pub fn with_counter(name: &str, counter: u32) -> String {
    format!("{name}_{counter}")
}

#[allow(clippy::expect_used)] // compile-time-constant pattern; cannot fail at runtime
fn name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // <basename>.YYYYMMDD[Thhmm[ss(Z|±hhmm)]][_NN]
        Regex::new(
            r"(?x)
            ^(?P<name>.+)\.
            (?P<Y>[0-9]{4})(?P<M>[0-9]{2})(?P<D>[0-9]{2})
            (T(?P<h>[0-9]{2})(?P<min>[0-9]{2})
                ((?P<s>[0-9]{2})(?P<z>Z|[+-][0-9]{4}))?
            )?
            (_(?P<nn>[0-9]+))?$",
        )
        .expect("naming regex is valid")
    })
}

/// Parse a btrbk-style subvolume name, or `None` if it does not match the scheme
/// (such names are left untouched by mybtrfs).
pub fn parse_name(name: &str) -> Option<ParsedName> {
    let caps = name_regex().captures(name)?;
    let group = |k: &str| caps.name(k).map(|m| m.as_str());

    let year: i32 = group("Y")?.parse().ok()?;
    let month: u32 = group("M")?.parse().ok()?;
    let day: u32 = group("D")?.parse().ok()?;
    let has_exact_time = group("h").is_some();
    let hour: u32 = group("h").and_then(|v| v.parse().ok()).unwrap_or(0);
    let minute: u32 = group("min").and_then(|v| v.parse().ok()).unwrap_or(0);
    let second: u32 = group("s").and_then(|v| v.parse().ok()).unwrap_or(0);
    let nn: u32 = group("nn").and_then(|v| v.parse().ok()).unwrap_or(0);

    let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    let offset = match group("z") {
        None => None,
        Some("Z") => Some(FixedOffset::east_opt(0)?),
        Some(offset_text) => Some(parse_offset(offset_text)?),
    };

    Some(ParsedName {
        basename: group("name")?.to_string(),
        naive,
        has_exact_time,
        offset,
        nn,
    })
}

/// Parse an `±hhmm` offset (e.g. `+0200`, `-0500`) into a [`FixedOffset`].
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i32 = text.get(1..3)?.parse().ok()?;
    let minutes: i32 = text.get(3..5)?.parse().ok()?;
    FixedOffset::east_opt(sign * (hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Timelike};

    /// Build a deterministic, offset-aware instant for tests (no host TZ).
    fn dt(
        off_secs: i32,
        y: i32,
        mo: u32,
        d: u32,
        h: u32,
        mi: u32,
        s: u32,
    ) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(off_secs)
            .unwrap()
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
    }

    // --- formatting ---

    #[test]
    fn formats_short() {
        assert_eq!(
            format_timestamp(dt(0, 2015, 8, 25, 15, 31, 23), TimestampFormat::Short),
            "20150825"
        );
    }

    #[test]
    fn formats_long() {
        assert_eq!(
            format_timestamp(dt(0, 2015, 8, 25, 15, 31, 23), TimestampFormat::Long),
            "20150825T1531"
        );
    }

    #[test]
    fn formats_long_iso_with_offset() {
        assert_eq!(
            format_timestamp(
                dt(2 * 3600, 2015, 8, 25, 15, 31, 23),
                TimestampFormat::LongIso
            ),
            "20150825T153123+0200"
        );
    }

    #[test]
    fn makes_name_and_counter() {
        assert_eq!(
            make_name("home", dt(0, 2024, 1, 2, 15, 31, 0), TimestampFormat::Long),
            "home.20240102T1531"
        );
        assert_eq!(
            with_counter("home.20240102T1531", 1),
            "home.20240102T1531_1"
        );
    }

    // --- parsing ---

    #[test]
    fn parses_long() {
        let p = parse_name("home.20240102T1531").unwrap();
        assert_eq!(p.basename, "home");
        assert_eq!(p.nn, 0);
        assert!(p.has_exact_time);
        assert!(p.offset.is_none());
        assert_eq!(
            p.naive,
            NaiveDate::from_ymd_opt(2024, 1, 2)
                .unwrap()
                .and_hms_opt(15, 31, 0)
                .unwrap()
        );
    }

    #[test]
    fn parses_short_without_time() {
        let p = parse_name("rootfs.20150825").unwrap();
        assert_eq!(p.basename, "rootfs");
        assert!(!p.has_exact_time);
        assert!(p.offset.is_none());
        assert_eq!(
            p.naive,
            NaiveDate::from_ymd_opt(2015, 8, 25)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        );
    }

    #[test]
    fn parses_long_iso_with_offset_and_seconds() {
        let p = parse_name("home.20150825T153123+0200").unwrap();
        assert!(p.has_exact_time);
        assert_eq!(p.offset, Some(FixedOffset::east_opt(2 * 3600).unwrap()));
        assert_eq!(p.naive.second(), 23);
    }

    #[test]
    fn parses_dotted_basename_and_counter() {
        let p = parse_name("my.data.20150825T1531_3").unwrap();
        assert_eq!(p.basename, "my.data");
        assert_eq!(p.nn, 3);
    }

    #[test]
    fn rejects_non_btrbk_names() {
        assert!(parse_name("random-subvol").is_none());
        assert!(parse_name("home.notadate").is_none());
        assert!(parse_name("home.20151345").is_none()); // invalid month/day
    }

    // --- round trips ---

    #[test]
    fn round_trips_long() {
        let original = dt(0, 2024, 12, 31, 23, 59, 0);
        let name = make_name("data", original, TimestampFormat::Long);
        let p = parse_name(&name).unwrap();
        assert_eq!(p.basename, "data");
        assert!(p.has_exact_time);
        assert_eq!(p.naive, original.naive_local());
    }

    #[test]
    fn round_trips_long_iso() {
        let original = dt(2 * 3600, 2024, 6, 1, 8, 5, 9);
        let name = make_name("vm", original, TimestampFormat::LongIso);
        let p = parse_name(&name).unwrap();
        assert_eq!(p.offset, Some(FixedOffset::east_opt(2 * 3600).unwrap()));
        assert_eq!(p.naive.second(), 9);
    }
}
