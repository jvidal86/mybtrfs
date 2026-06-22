//! `LocalFsAdapter` (`std::fs`) — implements `FilesystemPort`: path existence
//! checks, directory creation (default snapshot/target dirs), and rename /
//! move-aside (`D → D.broken` on restore). These are plain filesystem ops, not
//! btrfs commands — kept separate from `BtrfsCliAdapter` (SRP).

use std::path::Path;

use mybtrfs_application::ports::{FilesystemPort, PortError};

/// [`FilesystemPort`] over `std::fs`.
pub struct LocalFsAdapter;

impl LocalFsAdapter {
    /// Create a filesystem adapter backed by `std::fs`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalFsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesystemPort for LocalFsAdapter {
    fn exists(&self, path: &Path) -> Result<bool, PortError> {
        // `try_exists` distinguishes "absent" from "couldn't determine" (e.g. a
        // permission error on a parent), unlike `Path::exists`.
        path.try_exists().map_err(PortError::Io)
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), PortError> {
        std::fs::create_dir_all(path).map_err(PortError::Io)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<(), PortError> {
        std::fs::rename(from, to).map_err(PortError::Io)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique temp directory for one test, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("mybtrfs-localfs-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exists_reports_presence_and_absence() {
        let tmp = TempDir::new("exists");
        let fs = LocalFsAdapter::new();
        let present = tmp.path("here");
        std::fs::write(&present, b"x").unwrap();
        assert!(fs.exists(&present).unwrap());
        assert!(!fs.exists(&tmp.path("absent")).unwrap());
    }

    #[test]
    fn create_dir_all_makes_nested_dirs() {
        let tmp = TempDir::new("mkdir");
        let fs = LocalFsAdapter::new();
        let nested = tmp.path("a/b/c");
        fs.create_dir_all(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn rename_moves_a_path_aside() {
        let tmp = TempDir::new("rename");
        let fs = LocalFsAdapter::new();
        let from = tmp.path("orig");
        let to = tmp.path("orig.broken");
        std::fs::write(&from, b"data").unwrap();
        fs.rename(&from, &to).unwrap();
        assert!(!from.exists());
        assert!(to.exists());
    }
}
