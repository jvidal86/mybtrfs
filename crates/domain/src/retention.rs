//! `RetentionScheduler` — the pure hourly→daily→weekly→monthly→yearly cascade.
//!
//! Inputs: dated entries (each carrying its absolute instant for sorting and its
//! wall-clock time *in the reference timezone* for calendar math), a
//! [`RetentionPolicy`], and the reference `now` (also reference-tz wall-clock).
//! Output: a [`Schedule`] partitioning the payloads into preserve/delete.
//!
//! Faithful to btrbk `sub schedule`: the first/oldest entry of each period is the
//! representative and rolls up into the next tier; `preserve_min` is a separate
//! floor. Timezone is applied by the caller when building [`DatedEntry`] (so this
//! module is pure and tz-independent). btrbk-style CLI strings are parsed by
//! [`RetentionPolicy::parse`].
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use chrono::{Datelike, NaiveDateTime, TimeDelta, Timelike};
use std::collections::BTreeMap;

const HOURS_PER_DAY: i64 = 24;
const DAYS_PER_WEEK: i64 = 7;
const MONTHS_PER_YEAR: i64 = 12;

/// A calendar unit for `preserve_min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Hours,
    Days,
    Weeks,
    Months,
    Years,
}

/// The minimum-keep floor: a window in which *all* entries are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreserveMin {
    /// Preserve everything (btrbk default).
    All,
    /// Always preserve the single newest entry.
    Latest,
    /// No floor — preservation is decided solely by the tier schedule.
    None,
    /// Preserve everything within the last N units.
    Within(u32, Unit),
}

/// How many of a tier (hourly/daily/…) to preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierCount {
    /// Preserve all representatives of this tier.
    All,
    /// Preserve representatives for the last N periods.
    Count(u32),
}

/// A retention policy for one set (snapshots *or* backups).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Minimum-keep floor applied before tier logic (preserves a window unconditionally).
    pub preserve_min: PreserveMin,
    /// How many hourly representatives to keep; `None` means this tier is disabled.
    pub hourly: Option<TierCount>,
    /// How many daily representatives to keep; `None` means this tier is disabled.
    pub daily: Option<TierCount>,
    /// How many weekly representatives to keep; `None` means this tier is disabled.
    pub weekly: Option<TierCount>,
    /// How many monthly representatives to keep; `None` means this tier is disabled.
    pub monthly: Option<TierCount>,
    /// How many yearly representatives to keep; `None` means this tier is disabled.
    pub yearly: Option<TierCount>,
    /// Hour (0–23) at which a "day" begins.
    pub hour_of_day: u32,
    /// Weekday on which a "week" begins (also anchors months/years).
    pub day_of_week: chrono::Weekday,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            preserve_min: PreserveMin::All,
            hourly: None,
            daily: None,
            weekly: None,
            monthly: None,
            yearly: None,
            hour_of_day: 0,
            day_of_week: chrono::Weekday::Sun,
        }
    }
}

/// Failure parsing a btrbk-style retention string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RetentionParseError {
    /// A schedule token wasn't `<count|*><h|d|w|m|y>` (e.g. `7d`, `*w`).
    #[error("invalid retention token: {0:?}")]
    Token(String),
    /// `preserve_min` wasn't `all`, `latest`, `no`, or `<count><h|d|w|m|y>`.
    #[error("invalid preserve-min: {0:?}")]
    PreserveMin(String),
}

impl PreserveMin {
    /// Parse a `preserve_min` value: `all`, `latest`, `no`, or a window like
    /// `2d` / `18h` (btrbk-style).
    ///
    /// # Errors
    /// [`RetentionParseError::PreserveMin`] if the value is not recognized.
    pub fn parse(value: &str) -> Result<Self, RetentionParseError> {
        match value.trim() {
            "all" => Ok(Self::All),
            "latest" => Ok(Self::Latest),
            "no" => Ok(Self::None),
            other => split_count_unit(other)
                .map(|(count, unit)| Self::Within(count, unit))
                .ok_or_else(|| RetentionParseError::PreserveMin(other.to_string())),
        }
    }
}

impl RetentionPolicy {
    /// Build a policy from btrbk-style `preserve_min` and `preserve` (schedule)
    /// strings. The schedule is whitespace-separated `<count|*><unit>` tokens
    /// (e.g. `"24h 7d 4w 6m 5y"`, `"*d 4w"`); `"no"` or empty means no tiers.
    /// `hour_of_day`/`day_of_week` keep their defaults (set them separately).
    ///
    /// # Errors
    /// [`RetentionParseError`] if either string is malformed.
    pub fn parse(preserve_min: &str, preserve: &str) -> Result<Self, RetentionParseError> {
        let mut policy = Self {
            preserve_min: PreserveMin::parse(preserve_min)?,
            ..Self::default()
        };
        let schedule = preserve.trim();
        if !schedule.is_empty() && schedule != "no" {
            for token in schedule.split_whitespace() {
                let (unit, count) = parse_tier(token)?;
                // A unit must appear at most once: a repeated tier (e.g. a typo
                // `7d 4d`) would silently overwrite the first and drop the tier
                // the user meant — reject it like btrbk does.
                let tier = match unit {
                    Unit::Hours => &mut policy.hourly,
                    Unit::Days => &mut policy.daily,
                    Unit::Weeks => &mut policy.weekly,
                    Unit::Months => &mut policy.monthly,
                    Unit::Years => &mut policy.yearly,
                };
                if tier.is_some() {
                    return Err(RetentionParseError::Token(token.to_string()));
                }
                *tier = Some(count);
            }
        }
        Ok(policy)
    }
}

/// Map a unit suffix character to its [`Unit`].
fn unit_from_char(c: char) -> Option<Unit> {
    match c {
        'h' => Some(Unit::Hours),
        'd' => Some(Unit::Days),
        'w' => Some(Unit::Weeks),
        'm' => Some(Unit::Months),
        'y' => Some(Unit::Years),
        _ => None,
    }
}

/// Split a `<count><unit>` token into its count string and [`Unit`]; `None` if
/// the suffix isn't a known unit or the count is empty. (The unit chars are
/// ASCII, so trimming the final byte is always a valid boundary here.)
fn split_unit(token: &str) -> Option<(&str, Unit)> {
    let unit = unit_from_char(token.chars().next_back()?)?;
    let count = &token[..token.len() - 1];
    if count.is_empty() {
        None
    } else {
        Some((count, unit))
    }
}

/// Parse a numeric `<count><unit>` (no `*`), for `preserve_min` windows.
fn split_count_unit(token: &str) -> Option<(u32, Unit)> {
    let (count, unit) = split_unit(token)?;
    Some((count.parse().ok()?, unit))
}

/// Parse a schedule tier token `<count|*><unit>`.
fn parse_tier(token: &str) -> Result<(Unit, TierCount), RetentionParseError> {
    let (count, unit) =
        split_unit(token).ok_or_else(|| RetentionParseError::Token(token.to_string()))?;
    let tier = if count == "*" {
        TierCount::All
    } else {
        TierCount::Count(
            count
                .parse()
                .map_err(|_| RetentionParseError::Token(token.to_string()))?,
        )
    };
    Ok((unit, tier))
}

/// An entry to be scheduled. `instant` is the absolute time (for ordering);
/// `local` is the wall-clock time in the reference timezone (for calendar math).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatedEntry<T> {
    /// Unix timestamp (seconds) used for absolute ordering.
    pub instant: i64,
    /// Wall-clock time in the reference timezone, used for calendar-period math.
    pub local: NaiveDateTime,
    /// `true` when the source name carried an explicit HH:MM time component.
    pub has_exact_time: bool,
    /// Collision counter from the `_N` name suffix; `0` means no suffix.
    pub nn: u32,
    /// The caller-supplied data associated with this entry.
    pub payload: T,
}

/// The scheduler's verdict: payloads partitioned into preserve/delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule<T> {
    /// Entries that must be kept (sorted oldest-first by `instant`).
    pub preserve: Vec<T>,
    /// Entries that are safe to delete (sorted oldest-first by `instant`).
    pub delete: Vec<T>,
}

/// Calendar deltas of an entry relative to `now` (all "ago", in their unit).
struct Deltas {
    hours: i64,
    days: i64,
    weeks: i64,
    months: i64,
    years: i64,
}

/// Classify each entry as preserve or delete per `policy`, relative to `now`.
#[must_use]
pub fn schedule<T>(
    mut entries: Vec<DatedEntry<T>>,
    policy: &RetentionPolicy,
    now: NaiveDateTime,
) -> Schedule<T> {
    // Ascending by absolute time, then by collision counter (oldest first).
    entries.sort_by(|a, b| a.instant.cmp(&b.instant).then(a.nn.cmp(&b.nn)));

    let now_start_of_hour = start_of_hour(now);
    let deltas: Vec<Deltas> = entries
        .iter()
        .map(|entry| compute_deltas(entry, now, now_start_of_hour, policy))
        .collect();

    let count = entries.len();
    let mut preserve = vec![false; count];

    // preserve_min floor + first-of-hour buckets (oldest wins via insertion order).
    let mut first_hours: BTreeMap<i64, usize> = BTreeMap::new();
    for (idx, delta) in deltas.iter().enumerate() {
        if min_covers(policy, delta) {
            preserve[idx] = true;
        }
        first_hours.entry(delta.hours).or_insert(idx);
    }
    if matches!(policy.preserve_min, PreserveMin::Latest) && count > 0 {
        preserve[count - 1] = true;
    }

    // Cascade: each tier preserves its representatives (within its count) and
    // rolls the oldest representative up into the next coarser tier. The roll-up
    // happens unconditionally (independent of whether this tier preserves).
    let mut first_days: BTreeMap<i64, usize> = BTreeMap::new();
    for &idx in first_hours.values().rev() {
        if covers(policy.hourly, deltas[idx].hours) {
            preserve[idx] = true;
        }
        first_days.entry(deltas[idx].days).or_insert(idx);
    }
    let mut first_weeks: BTreeMap<i64, usize> = BTreeMap::new();
    for &idx in first_days.values().rev() {
        if covers(policy.daily, deltas[idx].days) {
            preserve[idx] = true;
        }
        first_weeks.entry(deltas[idx].weeks).or_insert(idx);
    }
    let mut first_weekly_months: BTreeMap<i64, usize> = BTreeMap::new();
    for &idx in first_weeks.values().rev() {
        if covers(policy.weekly, deltas[idx].weeks) {
            preserve[idx] = true;
        }
        first_weekly_months.entry(deltas[idx].months).or_insert(idx);
    }
    let mut first_monthly_years: BTreeMap<i64, usize> = BTreeMap::new();
    for &idx in first_weekly_months.values().rev() {
        if covers(policy.monthly, deltas[idx].months) {
            preserve[idx] = true;
        }
        first_monthly_years.entry(deltas[idx].years).or_insert(idx);
    }
    for &idx in first_monthly_years.values().rev() {
        if covers(policy.yearly, deltas[idx].years) {
            preserve[idx] = true;
        }
    }

    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for (idx, entry) in entries.into_iter().enumerate() {
        if preserve[idx] {
            keep.push(entry.payload);
        } else {
            drop.push(entry.payload);
        }
    }
    Schedule {
        preserve: keep,
        delete: drop,
    }
}

fn start_of_hour(dt: NaiveDateTime) -> NaiveDateTime {
    dt - TimeDelta::minutes(i64::from(dt.minute())) - TimeDelta::seconds(i64::from(dt.second()))
}

fn compute_deltas<T>(
    entry: &DatedEntry<T>,
    now: NaiveDateTime,
    now_start_of_hour: NaiveDateTime,
    policy: &RetentionPolicy,
) -> Deltas {
    let local = entry.local;

    let mut hours_from_day_start = i64::from(local.hour())
        - if entry.has_exact_time {
            i64::from(policy.hour_of_day)
        } else {
            0
        };
    let mut days_from_week_start = i64::from(local.weekday().num_days_from_sunday())
        - i64::from(policy.day_of_week.num_days_from_sunday());
    if hours_from_day_start < 0 {
        hours_from_day_start += HOURS_PER_DAY;
        days_from_week_start -= 1;
    }
    if days_from_week_start < 0 {
        days_from_week_start += DAYS_PER_WEEK;
    }

    // Months/years are anchored on the first `day_of_week` of the period.
    let mut month0 = i64::from(local.month0());
    let mut year = i64::from(local.year());
    if i64::from(local.day()) <= days_from_week_start {
        month0 -= 1;
        if month0 < 0 {
            month0 = MONTHS_PER_YEAR - 1;
            year -= 1;
        }
    }

    let hours = (now_start_of_hour - start_of_hour(local)).num_hours();
    let days = (hours + hours_from_day_start) / HOURS_PER_DAY;
    let weeks = (days + days_from_week_start) / DAYS_PER_WEEK;
    let years = i64::from(now.year()) - year;
    let months = years * MONTHS_PER_YEAR + (i64::from(now.month0()) - month0);

    Deltas {
        hours,
        days,
        weeks,
        months,
        years,
    }
}

/// Whether a tier preserves an entry whose delta (in that tier's unit) is `delta`.
fn covers(tier: Option<TierCount>, delta: i64) -> bool {
    match tier {
        Some(TierCount::All) => true,
        Some(TierCount::Count(count)) => count > 0 && delta <= i64::from(count),
        None => false,
    }
}

/// Whether the `preserve_min` floor covers an entry with these deltas.
fn min_covers(policy: &RetentionPolicy, deltas: &Deltas) -> bool {
    match policy.preserve_min {
        PreserveMin::All => true,
        PreserveMin::None | PreserveMin::Latest => false,
        PreserveMin::Within(count, unit) => {
            let delta = match unit {
                Unit::Hours => deltas.hours,
                Unit::Days => deltas.days,
                Unit::Weeks => deltas.weeks,
                Unit::Months => deltas.months,
                Unit::Years => deltas.years,
            };
            delta <= i64::from(count)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime};

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn at(
        label: &'static str,
        y: i32,
        mo: u32,
        d: u32,
        h: u32,
        mi: u32,
    ) -> DatedEntry<&'static str> {
        let local = dt(y, mo, d, h, mi);
        DatedEntry {
            instant: local.and_utc().timestamp(),
            local,
            has_exact_time: true,
            nn: 0,
            payload: label,
        }
    }

    fn sorted(mut v: Vec<&'static str>) -> Vec<&'static str> {
        v.sort_unstable();
        v
    }

    #[test]
    fn preserve_min_all_keeps_everything() {
        let entries = vec![at("a", 2024, 1, 1, 12, 0), at("b", 2024, 1, 2, 12, 0)];
        let p = RetentionPolicy::default(); // preserve_min = All
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["a", "b"]);
        assert!(s.delete.is_empty());
    }

    #[test]
    fn preserve_min_none_deletes_everything() {
        let entries = vec![at("a", 2024, 1, 1, 12, 0), at("b", 2024, 1, 2, 12, 0)];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert!(s.preserve.is_empty());
        assert_eq!(sorted(s.delete), vec!["a", "b"]);
    }

    #[test]
    fn preserve_min_latest_keeps_only_newest() {
        let entries = vec![
            at("old", 2024, 1, 1, 12, 0),
            at("mid", 2024, 1, 2, 12, 0),
            at("new", 2024, 1, 3, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::Latest,
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(s.preserve, vec!["new"]);
        assert_eq!(sorted(s.delete), vec!["mid", "old"]);
    }

    #[test]
    fn preserve_min_within_days_keeps_recent() {
        let entries = vec![
            at("d10", 2024, 1, 10, 12, 0),
            at("d09", 2024, 1, 9, 12, 0),
            at("d08", 2024, 1, 8, 12, 0),
            at("d07", 2024, 1, 7, 12, 0),
            at("d06", 2024, 1, 6, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::Within(2, Unit::Days),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["d08", "d09", "d10"]);
        assert_eq!(sorted(s.delete), vec!["d06", "d07"]);
    }

    #[test]
    fn daily_tier_keeps_first_of_day_for_n_days() {
        let entries = vec![
            at("d10", 2024, 1, 10, 12, 0),
            at("d09", 2024, 1, 9, 12, 0),
            at("d08", 2024, 1, 8, 12, 0),
            at("d07", 2024, 1, 7, 12, 0),
            at("d06", 2024, 1, 6, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            daily: Some(TierCount::Count(3)),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["d07", "d08", "d09", "d10"]);
        assert_eq!(sorted(s.delete), vec!["d06"]);
    }

    #[test]
    fn oldest_in_day_bucket_is_the_representative() {
        let entries = vec![
            at("morning", 2024, 1, 8, 8, 0),
            at("evening", 2024, 1, 8, 20, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            daily: Some(TierCount::Count(7)),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(s.preserve, vec!["morning"]);
        assert_eq!(s.delete, vec!["evening"]);
    }

    #[test]
    fn weekly_tier_keeps_first_of_week() {
        // Reference week starts on Sunday (default). One entry per week.
        let entries = vec![
            at("w0", 2024, 1, 10, 12, 0),
            at("w1", 2024, 1, 3, 12, 0),
            at("w2", 2023, 12, 27, 12, 0),
            at("w3", 2023, 12, 20, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            weekly: Some(TierCount::Count(2)),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["w0", "w1", "w2"]);
        assert_eq!(s.delete, vec!["w3"]);
    }

    #[test]
    fn monthly_tier_keeps_first_weekly_of_month() {
        // 20th of each month is past the first Sunday, so no month shift.
        let entries = vec![
            at("feb", 2024, 2, 20, 12, 0),
            at("jan", 2024, 1, 20, 12, 0),
            at("dec", 2023, 12, 20, 12, 0),
            at("nov", 2023, 11, 20, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            monthly: Some(TierCount::Count(2)),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 3, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["feb", "jan"]);
        assert_eq!(sorted(s.delete), vec!["dec", "nov"]);
    }

    #[test]
    fn all_tier_keeps_every_representative() {
        let entries = vec![
            at("d10", 2024, 1, 10, 12, 0),
            at("d09", 2024, 1, 9, 12, 0),
            at("d08", 2024, 1, 8, 12, 0),
        ];
        let p = RetentionPolicy {
            preserve_min: PreserveMin::None,
            daily: Some(TierCount::All),
            ..Default::default()
        };
        let s = schedule(entries, &p, dt(2024, 1, 10, 12, 0));
        assert_eq!(sorted(s.preserve), vec!["d08", "d09", "d10"]);
        assert!(s.delete.is_empty());
    }

    #[test]
    fn empty_input_yields_empty_schedule() {
        let p = RetentionPolicy::default();
        let s: Schedule<&str> = schedule(vec![], &p, dt(2024, 1, 10, 12, 0));
        assert!(s.preserve.is_empty());
        assert!(s.delete.is_empty());
    }

    // --- parsing ---

    #[test]
    fn parses_preserve_min_keywords_and_windows() {
        assert_eq!(PreserveMin::parse("all"), Ok(PreserveMin::All));
        assert_eq!(PreserveMin::parse("latest"), Ok(PreserveMin::Latest));
        assert_eq!(PreserveMin::parse("no"), Ok(PreserveMin::None));
        assert_eq!(
            PreserveMin::parse(" 2d "),
            Ok(PreserveMin::Within(2, Unit::Days))
        );
        assert_eq!(
            PreserveMin::parse("18h"),
            Ok(PreserveMin::Within(18, Unit::Hours))
        );
        assert!(PreserveMin::parse("nonsense").is_err());
        assert!(PreserveMin::parse("7").is_err()); // no unit
    }

    #[test]
    fn parses_full_retention_schedule() {
        let p = RetentionPolicy::parse("latest", "24h 7d 4w 6m 5y").unwrap();
        assert_eq!(p.preserve_min, PreserveMin::Latest);
        assert_eq!(p.hourly, Some(TierCount::Count(24)));
        assert_eq!(p.daily, Some(TierCount::Count(7)));
        assert_eq!(p.weekly, Some(TierCount::Count(4)));
        assert_eq!(p.monthly, Some(TierCount::Count(6)));
        assert_eq!(p.yearly, Some(TierCount::Count(5)));
    }

    #[test]
    fn parses_wildcard_tiers_and_partial_schedules() {
        let p = RetentionPolicy::parse("all", "*d 4w").unwrap();
        assert_eq!(p.daily, Some(TierCount::All));
        assert_eq!(p.weekly, Some(TierCount::Count(4)));
        assert_eq!(p.hourly, None);
        assert_eq!(p.monthly, None);
        assert_eq!(p.yearly, None);
    }

    #[test]
    fn no_or_empty_schedule_means_no_tiers() {
        let p = RetentionPolicy::parse("all", "no").unwrap();
        assert!(
            p.hourly.is_none()
                && p.daily.is_none()
                && p.weekly.is_none()
                && p.monthly.is_none()
                && p.yearly.is_none()
        );
        assert_eq!(
            RetentionPolicy::parse("all", "   ").unwrap(),
            RetentionPolicy::default()
        );
    }

    #[test]
    fn rejects_malformed_schedule_tokens() {
        assert!(RetentionPolicy::parse("all", "7x").is_err()); // unknown unit
        assert!(RetentionPolicy::parse("all", "d").is_err()); // no count
        assert!(RetentionPolicy::parse("all", "7d xyz").is_err()); // bad second token
        assert!(RetentionPolicy::parse("all", "7").is_err()); // no unit
    }

    #[test]
    fn rejects_duplicate_tier_tokens() {
        // A repeated unit (typo) must be rejected, not silently overwritten:
        // a user meaning "7d 4w" who mistypes "7d 4d" would otherwise lose the
        // weekly tier (and backups) with no error. btrbk rejects this too.
        assert!(RetentionPolicy::parse("all", "7d 4d").is_err());
        assert!(RetentionPolicy::parse("all", "7d 4w 4d").is_err());
        assert!(RetentionPolicy::parse("all", "*d 4d").is_err());
        // A valid, non-duplicate schedule still parses with the right tiers.
        let p = RetentionPolicy::parse("all", "7d 4w").unwrap();
        assert_eq!(p.daily, Some(TierCount::Count(7)));
        assert_eq!(p.weekly, Some(TierCount::Count(4)));
        assert_eq!(p.hourly, None);
        assert_eq!(p.monthly, None);
        assert_eq!(p.yearly, None);
    }
}
