//! `FileLock` — a process-level advisory lock (RAII) that stops two `mybtrfs`
//! runs from mutating overlapping state concurrently (mirrors btrbk's lockfile).
//! Backed by an exclusive `flock(2)` held for the lifetime of the guard; the OS
//! releases it automatically when the process exits, so a crash never leaves a
//! stale lock. See `documentation/05-e2e-test-spec.md` E2E-CC-09.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use mybtrfs_application::ports::PortError;

/// A held advisory lock; dropping the guard (or the process exiting) releases it.
#[must_use = "the lock is released as soon as the guard is dropped"]
#[derive(Debug)]
pub struct FileLock {
    /// The open file whose `flock` we hold; the lock is released when it closes.
    _file: File,
}

impl FileLock {
    /// Try to take an exclusive lock on `path` (the file is created if absent).
    ///
    /// Returns `Ok(Some(guard))` when acquired, `Ok(None)` when another run
    /// already holds the lock (the caller maps this to the lock-busy exit code),
    /// or `Err` on an I/O failure opening or locking the file.
    pub fn acquire(path: &Path) -> Result<Option<Self>, PortError> {
        // Open without truncating (the file holds no data — only its flock).
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(err.into()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique temp path for this test process/run (avoids cross-test clashes).
    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mybtrfs-lock-{tag}-{nanos}.lock"))
    }

    #[test]
    fn a_second_acquire_is_busy_until_the_first_is_released() {
        crate::init_test_logger();
        let path = temp_path("acquire");

        let first = FileLock::acquire(&path).unwrap();
        assert!(first.is_some(), "first acquire should succeed");

        // A concurrent run sees the lock as busy (None), never a second hold.
        assert!(
            FileLock::acquire(&path).unwrap().is_none(),
            "a second acquire while the lock is held should be busy"
        );

        // Releasing the first lock lets a later run acquire it.
        drop(first);
        let third = FileLock::acquire(&path).unwrap();
        assert!(
            third.is_some(),
            "acquire after the lock is released should succeed"
        );

        drop(third);
        std::fs::remove_file(&path).ok();
    }
}
