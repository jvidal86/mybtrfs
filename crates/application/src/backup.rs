//! `BackupService` — powers `run` (snapshot → send/receive → prune),
//! `snapshot`, and `resume` (send without a new snapshot). Resolves the
//! incremental parent via the domain, transfers via `TransferPort` (which
//! verifies), and delegates deletion to `RetentionService`.
//! See `documentation/01-phases-design-v2.md` Phases 1–2.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use mybtrfs_domain::model::{RelationshipGraph, Subvolume};
use mybtrfs_domain::naming::{TimestampFormat, make_name, next_free_name};
use mybtrfs_domain::parent::ParentSelection;
use mybtrfs_domain::retention::{RetentionPolicy, Schedule};
use mybtrfs_domain::safety::{SafetyContext, latest_common_pair};

use crate::ports::{
    ClockPort, DeleteCommit, PortError, SnapshotPort, SubvolumeRepository, TransferPort,
};
use crate::retention::RetentionService;

/// The outcome of a `run`: the source snapshot and the verified backup created
/// this run, plus the preserve/delete partitions from pruning each set.
#[derive(Debug)]
pub struct RunReport {
    /// The read-only source snapshot created this run.
    pub snapshot: Subvolume,
    /// The verified backup received on the target this run.
    pub backup: Subvolume,
    /// Snapshot-side retention result (over the source `snapshot_dir`).
    pub snapshots_pruned: Schedule<Subvolume>,
    /// Backup-side retention result (over the `target_dir`).
    pub backups_pruned: Schedule<Subvolume>,
}

/// Orchestrates the backup operations over the driven ports: `snapshot` and the
/// full-backup `run` (snapshot → send/receive → prune), delegating retention to
/// [`RetentionService`]. Incremental parent resolution is a later increment.
pub struct BackupService<'a> {
    clock: &'a dyn ClockPort,
    repo: &'a dyn SubvolumeRepository,
    snapshots: &'a dyn SnapshotPort,
    transfer: &'a dyn TransferPort,
    retention: &'a RetentionService<'a>,
    format: TimestampFormat,
}

impl<'a> BackupService<'a> {
    /// Construct a service over the injected clock, subvolume repository,
    /// snapshot and transfer ports, the retention service, and the timestamp
    /// format used for snapshot names.
    #[must_use]
    pub fn new(
        clock: &'a dyn ClockPort,
        repo: &'a dyn SubvolumeRepository,
        snapshots: &'a dyn SnapshotPort,
        transfer: &'a dyn TransferPort,
        retention: &'a RetentionService<'a>,
        format: TimestampFormat,
    ) -> Self {
        Self {
            clock,
            repo,
            snapshots,
            transfer,
            retention,
            format,
        }
    }

    /// Create a read-only snapshot of `source` inside `snapshot_dir`, named
    /// `<basename>.<timestamp>` from the injected clock. If that name already
    /// exists in `snapshot_dir`, a `_N` collision counter is appended so an
    /// existing snapshot is never clobbered (re-runs stay non-destructive).
    /// Returns the created subvolume as reported by the snapshot port.
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the repository or snapshot port.
    pub fn snapshot(
        &self,
        source: &Path,
        snapshot_dir: &Path,
        basename: &str,
    ) -> Result<Subvolume, PortError> {
        let base = make_name(basename, self.clock.now(), self.format);

        let existing = self.repo.list(snapshot_dir)?;
        let leaves: Vec<&str> = existing
            .iter()
            .filter(|sv| is_in_dir(snapshot_dir, sv))
            .filter_map(|sv| sv.path.file_name().and_then(OsStr::to_str))
            .collect();

        let dest = snapshot_dir.join(next_free_name(&base, &leaves));
        self.snapshots.create_readonly(source, &dest)
    }

    /// Full backup cycle: create a read-only snapshot of `source`, send/receive
    /// it (full) into `target_dir`, then prune backups (`target_policy`) and
    /// snapshots (`snapshot_policy`).
    ///
    /// The just-created snapshot and backup, and the latest common
    /// snapshot/backup pair, are force-preserved (invariants #3/#4) so the next
    /// incremental keeps a parent on both ends. The target was just reached, so
    /// snapshot deletion is not skipped (#5). On a delete error the prune aborts
    /// (fail-fast, decision ID-1). Returns a [`RunReport`].
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the repository, snapshot, transfer, or
    /// delete ports; or [`PortError::Verification`] if the target backups carry a
    /// duplicate uuid (the cloned-disk guard, invariant #10).
    pub fn run(
        &self,
        source: &Path,
        snapshot_dir: &Path,
        basename: &str,
        target_dir: &Path,
        snapshot_policy: &RetentionPolicy,
        target_policy: &RetentionPolicy,
    ) -> Result<RunReport, PortError> {
        let snapshot = self.snapshot(source, snapshot_dir, basename)?;
        let backup =
            self.transfer
                .send_receive(&snapshot, &ParentSelection::default(), target_dir)?;

        // Candidate sets: what's on disk now, plus the just-created entries (a
        // stateless repository may not observe them until the next run).
        let snapshots = self.collect_in(snapshot_dir, &snapshot)?;
        let backups = self.collect_in(target_dir, &backup)?;

        // Force-preserve anchors: the just-created pair plus the latest common
        // snapshot/backup pair.
        let target_graph = RelationshipGraph::build(backups.clone())
            .map_err(|err| PortError::Verification(err.to_string()))?;
        let mut snapshot_preserve = HashSet::from([snapshot.id]);
        let mut backup_preserve = HashSet::from([backup.id]);
        if let Some(pair) = latest_common_pair(&snapshots, &target_graph) {
            snapshot_preserve.insert(pair.snapshot.id);
            backup_preserve.extend(pair.backups.iter().map(|b| b.id));
        }

        // Prune backups first, then snapshots (parallel to btrbk).
        let backups_pruned = self.retention.prune(
            &backups,
            target_policy,
            &SafetyContext {
                force_preserve_ids: backup_preserve,
                target_aborted: false,
            },
            DeleteCommit::Deferred,
        )?;
        let snapshots_pruned = self.retention.prune(
            &snapshots,
            snapshot_policy,
            &SafetyContext {
                force_preserve_ids: snapshot_preserve,
                target_aborted: false,
            },
            DeleteCommit::Deferred,
        )?;

        Ok(RunReport {
            snapshot,
            backup,
            snapshots_pruned,
            backups_pruned,
        })
    }

    /// List the subvolumes directly in `dir`, ensuring `just_created` is included
    /// (a stateless repository may not observe it until the next run).
    fn collect_in(
        &self,
        dir: &Path,
        just_created: &Subvolume,
    ) -> Result<Vec<Subvolume>, PortError> {
        let mut subvols: Vec<Subvolume> = self
            .repo
            .list(dir)?
            .into_iter()
            .filter(|sv| is_in_dir(dir, sv))
            .collect();
        if !subvols.iter().any(|sv| sv.id == just_created.id) {
            subvols.push(just_created.clone());
        }
        Ok(subvols)
    }
}

/// Whether `sv` lives directly in `dir` (its parent directory is `dir`).
fn is_in_dir(dir: &Path, sv: &Subvolume) -> bool {
    sv.mountpoint.join(&sv.path).parent() == Some(dir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, FixedOffset};

    use mybtrfs_domain::model::{Subvolume, Uuid};
    use mybtrfs_domain::naming::TimestampFormat;
    use mybtrfs_domain::parent::ParentSelection;
    use mybtrfs_domain::retention::{PreserveMin, RetentionPolicy};

    use crate::backup::BackupService;
    use crate::ports::{
        ClockPort, DeleteCommit, DeletePort, PortError, SnapshotPort, SubvolumeRepository,
        TransferPort,
    };
    use crate::retention::RetentionService;

    /// A clock fixed at a single instant.
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

    /// A `SnapshotPort` that records its calls and returns a synthetic read-only
    /// subvolume at the requested destination.
    #[derive(Default)]
    struct RecordingSnapshot {
        readonly_calls: RefCell<Vec<(PathBuf, PathBuf)>>,
    }
    impl RecordingSnapshot {
        fn readonly_calls(&self) -> Vec<(PathBuf, PathBuf)> {
            self.readonly_calls.borrow().clone()
        }
    }
    impl SnapshotPort for RecordingSnapshot {
        fn create_readonly(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError> {
            self.readonly_calls
                .borrow_mut()
                .push((source.to_path_buf(), dest.to_path_buf()));
            Ok(fake_subvol(dest))
        }
        fn make_writable(&self, _source: &Path, _dest: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("restore path not exercised by this test")
        }
    }

    /// A `SubvolumeRepository` returning a fixed set of pre-existing subvolumes
    /// (the same set for any query; callers filter by directory).
    #[derive(Default)]
    struct FakeRepo {
        subvols: Vec<Subvolume>,
    }
    impl SubvolumeRepository for FakeRepo {
        fn show(&self, _path: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("show not exercised by these tests")
        }
        fn list(&self, _filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
            Ok(self.subvols.clone())
        }
    }

    /// A `TransferPort` recording its calls and returning a verified full backup.
    #[derive(Default)]
    struct RecordingTransfer {
        calls: RefCell<Vec<(Subvolume, ParentSelection, PathBuf)>>,
    }
    impl RecordingTransfer {
        fn calls(&self) -> Vec<(Subvolume, ParentSelection, PathBuf)> {
            self.calls.borrow().clone()
        }
    }
    impl TransferPort for RecordingTransfer {
        fn send_receive(
            &self,
            source: &Subvolume,
            selection: &ParentSelection,
            target_dir: &Path,
        ) -> Result<Subvolume, PortError> {
            self.calls.borrow_mut().push((
                source.clone(),
                selection.clone(),
                target_dir.to_path_buf(),
            ));
            Ok(received_backup(target_dir, source))
        }
    }

    /// A `DeletePort` recording the paths (and commit mode) it was asked to delete.
    #[derive(Default)]
    struct RecordingDeleter {
        deleted: RefCell<Vec<(PathBuf, DeleteCommit)>>,
    }
    impl RecordingDeleter {
        fn paths(&self) -> Vec<PathBuf> {
            self.deleted
                .borrow()
                .iter()
                .map(|(p, _)| p.clone())
                .collect()
        }
    }
    impl DeletePort for RecordingDeleter {
        fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
            self.deleted.borrow_mut().push((path.to_path_buf(), commit));
            Ok(())
        }
    }

    /// A canonical UUID derived from a small integer tag.
    fn uuid_hex(tag: u64) -> Uuid {
        Uuid::parse(&format!("{tag:08x}-0000-0000-0000-000000000000")).expect("valid uuid")
    }

    /// The synthetic read-only snapshot a `SnapshotPort` returns post-create.
    fn fake_subvol(path: &Path) -> Subvolume {
        let uuid = Uuid::parse("11111111-1111-1111-1111-111111111111").expect("valid uuid");
        Subvolume {
            id: 256,
            uuid: Some(uuid.clone()),
            parent_uuid: None,
            received_uuid: None,
            generation: 10,
            cgen: 10,
            readonly: true,
            path: path.to_path_buf(),
            fs_uuid: uuid,
            mountpoint: PathBuf::from("/mnt/pool"),
        }
    }

    /// A verified full backup as `btrfs receive` + verification would yield:
    /// read-only, with a `received_uuid` (the source's uuid) and no `parent_uuid`.
    fn received_backup(target_dir: &Path, source: &Subvolume) -> Subvolume {
        let leaf = source.path.file_name().expect("snapshot has a leaf name");
        Subvolume {
            id: 400,
            uuid: Uuid::parse("33333333-3333-3333-3333-333333333333"),
            parent_uuid: None,
            received_uuid: source.uuid.clone(),
            generation: 20,
            cgen: 20,
            readonly: true,
            path: target_dir.join(leaf),
            fs_uuid: Uuid::parse("44444444-4444-4444-4444-444444444444").expect("valid uuid"),
            mountpoint: PathBuf::from("/mnt/drive"),
        }
    }

    /// A pre-existing read-only snapshot at `rel` (relative to the pool mountpoint).
    fn existing_snapshot(rel: &str) -> Subvolume {
        ro(300, uuid_hex(300), None, "/mnt/pool", rel)
    }

    /// A pre-existing read-only subvolume builder for snapshots and backups.
    fn ro(id: u64, uuid: Uuid, received: Option<Uuid>, mount: &str, rel: &str) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid),
            parent_uuid: None,
            received_uuid: received,
            generation: 5,
            cgen: 5,
            readonly: true,
            path: PathBuf::from(rel),
            fs_uuid: uuid_hex(0),
            mountpoint: PathBuf::from(mount),
        }
    }

    #[test]
    fn snapshot_creates_readonly_snapshot_named_with_timestamp() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        let created = service
            .snapshot(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
            )
            .expect("snapshot succeeds");

        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531"),
            )]
        );
        assert!(created.readonly);
        assert_eq!(
            created.path,
            PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531")
        );
    }

    #[test]
    fn snapshot_appends_collision_counter_when_name_already_exists() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let repo = FakeRepo {
            subvols: vec![existing_snapshot(".mybtrfs_snapshots/home.20240102T1531")],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        service
            .snapshot(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
            )
            .expect("snapshot succeeds");

        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531_1"),
            )]
        );
    }

    #[test]
    fn run_snapshots_full_send_receives_then_keeps_all_by_default() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        let report = service
            .run(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(), // keep-all snapshots
                &RetentionPolicy::default(), // keep-all backups
            )
            .expect("run succeeds");

        // Snapshot created, then a FULL send/receive of it into the target.
        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531"),
            )]
        );
        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        let (sent, selection, target) = &calls[0];
        assert_eq!(
            sent.path,
            PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531")
        );
        assert!(selection.parent.is_none(), "full send has no parent");
        assert_eq!(target, &PathBuf::from("/mnt/drive/host"));

        // Verified backup, and keep-all deletes nothing.
        assert!(report.backup.readonly);
        assert!(report.backup.received_uuid.is_some());
        assert!(report.backup.parent_uuid.is_none());
        assert!(deleter.paths().is_empty());
        assert!(report.snapshots_pruned.delete.is_empty());
        assert!(report.backups_pruned.delete.is_empty());
    }

    #[test]
    fn run_prunes_orphans_but_force_preserves_the_just_created_pair() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        // An old snapshot and an old backup, each uncorrelated (orphans).
        let repo = FakeRepo {
            subvols: vec![
                ro(
                    10,
                    uuid_hex(10),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                ),
                ro(
                    20,
                    uuid_hex(20),
                    Some(uuid_hex(0xff)),
                    "/mnt/drive",
                    "host/home.20240101T1200",
                ),
            ],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        let aggressive = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let report = service
            .run(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &aggressive,
                &aggressive,
            )
            .expect("run succeeds");

        // The just-created snapshot (256) and backup (400) survive; the orphans go.
        let snap_delete: Vec<u64> = report
            .snapshots_pruned
            .delete
            .iter()
            .map(|s| s.id)
            .collect();
        let backup_delete: Vec<u64> = report.backups_pruned.delete.iter().map(|s| s.id).collect();
        assert_eq!(snap_delete, vec![10]);
        assert_eq!(backup_delete, vec![20]);
        assert!(report.snapshots_pruned.preserve.iter().any(|s| s.id == 256));
        assert!(report.backups_pruned.preserve.iter().any(|s| s.id == 400));

        let mut deleted = deleter.paths();
        deleted.sort();
        assert_eq!(
            deleted,
            vec![
                PathBuf::from("/mnt/drive/host/home.20240101T1200"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240101T1200"),
            ]
        );
    }
}
