//! Retention preview — pretty-print what `prune` would delete before it runs.
//!
//! TDD stubs for Phase 1: extend the existing `Schedule<T>` display logic into a
//! human-readable formatter. No new logic; just a view over existing prune results.

#[cfg(test)]
mod tests {
    use crate::retention::Schedule;
    use mybtrfs_domain::model::Subvolume;
    use std::path::PathBuf;

    /// Helper: construct a mock Subvolume for testing.
    fn mock_snapshot(name: &str, id: u64) -> Subvolume {
        Subvolume {
            id,
            uuid: format!("{:032x}", id).parse().unwrap(),
            parent_uuid: None,
            received_uuid: None,
            path: PathBuf::from(name),
            mountpoint: PathBuf::from("/mnt"),
            generation: 0,
            cgen: 0,
            readonly: true,
            fs_uuid: format!("{:032x}", id).parse().unwrap(),
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
        let output = format_schedule(&schedule);

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
        let output = format_schedule(&schedule);

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
        let output = format_schedule(&schedule);

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
        let output = format_schedule(&schedule);

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
        let output = format_schedule(&schedule);

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
        let output = format_schedule(&schedule);

        // Assert
        // TODO: verify snapshot name is present and readable in output
        // TODO: verify output is valid UTF-8 and doesn't contain unescaped control chars
    }

    /// **TEST: age calculation (e.g., "7 days ago") is based on system clock**
    ///
    /// Snapshot names contain ISO timestamps; formatter parses them and computes age.
    /// Requires ClockPort injection for "now".
    #[test]
    fn format_schedule_computes_age_from_snapshot_timestamp() {
        // Arrange
        // TODO: mock ClockPort to return a fixed "now" time
        // TODO: create a snapshot with a known timestamp (e.g., 7 days ago)
        let preserve = vec![mock_snapshot("data.20260617T143210", 1)]; // 7 days ago
        let schedule = Schedule {
            preserve,
            delete: vec![],
        };

        // Act
        let output = format_schedule(&schedule);

        // Assert
        // TODO: verify output shows "7 days ago" (or similar)
        // TODO: verify age is within 1 minute of expected (clock jitter tolerance)
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
// Stub function signatures (to be implemented)
// ============================================================================

/// Format a retention `Schedule<T>` as a human-readable preview string.
/// Separates PRESERVE and DELETE partitions, includes counts and ages.
///
/// # Arguments
/// * `schedule` — the computed schedule (preserve/delete partitions)
///
/// # Returns
/// A formatted string suitable for terminal display.
pub fn format_schedule(_schedule: &Schedule<Subvolume>) -> String {
    todo!("implement format_schedule")
}

/// Parse a snapshot name (ISO timestamp) and compute age relative to "now".
///
/// # Arguments
/// * `name` — snapshot basename (e.g., "data.20260624T143210")
/// * `now` — current time (injected from ClockPort)
///
/// # Returns
/// A human-readable age string (e.g., "7 days ago", "2 hours ago").
pub fn compute_age(_name: &str, _now: &std::time::SystemTime) -> String {
    todo!("implement compute_age")
}
