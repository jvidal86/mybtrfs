//! Integration tests for backup-set file functionality.
//!
//! These tests verify that the backup-set parser (in adapters) is correctly
//! exposed and functional from the CLI.

#![allow(clippy::unwrap_used)]

use mybtrfs_adapters::parse_backup_set;
use std::path::PathBuf;

#[test]
fn backup_set_parser_parses_single_entry() {
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
    assert_eq!(entries[0].target_dir, PathBuf::from("/mnt/backup/data"));
}

#[test]
fn backup_set_parser_parses_multiple_entries() {
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
fn backup_set_parser_with_retention_options() {
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
fn backup_set_parser_rejects_missing_required_field() {
    let toml = r#"
[[backup]]
source = "/data"
snapshot_dir = "/snapshots"
"#;

    let err = parse_backup_set(toml).unwrap_err();
    assert!(err.contains("basename"));
}

#[test]
fn backup_set_parser_rejects_empty_file() {
    let toml = "";
    let err = parse_backup_set(toml).unwrap_err();
    assert_eq!(err, "no [[backup]] entries found");
}
