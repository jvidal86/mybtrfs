//! Backup-set file parser — TOML-based format for specifying multiple backup jobs.
//!
//! A backup-set file contains `[[backup]]` entries, each defining a source→target
//! backup operation. This is pure parsing (no I/O), suitable for the adapters layer.

use std::path::PathBuf;

/// Extract an optional string field from a TOML table entry.
///
/// Returns `Ok(None)` when the key is absent, `Ok(Some(s))` when it is a string,
/// and `Err` when the key is present but is not a string (rule 16: present-but-malformed
/// is a parse error, never a silent coercion).
fn opt_str(obj: &toml::Table, key: &str, idx: usize) -> Result<Option<String>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("backup[{idx}].{key} must be a string")),
    }
}

/// Extract an optional non-negative integer field from a TOML table entry.
///
/// Returns `Ok(None)` when absent, `Ok(Some(n))` when an integer, and `Err` when
/// present but not an integer (rule 16).
fn opt_usize(obj: &toml::Table, key: &str, idx: usize) -> Result<Option<usize>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_integer()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("backup[{idx}].{key} must be an integer")),
    }
}

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
    /// Shell command run before creating the snapshot (via `sh -c`); overrides the CLI flag.
    pub pre_snapshot_hook: Option<String>,
    /// Shell command run after the snapshot is created (via `sh -c`); overrides the CLI flag.
    pub post_snapshot_hook: Option<String>,
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
    let table: toml::Table =
        toml::from_str(content).map_err(|e| format!("TOML parse error: {}", e))?;

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

            let incremental = opt_str(obj, "incremental", idx)?;

            let snapshot_preserve_min = opt_str(obj, "snapshot_preserve_min", idx)?;
            let snapshot_preserve_hourly = opt_usize(obj, "snapshot_preserve_hourly", idx)?;
            let snapshot_preserve_daily = opt_usize(obj, "snapshot_preserve_daily", idx)?;
            let snapshot_preserve_weekly = opt_usize(obj, "snapshot_preserve_weekly", idx)?;
            let snapshot_preserve_monthly = opt_usize(obj, "snapshot_preserve_monthly", idx)?;
            let snapshot_preserve_yearly = opt_usize(obj, "snapshot_preserve_yearly", idx)?;

            let target_preserve_min = opt_str(obj, "target_preserve_min", idx)?;
            let target_preserve_hourly = opt_usize(obj, "target_preserve_hourly", idx)?;
            let target_preserve_daily = opt_usize(obj, "target_preserve_daily", idx)?;
            let target_preserve_weekly = opt_usize(obj, "target_preserve_weekly", idx)?;
            let target_preserve_monthly = opt_usize(obj, "target_preserve_monthly", idx)?;
            let target_preserve_yearly = opt_usize(obj, "target_preserve_yearly", idx)?;

            let pre_snapshot_hook = opt_str(obj, "pre_snapshot_hook", idx)?;
            let post_snapshot_hook = opt_str(obj, "post_snapshot_hook", idx)?;

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
                pre_snapshot_hook,
                post_snapshot_hook,
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
    fn missing_basename_errors() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
target_dir = "/backup"
"#;

        let err = parse_backup_set(toml).unwrap_err();
        assert!(err.contains("basename"));
    }

    #[test]
    fn missing_snapshot_dir_errors() {
        let toml = r#"
[[backup]]
source = "/data"
basename = "data"
target_dir = "/backup"
"#;

        let err = parse_backup_set(toml).unwrap_err();
        assert!(err.contains("snapshot_dir"));
    }

    #[test]
    fn missing_target_dir_errors() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
basename = "data"
"#;

        let err = parse_backup_set(toml).unwrap_err();
        assert!(err.contains("target_dir"));
    }

    #[test]
    fn no_backup_entries_errors() {
        let toml = "";
        let err = parse_backup_set(toml).unwrap_err();
        assert_eq!(err, "no [[backup]] entries found");
    }

    #[test]
    fn parse_with_snapshot_hooks() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
basename = "data"
target_dir = "/backup"
pre_snapshot_hook = "sync && freeze-db.sh"
post_snapshot_hook = "unfreeze-db.sh"
"#;
        let entries = parse_backup_set(toml).unwrap();
        assert_eq!(
            entries[0].pre_snapshot_hook,
            Some("sync && freeze-db.sh".to_string())
        );
        assert_eq!(
            entries[0].post_snapshot_hook,
            Some("unfreeze-db.sh".to_string())
        );
    }

    #[test]
    fn wrong_type_field_is_an_error_not_silent_none() {
        // snapshot_preserve_daily must be an integer; passing a string violates rule 16.
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
basename = "data"
target_dir = "/backup"
snapshot_preserve_daily = "seven"
"#;
        let err = parse_backup_set(toml).unwrap_err();
        assert!(
            err.contains("snapshot_preserve_daily"),
            "error should name the offending field, got: {err}"
        );
        assert!(
            err.contains("integer"),
            "error should state the expected type, got: {err}"
        );
    }

    #[test]
    fn hooks_are_none_when_absent() {
        let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
basename = "data"
target_dir = "/backup"
"#;
        let entries = parse_backup_set(toml).unwrap();
        assert!(entries[0].pre_snapshot_hook.is_none());
        assert!(entries[0].post_snapshot_hook.is_none());
    }
}
