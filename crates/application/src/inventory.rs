//! `InventoryService` — read-only `list` and `stats` over the source snapshots
//! and target backups, correlating them via the domain. Depends only on
//! `SubvolumeRepository`. (`list-drives` is served by the `DriveDiscovery`
//! adapter at the composition root.) See `documentation/01-phases-design-v2.md`
//! Phase 3.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;

use mybtrfs_domain::model::{RelationshipGraph, Subvolume};
use mybtrfs_domain::naming::parse_name;
use mybtrfs_domain::parent::target_correlates;

use crate::ports::{PortError, SubvolumeRepository};

/// A source snapshot together with its correlated backups on the target (empty
/// when the snapshot is not yet backed up).
#[derive(Debug)]
pub struct BackupStatus {
    /// The source-side snapshot.
    pub snapshot: Subvolume,
    /// Correlated backups of `snapshot` on the target (may be empty).
    pub backups: Vec<Subvolume>,
}

/// A read-only inventory: each snapshot with its backups, plus the target
/// backups that correlate to no snapshot (orphans) or are incomplete (garbled).
#[derive(Debug)]
pub struct Inventory {
    /// Each source snapshot paired with its correlated backups.
    pub snapshots: Vec<BackupStatus>,
    /// Backups correlating to no source snapshot.
    pub orphan_backups: Vec<Subvolume>,
    /// Backups left incomplete by an interrupted receive (writable, no
    /// `received_uuid`).
    pub incomplete_backups: Vec<Subvolume>,
}

/// Aggregate counts (parallels btrbk `stats`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Number of source snapshots.
    pub snapshots: usize,
    /// Number of target backups (correlated + orphaned + incomplete).
    pub backups: usize,
    /// Backups correlated to a source snapshot.
    pub correlated: usize,
    /// Backups correlating to no source snapshot.
    pub orphaned: usize,
    /// Incomplete (garbled) backups.
    pub incomplete: usize,
}

/// Read-only inventory queries over the source/target repositories.
pub struct InventoryService<'a> {
    source_repo: &'a dyn SubvolumeRepository,
    target_repo: &'a dyn SubvolumeRepository,
}

impl<'a> InventoryService<'a> {
    /// Construct a service over the source and target subvolume repositories.
    #[must_use]
    pub fn new(
        source_repo: &'a dyn SubvolumeRepository,
        target_repo: &'a dyn SubvolumeRepository,
    ) -> Self {
        Self {
            source_repo,
            target_repo,
        }
    }

    /// Inventory the snapshots in `snapshot_dir` and backups in `target_dir`,
    /// correlating each snapshot with its backups and partitioning the remaining
    /// backups into orphaned vs incomplete. Foreign-named subvolumes are ignored.
    ///
    /// # Errors
    /// [`PortError`] from either repository; [`PortError::Verification`] if the
    /// target backups carry a duplicate uuid (cloned-disk guard, invariant #10).
    pub fn list(&self, snapshot_dir: &Path, target_dir: &Path) -> Result<Inventory, PortError> {
        let snapshots = managed_in(self.source_repo, snapshot_dir)?;
        let backups = managed_in(self.target_repo, target_dir)?;
        let graph = RelationshipGraph::build(backups.clone()).map_err(|err| {
            log::error!("duplicate uuid detected: {err} — cloned disk guard triggered");
            PortError::Verification(err.to_string())
        })?;

        let mut correlated_ids: HashSet<u64> = HashSet::new();
        let snapshot_statuses: Vec<BackupStatus> = snapshots
            .into_iter()
            .map(|snapshot| {
                let backups: Vec<Subvolume> = target_correlates(&snapshot, &graph)
                    .into_iter()
                    .cloned()
                    .collect();
                for backup in &backups {
                    correlated_ids.insert(backup.id);
                }
                BackupStatus { snapshot, backups }
            })
            .collect();

        let incomplete_backups: Vec<Subvolume> =
            backups.iter().filter(|b| b.is_garbled()).cloned().collect();
        let incomplete_ids: HashSet<u64> = incomplete_backups.iter().map(|b| b.id).collect();
        let orphan_backups: Vec<Subvolume> = backups
            .into_iter()
            .filter(|b| !correlated_ids.contains(&b.id) && !incomplete_ids.contains(&b.id))
            .collect();

        Ok(Inventory {
            snapshots: snapshot_statuses,
            orphan_backups,
            incomplete_backups,
        })
    }

    /// Aggregate counts over the same inventory.
    ///
    /// # Errors
    /// As [`InventoryService::list`].
    pub fn stats(&self, snapshot_dir: &Path, target_dir: &Path) -> Result<Stats, PortError> {
        let inventory = self.list(snapshot_dir, target_dir)?;
        let correlated: usize = inventory
            .snapshots
            .iter()
            .flat_map(|status| status.backups.iter().map(|backup| backup.id))
            .collect::<HashSet<u64>>()
            .len();
        let orphaned = inventory.orphan_backups.len();
        let incomplete = inventory.incomplete_backups.len();
        Ok(Stats {
            snapshots: inventory.snapshots.len(),
            backups: correlated + orphaned + incomplete,
            correlated,
            orphaned,
            incomplete,
        })
    }
}

/// The mybtrfs-named subvolumes directly in `dir` (via `repo`): filtered to those
/// whose parent directory is `dir` and whose leaf name matches the scheme.
fn managed_in(repo: &dyn SubvolumeRepository, dir: &Path) -> Result<Vec<Subvolume>, PortError> {
    let all = repo.list(dir)?;
    let total = all.len();
    let managed: Vec<Subvolume> = all
        .into_iter()
        .filter(|sv| sv.mountpoint.join(&sv.path).parent() == Some(dir))
        .filter(|sv| {
            sv.path
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(parse_name)
                .is_some()
        })
        .collect();
    let filtered = total.saturating_sub(managed.len());
    if filtered > 0 {
        log::trace!(
            "managed_in({}): filtered {filtered} foreign-named subvolume(s)",
            dir.display()
        );
    }
    Ok(managed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::{Path, PathBuf};

    use mybtrfs_domain::model::{Subvolume, Uuid};

    use crate::inventory::{InventoryService, Stats};
    use crate::ports::{PortError, SubvolumeRepository};

    /// A `SubvolumeRepository` returning a fixed set (callers filter by directory).
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

    fn uuid_hex(tag: u64) -> Uuid {
        Uuid::parse(&format!("{tag:08x}-0000-0000-0000-000000000000")).expect("valid uuid")
    }

    fn sub(
        id: u64,
        uuid: u64,
        received: Option<u64>,
        readonly: bool,
        mount: &str,
        rel: &str,
    ) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid_hex(uuid)),
            parent_uuid: None,
            received_uuid: received.map(uuid_hex),
            generation: 10,
            cgen: 10,
            readonly,
            path: PathBuf::from(rel),
            fs_uuid: uuid_hex(0),
            mountpoint: PathBuf::from(mount),
        }
    }

    /// Two snapshots; backup of the first; an orphan backup; a garbled backup.
    fn fixtures() -> (FakeRepo, FakeRepo) {
        let source = FakeRepo {
            subvols: vec![
                sub(
                    1,
                    1,
                    None,
                    true,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                ),
                sub(
                    2,
                    2,
                    None,
                    true,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240102T1200",
                ),
            ],
        };
        let target = FakeRepo {
            subvols: vec![
                sub(
                    11,
                    11,
                    Some(1),
                    true,
                    "/mnt/drive",
                    "host/home.20240101T1200",
                ), // of snap 1
                sub(
                    12,
                    12,
                    Some(99),
                    true,
                    "/mnt/drive",
                    "host/home.20231201T1200",
                ), // orphan
                sub(13, 13, None, false, "/mnt/drive", "host/home.20240103T1200"), // garbled
            ],
        };
        (source, target)
    }

    #[test]
    fn stats_counts_snapshots_backups_correlated_orphaned_incomplete() {
        let (source, target) = fixtures();
        let service = InventoryService::new(&source, &target);

        let stats = service
            .stats(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
            )
            .expect("stats succeeds");

        assert_eq!(
            stats,
            Stats {
                snapshots: 2,
                backups: 3,
                correlated: 1,
                orphaned: 1,
                incomplete: 1,
            }
        );
    }

    #[test]
    fn list_pairs_each_snapshot_with_its_backups_and_partitions_the_rest() {
        let (source, target) = fixtures();
        let service = InventoryService::new(&source, &target);

        let inv = service
            .list(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
            )
            .expect("list succeeds");

        // Snapshot 1 has backup 11; snapshot 2 is not yet backed up.
        assert_eq!(inv.snapshots.len(), 2);
        let snap1 = inv.snapshots.iter().find(|s| s.snapshot.id == 1).unwrap();
        assert_eq!(
            snap1.backups.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![11]
        );
        let snap2 = inv.snapshots.iter().find(|s| s.snapshot.id == 2).unwrap();
        assert!(snap2.backups.is_empty());

        // The orphan and the garbled backup are partitioned out.
        assert_eq!(
            inv.orphan_backups.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![12]
        );
        assert_eq!(
            inv.incomplete_backups
                .iter()
                .map(|b| b.id)
                .collect::<Vec<_>>(),
            vec![13]
        );
    }

    #[test]
    fn foreign_named_subvolumes_are_ignored() {
        let source = FakeRepo {
            subvols: vec![
                sub(
                    1,
                    1,
                    None,
                    true,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/home.20240101T1200",
                ),
                // Not a mybtrfs name — must not be counted.
                sub(
                    9,
                    9,
                    None,
                    true,
                    "/mnt/pool",
                    ".mybtrfs_snapshots/some-other-subvol",
                ),
            ],
        };
        let target = FakeRepo { subvols: vec![] };
        let service = InventoryService::new(&source, &target);

        let stats = service
            .stats(
                Path::new("/mnt/pool/.mybtrfs_snapshots"),
                Path::new("/mnt/drive/host"),
            )
            .expect("stats succeeds");

        assert_eq!(stats.snapshots, 1);
        assert_eq!(stats.backups, 0);
    }
}
