//! Snapshot diff — show changed bytes between two snapshots.
//!
//! Phase 3: Calculates changes using btrfs subvolume find-new.
//! Provides a ballpark figure of how much data changed, useful for
//! predicting incremental backup size.

use std::path::Path;

/// A diff summary between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSummary {
    /// Path to the older snapshot.
    pub older_path: String,
    /// Human-readable size of older snapshot (e.g., "1.2 GB").
    pub older_size_human: String,
    /// Path to the newer snapshot.
    pub newer_path: String,
    /// Human-readable size of newer snapshot (e.g., "1.5 GB").
    pub newer_size_human: String,
    /// Changed bytes (from `btrfs subvolume find-new`).
    pub changed_bytes: u64,
    /// Human-readable size (e.g., "300 MB").
    pub changed_size_human: String,
}

/// Service to compute diffs between snapshots.
pub struct DiffService;

impl DiffService {
    /// Estimate changed bytes between two snapshots.
    ///
    /// # Arguments
    /// * `older_bytes` — actual size of older snapshot (in bytes)
    /// * `newer_bytes` — actual size of newer snapshot (in bytes)
    /// * `changed_bytes` — bytes changed (from `btrfs subvolume find-new`)
    /// * `older_path` — path to the older snapshot
    /// * `newer_path` — path to the newer snapshot
    ///
    /// # Returns
    /// A `DiffSummary` with byte counts and human-readable sizes.
    #[must_use]
    pub fn estimate_changes(
        older_bytes: u64,
        newer_bytes: u64,
        changed_bytes: u64,
        older_path: &Path,
        newer_path: &Path,
    ) -> DiffSummary {
        let older_str = older_path.display().to_string();
        let newer_str = newer_path.display().to_string();

        let older_size_human = format_bytes(older_bytes);
        let newer_size_human = format_bytes(newer_bytes);
        let changed_size_human = format_bytes(changed_bytes);

        DiffSummary {
            older_path: older_str,
            older_size_human,
            newer_path: newer_str,
            newer_size_human,
            changed_bytes,
            changed_size_human,
        }
    }
}

/// Format bytes as human-readable size.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", bytes / 1000)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_includes_both_paths_and_sizes() {
        let older = Path::new("/snap/data.20260623T1432");
        let newer = Path::new("/snap/data.20260624T1432");

        let diff = DiffService::estimate_changes(
            1_000_000_000, // older: 1 GB
            1_300_000_000, // newer: 1.3 GB
            300_000_000,   // changed: 300 MB
            older,
            newer,
        );

        assert_eq!(diff.older_path, "/snap/data.20260623T1432");
        assert_eq!(diff.newer_path, "/snap/data.20260624T1432");
        assert_eq!(diff.changed_bytes, 300_000_000);
        assert!(diff.older_size_human.contains("GB"));
        assert!(diff.newer_size_human.contains("GB"));
        assert!(diff.changed_size_human.contains("MB"));
    }

    #[test]
    fn diff_formats_bytes_readable() {
        assert!(format_bytes(500_000).contains("KB"));
        assert!(format_bytes(500_000_000).contains("MB"));
        assert!(format_bytes(5_000_000_000).contains("GB"));
    }

    #[test]
    fn diff_handles_zero_changes() {
        let older = Path::new("/snap/a");
        let newer = Path::new("/snap/b");

        let diff = DiffService::estimate_changes(1_000_000_000, 1_000_000_000, 0, older, newer);

        assert_eq!(diff.changed_bytes, 0);
        assert_eq!(diff.changed_size_human, "0 KB");
    }

    #[test]
    fn diff_is_deterministic() {
        let older = Path::new("/snap/old");
        let newer = Path::new("/snap/new");

        let diff1 =
            DiffService::estimate_changes(1_000_000_000, 1_200_000_000, 200_000_000, older, newer);
        let diff2 =
            DiffService::estimate_changes(1_000_000_000, 1_200_000_000, 200_000_000, older, newer);

        assert_eq!(diff1, diff2);
    }
}
