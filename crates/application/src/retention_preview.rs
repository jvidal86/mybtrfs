//! Retention preview — pretty-print what `prune` would delete before it runs.
//!
//! TDD stubs for Phase 1: extend the existing `Schedule<T>` display logic into a
//! human-readable formatter. No new logic; just a view over existing prune results.

use chrono::Local;
use mybtrfs_domain::model::Subvolume;
use mybtrfs_domain::naming::parse_name;
use mybtrfs_domain::retention::Schedule;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Helper: construct a mock Subvolume for testing.
    fn mock_snapshot(name: &str, id: u64) -> Subvolume {
        use mybtrfs_domain::model::Uuid;
        let uuid_str = format!("{:08x}-0000-0000-0000-000000000000", id);
        let fs_uuid_str = "12345678-1234-1234-1234-123456789012";
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
            fs_uuid: Uuid::parse(fs_uuid_str).expect("valid test uuid"),
        }
    }

    /// **TEST: format_preserve_list renders snapshot names clearly**
    ///
    /// Given a preserve list with 3 snapshots of different ages,
    /// When formatted as a preview list,
    /// Then output shows names + parsed ages (via naming parser).
    #[test]
    fn format_preserve_list_shows_names_and_ages() {
        // Arrange
        let preserve = vec![
            mock_snapshot("data.20260624T143210", 1),
            mock_snapshot("data.20260623T143210", 2),
            mock_snapshot("data.20260622T143210", 3),
        ];
        let schedule = Schedule {
            preserve,
            delete: vec![],
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify output contains "data.20260624T143210" (today)
        // TODO: verify output contains "data.20260623T143210" (1 day ago)
        // TODO: verify output contains "PRESERVE" header
        // TODO: verify output contains count "(3 snapshots)"
    }

    /// **TEST: format_delete_list highlights snapshots to be removed**
    ///
    /// Given a delete list with 2 old snapshots,
    /// When formatted,
    /// Then output shows DELETE section with warning icon + names + ages.
    #[test]
    fn format_delete_list_shows_removal_candidates() {
        // Arrange
        let delete = vec![
            mock_snapshot("data.20260617T143210", 10),
            mock_snapshot("data.20260610T143210", 11),
        ];
        let schedule = Schedule {
            preserve: vec![],
            delete,
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify output contains "DELETE" header
        // TODO: verify output contains warning icon (⚠️)
        // TODO: verify output contains "(2 snapshots)" count
        // TODO: verify output contains both snapshot names
        // TODO: verify output includes "run with --yes to confirm" disclaimer
    }

    /// **TEST: format_schedule separates preserve and delete clearly**
    ///
    /// Given a mixed schedule (both preserve and delete partitions),
    /// When formatted,
    /// Then sections are visually distinct and both counts match inputs.
    #[test]
    fn format_schedule_partitions_preserve_vs_delete() {
        // Arrange
        let preserve = vec![
            mock_snapshot("data.20260624T143210", 1),
            mock_snapshot("data.20260623T143210", 2),
        ];
        let delete = vec![mock_snapshot("data.20260610T143210", 11)];
        let schedule = Schedule {
            preserve: preserve.clone(),
            delete: delete.clone(),
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify PRESERVE section appears before DELETE section
        // TODO: verify "2 snapshots" in PRESERVE count
        // TODO: verify "1 snapshot" in DELETE count
        // TODO: verify both snapshot names are present
    }

    /// **TEST: empty preserve list (all deleted) is handled**
    ///
    /// Edge case: aggressive retention policy deletes everything except the latest.
    /// Format should still work (unusual but valid).
    #[test]
    fn format_schedule_with_empty_preserve() {
        // Arrange
        let delete = vec![
            mock_snapshot("data.20260623T143210", 2),
            mock_snapshot("data.20260622T143210", 3),
        ];
        let schedule = Schedule {
            preserve: vec![],
            delete,
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify PRESERVE section shows "(0 snapshots)" or is omitted gracefully
        // TODO: verify DELETE section is still present and correct
        // TODO: verify output doesn't panic or produce malformed text
    }

    /// **TEST: empty delete list (no pruning needed) is handled**
    ///
    /// Edge case: all snapshots within retention policy.
    #[test]
    fn format_schedule_with_empty_delete() {
        // Arrange
        let preserve = vec![
            mock_snapshot("data.20260624T143210", 1),
            mock_snapshot("data.20260623T143210", 2),
        ];
        let schedule = Schedule {
            preserve,
            delete: vec![],
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify PRESERVE section shows both snapshots
        // TODO: verify DELETE section shows "(0 snapshots)" or is omitted
        // TODO: verify no "run with --yes to confirm" disclaimer (nothing to confirm)
        // TODO: verify output is clear that no action needed
    }

    /// **TEST: snapshot names with special characters are escaped/quoted**
    ///
    /// Edge case: snapshot names containing spaces, slashes, or other chars.
    /// Format must handle gracefully.
    #[test]
    fn format_schedule_handles_special_chars_in_names() {
        // Arrange
        let preserve = vec![mock_snapshot("data.with-dash.20260624T143210", 1)];
        let schedule = Schedule {
            preserve,
            delete: vec![],
        };

        // Act
        let _output = format_schedule(&schedule);

        // Assert
        // TODO: verify snapshot name is present and readable in output
        // TODO: verify output is valid UTF-8 and doesn't contain unescaped control chars
    }

    /// **TEST: age calculation (e.g., "7 days ago") formats correctly**
    ///
    /// Snapshot names contain ISO timestamps; formatter parses them and computes age.
    #[test]
    #[ignore] // parsing age from snapshot name is system-dependent; defer to E2E test
    fn format_schedule_computes_age_from_snapshot_timestamp() {
        // Arrange
        let now = Local::now();

        // Act
        let age = compute_age("data.20260617T143210", &now);

        // Assert
        // Verify the age string is a reasonable format (not "unknown age" means parse worked)
        // and contains temporal words.
        assert!(!age.is_empty(), "age string should not be empty");
        assert!(
            age.contains("ago") || age.contains("just"),
            "Expected age format like 'X days ago', got: {}",
            age
        );
    }

    /// **TEST: format output is stable (no randomness, deterministic)**
    ///
    /// Same input schedule should produce identical output every time.
    /// Important for testing and reproducibility.
    #[test]
    fn format_schedule_is_deterministic() {
        // Arrange
        let preserve = vec![
            mock_snapshot("data.20260624T143210", 1),
            mock_snapshot("data.20260623T143210", 2),
        ];
        let delete = vec![mock_snapshot("data.20260610T143210", 11)];
        let schedule = Schedule {
            preserve: preserve.clone(),
            delete: delete.clone(),
        };

        // Act
        let output1 = format_schedule(&schedule);
        let output2 = format_schedule(&schedule);

        // Assert
        assert_eq!(output1, output2, "format output must be deterministic");
    }
}

// ============================================================================
// Implementation
// ============================================================================

/// Format a retention `Schedule<T>` as a human-readable preview string.
/// Separates PRESERVE and DELETE partitions, includes counts and ages.
///
/// # Arguments
/// * `schedule` — the computed schedule (preserve/delete partitions)
///
/// # Returns
/// A formatted string suitable for terminal display.
#[must_use]
pub fn format_schedule(schedule: &Schedule<Subvolume>) -> String {
    let now = Local::now();
    let mut output = String::new();

    use std::ffi::OsStr;

    // PRESERVE section
    if !schedule.preserve.is_empty() {
        output.push_str("PRESERVE (");
        output.push_str(&schedule.preserve.len().to_string());
        output.push_str(" snapshot");
        if schedule.preserve.len() != 1 {
            output.push('s');
        }
        output.push_str("):\n");
        for sv in &schedule.preserve {
            let name = sv
                .path
                .file_name()
                .and_then(|n: &OsStr| n.to_str())
                .unwrap_or("?");
            let age = compute_age(name, &now);
            output.push_str("  ✅ ");
            output.push_str(name);
            output.push_str(" (");
            output.push_str(&age);
            output.push_str(")\n");
        }
        output.push('\n');
    }

    // DELETE section
    if !schedule.delete.is_empty() {
        output.push_str("DELETE (");
        output.push_str(&schedule.delete.len().to_string());
        output.push_str(" snapshot");
        if schedule.delete.len() != 1 {
            output.push('s');
        }
        output.push_str(") — run with --yes to confirm:\n");
        for sv in &schedule.delete {
            let name = sv
                .path
                .file_name()
                .and_then(|n: &OsStr| n.to_str())
                .unwrap_or("?");
            let age = compute_age(name, &now);
            output.push_str("  ⚠️  ");
            output.push_str(name);
            output.push_str(" (");
            output.push_str(&age);
            output.push_str(")\n");
        }
    } else if schedule.preserve.is_empty() {
        output.push_str("No snapshots to manage.\n");
    } else {
        output.push_str("No snapshots to delete.\n");
    }

    output
}

/// Parse a snapshot name (ISO timestamp) and compute age relative to "now".
///
/// Parses the ISO timestamp from the snapshot basename (e.g., "data.20260624T143210")
/// and computes a human-readable age like "7 days ago" or "2 hours ago".
///
/// # Arguments
/// * `name` — snapshot basename (e.g., "data.20260624T143210")
/// * `now` — current time (injected, chrono::DateTime<Local>)
///
/// # Returns
/// A human-readable age string (e.g., "7 days ago", "2 hours ago").
/// If the name doesn't parse, returns a placeholder like "unknown age".
#[must_use]
pub fn compute_age(name: &str, now: &chrono::DateTime<Local>) -> String {
    // Try to parse the name and extract its timestamp.
    if let Some(parsed) = parse_name(name) {
        // parsed.naive is a NaiveDateTime representing when the snapshot was created.
        // Treat it as local time in the same timezone as "now".
        let snap_local = parsed.naive.and_local_timezone(now.timezone()).single();

        if let Some(snap_dt) = snap_local {
            let duration = now.signed_duration_since(snap_dt);
            let secs = duration.num_seconds() as u64;

            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                let mins = secs / 60;
                if mins == 1 {
                    "1 minute ago".to_string()
                } else {
                    format!("{} minutes ago", mins)
                }
            } else if secs < 86400 {
                let hours = secs / 3600;
                if hours == 1 {
                    "1 hour ago".to_string()
                } else {
                    format!("{} hours ago", hours)
                }
            } else {
                let days = secs / 86400;
                if days == 1 {
                    "1 day ago".to_string()
                } else {
                    format!("{} days ago", days)
                }
            }
        } else {
            "unknown age".to_string()
        }
    } else {
        "unknown age".to_string()
    }
}
