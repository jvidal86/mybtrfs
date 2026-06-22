//! `BackupService` — powers `run` (snapshot → send/receive → prune),
//! `snapshot`, and `resume` (send without a new snapshot). Resolves the
//! incremental parent via the domain, transfers via `TransferPort` (which
//! verifies), and delegates deletion to `RetentionService`.
//! See `documentation/01-phases-design-v2.md` Phases 1–2.

use std::ffi::OsStr;
use std::path::Path;

use mybtrfs_domain::model::Subvolume;
use mybtrfs_domain::naming::{TimestampFormat, make_name, next_free_name};
use mybtrfs_domain::parent::ParentSelection;

use crate::ports::{ClockPort, PortError, SnapshotPort, SubvolumeRepository, TransferPort};

/// Orchestrates the backup operations over the driven ports. Phase 1 covers
/// `snapshot` and the full-backup `run`; incremental parent resolution and
/// pruning follow in later increments.
pub struct BackupService<'a> {
    clock: &'a dyn ClockPort,
    repo: &'a dyn SubvolumeRepository,
    snapshots: &'a dyn SnapshotPort,
    transfer: &'a dyn TransferPort,
    format: TimestampFormat,
}

impl<'a> BackupService<'a> {
    /// Construct a service over the injected clock, subvolume repository,
    /// snapshot and transfer ports, and the timestamp format used for snapshot
    /// names.
    #[must_use]
    pub fn new(
        clock: &'a dyn ClockPort,
        repo: &'a dyn SubvolumeRepository,
        snapshots: &'a dyn SnapshotPort,
        transfer: &'a dyn TransferPort,
        format: TimestampFormat,
    ) -> Self {
        Self {
            clock,
            repo,
            snapshots,
            transfer,
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
            .filter(|sv| {
                let abs = sv.mountpoint.join(&sv.path);
                abs.parent() == Some(snapshot_dir)
            })
            .filter_map(|sv| sv.path.file_name().and_then(OsStr::to_str))
            .collect();

        let dest = snapshot_dir.join(next_free_name(&base, &leaves));
        self.snapshots.create_readonly(source, &dest)
    }

    /// Full backup cycle (Phase 1): create a read-only snapshot of `source`, then
    /// send/receive it into `target_dir` as a **full** transfer (no parent). The
    /// transfer port verifies the received subvolume (read-only, `received_uuid`
    /// set, `parent_uuid` unset for a full backup). Returns the verified backup.
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the repository, snapshot, or transfer port.
    pub fn run(
        &self,
        source: &Path,
        snapshot_dir: &Path,
        basename: &str,
        target_dir: &Path,
    ) -> Result<Subvolume, PortError> {
        let snapshot = self.snapshot(source, snapshot_dir, basename)?;
        // Phase 1: full send — incremental parent / clone-source resolution is
        // a later increment.
        let selection = ParentSelection::default();
        self.transfer
            .send_receive(&snapshot, &selection, target_dir)
    }
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

    use crate::backup::BackupService;
    use crate::ports::{ClockPort, PortError, SnapshotPort, SubvolumeRepository, TransferPort};

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
            Ok(fake_subvol(dest, true))
        }
        fn make_writable(&self, _source: &Path, _dest: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("restore path not exercised by this test")
        }
    }

    /// A `SubvolumeRepository` returning a fixed set of pre-existing subvolumes.
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

    /// A synthetic subvolume standing in for what btrfs would report post-create.
    fn fake_subvol(path: &Path, readonly: bool) -> Subvolume {
        let uuid = Uuid::parse("11111111-1111-1111-1111-111111111111").expect("valid uuid");
        Subvolume {
            id: 256,
            uuid: Some(uuid.clone()),
            parent_uuid: None,
            received_uuid: None,
            generation: 10,
            cgen: 10,
            readonly,
            path: path.to_path_buf(),
            fs_uuid: uuid,
            mountpoint: PathBuf::from("/mnt/pool"),
        }
    }

    /// A pre-existing read-only snapshot at `rel` (relative to the pool mountpoint).
    fn existing_snapshot(rel: &str) -> Subvolume {
        let uuid = Uuid::parse("22222222-2222-2222-2222-222222222222").expect("valid uuid");
        Subvolume {
            id: 300,
            uuid: Some(uuid.clone()),
            parent_uuid: None,
            received_uuid: None,
            generation: 5,
            cgen: 5,
            readonly: true,
            path: PathBuf::from(rel),
            fs_uuid: uuid,
            mountpoint: PathBuf::from("/mnt/pool"),
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

    /// A verified full backup as `btrfs receive` + verification would yield:
    /// read-only, with a `received_uuid`, and no `parent_uuid`.
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

    #[test]
    fn snapshot_creates_readonly_snapshot_named_with_timestamp() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let service =
            BackupService::new(&clock, &repo, &snapshots, &transfer, TimestampFormat::Long);

        let created = service
            .snapshot(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
            )
            .expect("snapshot succeeds");

        // It asked the SnapshotPort for a read-only snapshot at the timestamped dest.
        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531"),
            )]
        );
        // And returned the resulting (read-only) subvolume.
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
        let service =
            BackupService::new(&clock, &repo, &snapshots, &transfer, TimestampFormat::Long);

        service
            .snapshot(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
            )
            .expect("snapshot succeeds");

        // The new snapshot is named `_1`; the existing one is never clobbered.
        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531_1"),
            )]
        );
    }

    #[test]
    fn run_snapshots_then_full_send_receives_to_target() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let service =
            BackupService::new(&clock, &repo, &snapshots, &transfer, TimestampFormat::Long);

        let backup = service
            .run(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
            )
            .expect("run succeeds");

        // A read-only snapshot is created first.
        assert_eq!(
            snapshots.readonly_calls(),
            vec![(
                PathBuf::from("/mnt/pool/home"),
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531"),
            )]
        );
        // Then a FULL send/receive of that snapshot into the target dir.
        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        let (sent, selection, target) = &calls[0];
        assert_eq!(
            sent.path,
            PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1531")
        );
        assert!(
            selection.parent.is_none(),
            "Phase 1 is a full send (no parent)"
        );
        assert!(selection.clone_sources.is_empty());
        assert_eq!(target, &PathBuf::from("/mnt/drive/host"));
        // The returned backup is verified: read-only, received_uuid set, full (no parent).
        assert!(backup.readonly);
        assert!(backup.received_uuid.is_some());
        assert!(backup.parent_uuid.is_none());
    }
}
