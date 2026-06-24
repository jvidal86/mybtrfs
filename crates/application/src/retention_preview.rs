//! Retention preview — format what `prune --dry-run` would keep and delete.
//!
//! Human-readable output with clear KEEP / DELETE sections, counts, and ages.
//! The clock is injected (`now: DateTime<FixedOffset>`) so output is
//! deterministic and fully unit-testable without ambient system time (rule 6).

use chrono::{DateTime, FixedOffset};

use mybtrfs_domain::model::Subvolume;
use mybtrfs_domain::naming::parse_name;
use mybtrfs_domain::retention::Schedule;

/// Format a retention [`Schedule<Subvolume>`] as a human-readable preview.
///
/// Output has two sections — KEEP and DELETE — each listing snapshot names
/// and their ages relative to `now`. When the delete list is empty the DELETE
/// section says "nothing to delete". No emoji or terminal escapes; plain text
/// suitable for pipes and log files.
///
/// # Arguments
/// * `schedule` — the computed schedule (preserve / delete partitions)
/// * `label`    — section label prefix (e.g. `"snapshots"`, `"backups"`)
/// * `now`      — injected current time (deterministic, from the `ClockPort`)
#[must_use]
pub fn format_schedule(
    schedule: &Schedule<Subvolume>,
    label: &str,
    now: DateTime<FixedOffset>,
) -> String {
    let mut out = String::new();

    // KEEP section
    out.push_str(&format!(
        "{} KEEP ({}):\n",
        label.to_uppercase(),
        count_label(schedule.preserve.len())
    ));
    if schedule.preserve.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for sv in &schedule.preserve {
            let name = snapshot_name(sv);
            let age = age_string(name, now);
            out.push_str(&format!("  keep  {name}  [{age}]\n"));
        }
    }

    // DELETE section
    out.push_str(&format!(
        "{} DELETE ({}):\n",
        label.to_uppercase(),
        count_label(schedule.delete.len())
    ));
    if schedule.delete.is_empty() {
        out.push_str("  (nothing to delete)\n");
    } else {
        for sv in &schedule.delete {
            let name = snapshot_name(sv);
            let age = age_string(name, now);
            out.push_str(&format!("  delete  {name}  [{age}]\n"));
        }
    }

    out
}

/// Compute a human-readable age string for a snapshot name at `now`.
///
/// Parses the ISO timestamp from the snapshot basename (e.g.
/// `"data.20260624T143210"`) and returns a phrase like `"3 days ago"` or
/// `"2 hours ago"`. Returns `"unknown age"` if the name cannot be parsed.
#[must_use]
pub fn age_string(name: &str, now: DateTime<FixedOffset>) -> String {
    let Some(parsed) = parse_name(name) else {
        return "unknown age".to_owned();
    };
    let snap_dt = match parsed.naive.and_local_timezone(now.timezone()).single() {
        Some(dt) => dt,
        None => return "unknown age".to_owned(),
    };
    let secs = now.signed_duration_since(snap_dt).num_seconds().max(0) as u64;

    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3_600 {
        plural(secs / 60, "minute")
    } else if secs < 86_400 {
        plural(secs / 3_600, "hour")
    } else {
        plural(secs / 86_400, "day")
    }
}

fn snapshot_name(sv: &Subvolume) -> &str {
    sv.path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

fn count_label(n: usize) -> String {
    if n == 1 {
        "1 snapshot".to_owned()
    } else {
        format!("{n} snapshots")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;

    use mybtrfs_domain::model::Uuid;

    use super::*;

    /// Fixed "now" for deterministic tests: 2026-06-24T14:32:10+00:00
    fn fixed_now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 6, 24, 14, 32, 10)
            .unwrap()
    }

    fn mock_sv(name: &str, id: u64) -> Subvolume {
        let uuid_str = format!("{:08x}-0000-0000-0000-000000000000", id);
        Subvolume {
            id,
            uuid: Uuid::parse(&uuid_str),
            parent_uuid: None,
            received_uuid: None,
            path: PathBuf::from(name),
            mountpoint: PathBuf::from("/mnt"),
            generation: 0,
            cgen: 0,
            readonly: true,
            fs_uuid: Uuid::parse("12345678-1234-1234-1234-123456789012").expect("valid uuid"),
        }
    }

    // ── age_string ────────────────────────────────────────────────────────────

    #[test]
    fn age_string_just_now() {
        // snapshot at exactly now (long format = Thhmm)
        let age = age_string("data.20260624T1432", fixed_now());
        assert_eq!(age, "just now");
    }

    #[test]
    fn age_string_minutes() {
        // snapshot 30 minutes before now (14:02)
        let age = age_string("data.20260624T1402", fixed_now());
        assert_eq!(age, "30 minutes ago");
    }

    #[test]
    fn age_string_one_hour() {
        // snapshot 1 hour before now (13:32)
        let age = age_string("data.20260624T1332", fixed_now());
        assert_eq!(age, "1 hour ago");
    }

    #[test]
    fn age_string_hours() {
        // snapshot 3 hours before now (11:32)
        let age = age_string("data.20260624T1132", fixed_now());
        assert_eq!(age, "3 hours ago");
    }

    #[test]
    fn age_string_one_day() {
        // snapshot 1 day before now (2026-06-23T14:32)
        let age = age_string("data.20260623T1432", fixed_now());
        assert_eq!(age, "1 day ago");
    }

    #[test]
    fn age_string_many_days() {
        // snapshot 7 days before now (2026-06-17T14:32)
        let age = age_string("data.20260617T1432", fixed_now());
        assert_eq!(age, "7 days ago");
    }

    #[test]
    fn age_string_unknown_for_unparseable_name() {
        let age = age_string("not-a-snapshot-name", fixed_now());
        assert_eq!(age, "unknown age");
    }

    // ── format_schedule ───────────────────────────────────────────────────────

    #[test]
    fn format_schedule_keep_section_lists_names_and_ages() {
        let schedule = Schedule {
            preserve: vec![
                mock_sv("data.20260624T1432", 1), // just now
                mock_sv("data.20260623T1432", 2), // 1 day ago
            ],
            delete: vec![],
        };
        let out = format_schedule(&schedule, "snapshots", fixed_now());

        assert!(
            out.contains("SNAPSHOTS KEEP (2 snapshots)"),
            "missing keep header: {out}"
        );
        assert!(
            out.contains("keep  data.20260624T1432"),
            "missing keep entry 1: {out}"
        );
        assert!(
            out.contains("keep  data.20260623T1432"),
            "missing keep entry 2: {out}"
        );
        assert!(out.contains("[just now]"), "missing age for today: {out}");
        assert!(
            out.contains("[1 day ago]"),
            "missing age for yesterday: {out}"
        );
    }

    #[test]
    fn format_schedule_delete_section_lists_names_and_ages() {
        let schedule = Schedule {
            preserve: vec![mock_sv("data.20260624T1432", 1)],
            delete: vec![
                mock_sv("data.20260617T1432", 10), // 7 days ago
                mock_sv("data.20260610T1432", 11), // 14 days ago
            ],
        };
        let out = format_schedule(&schedule, "snapshots", fixed_now());

        assert!(
            out.contains("SNAPSHOTS DELETE (2 snapshots)"),
            "missing delete header: {out}"
        );
        assert!(
            out.contains("delete  data.20260617T1432"),
            "missing delete entry 1: {out}"
        );
        assert!(
            out.contains("delete  data.20260610T1432"),
            "missing delete entry 2: {out}"
        );
        assert!(out.contains("[7 days ago]"), "missing age 7d: {out}");
        assert!(out.contains("[14 days ago]"), "missing age 14d: {out}");
    }

    #[test]
    fn format_schedule_empty_delete_shows_nothing_to_delete() {
        let schedule = Schedule {
            preserve: vec![mock_sv("data.20260624T1432", 1)],
            delete: vec![],
        };
        let out = format_schedule(&schedule, "snapshots", fixed_now());

        assert!(
            out.contains("(nothing to delete)"),
            "missing nothing-to-delete: {out}"
        );
        assert!(
            !out.contains("delete  "),
            "no delete entries expected: {out}"
        );
    }

    #[test]
    fn format_schedule_empty_preserve_shows_none() {
        let schedule = Schedule {
            preserve: vec![],
            delete: vec![mock_sv("data.20260617T1432", 10)],
        };
        let out = format_schedule(&schedule, "snapshots", fixed_now());

        assert!(out.contains("(none)"), "missing none placeholder: {out}");
        assert!(
            out.contains("delete  data.20260617T1432"),
            "missing delete entry: {out}"
        );
    }

    #[test]
    fn format_schedule_uses_label_as_section_prefix() {
        let schedule = Schedule {
            preserve: vec![],
            delete: vec![],
        };
        let snap_out = format_schedule(&schedule, "snapshots", fixed_now());
        let back_out = format_schedule(&schedule, "backups", fixed_now());

        assert!(
            snap_out.contains("SNAPSHOTS KEEP"),
            "snapshot label: {snap_out}"
        );
        assert!(
            back_out.contains("BACKUPS KEEP"),
            "backup label: {back_out}"
        );
    }

    #[test]
    fn format_schedule_singular_count_label() {
        let schedule = Schedule {
            preserve: vec![mock_sv("data.20260624T1432", 1)],
            delete: vec![mock_sv("data.20260617T1432", 2)],
        };
        let out = format_schedule(&schedule, "snapshots", fixed_now());

        assert!(out.contains("KEEP (1 snapshot)"), "singular keep: {out}");
        assert!(
            out.contains("DELETE (1 snapshot)"),
            "singular delete: {out}"
        );
    }

    #[test]
    fn format_schedule_is_deterministic() {
        let schedule = Schedule {
            preserve: vec![mock_sv("data.20260624T1432", 1)],
            delete: vec![mock_sv("data.20260617T1432", 2)],
        };
        let out1 = format_schedule(&schedule, "snapshots", fixed_now());
        let out2 = format_schedule(&schedule, "snapshots", fixed_now());
        assert_eq!(out1, out2);
    }
}
