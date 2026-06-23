//! Driving adapter + composition root: parses the command set with clap, wires
//! the concrete adapters into the use cases, and dispatches. Paths are
//! canonicalized/validated here (decision ID-2) and deletions are logged here
//! (decision ID-1); the use cases trust their inputs. See `documentation/01`
//! (CLI surface) and `02` §3.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use mybtrfs_adapters::{
    AutoPrompter, BtrfsCliAdapter, LocalFsAdapter, LsblkDriveDiscovery, StdioPrompter, SystemClock,
};
use mybtrfs_application::backup::{BackupService, ResumeReport, RunReport};
use mybtrfs_application::inventory::{Inventory, InventoryService, Stats};
use mybtrfs_application::ports::{
    DeleteCommit, DeletePort, DiscoveredFilesystem, DriveDiscoveryPort, FilesystemPort, PortError,
    Prompter,
};
use mybtrfs_application::prune::{PruneReport, PruneService};
use mybtrfs_application::restore::{RestoreReport, RestoreService};
use mybtrfs_application::retention::RetentionService;
use mybtrfs_domain::naming::TimestampFormat;
use mybtrfs_domain::parent::Incremental;
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
    /// Assume "yes" to confirmations (e.g. creating missing directories) — for
    /// non-interactive / cron use.
    #[arg(long, global = true)]
    yes: bool,
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
        /// `btrfs send -p` strategy.
        #[arg(long, value_enum, default_value_t = IncrementalArg::Yes)]
        incremental: IncrementalArg,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Re-send the latest not-yet-backed-up snapshot without creating a new one.
    Resume {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Base name of the snapshot series to resume.
        basename: String,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
        /// `btrfs send -p` strategy.
        #[arg(long, value_enum, default_value_t = IncrementalArg::Yes)]
        incremental: IncrementalArg,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Prune snapshots/backups per retention policy (no snapshot, no transfer).
    Prune {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem.
        target_dir: PathBuf,
        /// Show what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Restore a backup to a writable subvolume at `dest` (Phase 4).
    Restore {
        /// The backup: a read-only subvolume on the destination filesystem.
        backup: PathBuf,
        /// Where to create the writable restored subvolume.
        dest: PathBuf,
        /// If `dest` exists, move it aside to a non-colliding `<dest>.broken[.N]`
        /// instead of refusing.
        #[arg(long)]
        force: bool,
        /// Show the intended plan (move-aside + writable snapshot) without
        /// creating, moving, or deleting anything.
        #[arg(long)]
        dry_run: bool,
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

/// `btrfs send -p` strategy, mirroring [`Incremental`]; defaults to `yes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum IncrementalArg {
    /// Use a parent when one exists, else fall back to a full send.
    Yes,
    /// Require a related parent; never fall back to a full send.
    Strict,
    /// Always send full (no `-p`).
    No,
}

impl From<IncrementalArg> for Incremental {
    fn from(arg: IncrementalArg) -> Self {
        match arg {
            IncrementalArg::Yes => Self::Yes,
            IncrementalArg::Strict => Self::Strict,
            IncrementalArg::No => Self::No,
        }
    }
}

/// btrbk-style retention flags for a backup `run`/`resume`. The defaults
/// (`preserve_min all`, no tiers) keep everything — retention only prunes once
/// the caller opts into a schedule. See `documentation/01` and `domain::retention`.
#[derive(Args, Debug)]
struct RetentionArgs {
    /// Snapshot-side minimum-keep floor (`all` / `latest` / `no` / `<count><h|d|w|m|y>`).
    #[arg(long, default_value = "all")]
    snapshot_preserve_min: String,
    /// Snapshot-side tier schedule (e.g. `"24h 7d 4w 6m 5y"`; empty/`no` = no tiers).
    #[arg(long, default_value = "")]
    snapshot_preserve: String,
    /// Target-side minimum-keep floor (`all` / `latest` / `no` / `<count><h|d|w|m|y>`).
    #[arg(long, default_value = "all")]
    target_preserve_min: String,
    /// Target-side tier schedule (e.g. `"24h 7d 4w 6m 5y"`; empty/`no` = no tiers).
    #[arg(long, default_value = "")]
    target_preserve: String,
}

impl RetentionArgs {
    /// Parse the snapshot-side policy from its btrbk-style strings.
    fn snapshot_policy(&self) -> Result<RetentionPolicy> {
        parse_policy(&self.snapshot_preserve_min, &self.snapshot_preserve)
    }

    /// Parse the target-side policy from its btrbk-style strings.
    fn target_policy(&self) -> Result<RetentionPolicy> {
        parse_policy(&self.target_preserve_min, &self.target_preserve)
    }
}

/// Parse a single retention policy, mapping a malformed spec to a user-facing error.
fn parse_policy(preserve_min: &str, preserve: &str) -> Result<RetentionPolicy> {
    RetentionPolicy::parse(preserve_min, preserve).with_context(|| {
        format!("invalid retention policy (preserve_min={preserve_min:?}, preserve={preserve:?})")
    })
}

/// Parse the command line and run; the returned exit code reflects success.
#[must_use]
pub fn run() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(exit_code::FAILURE)
        }
    }
}

/// Wire the adapters into the use cases and execute the parsed command.
fn dispatch(cli: &Cli) -> Result<()> {
    let clock = SystemClock;
    let btrfs = BtrfsCliAdapter::new();
    let localfs = LocalFsAdapter::new();
    // `--yes` swaps the interactive prompter for the auto-confirming one.
    let prompter: Box<dyn Prompter> = if cli.yes {
        Box::new(AutoPrompter)
    } else {
        Box::new(StdioPrompter::new())
    };
    // The committing retention service deletes through `btrfs`, but logs each
    // deletion here so partial progress is visible if a fail-fast prune aborts
    // mid-loop (decision ID-1). The dry-run path keeps its own `DryRunDeletePort`.
    let logging_deleter = LoggingDeletePort::new(&btrfs);
    let retention = RetentionService::new(&clock, &logging_deleter);
    // One resolve-per-path adapter serves as both source and target repository
    // (it resolves each path's filesystem), plus the snapshot and transfer ports.
    // The incremental mode is per-command, so build the service where it's used.
    let backup_with = |incremental: Incremental| {
        BackupService::with_incremental(
            &clock,
            &btrfs, // source_repo
            &btrfs, // target_repo
            &btrfs, // snapshots
            &btrfs, // transfer
            &retention,
            TimestampFormat::Long,
            incremental,
        )
    };

    match &cli.command {
        Command::Snapshot {
            source,
            snapshot_dir,
            basename,
        } => {
            let source = validate_path(source)?;
            ensure_dir(&localfs, prompter.as_ref(), snapshot_dir)?;
            let snapshot_dir = validate_path(snapshot_dir)?;
            // Snapshotting doesn't send, so the incremental mode is irrelevant.
            let snapshot = backup_with(Incremental::Yes)
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
            incremental,
            retention: retention_args,
        } => {
            let source = validate_path(source)?;
            ensure_dir(&localfs, prompter.as_ref(), snapshot_dir)?;
            ensure_dir(&localfs, prompter.as_ref(), target_dir)?;
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            let snapshot_policy = retention_args.snapshot_policy()?;
            let target_policy = retention_args.target_policy()?;
            let report = backup_with((*incremental).into())
                .run(
                    &source,
                    &snapshot_dir,
                    basename,
                    &target_dir,
                    &snapshot_policy,
                    &target_policy,
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
            dry_run,
        } => {
            let backup = validate_path(backup)?;
            let dest = validate_new_path(dest)?;
            let report = RestoreService::new(&btrfs, &localfs)
                .restore(&backup, &dest, *force, *dry_run)
                .context("restore failed")?;
            print_restore_report(&report);
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
            incremental,
            retention: retention_args,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            let snapshot_policy = retention_args.snapshot_policy()?;
            let target_policy = retention_args.target_policy()?;
            let report = backup_with((*incremental).into())
                .resume(
                    &snapshot_dir,
                    basename,
                    &target_dir,
                    &snapshot_policy,
                    &target_policy,
                )
                .context("resume failed")?;
            print_resume_report(&report);
            Ok(())
        }
        Command::Prune {
            snapshot_dir,
            target_dir,
            dry_run,
            retention: retention_args,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            let snapshot_policy = retention_args.snapshot_policy()?;
            let target_policy = retention_args.target_policy()?;
            // On a dry run, swap in a delete port that only reports (no deletion).
            // The returned schedule still lists what *would* be deleted.
            let report = if *dry_run {
                let deleter = DryRunDeletePort;
                let dry_retention = RetentionService::new(&clock, &deleter);
                prune_with(
                    &btrfs,
                    &dry_retention,
                    &snapshot_dir,
                    &target_dir,
                    &snapshot_policy,
                    &target_policy,
                )
            } else {
                prune_with(
                    &btrfs,
                    &retention,
                    &snapshot_dir,
                    &target_dir,
                    &snapshot_policy,
                    &target_policy,
                )
            }
            .context("prune failed")?;
            print_prune_report(&report, *dry_run);
            Ok(())
        }
    }
}

/// Run a standalone prune via [`PruneService`] over the given retention service.
/// Factored out so the dry-run and committing paths share one call site while
/// each supplies its own delete port (via `retention`). One resolve-per-path
/// `btrfs` adapter serves as both source and target repository.
fn prune_with(
    btrfs: &BtrfsCliAdapter,
    retention: &RetentionService<'_>,
    snapshot_dir: &Path,
    target_dir: &Path,
    snapshot_policy: &RetentionPolicy,
    target_policy: &RetentionPolicy,
) -> Result<PruneReport, PortError> {
    PruneService::new(btrfs, btrfs, retention).prune(
        snapshot_dir,
        target_dir,
        snapshot_policy,
        target_policy,
    )
}

/// Ensure `dir` exists, creating it (and any missing parents) after confirmation
/// when it does not — interactively, or automatically under `--yes`. Declining
/// the prompt is an error: a missing directory must never silently spawn a stray
/// backup tree, so creation is always gated on an explicit yes (decision ID-2).
/// Used only by the commands that *write* into a directory (`run`/`snapshot`);
/// read-only commands keep [`validate_path`] (a missing dir there is just an error).
fn ensure_dir(fs: &dyn FilesystemPort, prompter: &dyn Prompter, dir: &Path) -> Result<()> {
    if fs.exists(dir)? {
        return Ok(());
    }
    let create = prompter.confirm(&format!(
        "Directory does not exist: {}. Create it?",
        dir.display()
    ))?;
    if !create {
        bail!(
            "directory does not exist (declined to create): {}",
            dir.display()
        );
    }
    fs.create_dir_all(dir)?;
    Ok(())
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

/// Print a one-fact-per-line summary of a standalone prune. On a dry run the
/// counts describe what *would* be deleted (the per-path lines came from the
/// dry-run delete port).
fn print_prune_report(report: &PruneReport, dry_run: bool) {
    let verb = if dry_run { "would prune" } else { "pruned" };
    println!(
        "{verb} {} snapshot(s), {} backup(s)",
        report.snapshots_pruned.delete.len(),
        report.backups_pruned.delete.len()
    );
}

/// Print a one-fact-per-line summary of a restore. On a dry run the lines
/// describe the intended plan (prefixed `would`) and nothing was changed.
fn print_restore_report(report: &RestoreReport) {
    if report.dry_run {
        if let Some(moved) = &report.moved_aside {
            println!(
                "would move aside existing destination to: {}",
                moved.display()
            );
        }
        println!("would restore to: {}", report.dest.display());
        return;
    }
    match &report.restored {
        Some(restored) => println!(
            "restored: {}",
            restored.mountpoint.join(&restored.path).display()
        ),
        None => println!("restored: {}", report.dest.display()),
    }
    if let Some(moved) = &report.moved_aside {
        println!("moved aside existing destination to: {}", moved.display());
    }
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

/// A [`DeletePort`] for `--dry-run`: reports each would-be deletion and deletes
/// nothing. The returned retention schedule still lists what *would* go, so the
/// caller's counts are accurate.
struct DryRunDeletePort;

impl DeletePort for DryRunDeletePort {
    fn delete(&self, path: &Path, _commit: DeleteCommit) -> Result<(), PortError> {
        println!("would delete: {}", path.display());
        Ok(())
    }
}

/// A [`DeletePort`] decorator that logs every committing deletion at `info`
/// (and any failure at `error`) before/while delegating to the wrapped port
/// (decision ID-1). Wired as the deleter for the committing `RetentionService`
/// so partial progress is visible at the default `info` level when a fail-fast
/// prune aborts mid-loop — otherwise the destructive path would be silent while
/// the safe dry-run path is verbose.
struct LoggingDeletePort<'a> {
    inner: &'a dyn DeletePort,
}

impl<'a> LoggingDeletePort<'a> {
    /// Wrap `inner`, logging each deletion before delegating to it.
    fn new(inner: &'a dyn DeletePort) -> Self {
        Self { inner }
    }
}

impl DeletePort for LoggingDeletePort<'_> {
    fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
        log::info!("deleting: {}", path.display());
        self.inner.delete(path, commit).inspect_err(|err| {
            log::error!("delete failed: {} ({err})", path.display());
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::cell::RefCell;

    /// A `FilesystemPort` with a fixed `exists` answer that records create_dir_all.
    struct FakeFs {
        exists: bool,
        created: RefCell<Vec<PathBuf>>,
    }
    impl FakeFs {
        fn new(exists: bool) -> Self {
            Self {
                exists,
                created: RefCell::new(Vec::new()),
            }
        }
    }
    impl FilesystemPort for FakeFs {
        fn exists(&self, _path: &Path) -> Result<bool, PortError> {
            Ok(self.exists)
        }
        fn create_dir_all(&self, path: &Path) -> Result<(), PortError> {
            self.created.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn rename(&self, _from: &Path, _to: &Path) -> Result<(), PortError> {
            unimplemented!("not exercised by these tests")
        }
    }

    /// A `Prompter` that confirms (or not) without reading stdin.
    struct FixedPrompter(bool);
    impl Prompter for FixedPrompter {
        fn confirm(&self, _prompt: &str) -> Result<bool, PortError> {
            Ok(self.0)
        }
        fn choose(&self, _prompt: &str, _options: &[String]) -> Result<Option<usize>, PortError> {
            Ok(None)
        }
    }

    #[test]
    fn ensure_dir_is_a_noop_when_the_directory_exists() {
        let fs = FakeFs::new(true);
        // Would decline if asked — but an existing dir is never prompted.
        assert!(ensure_dir(&fs, &FixedPrompter(false), Path::new("/snap")).is_ok());
        assert!(fs.created.borrow().is_empty());
    }

    #[test]
    fn ensure_dir_creates_after_confirmation() {
        let fs = FakeFs::new(false);
        assert!(ensure_dir(&fs, &FixedPrompter(true), Path::new("/snap")).is_ok());
        assert_eq!(*fs.created.borrow(), vec![PathBuf::from("/snap")]);
    }

    #[test]
    fn ensure_dir_errors_and_creates_nothing_when_declined() {
        let fs = FakeFs::new(false);
        assert!(ensure_dir(&fs, &FixedPrompter(false), Path::new("/snap")).is_err());
        assert!(fs.created.borrow().is_empty());
    }

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
                incremental,
                retention,
            } => {
                assert_eq!(source, PathBuf::from("/mnt/pool/home"));
                assert_eq!(snapshot_dir, PathBuf::from("/mnt/pool/.snapshots"));
                assert_eq!(basename, "home");
                assert_eq!(target_dir, PathBuf::from("/mnt/drive/host"));
                // Defaults: incremental on, keep-all retention.
                assert_eq!(incremental, IncrementalArg::Yes);
                assert_eq!(retention.snapshot_preserve_min, "all");
                assert_eq!(retention.snapshot_preserve, "");
            }
            _ => panic!("expected a Run command"),
        }
    }

    #[test]
    fn run_accepts_incremental_and_retention_flags() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "run",
            "/mnt/pool/home",
            "/mnt/pool/.snapshots",
            "home",
            "/mnt/drive/host",
            "--incremental",
            "strict",
            "--snapshot-preserve",
            "24h 7d 4w",
            "--target-preserve-min",
            "latest",
        ])
        .unwrap();
        let Command::Run {
            incremental,
            retention,
            ..
        } = cli.command
        else {
            panic!("expected a Run command");
        };
        assert_eq!(incremental, IncrementalArg::Strict);
        assert_eq!(Incremental::from(incremental), Incremental::Strict);
        assert_eq!(retention.snapshot_preserve, "24h 7d 4w");
        assert_eq!(retention.target_preserve_min, "latest");
        // The retention strings parse into real policies.
        assert!(retention.snapshot_policy().is_ok());
        assert!(retention.target_policy().is_ok());
    }

    #[test]
    fn rejects_a_malformed_retention_spec() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "run",
            "/s",
            "/d",
            "home",
            "/t",
            "--snapshot-preserve",
            "7x",
        ])
        .unwrap();
        let Command::Run { retention, .. } = cli.command else {
            panic!("expected a Run command");
        };
        // `7x` is not a valid tier unit → parsing the policy is an error.
        assert!(retention.snapshot_policy().is_err());
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
                incremental,
                ..
            } => {
                assert_eq!(snapshot_dir, PathBuf::from("/mnt/pool/.snapshots"));
                assert_eq!(basename, "home");
                assert_eq!(target_dir, PathBuf::from("/mnt/drive/host"));
                assert_eq!(incremental, IncrementalArg::Yes);
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

    #[test]
    fn parses_prune_command_with_dry_run_and_retention() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "prune",
            "/snap",
            "/target",
            "--dry-run",
            "--snapshot-preserve",
            "7d 4w",
        ])
        .unwrap();
        let Command::Prune {
            snapshot_dir,
            target_dir,
            dry_run,
            retention,
        } = cli.command
        else {
            panic!("expected a Prune command");
        };
        assert_eq!(snapshot_dir, PathBuf::from("/snap"));
        assert_eq!(target_dir, PathBuf::from("/target"));
        assert!(dry_run);
        assert_eq!(retention.snapshot_preserve, "7d 4w");
        assert!(retention.snapshot_policy().is_ok());
        // dry_run defaults off and both directories are required.
        let plain = Cli::try_parse_from(["mybtrfs", "prune", "/snap", "/target"]).unwrap();
        assert!(matches!(
            plain.command,
            Command::Prune { dry_run: false, .. }
        ));
        assert!(Cli::try_parse_from(["mybtrfs", "prune", "/snap"]).is_err());
    }

    #[test]
    fn parses_restore_command_with_force_and_dry_run() {
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "restore",
            "/mnt/drive/host/home.20240102T1531",
            "/mnt/pool/home_restored",
            "--force",
            "--dry-run",
        ])
        .unwrap();
        let Command::Restore {
            backup,
            dest,
            force,
            dry_run,
        } = cli.command
        else {
            panic!("expected a Restore command");
        };
        assert_eq!(backup, PathBuf::from("/mnt/drive/host/home.20240102T1531"));
        assert_eq!(dest, PathBuf::from("/mnt/pool/home_restored"));
        assert!(force);
        assert!(dry_run);
        // Both flags default off and the two positional paths are required.
        let plain = Cli::try_parse_from(["mybtrfs", "restore", "/backup", "/dest"]).unwrap();
        assert!(matches!(
            plain.command,
            Command::Restore {
                force: false,
                dry_run: false,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["mybtrfs", "restore", "/backup"]).is_err());
    }
}
