//! `RestoreService` — powers `restore` (a mybtrfs addition that automates
//! btrbk's documented manual procedure). Makes a **writable** snapshot of a
//! backup (never `property set ro=false`), and verifies the result's
//! `received_uuid` is empty so future incrementals stay intact (invariant #7).
//! See `documentation/01-phases-design-v2.md` Phase 4.
//!
//! A backup already on the destination filesystem is restored directly with a
//! writable snapshot. A backup that lives only on a **different** filesystem (a
//! remote/external target) is first **transferred back**: send/receive a
//! read-only copy onto the destination filesystem, make *that* writable, then
//! delete the intermediate copy. The two cases are told apart by whether `dest`
//! falls under the backup's filesystem mountpoint — and both failure modes are
//! safe: a misjudged same-fs makes a cross-filesystem `btrfs snapshot` fail
//! cleanly, and a misjudged remote does a correct (if needless) transfer.

use std::path::{Path, PathBuf};

use mybtrfs_domain::model::Subvolume;
use mybtrfs_domain::parent::ParentSelection;

use crate::ports::{
    DeleteCommit, DeletePort, FilesystemPort, PortError, SnapshotPort, SubvolumeRepository,
    TransferPort,
};

/// The outcome of a `restore`: the writable subvolume produced, where any
/// pre-existing destination was moved aside, and whether this was a dry run.
#[derive(Debug)]
pub struct RestoreReport {
    /// The destination path the restored subvolume occupies (or, on a dry run,
    /// would occupy).
    pub dest: PathBuf,
    /// The writable restored subvolume. `None` on a dry run, where nothing is
    /// created (invariant #8); `Some` on a committing run.
    pub restored: Option<Subvolume>,
    /// Where a pre-existing destination was (or, on a dry run, would be) moved —
    /// a non-colliding `<dest>.broken[.N]` — if `force` displaced one.
    pub moved_aside: Option<PathBuf>,
    /// True when the backup lived on a different filesystem and was (or, on a dry
    /// run, would be) transferred back via send/receive before being made
    /// writable; false for an in-place same-filesystem restore.
    pub transferred_back: bool,
    /// True when this report describes a side-effect-free `--dry-run` plan: no
    /// move-aside and no `make_writable` were executed (invariant #8).
    pub dry_run: bool,
}

/// Why a restore could not complete.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The destination already exists and `force` was not set.
    #[error("destination already exists: {0} (use --force to move it aside)")]
    DestinationExists(PathBuf),
    /// The restore did not yield a clean writable subvolume (it is read-only or
    /// carries a `received_uuid`). Restore must never produce a received
    /// subvolume — that would poison future incrementals (#7). If `force` had
    /// displaced an existing destination, `moved_aside` records where the
    /// original data now lives so the user can recover it.
    #[error(
        "restore did not yield a clean writable subvolume{}",
        moved_aside.as_ref().map(|p| format!(" (original moved aside to {})", p.display())).unwrap_or_default()
    )]
    NotCleanWritable {
        /// Where `force` moved the pre-existing destination, if any — so the
        /// stranded original can be found after a failed verification.
        moved_aside: Option<PathBuf>,
    },
    /// An underlying driven-port failure.
    #[error(transparent)]
    Port(#[from] PortError),
}

/// Orchestrates restore: guard the destination, bring the backup onto the
/// destination filesystem (directly, or by transferring it back), make a writable
/// snapshot, and verify the result is a clean writable copy.
pub struct RestoreService<'a> {
    repo: &'a dyn SubvolumeRepository,
    snapshots: &'a dyn SnapshotPort,
    transfer: &'a dyn TransferPort,
    deleter: &'a dyn DeletePort,
    fs: &'a dyn FilesystemPort,
}

impl<'a> RestoreService<'a> {
    /// Construct a service over the repository, snapshot, transfer, delete, and
    /// filesystem ports. The repository resolves the backup's filesystem (to tell
    /// a local restore from a transfer-back); transfer + delete carry out and then
    /// clean up a transfer-back.
    #[must_use]
    pub fn new(
        repo: &'a dyn SubvolumeRepository,
        snapshots: &'a dyn SnapshotPort,
        transfer: &'a dyn TransferPort,
        deleter: &'a dyn DeletePort,
        fs: &'a dyn FilesystemPort,
    ) -> Self {
        Self {
            repo,
            snapshots,
            transfer,
            deleter,
            fs,
        }
    }

    /// Restore `backup` (a read-only subvolume) to a writable subvolume at `dest`.
    ///
    /// If the backup lives on a different filesystem than `dest`, it is first
    /// transferred back (a full send/receive) into the destination filesystem and
    /// the resulting read-only copy is made writable, then deleted; otherwise the
    /// backup is made writable in place. Either way the writable copy comes from
    /// [`SnapshotPort::make_writable`] (never by flipping the read-only property,
    /// which would poison `received_uuid` — #7) and is verified.
    ///
    /// If `dest` already exists, restore refuses unless `force` is set, in which
    /// case the existing destination is moved aside to a non-colliding
    /// `<dest>.broken[.N]` first (an existing move-aside is never overwritten —
    /// the displaced dataset is preserved, not destroyed).
    ///
    /// When `dry_run` is set the call is a side-effect-free preview (invariant
    /// #8): it short-circuits *before* both the move-aside rename and
    /// `make_writable`, returning a [`RestoreReport`] whose `moved_aside` and
    /// (planned) `restored` describe the intended operations without executing
    /// either. The existence/`force` refusal still applies so the preview is
    /// honest about what would happen.
    ///
    /// # Errors
    /// [`RestoreError::DestinationExists`] if `dest` exists and `!force`;
    /// [`RestoreError::NotCleanWritable`] if the result isn't a clean writable
    /// copy; [`RestoreError::Port`] for any underlying port failure.
    pub fn restore(
        &self,
        backup: &Path,
        dest: &Path,
        force: bool,
        dry_run: bool,
    ) -> Result<RestoreReport, RestoreError> {
        log::info!(
            "restore: {} → {} (force={force}, dry_run={dry_run})",
            backup.display(),
            dest.display()
        );
        // Resolve the backup to learn which filesystem it is on (and to have a
        // Subvolume to send if a transfer-back is needed). `dest` is on the same
        // filesystem iff it falls under the backup's mountpoint; otherwise the
        // backup is remote and must be transferred back first.
        let backup_subvol = self.repo.show(backup)?;
        let transferred_back = !dest.starts_with(&backup_subvol.mountpoint);

        // Resolve the (collision-safe) move-aside target. This is a pure query
        // over `exists`; the rename itself happens only on a committing run.
        let moved_aside = if self.fs.exists(dest)? {
            if !force {
                log::warn!(
                    "restore: destination exists, refusing without --force: {}",
                    dest.display()
                );
                return Err(RestoreError::DestinationExists(dest.to_path_buf()));
            }
            Some(self.move_aside_target(dest)?)
        } else {
            None
        };

        if dry_run {
            if let Some(broken) = &moved_aside {
                log::info!(
                    "restore: [dry-run] would move aside {} → {}",
                    dest.display(),
                    broken.display()
                );
            }
            if transferred_back {
                log::info!(
                    "restore: [dry-run] would transfer back {} from another filesystem, then make it writable at {}",
                    backup.display(),
                    dest.display()
                );
            } else {
                log::info!(
                    "restore: [dry-run] would make writable {} → {}",
                    backup.display(),
                    dest.display()
                );
            }
            return Ok(RestoreReport {
                dest: dest.to_path_buf(),
                restored: None,
                moved_aside,
                transferred_back,
                dry_run: true,
            });
        }

        if let Some(broken) = &moved_aside {
            log::info!(
                "restore: moving aside {} → {}",
                dest.display(),
                broken.display()
            );
            self.fs.rename(dest, broken)?;
        }

        // Bring the backup onto the destination filesystem. For a remote backup
        // this means a full send/receive into the destination's directory, then
        // making *that* read-only copy writable (the intermediate is deleted only
        // after the result verifies, so a failure leaves it recoverable).
        let (restored, staging) = if transferred_back {
            let staging_dir = dest.parent().unwrap_or(dest);
            log::info!(
                "restore: backup is on another filesystem; transferring it back into {}",
                staging_dir.display()
            );
            let received = self.transfer.send_receive(
                &backup_subvol,
                &ParentSelection::default(),
                staging_dir,
            )?;
            let received_path = received.mountpoint.join(&received.path);
            let restored = self.snapshots.make_writable(&received_path, dest)?;
            (restored, Some(received_path))
        } else {
            (self.snapshots.make_writable(backup, dest)?, None)
        };

        if restored.readonly || restored.received_uuid.is_some() {
            log::error!(
                "restore: result is not a clean writable subvolume — invariant #7 violated{}",
                moved_aside
                    .as_ref()
                    .map(|p| format!(" (original moved aside to {})", p.display()))
                    .unwrap_or_default()
            );
            return Err(RestoreError::NotCleanWritable { moved_aside });
        }

        // The transfer-back staging copy is redundant now that `dest` is a clean
        // writable subvolume; remove it so the restore leaves only `dest`.
        if let Some(received_path) = staging {
            log::info!(
                "restore: removing transfer-back staging copy {}",
                received_path.display()
            );
            self.deleter
                .delete(&received_path, DeleteCommit::Deferred)?;
        }

        log::info!("restore: complete: {}", dest.display());
        Ok(RestoreReport {
            dest: dest.to_path_buf(),
            restored: Some(restored),
            moved_aside,
            transferred_back,
            dry_run: false,
        })
    }

    /// Pick a non-colliding move-aside path for `dest`: `<dest>.broken`, else
    /// `<dest>.broken.1`, `<dest>.broken.2`, … — the first that does not already
    /// exist. Mirrors the snapshot `_N` collision counter so a prior run's
    /// `.broken` is never clobbered (E2E-P4-04).
    ///
    /// # Errors
    /// [`PortError`] if an existence check fails.
    fn move_aside_target(&self, dest: &Path) -> Result<PathBuf, PortError> {
        let base = broken_path(dest);
        if !self.fs.exists(&base)? {
            return Ok(base);
        }
        // `base` is taken — step through suffixed names until a free one is found.
        let mut counter: u32 = 1;
        loop {
            let candidate = suffixed_broken_path(&base, counter);
            if !self.fs.exists(&candidate)? {
                log::debug!(
                    "restore: move-aside collision on {}, using {}",
                    base.display(),
                    candidate.display()
                );
                return Ok(candidate);
            }
            counter = counter.saturating_add(1);
        }
    }
}

/// The base move-aside path for an existing destination: `<dest>.broken`.
fn broken_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".broken");
    PathBuf::from(name)
}

/// A collision-counter variant of [`broken_path`]: `<dest>.broken.<n>`.
fn suffixed_broken_path(base: &Path, n: u32) -> PathBuf {
    let mut name = base.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use mybtrfs_domain::model::{Subvolume, Uuid};
    use mybtrfs_domain::parent::ParentSelection;

    use crate::ports::{
        DeleteCommit, DeletePort, FilesystemPort, PortError, SnapshotPort, SubvolumeRepository,
        TransferPort,
    };
    use crate::restore::{RestoreError, RestoreService};

    const DEST: &str = "/mnt/pool/home_restored";
    const LOCAL_BACKUP: &str = "/mnt/pool/.snapshots/home.20240102T1531";
    const REMOTE_BACKUP: &str = "/mnt/drive/host/home.20240102T1531";

    /// A `SubvolumeRepository` whose `show` returns one configured backup; its
    /// `mountpoint` is what tells a local restore from a transfer-back.
    struct FakeRepo {
        backup: Subvolume,
    }
    impl SubvolumeRepository for FakeRepo {
        fn show(&self, _path: &Path) -> Result<Subvolume, PortError> {
            Ok(self.backup.clone())
        }
        fn list(&self, _filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
            unimplemented!("restore never lists a filesystem")
        }
    }

    /// A repository whose backup is already on the destination filesystem.
    fn local_repo() -> FakeRepo {
        FakeRepo {
            backup: backup_on("/mnt/pool", ".snapshots/home.20240102T1531"),
        }
    }

    /// A `TransferPort` recording each `send_receive` (the source's absolute path
    /// and the receive directory) and returning a configured received copy; it
    /// asserts a transfer-back is always a full send (no parent).
    struct RecordingTransfer {
        calls: RefCell<Vec<(PathBuf, PathBuf)>>,
        received: Subvolume,
    }
    impl RecordingTransfer {
        fn returning(received: Subvolume) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                received,
            }
        }
        /// A transfer port for same-filesystem tests, where it is never called.
        fn unused() -> Self {
            Self::returning(backup_on("/mnt/pool", "unused"))
        }
        fn calls(&self) -> Vec<(PathBuf, PathBuf)> {
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
            assert!(
                selection.parent.is_none(),
                "a transfer-back must be a full send"
            );
            self.calls.borrow_mut().push((
                source.mountpoint.join(&source.path),
                target_dir.to_path_buf(),
            ));
            Ok(self.received.clone())
        }
    }

    /// A `DeletePort` recording the paths it was asked to delete.
    struct RecordingDeleter {
        calls: RefCell<Vec<PathBuf>>,
    }
    impl RecordingDeleter {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<PathBuf> {
            self.calls.borrow().clone()
        }
    }
    impl DeletePort for RecordingDeleter {
        fn delete(&self, path: &Path, _commit: DeleteCommit) -> Result<(), PortError> {
            self.calls.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    /// A backup subvolume on filesystem `mountpoint`, named `<mountpoint>/<rel>`
    /// (read-only, with a received_uuid — as a real backup carries).
    fn backup_on(mountpoint: &str, rel: &str) -> Subvolume {
        Subvolume {
            id: 300,
            uuid: Uuid::parse("33333333-3333-3333-3333-333333333333"),
            parent_uuid: None,
            received_uuid: Uuid::parse("44444444-4444-4444-4444-444444444444"),
            generation: 20,
            cgen: 20,
            readonly: true,
            path: PathBuf::from(rel),
            fs_uuid: Uuid::parse("88888888-8888-8888-8888-888888888888").expect("valid uuid"),
            mountpoint: PathBuf::from(mountpoint),
        }
    }

    /// The read-only copy a transfer-back receives onto the destination fs.
    fn received_on(mountpoint: &str, rel: &str) -> Subvolume {
        let mut received = backup_on(mountpoint, rel);
        received.id = 400;
        received
    }

    /// A `FilesystemPort` over an explicit set of pre-existing paths that records
    /// renames. `exists` answers from that set, so the move-aside collision logic
    /// can be exercised against several pre-existing `.broken[.N]` names.
    struct FakeFs {
        existing: Vec<PathBuf>,
        renames: RefCell<Vec<(PathBuf, PathBuf)>>,
    }
    impl FakeFs {
        /// Either no path exists (`false`) or only the single dest does (`true`),
        /// matching the original two-state fixture used by the earlier tests.
        fn new(dest_exists: bool) -> Self {
            let existing = if dest_exists {
                vec![PathBuf::from("/mnt/pool/home_restored")]
            } else {
                Vec::new()
            };
            Self {
                existing,
                renames: RefCell::new(Vec::new()),
            }
        }
        /// Treat exactly `paths` as pre-existing.
        fn with_existing(paths: &[&str]) -> Self {
            Self {
                existing: paths.iter().map(PathBuf::from).collect(),
                renames: RefCell::new(Vec::new()),
            }
        }
        fn renames(&self) -> Vec<(PathBuf, PathBuf)> {
            self.renames.borrow().clone()
        }
    }
    impl FilesystemPort for FakeFs {
        fn exists(&self, path: &Path) -> Result<bool, PortError> {
            Ok(self.existing.iter().any(|p| p == path))
        }
        fn create_dir_all(&self, _path: &Path) -> Result<(), PortError> {
            unimplemented!("not exercised by these tests")
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<(), PortError> {
            self.renames
                .borrow_mut()
                .push((from.to_path_buf(), to.to_path_buf()));
            Ok(())
        }
    }

    /// A `SnapshotPort` recording `make_writable` calls and returning a configured
    /// subvolume (writable + no received_uuid by default).
    struct RecordingMakeWritable {
        calls: RefCell<Vec<(PathBuf, PathBuf)>>,
        readonly: bool,
        received_uuid: Option<Uuid>,
    }
    impl RecordingMakeWritable {
        fn clean() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                readonly: false,
                received_uuid: None,
            }
        }
        /// A misbehaving port whose result still carries a received_uuid.
        fn yielding_received() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                readonly: false,
                received_uuid: Some(Uuid::parse("99999999-9999-9999-9999-999999999999").unwrap()),
            }
        }
        fn calls(&self) -> Vec<(PathBuf, PathBuf)> {
            self.calls.borrow().clone()
        }
    }
    impl SnapshotPort for RecordingMakeWritable {
        fn create_readonly(&self, _source: &Path, _dest: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("restore never creates a read-only snapshot")
        }
        fn make_writable(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError> {
            self.calls
                .borrow_mut()
                .push((source.to_path_buf(), dest.to_path_buf()));
            Ok(restored_subvol(
                dest,
                self.readonly,
                self.received_uuid.clone(),
            ))
        }
    }

    fn restored_subvol(path: &Path, readonly: bool, received: Option<Uuid>) -> Subvolume {
        Subvolume {
            id: 500,
            uuid: Uuid::parse("55555555-5555-5555-5555-555555555555"),
            parent_uuid: Uuid::parse("66666666-6666-6666-6666-666666666666"),
            received_uuid: received,
            generation: 30,
            cgen: 30,
            readonly,
            path: path.to_path_buf(),
            fs_uuid: Uuid::parse("77777777-7777-7777-7777-777777777777").expect("valid uuid"),
            mountpoint: PathBuf::from("/mnt/pool"),
        }
    }

    #[test]
    fn restore_to_fresh_dest_makes_a_writable_copy() {
        crate::init_test_logger();
        let fs = FakeFs::new(false); // dest does not exist
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                false,
                false,
            )
            .expect("restore succeeds");

        assert_eq!(
            snapshots.calls(),
            vec![(PathBuf::from(LOCAL_BACKUP), PathBuf::from(DEST))]
        );
        assert!(
            transfer.calls().is_empty(),
            "same-fs restore must not transfer"
        );
        assert!(
            deleter.calls().is_empty(),
            "same-fs restore deletes nothing"
        );
        assert!(!report.transferred_back);
        assert!(fs.renames().is_empty(), "nothing to move aside");
        assert!(report.moved_aside.is_none());
        assert!(!report.dry_run);
        let restored = report.restored.expect("committing run yields a subvolume");
        assert!(!restored.readonly);
        assert!(restored.received_uuid.is_none());
    }

    #[test]
    fn restore_refuses_existing_dest_without_force() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let err = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                false,
                false,
            )
            .expect_err("must refuse to overwrite");

        assert!(matches!(err, RestoreError::DestinationExists(_)));
        assert!(snapshots.calls().is_empty(), "no snapshot attempted");
        assert!(fs.renames().is_empty(), "nothing moved");
    }

    #[test]
    fn restore_force_moves_existing_dest_aside_then_restores() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                true,
                false,
            )
            .expect("restore succeeds with force");

        assert_eq!(
            fs.renames(),
            vec![(
                PathBuf::from("/mnt/pool/home_restored"),
                PathBuf::from("/mnt/pool/home_restored.broken"),
            )]
        );
        assert_eq!(
            report.moved_aside,
            Some(PathBuf::from("/mnt/pool/home_restored.broken"))
        );
        assert_eq!(snapshots.calls().len(), 1);
        assert!(!report.restored.expect("a subvolume was created").readonly);
    }

    #[test]
    fn restore_rejects_a_result_that_carries_a_received_uuid() {
        crate::init_test_logger();
        // Guards invariant #7: a restored subvolume must never be a received one.
        let fs = FakeFs::new(false);
        let snapshots = RecordingMakeWritable::yielding_received();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let err = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                false,
                false,
            )
            .expect_err("must reject a received subvolume");

        assert!(matches!(err, RestoreError::NotCleanWritable { .. }));
    }

    // ---- Bug D: restore --dry-run is a side-effect-free preview (#8 / P4-05) ----

    #[test]
    fn restore_dry_run_to_fresh_dest_mutates_nothing() {
        crate::init_test_logger();
        let fs = FakeFs::new(false); // dest does not exist
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                false,
                true, // dry_run
            )
            .expect("dry-run succeeds");

        assert!(
            snapshots.calls().is_empty(),
            "dry-run must not make_writable"
        );
        assert!(fs.renames().is_empty(), "dry-run must not rename");
        // The plan names the dest; nothing was created and nothing was moved aside
        // because the dest did not exist.
        assert!(report.dry_run);
        assert!(report.restored.is_none(), "dry-run creates no subvolume");
        assert_eq!(report.dest, PathBuf::from("/mnt/pool/home_restored"));
        assert!(report.moved_aside.is_none());
    }

    #[test]
    fn restore_dry_run_with_force_reports_move_aside_plan_without_executing() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                true, // force
                true, // dry_run
            )
            .expect("dry-run with force succeeds");

        assert!(
            snapshots.calls().is_empty(),
            "dry-run must not make_writable"
        );
        assert!(fs.renames().is_empty(), "dry-run must not rename");
        assert!(report.dry_run);
        // The plan names the move-aside target it *would* use.
        assert_eq!(
            report.moved_aside,
            Some(PathBuf::from("/mnt/pool/home_restored.broken"))
        );
    }

    #[test]
    fn restore_dry_run_still_refuses_existing_dest_without_force() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let err = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                false, // no force
                true,  // dry_run
            )
            .expect_err("dry-run must still report the refusal");

        assert!(matches!(err, RestoreError::DestinationExists(_)));
        assert!(snapshots.calls().is_empty());
        assert!(fs.renames().is_empty());
    }

    // ---- Bug E: move-aside picks a non-colliding `.broken[.N]` name (P4-04) ----

    #[test]
    fn restore_force_does_not_clobber_an_existing_broken_dir() {
        crate::init_test_logger();
        // `dest` exists AND a prior `<dest>.broken` already exists; the move-aside
        // must step to `<dest>.broken.1`, leaving the existing `.broken` untouched.
        let fs =
            FakeFs::with_existing(&["/mnt/pool/home_restored", "/mnt/pool/home_restored.broken"]);
        let snapshots = RecordingMakeWritable::clean();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                true,
                false,
            )
            .expect("restore succeeds with force");

        assert_eq!(
            fs.renames(),
            vec![(
                PathBuf::from("/mnt/pool/home_restored"),
                PathBuf::from("/mnt/pool/home_restored.broken.1"),
            )],
            "must not overwrite the existing .broken"
        );
        assert_eq!(
            report.moved_aside,
            Some(PathBuf::from("/mnt/pool/home_restored.broken.1"))
        );
    }

    // ---- Bug G: NotCleanWritable surfaces where the displaced original went ----

    #[test]
    fn not_clean_writable_carries_the_move_aside_path_when_force_displaced_dest() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists → force moves it aside
        let snapshots = RecordingMakeWritable::yielding_received();
        let repo = local_repo();
        let transfer = RecordingTransfer::unused();
        let deleter = RecordingDeleter::new();
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let err = service
            .restore(
                Path::new(LOCAL_BACKUP),
                Path::new("/mnt/pool/home_restored"),
                true, // force → move aside happens before the bad make_writable
                false,
            )
            .expect_err("must reject a received subvolume");

        match err {
            RestoreError::NotCleanWritable { moved_aside } => assert_eq!(
                moved_aside,
                Some(PathBuf::from("/mnt/pool/home_restored.broken")),
                "the error must tell the user where their original data is"
            ),
            other => panic!("expected NotCleanWritable, got {other:?}"),
        }
    }

    // ---- Item 7: transfer-back — restore a backup that lives only on a remote
    // filesystem (send/receive onto the dest fs, make writable, clean up). ----

    #[test]
    fn restore_from_remote_target_transfers_back_then_makes_writable() {
        crate::init_test_logger();
        // The backup lives only on `/mnt/drive`; `dest` is on `/mnt/pool`.
        let repo = FakeRepo {
            backup: backup_on("/mnt/drive", "host/home.20240102T1531"),
        };
        let snapshots = RecordingMakeWritable::clean();
        let transfer = RecordingTransfer::returning(received_on("/mnt/pool", "home.20240102T1531"));
        let deleter = RecordingDeleter::new();
        let fs = FakeFs::new(false);
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(Path::new(REMOTE_BACKUP), Path::new(DEST), false, false)
            .expect("remote restore succeeds");

        // 1) a full send/receive of the backup into the destination's directory,
        assert_eq!(
            transfer.calls(),
            vec![(PathBuf::from(REMOTE_BACKUP), PathBuf::from("/mnt/pool"))]
        );
        // 2) then the received copy is made writable at dest,
        assert_eq!(
            snapshots.calls(),
            vec![(
                PathBuf::from("/mnt/pool/home.20240102T1531"),
                PathBuf::from(DEST)
            )]
        );
        // 3) then the intermediate received copy is removed.
        assert_eq!(
            deleter.calls(),
            vec![PathBuf::from("/mnt/pool/home.20240102T1531")]
        );
        assert!(report.transferred_back);
        let restored = report.restored.expect("a writable subvolume");
        assert!(!restored.readonly);
        assert!(restored.received_uuid.is_none());
    }

    #[test]
    fn restore_from_remote_dry_run_plans_the_transfer_without_executing() {
        crate::init_test_logger();
        let repo = FakeRepo {
            backup: backup_on("/mnt/drive", "host/home.20240102T1531"),
        };
        let snapshots = RecordingMakeWritable::clean();
        let transfer = RecordingTransfer::returning(received_on("/mnt/pool", "home.20240102T1531"));
        let deleter = RecordingDeleter::new();
        let fs = FakeFs::new(false);
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let report = service
            .restore(Path::new(REMOTE_BACKUP), Path::new(DEST), false, true)
            .expect("dry-run succeeds");

        assert!(transfer.calls().is_empty(), "dry-run must not transfer");
        assert!(
            snapshots.calls().is_empty(),
            "dry-run must not make_writable"
        );
        assert!(deleter.calls().is_empty(), "dry-run must not delete");
        assert!(report.dry_run);
        assert!(
            report.transferred_back,
            "the plan records that it would transfer back"
        );
        assert!(report.restored.is_none());
    }

    #[test]
    fn remote_restore_keeps_the_staging_copy_when_the_result_is_not_clean() {
        crate::init_test_logger();
        // make_writable misbehaves (yields a received_uuid). The transfer-back
        // staging copy must be kept — not deleted — so the data stays recoverable
        // after the failed verification (delete happens only after a clean result).
        let repo = FakeRepo {
            backup: backup_on("/mnt/drive", "host/home.20240102T1531"),
        };
        let snapshots = RecordingMakeWritable::yielding_received();
        let transfer = RecordingTransfer::returning(received_on("/mnt/pool", "home.20240102T1531"));
        let deleter = RecordingDeleter::new();
        let fs = FakeFs::new(false);
        let service = RestoreService::new(&repo, &snapshots, &transfer, &deleter, &fs);

        let err = service
            .restore(Path::new(REMOTE_BACKUP), Path::new(DEST), false, false)
            .expect_err("must reject a received subvolume");

        assert!(matches!(err, RestoreError::NotCleanWritable { .. }));
        assert_eq!(transfer.calls().len(), 1, "the transfer-back happened");
        assert!(
            deleter.calls().is_empty(),
            "the staging copy must be kept for recovery on a failed restore"
        );
    }
}
