//! Driving adapter + composition root: parses the command set with clap, wires
//! the concrete adapters into the use cases, and dispatches. Paths are
//! canonicalized/validated here (decision ID-2) and deletions are logged here
//! (decision ID-1); the use cases trust their inputs. See `documentation/01`
//! (CLI surface) and `02` §3.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use mybtrfs_adapters::{BtrfsCliAdapter, SystemClock};
use mybtrfs_application::backup::{BackupService, RunReport};
use mybtrfs_application::ports::{DeleteCommit, DeletePort, PortError};
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
    /// Re-send an existing snapshot without creating a new one (Phase 2).
    Resume,
    /// Prune snapshots/backups per retention policy (Phase 3).
    Prune,
    /// Restore a backup to a writable subvolume (Phase 4).
    Restore,
    /// List subvolumes (Phase 3).
    List,
    /// Show backup statistics (Phase 3).
    Stats,
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
    // Deletions are logged at the composition root (decision ID-1).
    let deleter = LoggingDeletePort { inner: &btrfs };
    let retention = RetentionService::new(&clock, &deleter);
    let backup = BackupService::new(
        &clock,
        &btrfs,
        &btrfs,
        &btrfs,
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
        Command::Resume
        | Command::Prune
        | Command::Restore
        | Command::List
        | Command::Stats
        | Command::ListDrives => bail!("this command is not implemented yet"),
    }
}

/// Canonicalize and validate a path before handing it to the use cases
/// (decision ID-2): the result is absolute, symlink-resolved, and `..`-free, and
/// the path must already exist.
fn validate_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("invalid or missing path: {}", path.display()))
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
}
