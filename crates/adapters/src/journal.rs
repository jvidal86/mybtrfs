//! `FileJournal` — implements `Journal`: an append-only audit log. Each
//! [`Journal::record`] appends one line to a file (created if absent); the caller
//! formats the message (e.g. a timestamp + the action/source/target/status). A
//! no-op [`NullJournal`] is the default when no journal path is configured.
//! See `documentation/02-architecture-v2.md`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use mybtrfs_application::ports::{Journal, PortError};

/// Append-only [`Journal`] backed by a file.
pub struct FileJournal {
    path: PathBuf,
}

impl FileJournal {
    /// Create a journal that appends to `path` (created on first write).
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Journal for FileJournal {
    fn record(&self, message: &str) -> Result<(), PortError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }
}

/// A [`Journal`] that records nothing — the default when no journal is configured.
pub struct NullJournal;

impl Journal for NullJournal {
    fn record(&self, _message: &str) -> Result<(), PortError> {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A unique temp path for this test process/run (avoids collisions).
    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mybtrfs-journal-{tag}-{nanos}.log"))
    }

    #[test]
    fn appends_each_record_as_a_line_creating_the_file() {
        crate::init_test_logger();
        let path = temp_path("append");
        let journal = FileJournal::new(path.clone());

        journal.record("snapshot home.20240101T1200").unwrap();
        journal
            .record("backup home.20240101T1200 -> /mnt/drive")
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            contents,
            "snapshot home.20240101T1200\nbackup home.20240101T1200 -> /mnt/drive\n"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn null_journal_records_nothing() {
        crate::init_test_logger();
        assert!(NullJournal.record("ignored").is_ok());
    }
}
