//! `PruneService` — powers the standalone `prune` command. Gathers the source
//! snapshots and target backups, force-preserves the latest common
//! snapshot/backup pair (invariant #4) so a prune never strands the next
//! incremental, and deletes the rest per policy via [`RetentionService`]
//! (keep-all by default). Dry-run is the composition root's concern (inject a
//! no-op delete port). See `documentation/01-phases-design-v2.md` Phase 3.

use std::collections::HashSet;
use std::path::Path;

use mybtrfs_domain::model::{RelationshipGraph, Subvolume};
use mybtrfs_domain::retention::{RetentionPolicy, Schedule};
use mybtrfs_domain::safety::{SafetyContext, latest_common_pair};

use crate::ports::{DeleteCommit, NULL_PROGRESS, PortError, ProgressPort, SubvolumeRepository};
use crate::retention::RetentionService;

/// The outcome of a `prune`: the preserve/delete partitions for each set.
#[derive(Debug)]
pub struct PruneReport {
    /// Snapshot-side retention result (over the source `snapshot_dir`).
    pub snapshots_pruned: Schedule<Subvolume>,
    /// Backup-side retention result (over the `target_dir`).
    pub backups_pruned: Schedule<Subvolume>,
}

/// Orchestrates a standalone prune over the source/target repositories,
/// delegating per-set deletion to [`RetentionService`].
pub struct PruneService<'a> {
    source_repo: &'a dyn SubvolumeRepository,
    target_repo: &'a dyn SubvolumeRepository,
    retention: &'a RetentionService<'a>,
    /// Progress reporter; [`NullProgress`](crate::ports::NullProgress) by
    /// default (no-op). Set via [`with_progress`](Self::with_progress).
    progress: &'a dyn ProgressPort,
}

impl<'a> PruneService<'a> {
    /// Construct a service over the source/target repositories and the retention
    /// service. Progress reporting defaults to no-op.
    #[must_use]
    pub fn new(
        source_repo: &'a dyn SubvolumeRepository,
        target_repo: &'a dyn SubvolumeRepository,
        retention: &'a RetentionService<'a>,
    ) -> Self {
        Self {
            source_repo,
            target_repo,
            retention,
            progress: &NULL_PROGRESS,
        }
    }

    /// Set the [`ProgressPort`] for this service. Returns `self` for chaining.
    #[must_use]
    pub fn with_progress(mut self, progress: &'a dyn ProgressPort) -> Self {
        self.progress = progress;
        self
    }

    /// Prune snapshots in `snapshot_dir` (`snapshot_policy`) and backups in
    /// `target_dir` (`target_policy`), force-preserving the latest common
    /// snapshot/backup pair. Backups are pruned before snapshots (parallel to
    /// btrbk); a delete error aborts fail-fast (decision ID-1). Returns the
    /// preserve/delete partitions.
    ///
    /// # Errors
    /// [`PortError`] from either repository or the delete port;
    /// [`PortError::Verification`] if the target backups carry a duplicate uuid
    /// (the cloned-disk guard, invariant #10).
    pub fn prune(
        &self,
        snapshot_dir: &Path,
        target_dir: &Path,
        snapshot_policy: &RetentionPolicy,
        target_policy: &RetentionPolicy,
    ) -> Result<PruneReport, PortError> {
        self.progress
            .start_spinner("Scanning snapshots and backups…");
        let snapshots = subvols_in(self.source_repo, snapshot_dir)?;
        let backups = subvols_in(self.target_repo, target_dir)?;
        self.progress.finish("");

        // Force-preserve the latest common snapshot/backup pair (invariant #4).
        // (Mirrors BackupService::prune_both's anchor — minus any "just-created"
        // seed — and is a candidate for future DRY-ing with it.)
        let target_graph = RelationshipGraph::build(backups.clone())
            .map_err(|err| PortError::Verification(err.to_string()))?;
        let mut snapshot_preserve: HashSet<u64> = HashSet::new();
        let mut backup_preserve: HashSet<u64> = HashSet::new();
        if let Some(pair) = latest_common_pair(&snapshots, &target_graph) {
            snapshot_preserve.insert(pair.snapshot.id);
            backup_preserve.extend(pair.backups.iter().map(|b| b.id));
        }

        // Invariant #5: a missing destination must not cost the only resumable
        // copy. If the source has snapshots but the target lists ZERO backups, the
        // target is treated as unreachable/aborted (e.g. root-on-btrfs where an
        // unmounted drive's mountpoint resolves to `/`, yielding no backups) — so
        // ALL source-snapshot deletions are rescued. The backup side still prunes
        // normally (it has nothing to delete).
        let target_aborted = !snapshots.is_empty() && backups.is_empty();
        if target_aborted {
            log::warn!(
                "target appears unreachable (no backups for {} source snapshots); \
                 skipping all snapshot deletion (invariant #5)",
                snapshots.len()
            );
        }

        let backups_pruned = self.retention.prune(
            &backups,
            target_policy,
            &SafetyContext {
                force_preserve_ids: backup_preserve,
                target_aborted: false,
            },
            DeleteCommit::Deferred,
        )?;

        let backup_delete_count = backups_pruned.delete.len();
        if backup_delete_count > 0 {
            self.progress
                .start_bar("Pruning backups", backup_delete_count as u64);
            self.progress
                .finish(&format!("Pruned {backup_delete_count} backups"));
        }

        let snapshots_pruned = self.retention.prune(
            &snapshots,
            snapshot_policy,
            &SafetyContext {
                force_preserve_ids: snapshot_preserve,
                target_aborted,
            },
            DeleteCommit::Deferred,
        )?;

        let snapshot_delete_count = snapshots_pruned.delete.len();
        if snapshot_delete_count > 0 {
            self.progress
                .start_bar("Pruning snapshots", snapshot_delete_count as u64);
            self.progress
                .finish(&format!("Pruned {snapshot_delete_count} snapshots"));
        }

        Ok(PruneReport {
            snapshots_pruned,
            backups_pruned,
        })
    }
}

/// The subvolumes directly in `dir` (via `repo`, scoped to that directory's
/// filesystem). Foreign-named entries are passed through but are never deleted —
/// the retention scheduler skips names that don't parse.
fn subvols_in(repo: &dyn SubvolumeRepository, dir: &Path) -> Result<Vec<Subvolume>, PortError> {
    Ok(repo
        .list(dir)?
        .into_iter()
        .filter(|sv| sv.mountpoint.join(&sv.path).parent() == Some(dir))
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, FixedOffset};

    use mybtrfs_domain::model::{Subvolume, Uuid};
    use mybtrfs_domain::retention::{PreserveMin, RetentionPolicy};

    use crate::ports::{ClockPort, DeleteCommit, DeletePort, PortError, SubvolumeRepository};
    use crate::prune::PruneService;
    use crate::retention::RetentionService;

    struct FixedClock(DateTime<FixedOffset>);
    impl FixedClock {
        fn at(rfc3339: &str) -> Self {
            Self(DateTime::parse_from_rfc3339(rfc3339).expect("valid rfc3339"))
        }
    }
    impl ClockPort for FixedClock {
        fn now(&self) -> DateTime<FixedOffset> {
            self.0
        }
    }

    struct FakeRepo {
        subvols: Vec<Subvolume>,
    }
    impl SubvolumeRepository for FakeRepo {
        fn show(&self, _path: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("not exercised by these tests")
        }
        fn list(&self, _filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
            Ok(self.subvols.clone())
        }
    }

    #[derive(Default)]
    struct RecordingDeleter {
        deleted: RefCell<Vec<PathBuf>>,
    }
    impl RecordingDeleter {
        fn paths(&self) -> Vec<PathBuf> {
            self.deleted.borrow().clone()
        }
    }
    impl DeletePort for RecordingDeleter {
        fn delete(&self, path: &Path, _commit: DeleteCommit) -> Result<(), PortError> {
            self.deleted.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    fn uuid_hex(tag: u64) -> Uuid {
        Uuid::parse(&format!("{tag:08x}-0000-0000-0000-000000000000")).expect("valid uuid")
    }

    fn ro(
        id: u64,
        uuid: u64,
        received: Option<u64>,
        mount: &str,
        rel: &str,
        cgen: u64,
    ) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid_hex(uuid)),
            parent_uuid: None,
            received_uuid: received.map(uuid_hex),
            generation: cgen,
            cgen,
            readonly: true,
            path: PathBuf::from(rel),
            fs_uuid: uuid_hex(0),
            mountpoint: PathBuf::from(mount),
        }
    }

    /// S1 (backed up by B1) and a newer, un-backed S2.
    fn fixtures() -> (FakeRepo, FakeRepo) {
        let source = FakeRepo {
            subvols: vec![
                ro(
                    1,
                    1,
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                    5,
                ),
                ro(
                    2,
                    2,
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240102T1200",
                    10,
                ),
            ],
        };
        let target = FakeRepo {
            subvols: vec![ro(
                11,
                11,
                Some(1),
                "/mnt/drive",
                "host/home.20240101T1200",
                6,
            )],
        };
        (source, target)
    }

    #[test]
    fn keep_all_default_prunes_nothing() {
        crate::init_test_logger();
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let (source, target) = fixtures();
        let service = PruneService::new(&source, &target, &retention);

        let report = service
            .prune(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(), // keep-all
                &RetentionPolicy::default(),
            )
            .expect("prune succeeds");

        assert!(deleter.paths().is_empty());
        assert!(report.snapshots_pruned.delete.is_empty());
        assert!(report.backups_pruned.delete.is_empty());
    }

    #[test]
    fn aggressive_policy_prunes_but_preserves_the_latest_common_pair() {
        crate::init_test_logger();
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let (source, target) = fixtures();
        let service = PruneService::new(&source, &target, &retention);

        let aggressive = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let report = service
            .prune(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
                &aggressive,
                &aggressive,
            )
            .expect("prune succeeds");

        // The latest common pair (snapshot 1 + its backup 11) is preserved; the
        // un-backed newer snapshot (2) is deleted.
        let snap_delete: Vec<u64> = report
            .snapshots_pruned
            .delete
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(snap_delete, vec![2]);
        assert!(report.snapshots_pruned.preserve.iter().any(|s| s.id == 1));
        assert!(report.backups_pruned.delete.is_empty());
        assert!(report.backups_pruned.preserve.iter().any(|b| b.id == 11));

        assert_eq!(
            deleter.paths(),
            vec![PathBuf::from(
                "/mnt/pool/.mybtrfs_snapshots/home.20240102T1200"
            )]
        );
    }

    #[test]
    fn empty_target_treated_as_aborted_rescues_all_source_snapshots() {
        crate::init_test_logger();
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        // Source snapshots present, but the TARGET is empty (invariant #5: an
        // unmounted/unreachable drive — e.g. root-on-btrfs where the mountpoint
        // resolves to `/` — lists zero backups). The snapshots are the only
        // resumable copies and must NOT be pruned.
        let source = FakeRepo {
            subvols: vec![
                ro(
                    1,
                    1,
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                    5,
                ),
                ro(
                    2,
                    2,
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240102T1200",
                    10,
                ),
            ],
        };
        let target = FakeRepo { subvols: vec![] };
        let service = PruneService::new(&source, &target, &retention);

        let aggressive = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let report = service
            .prune(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
                &aggressive,
                &aggressive,
            )
            .expect("prune succeeds");

        // ZERO source snapshots deleted: the empty target is treated as aborted.
        assert!(
            report.snapshots_pruned.delete.is_empty(),
            "no source snapshot may be pruned when the target is unreachable"
        );
        let preserved: Vec<u64> = report
            .snapshots_pruned
            .preserve
            .iter()
            .map(|s| s.id)
            .collect();
        assert!(preserved.contains(&1));
        assert!(preserved.contains(&2));
        // No source snapshot path was ever handed to the delete port.
        assert!(
            !deleter.paths().iter().any(|p| p.starts_with("/mnt/pool")),
            "no source snapshot deletion may reach the delete port"
        );
    }

    #[test]
    fn empty_source_and_empty_target_prunes_nothing() {
        crate::init_test_logger();
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        // Both sides empty: nothing to prune and no spurious aborted behavior.
        let source = FakeRepo { subvols: vec![] };
        let target = FakeRepo { subvols: vec![] };
        let service = PruneService::new(&source, &target, &retention);

        let aggressive = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let report = service
            .prune(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
                &aggressive,
                &aggressive,
            )
            .expect("prune succeeds");

        assert!(deleter.paths().is_empty());
        assert!(report.snapshots_pruned.delete.is_empty());
        assert!(report.backups_pruned.delete.is_empty());
    }
}
