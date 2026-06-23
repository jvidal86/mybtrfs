//! `RestoreService` — powers `restore` (a mybtrfs addition that automates
//! btrbk's documented manual procedure). Makes a **writable** snapshot of a
//! backup (never `property set ro=false`), and verifies the result's
//! `received_uuid` is empty so future incrementals stay intact (invariant #7).
//! See `documentation/01-phases-design-v2.md` Phase 4.
//!
//! This increment restores a backup that is already a read-only subvolume on the
//! destination filesystem; transferring a backup back from a remote target first
//! is a later increment.

use std::path::{Path, PathBuf};

use mybtrfs_domain::model::Subvolume;

use crate::ports::{FilesystemPort, PortError, SnapshotPort};

/// The outcome of a `restore`: the writable subvolume produced, and the path an
/// existing destination was moved aside to (if `force` displaced one).
#[derive(Debug)]
pub struct RestoreReport {
    /// The writable restored subvolume.
    pub restored: Subvolume,
    /// Where a pre-existing destination was moved (`<dest>.broken`), if any.
    pub moved_aside: Option<PathBuf>,
}

/// Why a restore could not complete.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The destination already exists and `force` was not set.
    #[error("destination already exists: {0} (use --force to move it aside)")]
    DestinationExists(PathBuf),
    /// The restore did not yield a clean writable subvolume (it is read-only or
    /// carries a `received_uuid`). Restore must never produce a received
    /// subvolume — that would poison future incrementals (#7).
    #[error("restore did not yield a clean writable subvolume")]
    NotCleanWritable,
    /// An underlying driven-port failure.
    #[error(transparent)]
    Port(#[from] PortError),
}

/// Orchestrates restore: guard the destination, make a writable snapshot of the
/// backup, and verify the result is a clean writable copy.
pub struct RestoreService<'a> {
    snapshots: &'a dyn SnapshotPort,
    fs: &'a dyn FilesystemPort,
}

impl<'a> RestoreService<'a> {
    /// Construct a service over the snapshot and filesystem ports.
    #[must_use]
    pub fn new(snapshots: &'a dyn SnapshotPort, fs: &'a dyn FilesystemPort) -> Self {
        Self { snapshots, fs }
    }

    /// Restore `backup` (a read-only subvolume accessible on the destination
    /// filesystem) to a writable subvolume at `dest`.
    ///
    /// If `dest` already exists, restore refuses unless `force` is set, in which
    /// case the existing destination is moved aside to `<dest>.broken` first. The
    /// writable copy is created via [`SnapshotPort::make_writable`] (a `btrfs
    /// subvolume snapshot` without `-r`) — never by flipping the read-only
    /// property, which would poison `received_uuid` (#7). The result is verified
    /// to be writable with no `received_uuid`.
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
    ) -> Result<RestoreReport, RestoreError> {
        log::info!(
            "restore: {} → {} (force={force})",
            backup.display(),
            dest.display()
        );
        let moved_aside = if self.fs.exists(dest)? {
            if !force {
                log::warn!(
                    "restore: destination exists, refusing without --force: {}",
                    dest.display()
                );
                return Err(RestoreError::DestinationExists(dest.to_path_buf()));
            }
            let broken = broken_path(dest);
            log::info!(
                "restore: moving aside {} → {}",
                dest.display(),
                broken.display()
            );
            self.fs.rename(dest, &broken)?;
            Some(broken)
        } else {
            None
        };

        let restored = self.snapshots.make_writable(backup, dest)?;
        if restored.readonly || restored.received_uuid.is_some() {
            log::error!(
                "restore: result is not a clean writable subvolume — invariant #7 violated"
            );
            return Err(RestoreError::NotCleanWritable);
        }

        log::info!("restore: complete: {}", dest.display());
        Ok(RestoreReport {
            restored,
            moved_aside,
        })
    }
}

/// The move-aside path for an existing destination: `<dest>.broken`.
fn broken_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".broken");
    PathBuf::from(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use mybtrfs_domain::model::{Subvolume, Uuid};

    use crate::ports::{FilesystemPort, PortError, SnapshotPort};
    use crate::restore::{RestoreError, RestoreService};

    /// A `FilesystemPort` with a fixed `exists` answer that records renames.
    struct FakeFs {
        exists: bool,
        renames: RefCell<Vec<(PathBuf, PathBuf)>>,
    }
    impl FakeFs {
        fn new(exists: bool) -> Self {
            Self {
                exists,
                renames: RefCell::new(Vec::new()),
            }
        }
        fn renames(&self) -> Vec<(PathBuf, PathBuf)> {
            self.renames.borrow().clone()
        }
    }
    impl FilesystemPort for FakeFs {
        fn exists(&self, _path: &Path) -> Result<bool, PortError> {
            Ok(self.exists)
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
        let service = RestoreService::new(&snapshots, &fs);

        let report = service
            .restore(
                Path::new("/mnt/drive/host/home.20240102T1531"),
                Path::new("/mnt/pool/home_restored"),
                false,
            )
            .expect("restore succeeds");

        assert_eq!(
            snapshots.calls(),
            vec![(
                PathBuf::from("/mnt/drive/host/home.20240102T1531"),
                PathBuf::from("/mnt/pool/home_restored"),
            )]
        );
        assert!(fs.renames().is_empty(), "nothing to move aside");
        assert!(report.moved_aside.is_none());
        assert!(!report.restored.readonly);
        assert!(report.restored.received_uuid.is_none());
    }

    #[test]
    fn restore_refuses_existing_dest_without_force() {
        crate::init_test_logger();
        let fs = FakeFs::new(true); // dest exists
        let snapshots = RecordingMakeWritable::clean();
        let service = RestoreService::new(&snapshots, &fs);

        let err = service
            .restore(
                Path::new("/mnt/drive/host/home.20240102T1531"),
                Path::new("/mnt/pool/home_restored"),
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
        let service = RestoreService::new(&snapshots, &fs);

        let report = service
            .restore(
                Path::new("/mnt/drive/host/home.20240102T1531"),
                Path::new("/mnt/pool/home_restored"),
                true,
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
        assert!(!report.restored.readonly);
    }

    #[test]
    fn restore_rejects_a_result_that_carries_a_received_uuid() {
        crate::init_test_logger();
        // Guards invariant #7: a restored subvolume must never be a received one.
        let fs = FakeFs::new(false);
        let snapshots = RecordingMakeWritable::yielding_received();
        let service = RestoreService::new(&snapshots, &fs);

        let err = service
            .restore(
                Path::new("/mnt/drive/host/home.20240102T1531"),
                Path::new("/mnt/pool/home_restored"),
                false,
            )
            .expect_err("must reject a received subvolume");

        assert!(matches!(err, RestoreError::NotCleanWritable));
    }
}
