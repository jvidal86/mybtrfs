//! `BackupService` — powers `run` (snapshot → send/receive → prune),
//! `snapshot`, and `resume` (send without a new snapshot). Resolves the
//! incremental parent via the domain, transfers via `TransferPort` (which
//! verifies), and delegates deletion to `RetentionService`.
//! See `documentation/01-phases-design-v2.md` Phases 1–2.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use mybtrfs_domain::model::{RelationshipGraph, Subvolume};
use mybtrfs_domain::naming::{TimestampFormat, make_name, next_free_name, parse_name};
use mybtrfs_domain::parent::{Incremental, ParentSelection, best_parent, target_correlates};
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

/// The outcome of a `resume`: the backup transferred this run (`None` if the
/// latest snapshot was already backed up — nothing to resume), plus the
/// preserve/delete partitions from pruning each set.
#[derive(Debug)]
pub struct ResumeReport {
    /// The verified backup transferred this run, or `None` if nothing was due.
    pub transferred: Option<Subvolume>,
    /// Snapshot-side retention result (over the source `snapshot_dir`).
    pub snapshots_pruned: Schedule<Subvolume>,
    /// Backup-side retention result (over the `target_dir`).
    pub backups_pruned: Schedule<Subvolume>,
}

/// The outcome of a standalone `prune` (no snapshot, no transfer): the
/// preserve/delete partitions for the snapshot set and the backup set.
#[derive(Debug)]
pub struct PruneReport {
    /// Snapshot-side retention result (over `snapshot_dir`).
    pub snapshots_pruned: Schedule<Subvolume>,
    /// Backup-side retention result (over `target_dir`).
    pub backups_pruned: Schedule<Subvolume>,
}

/// Orchestrates the backup operations over the driven ports: `snapshot`, `run`
/// (snapshot → send/receive → prune), and `resume`, delegating retention to
/// [`RetentionService`]. `run`/`resume` send incrementally (`send -p`, plus any
/// `-c` clone sources) when a correlated parent exists, per the [`Incremental`]
/// mode (`Yes` by default).
pub struct BackupService<'a> {
    clock: &'a dyn ClockPort,
    /// Repository scoped to the **source** filesystem (where snapshots live).
    source_repo: &'a dyn SubvolumeRepository,
    /// Repository scoped to the **target** filesystem (where backups live). A
    /// btrfs repository stamps each subvolume with its filesystem's
    /// uuid/mountpoint, so source and target need distinct, correctly-scoped
    /// repositories — one repo can't correctly describe subvolumes on both.
    target_repo: &'a dyn SubvolumeRepository,
    snapshots: &'a dyn SnapshotPort,
    transfer: &'a dyn TransferPort,
    retention: &'a RetentionService<'a>,
    format: TimestampFormat,
    /// Incremental-send strategy — `Yes` (use a parent when one exists, else
    /// full), `Strict` (require a parent), or `No` (always full).
    incremental: Incremental,
}

impl<'a> BackupService<'a> {
    /// Construct a service with the default incremental mode ([`Incremental::Yes`]):
    /// over the injected clock, the source/target subvolume repositories, the
    /// snapshot and transfer ports, the retention service, and the timestamp
    /// format used for snapshot names.
    #[must_use]
    pub fn new(
        clock: &'a dyn ClockPort,
        source_repo: &'a dyn SubvolumeRepository,
        target_repo: &'a dyn SubvolumeRepository,
        snapshots: &'a dyn SnapshotPort,
        transfer: &'a dyn TransferPort,
        retention: &'a RetentionService<'a>,
        format: TimestampFormat,
    ) -> Self {
        Self::with_incremental(
            clock,
            source_repo,
            target_repo,
            snapshots,
            transfer,
            retention,
            format,
            Incremental::Yes,
        )
    }

    /// Like [`new`](Self::new) but with an explicit [`Incremental`] mode
    /// (`Strict` to require a parent, `No` to force full sends).
    #[must_use]
    #[allow(clippy::too_many_arguments)] // composition root; an args-struct would break `new`
    pub fn with_incremental(
        clock: &'a dyn ClockPort,
        source_repo: &'a dyn SubvolumeRepository,
        target_repo: &'a dyn SubvolumeRepository,
        snapshots: &'a dyn SnapshotPort,
        transfer: &'a dyn TransferPort,
        retention: &'a RetentionService<'a>,
        format: TimestampFormat,
        incremental: Incremental,
    ) -> Self {
        Self {
            clock,
            source_repo,
            target_repo,
            snapshots,
            transfer,
            retention,
            format,
            incremental,
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

        let existing = self.source_repo.list(snapshot_dir)?;
        let leaves: Vec<&str> = existing
            .iter()
            .filter(|sv| is_in_dir(snapshot_dir, sv))
            .filter_map(|sv| sv.path.file_name().and_then(OsStr::to_str))
            .collect();

        let free_name = next_free_name(&base, &leaves);
        if free_name != base {
            log::debug!("name collision for {base}, using {free_name}");
        }
        let dest = snapshot_dir.join(&free_name);
        log::info!("snapshot: {} → {}", source.display(), dest.display());
        self.snapshots.create_readonly(source, &dest)
    }

    /// Full backup cycle: create a read-only snapshot of `source`, send/receive
    /// it into `target_dir` (incrementally when a correlated parent exists, else
    /// full), then prune backups (`target_policy`) and snapshots (`snapshot_policy`).
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
        log::info!("run: {} → {}", source.display(), target_dir.display());
        let snapshot = self.snapshot(source, snapshot_dir, basename)?;
        log::info!(
            "run: snapshot created: {}",
            snapshot.mountpoint.join(&snapshot.path).display()
        );

        // Source snapshots (incl. the new one) and the backups already on target —
        // each read from its own filesystem's repository — drive parent resolution.
        let snapshots = self.collect_in(self.source_repo, snapshot_dir, &snapshot)?;
        let mut backups = existing_in(self.target_repo, target_dir)?;

        let selection = self.choose_parent(&snapshot, &snapshots, &backups)?;
        let backup = self
            .transfer
            .send_receive(&snapshot, &selection, target_dir)?;
        log::info!(
            "run: backup received: {}",
            backup.mountpoint.join(&backup.path).display()
        );
        if !backups.iter().any(|b| b.id == backup.id) {
            backups.push(backup.clone());
        }

        // Force-preserve the just-created snapshot and backup; `prune_both` adds
        // the latest common pair.
        let (snapshots_pruned, backups_pruned) = self.prune_both(
            &snapshots,
            &backups,
            snapshot_policy,
            target_policy,
            HashSet::from([snapshot.id]),
            HashSet::from([backup.id]),
        )?;

        Ok(RunReport {
            snapshot,
            backup,
            snapshots_pruned,
            backups_pruned,
        })
    }

    /// Resume cycle: send/receive an **existing** snapshot (no new snapshot), then
    /// prune. Sends the latest snapshot named for `basename` in `snapshot_dir`
    /// that does **not** yet have a correlated backup on the target (so an already
    /// backed-up snapshot is never re-sent), incrementally when a correlated
    /// parent exists; if every snapshot is already backed up, nothing is
    /// transferred. Returns a [`ResumeReport`].
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the repositories, transfer, or delete
    /// ports; or [`PortError::Verification`] on a duplicate-uuid backup set.
    pub fn resume(
        &self,
        snapshot_dir: &Path,
        basename: &str,
        target_dir: &Path,
        snapshot_policy: &RetentionPolicy,
        target_policy: &RetentionPolicy,
    ) -> Result<ResumeReport, PortError> {
        let snapshots = existing_in(self.source_repo, snapshot_dir)?;
        let mut backups = existing_in(self.target_repo, target_dir)?;

        // The latest snapshot for `basename` lacking a correlated backup on target.
        let graph = RelationshipGraph::build(backups.clone())
            .map_err(|err| PortError::Verification(err.to_string()))?;
        let due = snapshots
            .iter()
            .filter(|sv| name_basename(sv).as_deref() == Some(basename))
            .filter(|sv| target_correlates(sv, &graph).is_empty())
            .max_by(|a, b| {
                a.reference_generation()
                    .cmp(&b.reference_generation())
                    .then(a.id.cmp(&b.id))
            })
            .cloned();

        let mut backup_preserve = HashSet::new();
        let transferred = match &due {
            Some(snapshot) => {
                log::info!(
                    "resume: sending {} → {}",
                    snapshot.mountpoint.join(&snapshot.path).display(),
                    target_dir.display()
                );
                let selection = self.choose_parent(snapshot, &snapshots, &backups)?;
                let backup = self
                    .transfer
                    .send_receive(snapshot, &selection, target_dir)?;
                backup_preserve.insert(backup.id);
                if !backups.iter().any(|b| b.id == backup.id) {
                    backups.push(backup.clone());
                }
                Some(backup)
            }
            None => {
                log::info!("resume: already up-to-date, nothing to transfer");
                None
            }
        };

        let (snapshots_pruned, backups_pruned) = self.prune_both(
            &snapshots,
            &backups,
            snapshot_policy,
            target_policy,
            HashSet::new(),
            backup_preserve,
        )?;

        Ok(ResumeReport {
            transferred,
            snapshots_pruned,
            backups_pruned,
        })
    }

    /// Standalone prune: apply retention to the snapshots in `snapshot_dir`
    /// (`snapshot_policy`) and the backups in `target_dir` (`target_policy`)
    /// without creating a snapshot or transferring anything. Nothing is
    /// just-created, but `prune_both` still force-preserves the latest common
    /// snapshot/backup pair (invariant #1) so a later incremental keeps a parent
    /// on both ends. Backups are pruned before snapshots; a delete error aborts
    /// fail-fast (decision ID-1). Returns a [`PruneReport`].
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the repositories or the delete port; or
    /// [`PortError::Verification`] if the target backups carry a duplicate uuid
    /// (the cloned-disk guard, invariant #10).
    pub fn prune(
        &self,
        snapshot_dir: &Path,
        target_dir: &Path,
        snapshot_policy: &RetentionPolicy,
        target_policy: &RetentionPolicy,
    ) -> Result<PruneReport, PortError> {
        let snapshots = existing_in(self.source_repo, snapshot_dir)?;
        let backups = existing_in(self.target_repo, target_dir)?;
        let (snapshots_pruned, backups_pruned) = self.prune_both(
            &snapshots,
            &backups,
            snapshot_policy,
            target_policy,
            HashSet::new(),
            HashSet::new(),
        )?;
        Ok(PruneReport {
            snapshots_pruned,
            backups_pruned,
        })
    }

    /// Prune snapshots (`snapshot_policy`) and backups (`target_policy`),
    /// force-preserving the given ids plus the latest common snapshot/backup pair
    /// (invariants #3/#4). The target was just reached, so snapshot deletion is
    /// not skipped (#5). Backups are pruned before snapshots (parallel to btrbk);
    /// a delete error aborts fail-fast (decision ID-1).
    fn prune_both(
        &self,
        snapshots: &[Subvolume],
        backups: &[Subvolume],
        snapshot_policy: &RetentionPolicy,
        target_policy: &RetentionPolicy,
        mut snapshot_preserve: HashSet<u64>,
        mut backup_preserve: HashSet<u64>,
    ) -> Result<(Schedule<Subvolume>, Schedule<Subvolume>), PortError> {
        let target_graph = RelationshipGraph::build(backups.to_vec()).map_err(|err| {
            log::error!("duplicate uuid detected in backups: {err}");
            PortError::Verification(err.to_string())
        })?;
        if let Some(pair) = latest_common_pair(snapshots, &target_graph) {
            snapshot_preserve.insert(pair.snapshot.id);
            backup_preserve.extend(pair.backups.iter().map(|b| b.id));
        }

        let backups_pruned = self.retention.prune(
            backups,
            target_policy,
            &SafetyContext {
                force_preserve_ids: backup_preserve,
                target_aborted: false,
            },
            DeleteCommit::Deferred,
        )?;
        let snapshots_pruned = self.retention.prune(
            snapshots,
            snapshot_policy,
            &SafetyContext {
                force_preserve_ids: snapshot_preserve,
                target_aborted: false,
            },
            DeleteCommit::Deferred,
        )?;
        Ok((snapshots_pruned, backups_pruned))
    }

    /// Resolve the incremental parent (and any `-c` clone sources) for `snapshot`:
    /// the newest source snapshot with a correlated backup on the target, honoring
    /// the configured [`Incremental`] mode. Returns a full-send selection when
    /// nothing qualifies (or the mode forbids a parent).
    ///
    /// # Errors
    /// [`PortError::Verification`] if either set carries a duplicate uuid.
    fn choose_parent(
        &self,
        snapshot: &Subvolume,
        source_snapshots: &[Subvolume],
        target_backups: &[Subvolume],
    ) -> Result<ParentSelection, PortError> {
        let source_graph = RelationshipGraph::build(source_snapshots.to_vec())
            .map_err(|err| PortError::Verification(err.to_string()))?;
        let target_graph = RelationshipGraph::build(target_backups.to_vec())
            .map_err(|err| PortError::Verification(err.to_string()))?;
        Ok(best_parent(
            snapshot,
            &source_graph,
            &target_graph,
            self.incremental,
        ))
    }

    /// List the subvolumes directly in `dir` (via `repo`, scoped to that
    /// directory's filesystem), ensuring `just_created` is included (a stateless
    /// repository may not observe it until the next run).
    fn collect_in(
        &self,
        repo: &dyn SubvolumeRepository,
        dir: &Path,
        just_created: &Subvolume,
    ) -> Result<Vec<Subvolume>, PortError> {
        let mut subvols = existing_in(repo, dir)?;
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

/// List the subvolumes directly in `dir` via `repo` (scoped to that directory's
/// filesystem), filtered to those whose parent directory is exactly `dir`.
fn existing_in(repo: &dyn SubvolumeRepository, dir: &Path) -> Result<Vec<Subvolume>, PortError> {
    Ok(repo
        .list(dir)?
        .into_iter()
        .filter(|sv| is_in_dir(dir, sv))
        .collect())
}

/// The mybtrfs basename parsed from `sv`'s leaf name, or `None` if the name
/// doesn't match the scheme (a foreign subvolume).
fn name_basename(sv: &Subvolume) -> Option<String> {
    sv.path
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(parse_name)
        .map(|parsed| parsed.basename)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, FixedOffset};

    use mybtrfs_domain::model::{Subvolume, Uuid};
    use mybtrfs_domain::naming::TimestampFormat;
    use mybtrfs_domain::parent::{Incremental, ParentSelection};
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
        Subvolume {
            id: 256,
            uuid: Uuid::parse("11111111-1111-1111-1111-111111111111"),
            parent_uuid: None,
            received_uuid: None,
            generation: 10,
            cgen: 10,
            readonly: true,
            path: path.to_path_buf(),
            // Same source filesystem as the existing snapshots, so an incremental
            // parent is reachable.
            fs_uuid: uuid_hex(0),
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
        ro_cgen(id, uuid, received, mount, rel, 5)
    }

    /// Like [`ro`] but with an explicit creation generation (for recency ordering).
    fn ro_cgen(
        id: u64,
        uuid: Uuid,
        received: Option<Uuid>,
        mount: &str,
        rel: &str,
        cgen: u64,
    ) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid),
            parent_uuid: None,
            received_uuid: received,
            generation: cgen,
            cgen,
            readonly: true,
            path: PathBuf::from(rel),
            fs_uuid: uuid_hex(0),
            mountpoint: PathBuf::from(mount),
        }
    }

    #[test]
    fn snapshot_creates_readonly_snapshot_named_with_timestamp() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let source_repo = FakeRepo::default();
        let target_repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
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
        let source_repo = FakeRepo {
            subvols: vec![existing_snapshot(".mybtrfs_snapshots/home.20240102T1531")],
        };
        let target_repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
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
        let source_repo = FakeRepo::default();
        let target_repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
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
        // An old snapshot (source fs) and an old backup (target fs), each
        // uncorrelated (orphans) — read from their respective repositories.
        let source_repo = FakeRepo {
            subvols: vec![ro(
                10,
                uuid_hex(10),
                None,
                "/mnt/pool",
                ".mybtrfs_snapshots/home.20240101T1200",
            )],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro(
                20,
                uuid_hex(20),
                Some(uuid_hex(0xff)),
                "/mnt/drive",
                "host/home.20240101T1200",
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
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

    #[test]
    fn resume_sends_latest_unbacked_snapshot() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let source_repo = FakeRepo {
            subvols: vec![
                ro_cgen(
                    1,
                    uuid_hex(1),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                    5,
                ),
                ro_cgen(
                    2,
                    uuid_hex(2),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240102T1200",
                    10,
                ),
            ],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro_cgen(
                11,
                uuid_hex(11),
                Some(uuid_hex(1)), // backup of snapshot 1 only
                "/mnt/drive",
                "host/home.20240101T1200",
                6,
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        let report = service
            .resume(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(),
                &RetentionPolicy::default(),
            )
            .expect("resume succeeds");

        // No new snapshot is created; the newer un-backed snapshot (id 2) is sent.
        assert!(
            snapshots.readonly_calls().is_empty(),
            "resume creates no snapshot"
        );
        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        let (sent, selection, target) = &calls[0];
        assert_eq!(sent.id, 2);
        // S2 is sent incrementally, with its correlated predecessor S1 as parent.
        assert_eq!(selection.parent.as_ref().map(|p| p.id), Some(1));
        assert_eq!(target, &PathBuf::from("/mnt/drive/host"));
        let transferred = report.transferred.expect("a backup was transferred");
        assert_eq!(transferred.received_uuid, Some(uuid_hex(2)));
        assert!(deleter.paths().is_empty());
    }

    #[test]
    fn resume_is_noop_when_latest_snapshot_already_backed_up() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let source_repo = FakeRepo {
            subvols: vec![ro_cgen(
                1,
                uuid_hex(1),
                None,
                "/mnt/pool",
                ".mybtrfs_snapshots/home.20240101T1200",
                5,
            )],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro_cgen(
                11,
                uuid_hex(11),
                Some(uuid_hex(1)),
                "/mnt/drive",
                "host/home.20240101T1200",
                6,
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        let report = service
            .resume(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(),
                &RetentionPolicy::default(),
            )
            .expect("resume succeeds");

        assert!(transfer.calls().is_empty(), "nothing to resume");
        assert!(report.transferred.is_none());
        assert!(deleter.paths().is_empty());
    }

    #[test]
    fn resume_only_considers_the_requested_basename() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let source_repo = FakeRepo {
            subvols: vec![
                ro_cgen(
                    1,
                    uuid_hex(1),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                    5,
                ),
                // A newer snapshot of a DIFFERENT subvolume — must be ignored.
                ro_cgen(
                    2,
                    uuid_hex(2),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/rootfs.20240102T1200",
                    10,
                ),
            ],
        };
        let target_repo = FakeRepo::default();
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        service
            .resume(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(),
                &RetentionPolicy::default(),
            )
            .expect("resume succeeds");

        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.id, 1, "sent the home snapshot, not rootfs");
    }

    #[test]
    fn run_uses_correlated_prior_snapshot_as_incremental_parent() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        // A prior snapshot (1) with its backup (11) on the target → 1 is a valid parent.
        let source_repo = FakeRepo {
            subvols: vec![ro(
                1,
                uuid_hex(1),
                None,
                "/mnt/pool",
                ".mybtrfs_snapshots/home.20240101T1200",
            )],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro(
                11,
                uuid_hex(11),
                Some(uuid_hex(1)),
                "/mnt/drive",
                "host/home.20240101T1200",
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        service
            .run(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(),
                &RetentionPolicy::default(),
            )
            .expect("run succeeds");

        // The new snapshot (256) is sent with the prior snapshot (1) as -p parent.
        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        let (sent, selection, _) = &calls[0];
        assert_eq!(sent.id, 256);
        assert_eq!(selection.parent.as_ref().map(|p| p.id), Some(1));
    }

    #[test]
    fn with_incremental_no_forces_a_full_send_even_when_a_parent_exists() {
        let clock = FixedClock::at("2024-01-02T15:31:00+00:00");
        let source_repo = FakeRepo {
            subvols: vec![ro(
                1,
                uuid_hex(1),
                None,
                "/mnt/pool",
                ".mybtrfs_snapshots/home.20240101T1200",
            )],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro(
                11,
                uuid_hex(11),
                Some(uuid_hex(1)),
                "/mnt/drive",
                "host/home.20240101T1200",
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::with_incremental(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
            Incremental::No,
        );

        service
            .run(
                Path::new("/mnt/pool/home"),
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                "home",
                Path::new("/mnt/drive/host"),
                &RetentionPolicy::default(),
                &RetentionPolicy::default(),
            )
            .expect("run succeeds");

        // Incremental::No → a full send despite an available parent.
        let calls = transfer.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.parent.is_none());
    }

    #[test]
    fn prune_deletes_by_policy_without_creating_or_transferring() {
        let clock = FixedClock::at("2024-06-01T00:00:00+00:00");
        // One orphan snapshot and one orphan backup (uncorrelated), valid names.
        let source_repo = FakeRepo {
            subvols: vec![ro(
                5,
                uuid_hex(5),
                None,
                "/mnt/pool",
                ".mybtrfs_snapshots/home.20240101T1000",
            )],
        };
        let target_repo = FakeRepo {
            subvols: vec![ro(
                22,
                uuid_hex(22),
                Some(uuid_hex(0xff)),
                "/mnt/drive",
                "host/home.20240101T1000",
            )],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
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
            .prune(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
                &aggressive,
                &aggressive,
            )
            .expect("prune succeeds");

        // A standalone prune neither snapshots nor transfers.
        assert!(
            snapshots.readonly_calls().is_empty(),
            "prune creates nothing"
        );
        assert!(transfer.calls().is_empty(), "prune transfers nothing");
        // Both uncorrelated orphans are deleted under the aggressive policy.
        let snap_delete: Vec<u64> = report
            .snapshots_pruned
            .delete
            .iter()
            .map(|s| s.id)
            .collect();
        let backup_delete: Vec<u64> = report.backups_pruned.delete.iter().map(|s| s.id).collect();
        assert_eq!(snap_delete, vec![5]);
        assert_eq!(backup_delete, vec![22]);
    }

    #[test]
    fn prune_force_preserves_the_latest_common_pair() {
        let clock = FixedClock::at("2024-06-01T00:00:00+00:00");
        // A correlated pair (S1 <-> B11) plus an uncorrelated orphan on each side.
        let source_repo = FakeRepo {
            subvols: vec![
                ro(
                    1,
                    uuid_hex(1),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240102T1200",
                ),
                ro(
                    5,
                    uuid_hex(5),
                    None,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1000",
                ),
            ],
        };
        let target_repo = FakeRepo {
            subvols: vec![
                ro(
                    11,
                    uuid_hex(11),
                    Some(uuid_hex(1)), // correlated to S1
                    "/mnt/drive",
                    "host/home.20240102T1200",
                ),
                ro(
                    22,
                    uuid_hex(22),
                    Some(uuid_hex(0xff)), // orphan
                    "/mnt/drive",
                    "host/home.20240101T1000",
                ),
            ],
        };
        let snapshots = RecordingSnapshot::default();
        let transfer = RecordingTransfer::default();
        let deleter = RecordingDeleter::default();
        let retention = RetentionService::new(&clock, &deleter);
        let service = BackupService::new(
            &clock,
            &source_repo,
            &target_repo,
            &snapshots,
            &transfer,
            &retention,
            TimestampFormat::Long,
        );

        // PreserveMin::None with no tiers: the raw schedule would delete everything.
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

        // The latest common pair (S1 / B11) is force-preserved (invariant #1), so
        // the next incremental still has a parent on both ends — even though the
        // policy alone would have deleted it.
        assert!(report.snapshots_pruned.preserve.iter().any(|s| s.id == 1));
        assert!(report.backups_pruned.preserve.iter().any(|b| b.id == 11));
        assert!(!report.snapshots_pruned.delete.iter().any(|s| s.id == 1));
        assert!(!report.backups_pruned.delete.iter().any(|b| b.id == 11));
        // The uncorrelated orphans are still pruned.
        assert!(report.snapshots_pruned.delete.iter().any(|s| s.id == 5));
        assert!(report.backups_pruned.delete.iter().any(|b| b.id == 22));
    }
}
