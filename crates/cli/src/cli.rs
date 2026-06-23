//! Driving adapter + composition root: parses the command set with clap, wires
//! the concrete adapters into the use cases, and dispatches. Paths are
//! canonicalized/validated here (decision ID-2) and deletions are logged here
//! (decision ID-1); the use cases trust their inputs. See `documentation/01`
//! (CLI surface) and `02` §3.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use mybtrfs_adapters::{BtrfsCliAdapter, LocalFsAdapter, LsblkDriveDiscovery, SystemClock};
use mybtrfs_application::backup::{BackupService, ResumeReport, RunReport};
use mybtrfs_application::inventory::{Inventory, InventoryService, Stats};
use mybtrfs_application::ports::{
    DeleteCommit, DeletePort, DiscoveredFilesystem, DriveDiscoveryPort, PortError,
};
use mybtrfs_application::restore::RestoreService;
use mybtrfs_application::retention::RetentionService;
use mybtrfs_domain::naming::TimestampFormat;
use mybtrfs_domain::retention::RetentionPolicy;

/// Process exit codes (central table — RULES rule 14).
mod exit_code {
    /// A command failed.
    pub const FAILURE: u8 = 1;
}

/// `mybtrfs` — a backup tool for btrfs subvolumes (a Rust reimagining of btrbk).
#[derive(Parser)]
#[command(name = "mybtrfs", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The command set (see `documentation/01`). Phase 1 implements `snapshot` and
/// `run`; the rest are placeholders for later phases / not-yet-wired use cases.
#[derive(Subcommand)]
enum Command {
    /// Create a read-only snapshot of a source subvolume.
    Snapshot {
        /// Source subvolume to snapshot.
        source: PathBuf,
        /// Directory that will hold the snapshot.
        snapshot_dir: PathBuf,
        /// Base name for the snapshot (a timestamp is appended).
        basename: String,
    },
    /// Full backup: snapshot the source, send/receive to the target, then prune.
    Run {
        /// Source subvolume to back up.
        source: PathBuf,
        /// Directory that will hold the source-side snapshot.
        snapshot_dir: PathBuf,
        /// Base name for the snapshot (a timestamp is appended).
        basename: String,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
    },
    /// Re-send the latest not-yet-backed-up snapshot without creating a new one.
    Resume {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Base name of the snapshot series to resume.
        basename: String,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
    },
    /// Prune snapshots/backups per retention policy (Phase 3).
    Prune,
    /// Restore a backup to a writable subvolume at `dest` (Phase 4).
    Restore {
        /// The backup: a read-only subvolume on the destination filesystem.
        backup: PathBuf,
        /// Where to create the writable restored subvolume.
        dest: PathBuf,
        /// If `dest` exists, move it aside to `<dest>.broken` instead of refusing.
        #[arg(long)]
        force: bool,
    },
    /// List source snapshots with their correlated backups (Phase 3).
    List {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
    },
    /// Show aggregate backup statistics (Phase 3).
    Stats {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
    },
    /// List candidate backup drives (Phase 1 UX).
    ListDrives,
}

/// Parse the command line and run; the returned exit code reflects success.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(exit_code::FAILURE)
        }
    }
}

/// Wire the adapters into the use cases and execute `command`.
fn dispatch(command: &Command) -> Result<()> {
    let clock = SystemClock;
    let btrfs = BtrfsCliAdapter::new();
    let localfs = LocalFsAdapter::new();
    // Deletions are logged at the composition root (decision ID-1).
    let deleter = LoggingDeletePort { inner: &btrfs };
    let retention = RetentionService::new(&clock, &deleter);
    // One resolve-per-path adapter serves as both source and target repository
    // (it resolves each path's filesystem), plus the snapshot and transfer ports.
    let backup = BackupService::new(
        &clock,
        &btrfs, // source_repo
        &btrfs, // target_repo
        &btrfs, // snapshots
        &btrfs, // transfer
        &retention,
        TimestampFormat::Long,
    );

    match command {
        Command::Snapshot {
            source,
            snapshot_dir,
            basename,
        } => {
            let source = validate_path(source)?;
            let snapshot_dir = validate_path(snapshot_dir)?;
            let snapshot = backup
                .snapshot(&source, &snapshot_dir, basename)
                .context("failed to create snapshot")?;
            println!(
                "created snapshot: {}",
                snapshot.mountpoint.join(&snapshot.path).display()
            );
            Ok(())
        }
        Command::Run {
            source,
            snapshot_dir,
            basename,
            target_dir,
        } => {
            let source = validate_path(source)?;
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            // Keep-all retention by default (Phase 1); policy flags arrive in Phase 3.
            let report = backup
                .run(
                    &source,
                    &snapshot_dir,
                    basename,
                    &target_dir,
                    &RetentionPolicy::default(),
                    &RetentionPolicy::default(),
                )
                .context("backup run failed")?;
            print_run_report(&report);
            Ok(())
        }
        Command::ListDrives => {
            let drives = LsblkDriveDiscovery::new()
                .detect()
                .context("drive discovery failed")?;
            print_drives(&drives);
            Ok(())
        }
        Command::Restore {
            backup,
            dest,
            force,
        } => {
            let backup = validate_path(backup)?;
            let dest = validate_new_path(dest)?;
            let report = RestoreService::new(&btrfs, &localfs)
                .restore(&backup, &dest, *force)
                .context("restore failed")?;
            println!(
                "restored: {}",
                report
                    .restored
                    .mountpoint
                    .join(&report.restored.path)
                    .display()
            );
            if let Some(moved) = &report.moved_aside {
                println!("moved aside existing destination to: {}", moved.display());
            }
            Ok(())
        }
        Command::List {
            snapshot_dir,
            target_dir,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            let inventory = InventoryService::new(&btrfs, &btrfs)
                .list(&snapshot_dir, &target_dir)
                .context("listing the inventory failed")?;
            print_inventory(&inventory);
            Ok(())
        }
        Command::Stats {
            snapshot_dir,
            target_dir,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            let stats = InventoryService::new(&btrfs, &btrfs)
                .stats(&snapshot_dir, &target_dir)
                .context("computing statistics failed")?;
            print_stats(&stats);
            Ok(())
        }
        Command::Resume {
            snapshot_dir,
            basename,
            target_dir,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            // Keep-all retention by default (policy flags arrive in Phase 3).
            let report = backup
                .resume(
                    &snapshot_dir,
                    basename,
                    &target_dir,
                    &RetentionPolicy::default(),
                    &RetentionPolicy::default(),
                )
                .context("resume failed")?;
            print_resume_report(&report);
            Ok(())
        }
        Command::Prune => {
            bail!("this command is not implemented yet")
        }
    }
}

/// Canonicalize and validate a path before handing it to the use cases
/// (decision ID-2): the result is absolute, symlink-resolved, and `..`-free, and
/// the path must already exist.
fn validate_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("invalid or missing path: {}", path.display()))
}

/// Validate a not-yet-existing destination (decision ID-2): its parent must
/// exist; returns `<canonical-parent>/<final-component>`.
fn validate_new_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .context("destination must have a parent directory")?;
    let name = path
        .file_name()
        .context("destination must have a final path component")?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("destination parent does not exist: {}", parent.display()))?;
    Ok(canonical_parent.join(name))
}

/// Print a one-fact-per-line summary of a completed run.
fn print_run_report(report: &RunReport) {
    println!(
        "snapshot: {}",
        report
            .snapshot
            .mountpoint
            .join(&report.snapshot.path)
            .display()
    );
    println!(
        "backup:   {}",
        report.backup.mountpoint.join(&report.backup.path).display()
    );
    println!(
        "pruned {} snapshot(s), {} backup(s)",
        report.snapshots_pruned.delete.len(),
        report.backups_pruned.delete.len()
    );
}

/// Print a one-fact-per-line summary of a completed resume.
fn print_resume_report(report: &ResumeReport) {
    match &report.transferred {
        Some(backup) => println!(
            "transferred: {}",
            backup.mountpoint.join(&backup.path).display()
        ),
        None => println!("nothing to resume: the latest snapshot is already backed up"),
    }
    println!(
        "pruned {} snapshot(s), {} backup(s)",
        report.snapshots_pruned.delete.len(),
        report.backups_pruned.delete.len()
    );
}

/// Print a one-fact-per-line inventory: each snapshot with its backups, then the
/// orphaned and incomplete backups.
fn print_inventory(inventory: &Inventory) {
    for status in &inventory.snapshots {
        println!(
            "snapshot: {}",
            status
                .snapshot
                .mountpoint
                .join(&status.snapshot.path)
                .display()
        );
        if status.backups.is_empty() {
            println!("  (no backups)");
        } else {
            for backup in &status.backups {
                println!(
                    "  backup: {}",
                    backup.mountpoint.join(&backup.path).display()
                );
            }
        }
    }
    for orphan in &inventory.orphan_backups {
        println!(
            "orphan backup: {}",
            orphan.mountpoint.join(&orphan.path).display()
        );
    }
    for incomplete in &inventory.incomplete_backups {
        println!(
            "incomplete backup: {}",
            incomplete.mountpoint.join(&incomplete.path).display()
        );
    }
}

/// Print aggregate backup statistics, one fact per line.
fn print_stats(stats: &Stats) {
    println!("snapshots:   {}", stats.snapshots);
    println!("backups:     {}", stats.backups);
    println!("correlated:  {}", stats.correlated);
    println!("orphaned:    {}", stats.orphaned);
    println!("incomplete:  {}", stats.incomplete);
}

/// Print the discovered btrfs filesystems (backup-target candidates).
fn print_drives(drives: &[DiscoveredFilesystem]) {
    if drives.is_empty() {
        println!("no mounted btrfs filesystems found");
        return;
    }
    for drive in drives {
        let label = drive.label.as_deref().unwrap_or("-");
        let kind = if drive.removable {
            "removable"
        } else {
            "fixed"
        };
        println!(
            "{}\t{}\tlabel={label}\t{kind}\t{}",
            drive.mountpoint.display(),
            drive.device.display(),
            drive.fs_uuid
        );
    }
}

/// Wraps a [`DeletePort`] to log each deletion (observability lives at the
/// composition root — decision ID-1).
struct LoggingDeletePort<'a> {
    inner: &'a dyn DeletePort,
}

impl DeletePort for LoggingDeletePort<'_> {
    fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
        println!("deleting: {}", path.display());
        self.inner.delete(path, commit)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_run_command_with_four_paths() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "run",
            "/mnt/pool/home",
            "/mnt/pool/.snapshots",
            "home",
            "/mnt/drive/host",
        ])
        .unwrap();
        match cli.command {
            Command::Run {
                source,
                snapshot_dir,
                basename,
                target_dir,
            } => {
                assert_eq!(source, PathBuf::from("/mnt/pool/home"));
                assert_eq!(snapshot_dir, PathBuf::from("/mnt/pool/.snapshots"));
                assert_eq!(basename, "home");
                assert_eq!(target_dir, PathBuf::from("/mnt/drive/host"));
            }
            _ => panic!("expected a Run command"),
        }
    }

    #[test]
    fn snapshot_requires_all_its_arguments() {
        // Missing the basename + snapshot_dir → clap rejects it.
        assert!(Cli::try_parse_from(["mybtrfs", "snapshot", "/only/source"]).is_err());
    }

    #[test]
    fn parses_resume_command() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "resume",
            "/mnt/pool/.snapshots",
            "home",
            "/mnt/drive/host",
        ])
        .unwrap();
        match cli.command {
            Command::Resume {
                snapshot_dir,
                basename,
                target_dir,
            } => {
                assert_eq!(snapshot_dir, PathBuf::from("/mnt/pool/.snapshots"));
                assert_eq!(basename, "home");
                assert_eq!(target_dir, PathBuf::from("/mnt/drive/host"));
            }
            _ => panic!("expected a Resume command"),
        }
    }

    #[test]
    fn list_and_stats_take_two_dirs() {
        let list = Cli::try_parse_from(["mybtrfs", "list", "/snap", "/target"]).unwrap();
        assert!(matches!(list.command, Command::List { .. }));
        let stats = Cli::try_parse_from(["mybtrfs", "stats", "/snap", "/target"]).unwrap();
        assert!(matches!(stats.command, Command::Stats { .. }));
        // Each requires both directories.
        assert!(Cli::try_parse_from(["mybtrfs", "list", "/snap"]).is_err());
        assert!(Cli::try_parse_from(["mybtrfs", "stats", "/snap"]).is_err());
    }
}
