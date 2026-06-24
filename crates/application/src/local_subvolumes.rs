//! `LocalSubvolumesService` — read-only enumeration of *every* btrfs subvolume on
//! the local system. Composes two existing ports: discover the mounted btrfs
//! filesystems ([`DriveDiscoveryPort`]), then list each one's subvolumes
//! ([`SubvolumeRepository`]). No new port is needed — this is pure orchestration.
//!
//! Unlike [`crate::inventory`] (which filters to mybtrfs-named snapshots/backups
//! under a single directory), this lists *all* subvolumes across all filesystems
//! — the "what btrfs units exist on this machine" view used to pick a backup
//! source. See `documentation/01-phases-design-v2.md` (CLI surface).

use mybtrfs_domain::model::Subvolume;

use crate::ports::{DriveDiscoveryPort, PortError, SubvolumeRepository};

/// Read-only enumeration of the local system's btrfs subvolumes.
pub struct LocalSubvolumesService<'a> {
    /// Discovers the mounted btrfs filesystems (via `lsblk`; needs no root).
    discovery: &'a dyn DriveDiscoveryPort,
    /// Lists the subvolumes of a given filesystem (via `btrfs subvolume list`).
    repo: &'a dyn SubvolumeRepository,
}

impl<'a> LocalSubvolumesService<'a> {
    /// Construct a service over the drive-discovery and subvolume-listing ports.
    #[must_use]
    pub fn new(discovery: &'a dyn DriveDiscoveryPort, repo: &'a dyn SubvolumeRepository) -> Self {
        Self { discovery, repo }
    }

    /// Every subvolume on every mounted btrfs filesystem, sorted by
    /// `(mountpoint, id)` for stable, deterministic output. Each filesystem is
    /// discovered once (drive discovery deduplicates by fs UUID), then listed via
    /// any of its mountpoints.
    ///
    /// # Errors
    /// [`PortError`] from drive discovery, or from listing any one filesystem
    /// (e.g. [`PortError::Command`] when `btrfs subvolume list` is run without
    /// root). Listing fails fast: one unreadable filesystem fails the whole call.
    pub fn list_all(&self) -> Result<Vec<Subvolume>, PortError> {
        let filesystems = self.discovery.detect()?;
        let mut all = Vec::new();
        for fs in &filesystems {
            log::debug!(
                "listing subvolumes on {} ({})",
                fs.mountpoint.display(),
                fs.fs_uuid
            );
            all.extend(self.repo.list(&fs.mountpoint)?);
        }
        all.sort_by(|left, right| {
            left.mountpoint
                .cmp(&right.mountpoint)
                .then(left.id.cmp(&right.id))
        });
        log::debug!(
            "found {} subvolume(s) across {} filesystem(s)",
            all.len(),
            filesystems.len()
        );
        Ok(all)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use mybtrfs_domain::model::{Subvolume, Uuid};

    use crate::local_subvolumes::LocalSubvolumesService;
    use crate::ports::{DiscoveredFilesystem, DriveDiscoveryPort, PortError, SubvolumeRepository};

    fn uuid_hex(tag: u64) -> Uuid {
        Uuid::parse(&format!("{tag:08x}-0000-0000-0000-000000000000")).expect("valid uuid")
    }

    fn fs(mountpoint: &str, fs_uuid: u64) -> DiscoveredFilesystem {
        DiscoveredFilesystem {
            device: PathBuf::from("/dev/sdz"),
            mountpoint: PathBuf::from(mountpoint),
            fs_uuid: uuid_hex(fs_uuid),
            label: None,
            removable: false,
        }
    }

    fn sub(id: u64, mount: &str, rel: &str) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid_hex(id)),
            parent_uuid: None,
            received_uuid: None,
            generation: 10,
            cgen: 10,
            readonly: false,
            path: PathBuf::from(rel),
            fs_uuid: uuid_hex(0),
            mountpoint: PathBuf::from(mount),
        }
    }

    /// Drive discovery returning a fixed filesystem list.
    struct FakeDiscovery(Vec<DiscoveredFilesystem>);
    impl DriveDiscoveryPort for FakeDiscovery {
        fn detect(&self) -> Result<Vec<DiscoveredFilesystem>, PortError> {
            Ok(self.0.clone())
        }
    }

    /// Drive discovery that always errors (to test propagation).
    struct FailingDiscovery;
    impl DriveDiscoveryPort for FailingDiscovery {
        fn detect(&self) -> Result<Vec<DiscoveredFilesystem>, PortError> {
            Err(PortError::Command("lsblk failed".to_owned()))
        }
    }

    /// Repository returning per-filesystem subvolumes keyed by mountpoint; an
    /// unknown mountpoint yields a command error (mirrors a `btrfs` failure).
    struct FakeRepo(HashMap<PathBuf, Vec<Subvolume>>);
    impl SubvolumeRepository for FakeRepo {
        fn show(&self, _path: &Path) -> Result<Subvolume, PortError> {
            unimplemented!("not exercised by these tests")
        }
        fn list(&self, filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
            self.0
                .get(filesystem)
                .cloned()
                .ok_or_else(|| PortError::Command(format!("no such fs: {}", filesystem.display())))
        }
    }

    #[test]
    fn lists_every_subvolume_across_all_filesystems_sorted() {
        crate::init_test_logger();
        let discovery = FakeDiscovery(vec![fs("/home", 1), fs("/mnt/drive", 2)]);
        let repo = FakeRepo(HashMap::from([
            (
                PathBuf::from("/home"),
                // Intentionally out of id order to prove the sort.
                vec![sub(257, "/home", "@home"), sub(256, "/home", "@")],
            ),
            (
                PathBuf::from("/mnt/drive"),
                vec![sub(300, "/mnt/drive", "backup")],
            ),
        ]));

        let found = LocalSubvolumesService::new(&discovery, &repo)
            .list_all()
            .expect("listing succeeds");

        // Sorted by (mountpoint, id): /home before /mnt/drive; within /home, 256<257.
        let order: Vec<(u64, &str)> = found
            .iter()
            .map(|s| (s.id, s.mountpoint.to_str().unwrap()))
            .collect();
        assert_eq!(
            order,
            vec![(256, "/home"), (257, "/home"), (300, "/mnt/drive")]
        );
    }

    #[test]
    fn empty_when_no_btrfs_filesystems() {
        crate::init_test_logger();
        let discovery = FakeDiscovery(vec![]);
        let repo = FakeRepo(HashMap::new());
        let found = LocalSubvolumesService::new(&discovery, &repo)
            .list_all()
            .expect("listing succeeds");
        assert!(found.is_empty());
    }

    #[test]
    fn propagates_a_discovery_error() {
        crate::init_test_logger();
        let repo = FakeRepo(HashMap::new());
        let err = LocalSubvolumesService::new(&FailingDiscovery, &repo)
            .list_all()
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }

    #[test]
    fn fails_fast_when_a_filesystem_cannot_be_listed() {
        crate::init_test_logger();
        // Discovery finds a filesystem the repo can't list (e.g. permission denied).
        let discovery = FakeDiscovery(vec![fs("/home", 1)]);
        let repo = FakeRepo(HashMap::new()); // no entry for /home → list errors
        let err = LocalSubvolumesService::new(&discovery, &repo)
            .list_all()
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }
}
