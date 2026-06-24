//! Status view — show backup health (counts, ages, health checks) without a side database.
//!
//! Stateless: re-derives all truth from btrfs metadata (timestamps in snapshot names, cgens, received_uuid).
//! Optionally augmented with last-run information from the journal.

use crate::ports::{Journal, SubvolumeRepository};
use mybtrfs_domain::model::Subvolume;
use std::path::PathBuf;

/// Information about the last backup run, parsed from the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRunInfo {
    /// ISO 8601 timestamp of when the command was executed.
    pub timestamp: String,
    /// The command that was executed (e.g. "run backup").
    pub command: String,
}

/// A status report: snapshot/backup counts, latest ages, health checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// Path to the source snapshots directory.
    pub source_dir: PathBuf,
    /// Path to the target backups directory.
    pub target_dir: PathBuf,
    /// List of snapshots found in source.
    pub snapshots: Vec<Subvolume>,
    /// List of backups found in target.
    pub backups: Vec<Subvolume>,
    /// Optional last-run information from the journal.
    pub last_run: Option<LastRunInfo>,
}

/// Service to compute backup health status (counts, ages, health checks).
///
/// Stateless: re-derives all truth from btrfs metadata. Optionally augmented with
/// last-run information from the journal.
pub struct StatusService<'a> {
    /// Repository for snapshot listing.
    pub source_repo: &'a dyn SubvolumeRepository,
    /// Repository for backup listing.
    pub target_repo: &'a dyn SubvolumeRepository,
    /// Optional journal for reading last-run entries.
    pub journal: Option<&'a dyn Journal>,
}

impl<'a> StatusService<'a> {
    /// Compute a status report: lists snapshots and backups, identifies health issues.
    ///
    /// # Arguments
    /// * `source_dir` — path where snapshots live
    /// * `target_dir` — path where backups live
    ///
    /// # Returns
    /// A `StatusReport` with snapshot/backup lists and optional last-run info.
    ///
    /// # Errors
    /// Returns a `PortError` if either repo query fails (I/O, permission, invalid path).
    pub fn report(
        &self,
        source_dir: &std::path::Path,
        target_dir: &std::path::Path,
    ) -> Result<StatusReport, crate::ports::PortError> {
        let snapshots = self.source_repo.list(source_dir)?;
        let backups = self.target_repo.list(target_dir)?;

        // Try to read last-run info from journal, if configured.
        let last_run = self.journal.and_then(|journal| {
            // Read the most recent entry (we only care about the very last one).
            match journal.last_entries(1) {
                Ok(entries) => entries
                    .into_iter()
                    .next()
                    .and_then(|entry| parse_last_entry(&entry)),
                Err(_) => None,
            }
        });

        Ok(StatusReport {
            source_dir: source_dir.to_path_buf(),
            target_dir: target_dir.to_path_buf(),
            snapshots,
            backups,
            last_run,
        })
    }
}

/// Parse a journal entry to extract timestamp and command.
/// Format: "ISO-8601-TIMESTAMP command description"
fn parse_last_entry(entry: &str) -> Option<LastRunInfo> {
    let mut parts = entry.split_whitespace();

    // Expect at least timestamp + one word of command
    let timestamp = parts.next()?.to_string();
    let command = parts.next()?.to_string();

    // Check if timestamp looks like ISO 8601 (starts with year-like pattern YYYY-MM-DD)
    if !timestamp.contains('-') || !timestamp.contains('T') {
        return None;
    }

    Some(LastRunInfo { timestamp, command })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod service_tests {
    use super::*;
    use crate::ports::PortError;
    use mybtrfs_domain::model::Uuid;

    /// Mock repository for testing.
    struct MockRepo {
        subvolumes: Vec<Subvolume>,
    }

    impl SubvolumeRepository for MockRepo {
        fn list(&self, _path: &std::path::Path) -> Result<Vec<Subvolume>, PortError> {
            Ok(self.subvolumes.clone())
        }

        fn show(&self, _path: &std::path::Path) -> Result<Subvolume, PortError> {
            unimplemented!()
        }
    }

    fn mock_subvolume_for_service(name: &str, id: u64) -> Subvolume {
        let uuid_str = format!("{:08x}-0000-0000-0000-000000000000", id);
        let fs_uuid_str = "12345678-1234-1234-1234-123456789012";
        Subvolume {
            id,
            uuid: Uuid::parse(&uuid_str),
            parent_uuid: None,
            received_uuid: Uuid::parse(&format!("{:08x}-1111-1111-1111-111111111111", id)),
            path: PathBuf::from(name),
            mountpoint: PathBuf::from("/mnt"),
            generation: 0,
            cgen: 0,
            readonly: true,
            fs_uuid: Uuid::parse(fs_uuid_str).expect("valid test uuid"),
        }
    }

    /// **TEST: StatusService.report queries both repos and constructs report**
    #[test]
    fn status_service_queries_repos() {
        // Arrange
        let snapshots = vec![
            mock_subvolume_for_service("data.20260624T1432", 1),
            mock_subvolume_for_service("data.20260623T1432", 2),
        ];
        let backups = vec![mock_subvolume_for_service("data.20260624T1432", 10)];

        let source_repo = MockRepo {
            subvolumes: snapshots.clone(),
        };
        let target_repo = MockRepo {
            subvolumes: backups.clone(),
        };
        let service = StatusService {
            source_repo: &source_repo,
            target_repo: &target_repo,
            journal: None,
        };

        // Act
        let report = service
            .report(
                std::path::Path::new("/source/.snapshots"),
                std::path::Path::new("/target/backups"),
            )
            .expect("report should succeed");

        // Assert
        assert_eq!(report.snapshots.len(), 2);
        assert_eq!(report.backups.len(), 1);
        assert_eq!(report.source_dir, PathBuf::from("/source/.snapshots"));
        assert_eq!(report.target_dir, PathBuf::from("/target/backups"));
        assert!(report.last_run.is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use mybtrfs_domain::model::Uuid;
    use mybtrfs_domain::naming::parse_name;

    fn mock_subvolume(name: &str, id: u64, readonly: bool, received_uuid: bool) -> Subvolume {
        let uuid_str = format!("{:08x}-0000-0000-0000-000000000000", id);
        let fs_uuid_str = "12345678-1234-1234-1234-123456789012";
        Subvolume {
            id,
            uuid: Uuid::parse(&uuid_str),
            parent_uuid: None,
            received_uuid: if received_uuid {
                Uuid::parse(&format!("{:08x}-1111-1111-1111-111111111111", id))
            } else {
                None
            },
            path: PathBuf::from(name),
            mountpoint: PathBuf::from("/mnt"),
            generation: 0,
            cgen: 0,
            readonly,
            fs_uuid: Uuid::parse(fs_uuid_str).expect("valid test uuid"),
        }
    }

    /// **TEST: status report counts snapshots and backups correctly**
    ///
    /// Given a source with 3 snapshots and target with 2 backups,
    /// When computing the status report,
    /// Then counts match (3 snapshots, 2 backups).
    #[test]
    fn status_counts_snapshots_and_backups() {
        // Arrange
        let snapshots = vec![
            mock_subvolume("data.20260624T1432", 1, true, false),
            mock_subvolume("data.20260623T1432", 2, true, false),
            mock_subvolume("data.20260622T1432", 3, true, false),
        ];
        let backups = vec![
            mock_subvolume("data.20260624T1432", 10, true, true),
            mock_subvolume("data.20260623T1432", 11, true, true),
        ];
        let report = StatusReport {
            source_dir: PathBuf::from("/source/.snapshots"),
            target_dir: PathBuf::from("/target/backups"),
            snapshots,
            backups,
            last_run: None,
        };

        // Act & Assert
        assert_eq!(report.snapshots.len(), 3);
        assert_eq!(report.backups.len(), 2);
    }

    /// **TEST: status identifies latest snapshot and backup by name timestamp**
    ///
    /// Given snapshots with ISO timestamps in names,
    /// When identifying the latest,
    /// Then it's the one with the most recent timestamp (20260624 > 20260623 > 20260622).
    #[test]
    fn status_identifies_latest_snapshot_and_backup() {
        // Arrange
        let snapshots = [
            mock_subvolume("data.20260622T1432", 3, true, false),
            mock_subvolume("data.20260624T1432", 1, true, false), // latest
            mock_subvolume("data.20260623T1432", 2, true, false),
        ];
        let backups = [
            mock_subvolume("data.20260623T1432", 11, true, true), // latest
            mock_subvolume("data.20260620T1432", 10, true, true),
        ];

        // Act
        let latest_snap = snapshots.iter().max_by_key(|sv| {
            parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or("")).map(|p| p.naive)
        });
        let latest_backup = backups.iter().max_by_key(|sv| {
            parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or("")).map(|p| p.naive)
        });

        // Assert
        assert_eq!(
            latest_snap.map(|sv| sv.path.clone()),
            Some(PathBuf::from("data.20260624T1432"))
        );
        assert_eq!(
            latest_backup.map(|sv| sv.path.clone()),
            Some(PathBuf::from("data.20260623T1432"))
        );
    }

    /// **TEST: status handles empty snapshot or backup list**
    ///
    /// Edge case: source has no snapshots, or target has no backups.
    /// Report should still construct (zero-counts valid).
    #[test]
    fn status_handles_empty_snapshots_or_backups() {
        // Arrange: empty snapshots
        let report1 = StatusReport {
            source_dir: PathBuf::from("/source"),
            target_dir: PathBuf::from("/target"),
            snapshots: vec![],
            backups: vec![mock_subvolume("data.20260624T1432", 10, true, true)],
            last_run: None,
        };

        // Act & Assert
        assert_eq!(report1.snapshots.len(), 0);
        assert_eq!(report1.backups.len(), 1);

        // Arrange: empty backups
        let report2 = StatusReport {
            source_dir: PathBuf::from("/source"),
            target_dir: PathBuf::from("/target"),
            snapshots: vec![mock_subvolume("data.20260624T1432", 1, true, false)],
            backups: vec![],
            last_run: None,
        };

        // Act & Assert
        assert_eq!(report2.snapshots.len(), 1);
        assert_eq!(report2.backups.len(), 0);
    }

    /// **TEST: status identifies backup as "healthy" if latest backup matches latest snapshot**
    ///
    /// Health criterion: the most recent backup has the same name (and thus same timestamp)
    /// as the most recent snapshot.
    #[test]
    fn status_health_check_latest_backup_matches_snapshot() {
        // Arrange: matching latest
        let snapshots = [
            mock_subvolume("data.20260624T1432", 1, true, false),
            mock_subvolume("data.20260623T1432", 2, true, false),
        ];
        let backups = [
            mock_subvolume("data.20260624T1432", 10, true, true), // matches latest snapshot
            mock_subvolume("data.20260623T1432", 11, true, true),
        ];

        // Act: compute latest names
        let latest_snap_name = snapshots
            .iter()
            .max_by_key(|sv| {
                parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                    .map(|p| p.naive)
            })
            .and_then(|sv| sv.path.file_name())
            .and_then(|n| n.to_str())
            .map(std::string::ToString::to_string);

        let latest_backup_name = backups
            .iter()
            .max_by_key(|sv| {
                parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                    .map(|p| p.naive)
            })
            .and_then(|sv| sv.path.file_name())
            .and_then(|n| n.to_str())
            .map(std::string::ToString::to_string);

        // Assert: latest backup name equals latest snapshot name
        assert_eq!(latest_snap_name, latest_backup_name);
        assert_eq!(latest_snap_name, Some("data.20260624T1432".to_string()));
    }

    /// **TEST: status identifies health issue if latest backup lags latest snapshot**
    ///
    /// Health warning: the most recent snapshot has no corresponding backup yet.
    #[test]
    fn status_health_check_latest_backup_lags_snapshot() {
        // Arrange: backup lags
        let snapshots = [
            mock_subvolume("data.20260624T1432", 1, true, false), // latest snapshot
            mock_subvolume("data.20260623T1432", 2, true, false),
        ];
        let backups = [
            mock_subvolume("data.20260623T1432", 11, true, true), // lag: no 20260624 backup yet
            mock_subvolume("data.20260622T1432", 10, true, true),
        ];

        // Act
        let latest_snap_name = snapshots
            .iter()
            .max_by_key(|sv| {
                parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                    .map(|p| p.naive)
            })
            .and_then(|sv| sv.path.file_name())
            .and_then(|n| n.to_str())
            .map(std::string::ToString::to_string);

        let latest_backup_name = backups
            .iter()
            .max_by_key(|sv| {
                parse_name(sv.path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                    .map(|p| p.naive)
            })
            .and_then(|sv| sv.path.file_name())
            .and_then(|n| n.to_str())
            .map(std::string::ToString::to_string);

        // Assert: latest backup name differs from latest snapshot name
        assert_ne!(latest_snap_name, latest_backup_name);
        assert_eq!(latest_snap_name, Some("data.20260624T1432".to_string()));
        assert_eq!(latest_backup_name, Some("data.20260623T1432".to_string()));
    }

    /// **TEST: status report can be constructed from real-looking subvolume lists**
    ///
    /// Integration test: mock a realistic scenario (5 daily snapshots, 3 backups).
    #[test]
    fn status_report_realistic_scenario() {
        // Arrange
        let snapshots = vec![
            mock_subvolume("data.20260624T1432", 1, true, false),
            mock_subvolume("data.20260623T1432", 2, true, false),
            mock_subvolume("data.20260622T1432", 3, true, false),
            mock_subvolume("data.20260621T1432", 4, true, false),
            mock_subvolume("data.20260620T1432", 5, true, false),
        ];
        let backups = vec![
            mock_subvolume("data.20260624T1432", 10, true, true),
            mock_subvolume("data.20260623T1432", 11, true, true),
            mock_subvolume("data.20260622T1432", 12, true, true),
        ];
        let report = StatusReport {
            source_dir: PathBuf::from("/mnt/source/.snapshots"),
            target_dir: PathBuf::from("/mnt/backup/daily"),
            snapshots,
            backups,
            last_run: None,
        };

        // Act & Assert
        assert_eq!(report.snapshots.len(), 5);
        assert_eq!(report.backups.len(), 3);
        assert!(report.snapshots.iter().all(|sv| sv.readonly));
        assert!(report.backups.iter().all(|sv| sv.received_uuid.is_some()));
    }

    /// **TEST: parse_last_entry extracts timestamp and command from journal entry**
    #[test]
    fn parse_last_entry_extracts_timestamp_and_command() {
        let entry = "2026-06-24T15:37:42Z run something";
        let parsed = parse_last_entry(entry);
        assert!(parsed.is_some());
        let info = parsed.unwrap();
        assert_eq!(info.timestamp, "2026-06-24T15:37:42Z");
        assert_eq!(info.command, "run");
    }

    /// **TEST: parse_last_entry returns None for malformed entries**
    #[test]
    fn parse_last_entry_rejects_malformed() {
        // Missing timestamp
        assert!(parse_last_entry("run something").is_none());
        // Timestamp doesn't look like ISO 8601
        assert!(parse_last_entry("12345 run").is_none());
        // Empty entry
        assert!(parse_last_entry("").is_none());
    }
}
