//! Backup-set file parser — TOML-based format for specifying multiple backup jobs.
//!
//! A backup-set file contains `[[backup]]` entries, each defining a source→target
//! backup operation. This is pure parsing (no I/O), suitable for the adapters layer.

use std::path::PathBuf;

/// A single backup entry parsed from a backup-set file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// Source subvolume to back up.
    pub source: PathBuf,
    /// Directory that will hold the source-side snapshot.
    pub snapshot_dir: PathBuf,
    /// Base name for the snapshot (timestamp appended).
    pub basename: String,
    /// Target directory for the backup (local path or ssh://... endpoint).
    pub target_dir: PathBuf,
    /// Optional `btrfs send -p` strategy; default "yes".
    pub incremental: Option<String>,
    /// Optional minimum snapshots to preserve; parsed from retention fields.
    pub snapshot_preserve_min: Option<String>,
    pub snapshot_preserve_hourly: Option<usize>,
    pub snapshot_preserve_daily: Option<usize>,
    pub snapshot_preserve_weekly: Option<usize>,
    pub snapshot_preserve_monthly: Option<usize>,
    pub snapshot_preserve_yearly: Option<usize>,
    /// Optional minimum backups to preserve.
    pub target_preserve_min: Option<String>,
    pub target_preserve_hourly: Option<usize>,
    pub target_preserve_daily: Option<usize>,
    pub target_preserve_weekly: Option<usize>,
    pub target_preserve_monthly: Option<usize>,
    pub target_preserve_yearly: Option<usize>,
}

/// Parse a TOML backup-set file into a list of backup entries.
///
/// # Arguments
/// * `content` — the TOML file contents as a string
///
/// # Returns
/// A `Vec<BackupEntry>` or an error if parsing fails.
///
/// # Errors
/// Returns an error if the TOML is malformed or lacks required fields.
#[must_use]
pub fn parse_backup_set(content: &str) -> Result<Vec<BackupEntry>, String> {
    use toml::Table;

    let table: Table = toml::from_str(content).map_err(|e| format!("TOML parse error: {}", e))?;

    let mut entries = Vec::new();

    if let Some(backups) = table.get("backup") {
        let arr = backups
            .as_array()
            .ok_or("'backup' must be an array of tables")?;

        for (idx, item) in arr.iter().enumerate() {
            let obj = item
                .as_table()
                .ok_or_else(|| format!("backup[{}] must be a table", idx))?;

            let source = obj
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("backup[{}].source is required and must be a string", idx))?
                .parse::<PathBuf>()
                .map_err(|_| format!("backup[{}].source is not a valid path", idx))?;

            let snapshot_dir = obj
                .get("snapshot_dir")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "backup[{}].snapshot_dir is required and must be a string",
                        idx
                    )
                })?
                .parse::<PathBuf>()
                .map_err(|_| format!("backup[{}].snapshot_dir is not a valid path", idx))?;

            let basename = obj
                .get("basename")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!("backup[{}].basename is required and must be a string", idx)
                })?
                .to_string();

            let target_dir = obj
                .get("target_dir")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "backup[{}].target_dir is required and must be a string",
                        idx
                    )
                })?
                .parse::<PathBuf>()
                .map_err(|_| format!("backup[{}].target_dir is not a valid path", idx))?;

            let incremental = obj
                .get("incremental")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let snapshot_preserve_min = obj
                .get("snapshot_preserve_min")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let snapshot_preserve_hourly = obj
                .get("snapshot_preserve_hourly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let snapshot_preserve_daily = obj
                .get("snapshot_preserve_daily")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let snapshot_preserve_weekly = obj
                .get("snapshot_preserve_weekly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let snapshot_preserve_monthly = obj
                .get("snapshot_preserve_monthly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let snapshot_preserve_yearly = obj
                .get("snapshot_preserve_yearly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);

            let target_preserve_min = obj
                .get("target_preserve_min")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let target_preserve_hourly = obj
                .get("target_preserve_hourly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let target_preserve_daily = obj
                .get("target_preserve_daily")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let target_preserve_weekly = obj
                .get("target_preserve_weekly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let target_preserve_monthly = obj
                .get("target_preserve_monthly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);
            let target_preserve_yearly = obj
                .get("target_preserve_yearly")
                .and_then(|v| v.as_integer())
                .map(|n| n as usize);

            entries.push(BackupEntry {
                source,
                snapshot_dir,
                basename,
                target_dir,
                incremental,
                snapshot_preserve_min,
                snapshot_preserve_hourly,
                snapshot_preserve_daily,
                snapshot_preserve_weekly,
                snapshot_preserve_monthly,
                snapshot_preserve_yearly,
                target_preserve_min,
                target_preserve_hourly,
                target_preserve_daily,
                target_preserve_weekly,
                target_preserve_monthly,
                target_preserve_yearly,
            });
        }
    }

    if entries.is_empty() {
        return Err("no [[backup]] entries found".to_string());
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_backup_entry() {
        let toml = r#"
[[backup]]
source = "/mnt/source/@data"
snapshot_dir = "/mnt/source/snapshots"
basename = "data"
target_dir = "/mnt/backup/data"
"#;

        let entries = parse_backup_set(toml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, PathBuf::from("/mnt/source/@data"));
        assert_eq!(entries[0].basename, "data");
    }

    #[test]
    fn parse_multiple_backup_entries() {
        let toml = r#"
[[backup]]
source = "/mnt/source/@data"
snapshot_dir = "/mnt/source/snapshots"
basename = "data"
target_dir = "/mnt/backup/data"

[[backup]]
source = "/mnt/source/@home"
snapshot_dir = "/mnt/source/snapshots"
basename = "home"
target_dir = "/mnt/backup/home"
"#;

        let entries = parse_backup_set(toml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].basename, "data");
        assert_eq!(entries[1].basename, "home");
    }

    #[test]
    fn parse_with_retention_options() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
basename = "data"
target_dir = "/backup"
snapshot_preserve_min = "latest"
snapshot_preserve_daily = 7
target_preserve_min = "latest"
target_preserve_daily = 30
"#;

        let entries = parse_backup_set(toml).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].snapshot_preserve_min, Some("latest".to_string()));
        assert_eq!(entries[0].snapshot_preserve_daily, Some(7));
        assert_eq!(entries[0].target_preserve_daily, Some(30));
    }

    #[test]
    fn missing_required_field_errors() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
"#;

        let err = parse_backup_set(toml).unwrap_err();
        assert!(err.contains("basename"));
    }

    #[test]
    fn no_backup_entries_errors() {
        let toml = "";
        let err = parse_backup_set(toml).unwrap_err();
        assert_eq!(err, "no [[backup]] entries found");
    }
}
