//! `FileJournal` — implements `Journal`: an append-only audit log. Each
//! [`Journal::record`] appends one line to a file (created if absent); the caller
//! formats the message (e.g. a timestamp + the action/source/target/status). A
//! no-op [`NullJournal`] is the default when no journal path is configured.
//! See `documentation/02-architecture-v2.md`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
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
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{message}")?;
        Ok(())
    }

    fn last_entries(&self, count: usize) -> Result<Vec<String>, PortError> {
        // If the journal file doesn't exist, return an empty vector (not an error).
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        // Read all lines from the journal file.
        let file = std::fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut lines: Vec<String> = Vec::new();
        // Intentionally skip lines that fail to parse; journal may have partial writes.
        #[allow(clippy::manual_flatten)]
        for line_result in reader.lines() {
            if let Ok(line) = line_result {
                lines.push(line);
            }
        }

        // Return the last `count` entries, most recent first (i.e., reversed).
        let start = if lines.len() > count {
            lines.len() - count
        } else {
            0
        };
        let mut result: Vec<String> = lines[start..].to_vec();
        result.reverse();
        Ok(result)
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

    #[test]
    fn last_entries_returns_empty_if_journal_not_exists() {
        crate::init_test_logger();
        let path = temp_path("nonexistent");
        let journal = FileJournal::new(path);
        let entries = journal.last_entries(5).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn last_entries_returns_all_if_fewer_than_count() {
        crate::init_test_logger();
        let path = temp_path("fewer");
        let journal = FileJournal::new(path.clone());

        journal.record("entry 1").unwrap();
        journal.record("entry 2").unwrap();
        journal.record("entry 3").unwrap();

        let entries = journal.last_entries(10).unwrap();
        assert_eq!(entries.len(), 3);
        // Most recent first
        assert_eq!(entries[0], "entry 3");
        assert_eq!(entries[1], "entry 2");
        assert_eq!(entries[2], "entry 1");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn last_entries_returns_most_recent_in_order() {
        crate::init_test_logger();
        let path = temp_path("truncate");
        let journal = FileJournal::new(path.clone());

        journal.record("entry 1").unwrap();
        journal.record("entry 2").unwrap();
        journal.record("entry 3").unwrap();
        journal.record("entry 4").unwrap();
        journal.record("entry 5").unwrap();

        let entries = journal.last_entries(2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], "entry 5"); // most recent
        assert_eq!(entries[1], "entry 4");
        std::fs::remove_file(&path).unwrap();
    }
}
