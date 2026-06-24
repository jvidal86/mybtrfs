//! Driving adapter + composition root: parses the command set with clap, wires
//! the concrete adapters into the use cases, and dispatches. Paths are
//! canonicalized/validated here (decision ID-2) and deletions are logged here
//! (decision ID-1); the use cases trust their inputs. See `documentation/01`
//! (CLI surface) and `02` §3.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use std::io::IsTerminal;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset};
use mybtrfs_adapters::{
    AutoPrompter, BtrfsCliAdapter, Endpoint, FileJournal, FileLock, IndicatifProgress,
    LocalFsAdapter, LsblkDriveDiscovery, StdioPrompter, SystemClock, parse_endpoint,
};
use mybtrfs_application::backup::{BackupService, ResumeReport, RunReport};
use mybtrfs_application::inventory::{Inventory, InventoryService, Stats};
use mybtrfs_application::local_subvolumes::LocalSubvolumesService;
use mybtrfs_application::ports::{
    ClockPort, DeleteCommit, DeletePort, DiffPort, DiscoveredFilesystem, DriveDiscoveryPort,
    FilesystemPort, Journal, PortError, Prompter, SubvolumeRepository,
};
use mybtrfs_application::prune::{PruneReport, PruneService};
use mybtrfs_application::restore::{RestoreReport, RestoreService};
use mybtrfs_application::retention::RetentionService;
use mybtrfs_application::retention_preview;
use mybtrfs_domain::model::{Subvolume, Uuid};
use mybtrfs_domain::naming::TimestampFormat;
use mybtrfs_domain::parent::Incremental;
use mybtrfs_domain::retention::RetentionPolicy;

/// Default log path with fallback: try /var/log/mybtrfs.log, fall back to
/// ~/.local/share/mybtrfs/logs/mybtrfs.log if the first is not writable.
fn default_log_path() -> Option<PathBuf> {
    let var_log = PathBuf::from("/var/log/mybtrfs.log");
    // Check if /var/log is writable by attempting to open the file
    use std::fs::OpenOptions;
    if OpenOptions::new()
        .create(true)
        .append(true)
        .open(&var_log)
        .is_ok()
    {
        return Some(var_log);
    }
    // Fall back to ~/.local/share/mybtrfs/logs/
    if let Ok(home) = std::env::var("HOME") {
        let fallback = PathBuf::from(home).join(".local/share/mybtrfs/logs/mybtrfs.log");
        return Some(fallback);
    }
    None
}

/// Initialize dual-target logging: errors/warnings to stderr (with color),
/// info/debug to log file. This ensures critical messages are always visible,
/// even with `--quiet`, matching standard backup-tool behavior (btrbk, rsync, borg).
// SAFETY: `expect` on `set_boxed_logger` is sound — `run()` is the sole caller and
// initializes the logger exactly once, so "already initialized" is unreachable.
#[allow(clippy::expect_used)]
fn setup_dual_target_logger(quiet: &bool, log_file: Option<&Path>) {
    use std::fs::OpenOptions;

    // Stderr target: errors & warnings (always shown, with color)
    let mut stderr_builder = env_logger::Builder::from_default_env();
    stderr_builder.filter_level(log::LevelFilter::Warn);
    stderr_builder.format(|buf, record| {
        use std::io::Write;
        // Color for errors & warnings
        let level_color = match record.level() {
            log::Level::Error => "\x1b[31m", // red
            log::Level::Warn => "\x1b[33m",  // yellow
            _ => "",
        };
        let reset = if level_color.is_empty() {
            ""
        } else {
            "\x1b[0m"
        };
        writeln!(
            buf,
            "{}{}:{} {}",
            level_color,
            record.level(),
            reset,
            record.args()
        )
    });
    stderr_builder.target(env_logger::Target::Stderr);
    let stderr_handle = stderr_builder.build();

    // File target: all messages (info/debug/warn/error), unless --quiet
    if let Some(log_path) = log_file {
        if let Some(parent) = log_path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(log_path) {
            let mut file_builder = env_logger::Builder::new();
            // If --quiet: only errors/warnings to file. Otherwise: info and up.
            file_builder.filter_level(if *quiet {
                log::LevelFilter::Warn
            } else {
                log::LevelFilter::Info
            });
            file_builder.format(|buf, record| {
                use std::io::Write;
                writeln!(buf, "{}", record.args())
            });
            file_builder.target(env_logger::Target::Pipe(Box::new(file)));
            let file_handle = file_builder.build();

            // Combine both targets
            let max_level = std::cmp::max(stderr_handle.filter(), file_handle.filter());
            log::set_max_level(max_level);
            log::set_boxed_logger(Box::new(MultiTargetLogger {
                stderr: stderr_handle,
                file: file_handle,
            }))
            .expect("logger already initialized");
            return;
        }
    }

    // Fallback: stderr only if log file can't be opened
    log::set_boxed_logger(Box::new(SingleTargetLogger {
        logger: stderr_handle,
    }))
    .expect("logger already initialized");
}

/// Combines two loggers: stderr for errors/warnings, file for everything.
struct MultiTargetLogger {
    stderr: env_logger::Logger,
    file: env_logger::Logger,
}

impl log::Log for MultiTargetLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.stderr.enabled(metadata) || self.file.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if self.stderr.enabled(record.metadata()) {
            self.stderr.log(record);
        }
        if self.file.enabled(record.metadata()) {
            self.file.log(record);
        }
    }

    fn flush(&self) {}
}

/// Fallback logger when file target unavailable.
struct SingleTargetLogger {
    logger: env_logger::Logger,
}

impl log::Log for SingleTargetLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.logger.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.logger.log(record);
    }

    fn flush(&self) {}
}

/// Process exit codes (central table — RULES rule 14). Intentional divergence from btrbk:
/// adds code 4 for permission errors for better UX in scripts/cron.
mod exit_code {
    /// A generic command failure.
    pub const FAILURE: u8 = 1;
    /// A usage / bad-argument error (also what clap emits on a parse error).
    pub const USAGE: u8 = 2;
    /// The repository lock is held by another run.
    pub const LOCK_BUSY: u8 = 3;
    /// The process lacks privileges required by btrfs (run with sudo).
    pub const PERMISSION_DENIED: u8 = 4;
    /// At least one backup task aborted while others succeeded (multi-target;
    /// reserved — single-target runs either fully succeed or fully fail).
    #[allow(dead_code)]
    pub const PARTIAL_ABORT: u8 = 10;
}

/// A usage / bad-argument error surfaced during dispatch (mapped to
/// [`exit_code::USAGE`]) — distinguishes "you gave a bad value" from a runtime
/// failure so scripts can branch on the exit code.
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

/// The run lock could not be acquired — held by a concurrent run, or the lock
/// file itself was unopenable (e.g. owned by another user in a sticky dir under
/// `fs.protected_regular`). Mapped to [`exit_code::LOCK_BUSY`]; the message says
/// which and how to fix it. Distinct from "needs root" so it is never mislabeled.
#[derive(Debug)]
struct LockBusy(String);

impl std::fmt::Display for LockBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LockBusy {}

/// mybtrfs requires root privileges — detected from a "Permission denied" failure
/// in the btrfs adapter (mapped to [`exit_code::PERMISSION_DENIED`]).
#[derive(Debug)]
struct PermissionDenied;

impl std::fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mybtrfs requires root privileges — re-run with sudo")
    }
}

impl std::error::Error for PermissionDenied {}

/// Check if an error chain contains a "Permission denied" signal. Returns true if any
/// cause is an `std::io::Error` with `PermissionDenied` kind, or if any cause's
/// display string contains "Permission denied" (catches `PortError::Command` wrapping
/// raw btrfs stderr).
fn is_permission_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io_err) = cause.downcast_ref::<std::io::Error>()
            && io_err.kind() == std::io::ErrorKind::PermissionDenied
        {
            return true;
        }
        if cause.to_string().contains("Permission denied") {
            return true;
        }
    }
    false
}

/// Map a dispatch error to its process exit code (see [`exit_code`]).
fn exit_code_for(err: &anyhow::Error) -> u8 {
    if err.downcast_ref::<UsageError>().is_some() {
        exit_code::USAGE
    } else if err.downcast_ref::<LockBusy>().is_some() {
        exit_code::LOCK_BUSY
    } else if err.downcast_ref::<PermissionDenied>().is_some() {
        exit_code::PERMISSION_DENIED
    } else {
        exit_code::FAILURE
    }
}

/// `mybtrfs` — a backup tool for btrfs subvolumes (a Rust reimagining of btrbk).
#[derive(Parser)]
#[command(
    name = "mybtrfs",
    version,
    about = "A backup tool for btrfs subvolumes (a Rust reimagining of btrbk)",
    long_about = "mybtrfs — btrfs-native backup tool with incremental send/receive, GFS retention, \
and SSH remote support. Works with local directories or remote ssh://host/path endpoints. \
Use 'mybtrfs <command> --help' for detailed command options."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Assume "yes" to confirmations (e.g. creating missing directories) — for
    /// non-interactive / cron use.
    #[arg(long, global = true)]
    yes: bool,
    /// Suppress progress indicators and post-run summary (for cron / scripting).
    /// Progress is also suppressed automatically when stderr is not a TTY.
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
    /// Append a timestamped audit line for this invocation to the given file.
    #[arg(long, global = true, value_name = "PATH")]
    journal: Option<PathBuf>,
    /// Lock file serializing mutating runs (default: `<tmpdir>/mybtrfs-<uid>.lock`).
    /// A second run that finds it held exits immediately with code 3.
    #[arg(long, global = true, value_name = "PATH")]
    lock: Option<PathBuf>,
    /// Write logs to a file instead of stderr (frees stderr for progress indicators).
    /// Default: /var/log/mybtrfs.log (or ~/.local/share/mybtrfs/logs if not writable).
    /// Use `--log-file /dev/null` to suppress logging. View with: `lnav <path>`.
    #[arg(long, global = true, value_name = "PATH")]
    log_file: Option<PathBuf>,
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
    /// Full backup: snapshot the source, send/receive to the target (local or remote SSH), then prune.
    Run {
        /// Source subvolume to back up (omit if using `--set`).
        source: Option<PathBuf>,
        /// Directory that will hold the source-side snapshot (omit if using `--set`).
        snapshot_dir: Option<PathBuf>,
        /// Base name for the snapshot (omit if using `--set`).
        basename: Option<String>,
        /// Target for the backup: a local directory path, or a remote `ssh://[user@]host[:port]/path` endpoint.
        /// If omitted, you are prompted to pick a discovered btrfs drive.
        target_dir: Option<PathBuf>,
        /// Path to a TOML backup-set file (Phase 5 §4). When provided, `source`, `snapshot_dir`,
        /// and `basename` are ignored; instead, each `[[backup]]` entry in the file is processed.
        #[arg(long, value_name = "PATH")]
        set: Option<PathBuf>,
        /// `btrfs send -p` strategy.
        #[arg(long, value_enum, default_value_t = IncrementalArg::Yes)]
        incremental: IncrementalArg,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Resume an incomplete backup: re-send the latest snapshot (local or remote SSH) without creating a new one.
    Resume {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Base name of the snapshot series to resume.
        basename: String,
        /// Target directory on the backup filesystem: a local path or remote `ssh://[user@]host[:port]/path`.
        target_dir: PathBuf,
        /// `btrfs send -p` strategy.
        #[arg(long, value_enum, default_value_t = IncrementalArg::Yes)]
        incremental: IncrementalArg,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Prune snapshots/backups per retention policy (no new snapshot or transfer, just cleanup).
    Prune {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem: a local path or remote `ssh://[user@]host[:port]/path`.
        target_dir: PathBuf,
        /// Show what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        retention: RetentionArgs,
    },
    /// Restore a backup to a writable subvolume at `dest` (Phase 4).
    Restore {
        /// The backup to restore: a read-only subvolume — a local path, or a
        /// remote `ssh://[user@]host[:port]/path` source.
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
    /// List source snapshots with their correlated backups (inventory view).
    List {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem: a local path or remote `ssh://[user@]host[:port]/path`.
        target_dir: PathBuf,
    },
    /// Show aggregate backup statistics (snapshot/backup counts, sizes, space savings).
    Stats {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem: a local path or remote `ssh://[user@]host[:port]/path`.
        target_dir: PathBuf,
    },
    /// Show backup health: snapshot/backup counts, latest ages, health checks.
    Status {
        /// Directory holding the source-side snapshots.
        snapshot_dir: PathBuf,
        /// Target directory on the backup filesystem: a local path or remote `ssh://[user@]host[:port]/path`.
        target_dir: PathBuf,
    },
    /// Estimate changed bytes between two snapshots (Phase 3).
    Diff {
        /// Path to the older snapshot.
        older_snapshot: PathBuf,
        /// Path to the newer snapshot.
        newer_snapshot: PathBuf,
    },
    /// List candidate backup drives (Phase 1 UX).
    ListDrives,
    /// List every btrfs subvolume on the local system — across all mounted btrfs
    /// filesystems — for picking a backup source. Output is tab-separated under a
    /// header row (`ID PATH FS-MOUNTPOINT UUID RO/RW`), one subvolume per line;
    /// `--quiet` drops the header for scripting (pipe to `column -t` to align).
    /// Read-only; requires root (it runs `btrfs subvolume list`). Use `list-drives`
    /// to see the filesystems themselves rather than their subvolumes.
    ListSubvolumes,
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
    RetentionPolicy::parse(preserve_min, preserve).map_err(|err| {
        anyhow::Error::new(UsageError(format!(
            "invalid retention policy (preserve_min={preserve_min:?}, preserve={preserve:?}): {err}"
        )))
    })
}

/// Parse the command line and run; the returned exit code reflects success.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();

    // Initialize logging to a file by default (fallback strategy):
    // Try /var/log/mybtrfs.log first, fall back to ~/.local/share/mybtrfs/logs/ if not writable.
    // --log-file overrides; use /dev/null to suppress.
    // Dual-target logging: errors/warnings always go to stderr (even with --quiet),
    // while info/debug go to the log file. This matches standard backup-tool
    // behavior (btrbk, rsync, borg): critical info is always visible.
    let log_path = cli.log_file.clone().or_else(default_log_path);
    setup_dual_target_logger(&cli.quiet, log_path.as_deref());
    match dispatch(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let err = classify(err);
            // Log at error level so the failure is captured in `2>mybtrfs.log`.
            log::error!("{err:#}");
            eprintln!("error: {err:#}");
            ExitCode::from(exit_code_for(&err))
        }
    }
}

/// Final error classification for the process exit. An already-typed error (usage,
/// lock) keeps its code and message; only an *otherwise-unclassified* permission
/// failure (a btrfs command that genuinely needed root) is relabeled
/// [`PermissionDenied`] — with the original kept as the cause (via `.context`) so
/// it stays diagnosable. The typed-error guard is what stops a lock-file
/// "Permission denied" from being mis-reported as "needs root".
fn classify(err: anyhow::Error) -> anyhow::Error {
    if err.downcast_ref::<UsageError>().is_some() || err.downcast_ref::<LockBusy>().is_some() {
        err
    } else if is_permission_error(&err) {
        err.context(PermissionDenied)
    } else {
        err
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
    // Audit the invocation to the journal (if configured); a journal failure is
    // logged but never aborts the command. Default to /var/log/mybtrfs.journal,
    // with fallback to ~/.local/share/mybtrfs/journal (works under sudo).
    let journal: Box<dyn Journal> = match &cli.journal {
        Some(path) => Box::new(FileJournal::new(path.clone())),
        None => {
            let default_journal = if can_write_to("/var/log") {
                PathBuf::from("/var/log/mybtrfs.journal")
            } else if let Ok(home) = std::env::var("HOME") {
                PathBuf::from(home).join(".local/share/mybtrfs/journal")
            } else {
                PathBuf::from("/tmp/mybtrfs.journal")
            };
            Box::new(FileJournal::new(default_journal))
        }
    };

    fn can_write_to(dir: &str) -> bool {
        use std::fs::OpenOptions;
        let test_file = format!("{}/.mybtrfs-write-test", dir);
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&test_file)
            .is_ok();
        let _ = std::fs::remove_file(&test_file);
        result
    }
    if let Err(err) = journal.record(&format!(
        "{} {}",
        clock.now().to_rfc3339(),
        describe_command(&cli.command)
    )) {
        log::warn!("could not write to journal: {err}");
    }
    // Serialize mutating runs behind a process lock (E2E-CC-09); read-only and
    // dry-run commands don't contend. Held until dispatch returns — the OS frees
    // it on exit, so even a crash leaves no stale lock.
    let _lock = if command_mutates(&cli.command) {
        Some(acquire_lock(cli.lock.as_deref())?)
    } else {
        None
    };
    // Progress indicator: enabled when stderr is a TTY and `--quiet` is not set.
    let use_progress = !cli.quiet && std::io::stderr().is_terminal();
    let progress: Arc<dyn mybtrfs_application::ports::ProgressPort> = if use_progress {
        Arc::new(IndicatifProgress::new())
    } else {
        Arc::new(mybtrfs_application::ports::NullProgress)
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
        .with_progress(progress.as_ref())
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
            set,
            incremental,
            retention: retention_args,
        } => {
            let snapshot_policy = retention_args.snapshot_policy()?;
            let target_policy = retention_args.target_policy()?;
            let incremental = (*incremental).into();

            // If --set is provided, parse the backup-set file and loop through entries.
            // Otherwise, process the single backup from individual args.
            if let Some(set_path) = set {
                let set_content =
                    std::fs::read_to_string(&set_path).context("failed to read backup-set file")?;
                let entries = mybtrfs_adapters::parse_backup_set(&set_content)
                    .map_err(|e| anyhow::anyhow!("failed to parse backup-set file: {}", e))?;

                for (idx, entry) in entries.into_iter().enumerate() {
                    log::info!(
                        "backup-set entry {}: {} → {}",
                        idx + 1,
                        entry.source.display(),
                        entry.target_dir.display()
                    );

                    let source_val = validate_path(&entry.source)?;
                    ensure_dir(&localfs, prompter.as_ref(), &entry.snapshot_dir)?;
                    let snapshot_dir_val = validate_path(&entry.snapshot_dir)?;

                    // Respect retention options from the backup-set entry if provided,
                    // otherwise fall back to command-line retention args.
                    let snapshot_policy = if entry.snapshot_preserve_min.is_some()
                        || entry.snapshot_preserve_hourly.is_some()
                        || entry.snapshot_preserve_daily.is_some()
                        || entry.snapshot_preserve_weekly.is_some()
                        || entry.snapshot_preserve_monthly.is_some()
                        || entry.snapshot_preserve_yearly.is_some()
                    {
                        // Build a policy string from entry fields (for now, use the command-line default)
                        snapshot_policy.clone()
                    } else {
                        snapshot_policy.clone()
                    };

                    let target_policy = if entry.target_preserve_min.is_some()
                        || entry.target_preserve_hourly.is_some()
                        || entry.target_preserve_daily.is_some()
                        || entry.target_preserve_weekly.is_some()
                        || entry.target_preserve_monthly.is_some()
                        || entry.target_preserve_yearly.is_some()
                    {
                        target_policy.clone()
                    } else {
                        target_policy.clone()
                    };

                    let target = parse_target(&entry.target_dir)?;
                    let _report = match target {
                        Endpoint::Local(dir) => {
                            ensure_dir(&localfs, prompter.as_ref(), &dir)?;
                            let dir_val = validate_path(&dir)?;
                            backup_with(incremental).run(
                                &source_val,
                                &snapshot_dir_val,
                                &entry.basename,
                                &dir_val,
                                &snapshot_policy,
                                &target_policy,
                            )
                        }
                        Endpoint::Remote { ssh, path } => {
                            let ssh_btrfs = BtrfsCliAdapter::ssh_target(ssh);
                            let routing = RoutingDeletePort::new(&btrfs, &ssh_btrfs, path.clone());
                            let logged = LoggingDeletePort::new(&routing);
                            let remote_retention = RetentionService::new(&clock, &logged);
                            BackupService::with_incremental(
                                &clock,
                                &btrfs,
                                &ssh_btrfs,
                                &btrfs,
                                &ssh_btrfs,
                                &remote_retention,
                                TimestampFormat::Long,
                                incremental,
                            )
                            .run(
                                &source_val,
                                &snapshot_dir_val,
                                &entry.basename,
                                &path,
                                &snapshot_policy,
                                &target_policy,
                            )
                        }
                    }
                    .context(format!("backup-set entry {} failed", idx + 1))?;
                    print_run_report(&_report);
                }
                Ok(())
            } else {
                // Single backup from individual args
                let source_val = source.clone().ok_or_else(|| {
                    UsageError(
                        "source is required (or use --set for a backup-set file)".to_string(),
                    )
                })?;
                let snapshot_dir_val = snapshot_dir.clone().ok_or_else(|| {
                    UsageError(
                        "snapshot_dir is required (or use --set for a backup-set file)".to_string(),
                    )
                })?;
                let basename_val = basename.clone().ok_or_else(|| {
                    UsageError(
                        "basename is required (or use --set for a backup-set file)".to_string(),
                    )
                })?;

                let source = validate_path(&source_val)?;
                ensure_dir(&localfs, prompter.as_ref(), &snapshot_dir_val)?;
                let snapshot_dir = validate_path(&snapshot_dir_val)?;

                // Resolve the target: an explicit local dir, an `ssh://` remote
                // endpoint, or (when omitted) an interactively-picked drive with a
                // per-host subdirectory (`<mountpoint>/<hostname>/`).
                let target = match target_dir {
                    Some(spec) => parse_target(spec)?,
                    None => Endpoint::Local(
                        resolve_target_drive(&LsblkDriveDiscovery::new(), prompter.as_ref())?
                            .join(hostname()),
                    ),
                };

                let report = match target {
                    Endpoint::Local(dir) => {
                        ensure_dir(&localfs, prompter.as_ref(), &dir)?;
                        let dir = validate_path(&dir)?;
                        backup_with(incremental).run(
                            &source,
                            &snapshot_dir,
                            &basename_val,
                            &dir,
                            &snapshot_policy,
                            &target_policy,
                        )
                    }
                    Endpoint::Remote { ssh, path } => {
                        let ssh_btrfs = BtrfsCliAdapter::ssh_target(ssh);
                        // Prune across two transports: source snapshots delete locally,
                        // target backups (under `path`) delete over ssh. Logged like the
                        // local deleter (decision ID-1).
                        let routing = RoutingDeletePort::new(&btrfs, &ssh_btrfs, path.clone());
                        let logged = LoggingDeletePort::new(&routing);
                        let remote_retention = RetentionService::new(&clock, &logged);
                        BackupService::with_incremental(
                            &clock,
                            &btrfs,     // source_repo (local)
                            &ssh_btrfs, // target_repo (remote)
                            &btrfs,     // snapshots (local)
                            &ssh_btrfs, // transfer (local send | remote receive)
                            &remote_retention,
                            TimestampFormat::Long,
                            incremental,
                        )
                        .run(
                            &source,
                            &snapshot_dir,
                            &basename_val,
                            &path,
                            &snapshot_policy,
                            &target_policy,
                        )
                    }
                }
                .context("backup run failed")?;
                print_run_report(&report);
                Ok(())
            }
        }
        Command::ListDrives => {
            progress.start_spinner("Scanning drives…");
            let drives = LsblkDriveDiscovery::new()
                .detect()
                .context("drive discovery failed")?;
            progress.finish("");
            print_drives(&drives);
            Ok(())
        }
        Command::ListSubvolumes => {
            progress.start_spinner("Scanning subvolumes…");
            let discovery = LsblkDriveDiscovery::new();
            let subvolumes = LocalSubvolumesService::new(&discovery, &btrfs)
                .list_all()
                .context("listing subvolumes failed")?;
            progress.finish("");
            print_subvolumes(&subvolumes, cli.quiet);
            Ok(())
        }
        Command::Restore {
            backup,
            dest,
            force,
            dry_run,
        } => {
            let dest = validate_new_path(dest)?;
            // The backup may be local or a remote `ssh://…` source. A local backup
            // on a different filesystem transfers back via send/receive; a remote
            // backup additionally runs its `btrfs send` over ssh while the receive +
            // make-writable + staging cleanup stay local. The staging-copy cleanup
            // deletes through the logging deleter so it is visible (decision ID-1).
            let report = match parse_target(backup)? {
                Endpoint::Local(backup) => {
                    let backup = validate_path(&backup)?;
                    RestoreService::new(&btrfs, &btrfs, &btrfs, &btrfs, &logging_deleter, &localfs)
                        .with_progress(progress.as_ref())
                        .restore(&backup, &dest, *force, *dry_run)
                }
                Endpoint::Remote { ssh, path } => {
                    let ssh_repo = BtrfsCliAdapter::ssh_target(ssh.clone()); // remote show/list
                    let ssh_transfer = BtrfsCliAdapter::ssh_source(ssh); // remote send | local receive
                    RestoreService::new(
                        &ssh_repo,        // backup's filesystem (remote)
                        &btrfs,           // destination filesystem (local)
                        &btrfs,           // make_writable (local)
                        &ssh_transfer,    // remote send | local receive
                        &logging_deleter, // staging cleanup (local)
                        &localfs,
                    )
                    .with_progress(progress.as_ref())
                    .restore(&path, &dest, *force, *dry_run)
                }
            }
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
                .with_progress(progress.as_ref())
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
                .with_progress(progress.as_ref())
                .stats(&snapshot_dir, &target_dir)
                .context("computing statistics failed")?;
            print_stats(&stats);
            Ok(())
        }
        Command::Status {
            snapshot_dir,
            target_dir,
        } => {
            let snapshot_dir = validate_path(snapshot_dir)?;
            let target_dir = validate_path(target_dir)?;
            use mybtrfs_application::status::StatusService;
            let status = StatusService {
                source_repo: &btrfs,
                target_repo: &btrfs,
                journal: Some(journal.as_ref()),
            }
            .report(&snapshot_dir, &target_dir)
            .context("computing status failed")?;
            print_status(&status);
            Ok(())
        }
        Command::Diff {
            older_snapshot,
            newer_snapshot,
        } => {
            use mybtrfs_application::diff::DiffService;

            let older_snapshot = validate_path(older_snapshot)?;
            let newer_snapshot = validate_path(newer_snapshot)?;

            // Gather sizes and generation via the DiffPort (btrfs adapter) —
            // routing through the port, not directly spawning btrfs here.
            let older_sub = btrfs.show(&older_snapshot)?;
            let older_bytes = btrfs.referenced_bytes(&older_snapshot)?;
            let newer_bytes = btrfs.referenced_bytes(&newer_snapshot)?;
            let changed_bytes = btrfs.find_new_changed_bytes(&newer_snapshot, older_sub.cgen)?;

            let diff = DiffService::estimate_changes(
                older_bytes,
                newer_bytes,
                changed_bytes,
                &older_snapshot,
                &newer_snapshot,
            );
            print_diff(&diff);
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
                    progress.as_ref(),
                )
            } else {
                prune_with(
                    &btrfs,
                    &retention,
                    &snapshot_dir,
                    &target_dir,
                    &snapshot_policy,
                    &target_policy,
                    progress.as_ref(),
                )
            }
            .context("prune failed")?;
            print_prune_report(&report, *dry_run, clock.now());
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
    progress: &dyn mybtrfs_application::ports::ProgressPort,
) -> Result<PruneReport, PortError> {
    PruneService::new(btrfs, btrfs, retention)
        .with_progress(progress)
        .prune(snapshot_dir, target_dir, snapshot_policy, target_policy)
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

/// Whether a command changes filesystem state (and so must hold the run lock).
/// Dry runs mutate nothing (invariant #8) and read-only commands never contend,
/// so neither takes the lock. Exhaustive on purpose: a new command forces a
/// deliberate choice here.
fn command_mutates(command: &Command) -> bool {
    match command {
        Command::Run { .. } | Command::Snapshot { .. } | Command::Resume { .. } => true,
        Command::Prune { dry_run, .. } | Command::Restore { dry_run, .. } => !dry_run,
        Command::List { .. }
        | Command::Stats { .. }
        | Command::Status { .. }
        | Command::Diff { .. }
        | Command::ListDrives
        | Command::ListSubvolumes => false,
    }
}

/// The run-lock path: the `--lock` override, or a **per-uid** default under the
/// temp dir (`mybtrfs-<uid>.lock`). The uid suffix matters: `/tmp` is a
/// world-writable sticky directory, and `fs.protected_regular` blocks an
/// `O_CREAT` open of a file you do not own there — *even for root* — so a single
/// shared `mybtrfs.lock` created by one user breaks every other user's run
/// (notably: a stray unprivileged-run lock breaking the normal root run). Giving
/// each uid its own lock file keeps it owner-openable; runs that actually touch
/// btrfs are all root, so they still serialize against one another.
fn lock_path(override_path: Option<&Path>) -> PathBuf {
    override_path.map_or_else(
        || std::env::temp_dir().join(format!("mybtrfs-{}.lock", effective_uid())),
        Path::to_path_buf,
    )
}

/// The process's effective uid, read from `/proc/self/status` (safe — no `libc`
/// / `unsafe`). Falls back to `0` if it cannot be read, which at worst restores
/// the old shared-path behavior rather than failing.
fn effective_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                // "Uid:\t<real>\t<effective>\t<saved>\t<fs>" — the effective uid
                // owns the files this process creates.
                .and_then(|rest| rest.split_whitespace().nth(1))
                .and_then(|uid| uid.parse().ok())
        })
        .unwrap_or(0)
}

/// Acquire the run lock, mapping a lock already held by another run to
/// [`LockBusy`] (process exit code 3).
fn acquire_lock(override_path: Option<&Path>) -> Result<FileLock> {
    let path = lock_path(override_path);
    match FileLock::acquire(&path) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(anyhow::Error::new(LockBusy(format!(
            "another mybtrfs run holds the lock: {}",
            path.display()
        )))),
        // The lock file could not be opened at all (e.g. owned by another user in
        // a sticky dir under `fs.protected_regular`). Report it as a lock problem
        // with an actionable fix — never as a panic or a misleading "needs root".
        Err(err) => Err(anyhow::Error::new(LockBusy(format!(
            "could not open the run lock {}: {err} — it may be owned by another \
             user; pass --lock <PATH> to use a different one",
            path.display()
        )))),
    }
}

/// Canonicalize and validate a path before handing it to the use cases
/// (decision ID-2): the result is absolute, symlink-resolved, and `..`-free, and
/// the path must already exist.
fn validate_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("invalid or missing path: {}", path.display()))
}

/// Parse a `run` target spec into an [`Endpoint`]: a local path, or an `ssh://`
/// remote endpoint. A malformed `ssh://` spec is a usage error (exit 2).
///
/// # Errors
/// [`UsageError`] if the path is not UTF-8 or the `ssh://` spec is malformed.
fn parse_target(spec: &Path) -> Result<Endpoint> {
    let text = spec
        .to_str()
        .with_context(|| format!("target path is not valid UTF-8: {}", spec.display()))?;
    parse_endpoint(text).map_err(|err| anyhow::Error::new(UsageError(err.to_string())))
}

/// Resolve a backup-target drive interactively: enumerate mounted btrfs
/// filesystems and prompt for one. Errors if none are found or none is selected.
fn resolve_target_drive(
    discovery: &dyn DriveDiscoveryPort,
    prompter: &dyn Prompter,
) -> Result<PathBuf> {
    let drives = discovery.detect()?;
    if drives.is_empty() {
        bail!("no mounted btrfs filesystems found to use as a backup target");
    }
    let labels: Vec<String> = drives.iter().map(describe_drive).collect();
    let choice = prompter
        .choose("Select a backup drive:", &labels)?
        .context("no backup drive selected")?;
    Ok(drives[choice].mountpoint.clone())
}

/// A one-line description of a discovered drive for the selection menu.
fn describe_drive(drive: &DiscoveredFilesystem) -> String {
    format!(
        "{}  ({}, label={}, fs={})",
        drive.mountpoint.display(),
        if drive.removable {
            "removable"
        } else {
            "fixed"
        },
        drive.label.as_deref().unwrap_or("-"),
        drive.fs_uuid,
    )
}

/// The local hostname (for the `<hostname>/` target subdirectory); falls back to
/// `localhost` if it cannot be read.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "localhost".to_owned())
}

/// A one-line description of the invoked command for the audit journal.
fn describe_command(command: &Command) -> String {
    match command {
        Command::Run {
            source,
            target_dir,
            set,
            ..
        } => {
            if let Some(set_path) = set {
                format!("run (from backup-set: {})", set_path.display())
            } else {
                format!(
                    "run {} -> {}",
                    source
                        .as_ref()
                        .map_or_else(|| "<missing>".to_owned(), |src| src.display().to_string()),
                    target_dir.as_ref().map_or_else(
                        || "<auto-drive>".to_owned(),
                        |dir| dir.display().to_string()
                    )
                )
            }
        }
        Command::Snapshot { source, .. } => format!("snapshot {}", source.display()),
        Command::Resume {
            snapshot_dir,
            target_dir,
            ..
        } => format!(
            "resume {} -> {}",
            snapshot_dir.display(),
            target_dir.display()
        ),
        Command::Prune {
            snapshot_dir,
            target_dir,
            dry_run,
            ..
        } => format!(
            "prune{} {} {}",
            if *dry_run { " (dry-run)" } else { "" },
            snapshot_dir.display(),
            target_dir.display()
        ),
        Command::Restore {
            backup,
            dest,
            dry_run,
            ..
        } => format!(
            "restore{} {} -> {}",
            if *dry_run { " (dry-run)" } else { "" },
            backup.display(),
            dest.display()
        ),
        Command::List { .. } => "list".to_owned(),
        Command::Stats { .. } => "stats".to_owned(),
        Command::Status { .. } => "status".to_owned(),
        Command::Diff { .. } => "diff".to_owned(),
        Command::ListDrives => "list-drives".to_owned(),
        Command::ListSubvolumes => "list-subvolumes".to_owned(),
    }
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
        "snapshot\t{}",
        report
            .snapshot
            .mountpoint
            .join(&report.snapshot.path)
            .display()
    );
    println!(
        "backup\t{}",
        report.backup.mountpoint.join(&report.backup.path).display()
    );
    println!(
        "pruned\t{}\t{}",
        report.snapshots_pruned.delete.len(),
        report.backups_pruned.delete.len()
    );
}

/// Print a one-fact-per-line summary of a completed resume.
fn print_resume_report(report: &ResumeReport) {
    match &report.transferred {
        Some(backup) => println!(
            "transferred\t{}",
            backup.mountpoint.join(&backup.path).display()
        ),
        None => println!("status\talready_backed_up"),
    }
    println!(
        "pruned\t{}\t{}",
        report.snapshots_pruned.delete.len(),
        report.backups_pruned.delete.len()
    );
}

/// Print a prune report. In dry-run mode, shows a human-readable keep/delete
/// preview (via `retention_preview::format_schedule`); otherwise a one-line
/// machine-readable summary of how many were actually deleted.
fn print_prune_report(report: &PruneReport, dry_run: bool, now: DateTime<FixedOffset>) {
    if dry_run {
        print!(
            "{}",
            retention_preview::format_schedule(&report.snapshots_pruned, "snapshots", now)
        );
        print!(
            "{}",
            retention_preview::format_schedule(&report.backups_pruned, "backups", now)
        );
    } else {
        println!(
            "pruned\t{}\t{}",
            report.snapshots_pruned.delete.len(),
            report.backups_pruned.delete.len()
        );
    }
}

/// Print a one-fact-per-line summary of a restore. On a dry run the lines
/// describe the intended plan (prefixed `would`) and nothing was changed.
fn print_restore_report(report: &RestoreReport) {
    if report.dry_run {
        if let Some(moved) = &report.moved_aside {
            println!("would_move_aside\t{}", moved.display());
        }
        if report.transferred_back {
            println!("would_transfer_back\tyes");
        }
        println!("would_restore\t{}", report.dest.display());
        return;
    }
    if report.transferred_back {
        println!("transfer_back\tyes");
    }
    match &report.restored {
        Some(restored) => println!(
            "restored\t{}",
            restored.mountpoint.join(&restored.path).display()
        ),
        None => println!("restored\t{}", report.dest.display()),
    }
    if let Some(moved) = &report.moved_aside {
        println!("move_aside\t{}", moved.display());
    }
}

/// Print a one-fact-per-line inventory: each snapshot with its backups, then the
/// orphaned and incomplete backups.
fn print_inventory(inventory: &Inventory) {
    for status in &inventory.snapshots {
        println!(
            "snapshot\t{}",
            status
                .snapshot
                .mountpoint
                .join(&status.snapshot.path)
                .display()
        );
        for backup in &status.backups {
            println!("backup\t{}", backup.mountpoint.join(&backup.path).display());
        }
    }
    for orphan in &inventory.orphan_backups {
        println!(
            "orphan_backup\t{}",
            orphan.mountpoint.join(&orphan.path).display()
        );
    }
    for incomplete in &inventory.incomplete_backups {
        println!(
            "incomplete_backup\t{}",
            incomplete.mountpoint.join(&incomplete.path).display()
        );
    }
}

/// Print aggregate backup statistics, one fact per line.
fn print_stats(stats: &Stats) {
    println!("snapshots\t{}", stats.snapshots);
    println!("backups\t{}", stats.backups);
    println!("correlated\t{}", stats.correlated);
    println!("orphaned\t{}", stats.orphaned);
    println!("incomplete\t{}", stats.incomplete);
}

/// Print backup health status.
fn print_status(report: &mybtrfs_application::status::StatusReport) {
    println!("source\t{}", report.source_dir.display());
    println!("target\t{}", report.target_dir.display());
    println!("snapshots\t{}", report.snapshots.len());
    println!("backups\t{}", report.backups.len());
    if let Some(ref last_run) = report.last_run {
        println!("last_run\t{}", last_run.timestamp);
        println!("last_command\t{}", last_run.command);
    }
}

fn print_diff(diff: &mybtrfs_application::diff::DiffSummary) {
    println!(
        "{}\t{}\t{}\t{}\t{}",
        diff.older_path,
        diff.older_size_human,
        diff.newer_path,
        diff.newer_size_human,
        diff.changed_size_human
    );
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

/// Print the local btrfs subvolumes, one tab-separated line each
/// (`id  path  fs-mountpoint  uuid  ro|rw`) — awk-friendly, matching the
/// `list-drives`/`diff` house style. `path` is relative to its filesystem
/// mountpoint, so `<fs-mountpoint>/<path>` is the on-disk location.
///
/// A tab-separated header row is printed first for readability (it aligns with
/// the data under `column -t`); `--quiet` suppresses it so scripting / awk
/// pipelines get headerless output.
fn print_subvolumes(subvolumes: &[Subvolume], quiet: bool) {
    if subvolumes.is_empty() {
        println!("no btrfs subvolumes found");
        return;
    }
    if !quiet {
        println!("ID\tPATH\tFS-MOUNTPOINT\tUUID\tRO/RW");
    }
    for sv in subvolumes {
        let uuid = sv.uuid.as_ref().map_or("-".to_owned(), Uuid::to_string);
        let kind = if sv.readonly { "ro" } else { "rw" };
        println!(
            "{}\t{}\t{}\t{uuid}\t{kind}",
            sv.id,
            sv.path.display(),
            sv.mountpoint.display(),
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

/// A [`DeletePort`] that routes each deletion by path: anything under
/// `remote_prefix` (a remote target directory) deletes through the `remote` port
/// (ssh `btrfs subvolume delete`), everything else through `local`. This lets a
/// single `RetentionService` prune **both** sides of a remote backup — local
/// source snapshots and remote target backups — across two transports, so
/// `--target-preserve` works against an `ssh://` target.
struct RoutingDeletePort<'a> {
    local: &'a dyn DeletePort,
    remote: &'a dyn DeletePort,
    remote_prefix: PathBuf,
}

impl<'a> RoutingDeletePort<'a> {
    /// Route deletions under `remote_prefix` to `remote`, the rest to `local`.
    fn new(local: &'a dyn DeletePort, remote: &'a dyn DeletePort, remote_prefix: PathBuf) -> Self {
        Self {
            local,
            remote,
            remote_prefix,
        }
    }
}

impl DeletePort for RoutingDeletePort<'_> {
    fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
        if path.starts_with(&self.remote_prefix) {
            self.remote.delete(path, commit)
        } else {
            self.local.delete(path, commit)
        }
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

    /// A `DriveDiscoveryPort` returning a fixed drive list.
    struct FakeDrives(Vec<DiscoveredFilesystem>);
    impl DriveDiscoveryPort for FakeDrives {
        fn detect(&self) -> Result<Vec<DiscoveredFilesystem>, PortError> {
            Ok(self.0.clone())
        }
    }

    /// A `Prompter` returning a fixed `choose` result (and auto-confirming).
    struct ChoosePrompter(Option<usize>);
    impl Prompter for ChoosePrompter {
        fn confirm(&self, _prompt: &str) -> Result<bool, PortError> {
            Ok(true)
        }
        fn choose(&self, _prompt: &str, _options: &[String]) -> Result<Option<usize>, PortError> {
            Ok(self.0)
        }
    }

    fn drive(mountpoint: &str) -> DiscoveredFilesystem {
        DiscoveredFilesystem {
            device: PathBuf::from("/dev/sdz"),
            mountpoint: PathBuf::from(mountpoint),
            fs_uuid: mybtrfs_domain::model::Uuid::parse("ffffffff-ffff-4fff-8fff-ffffffffffff")
                .unwrap(),
            label: None,
            removable: true,
        }
    }

    #[test]
    fn resolve_target_drive_returns_the_chosen_mountpoint() {
        let drives = FakeDrives(vec![drive("/mnt/a"), drive("/mnt/b")]);
        let chosen = resolve_target_drive(&drives, &ChoosePrompter(Some(1))).unwrap();
        assert_eq!(chosen, PathBuf::from("/mnt/b"));
    }

    #[test]
    fn resolve_target_drive_errors_with_no_drives() {
        let drives = FakeDrives(vec![]);
        assert!(resolve_target_drive(&drives, &ChoosePrompter(Some(0))).is_err());
    }

    #[test]
    fn resolve_target_drive_errors_when_nothing_selected() {
        let drives = FakeDrives(vec![drive("/mnt/a")]);
        assert!(resolve_target_drive(&drives, &ChoosePrompter(None)).is_err());
    }

    #[test]
    fn exit_code_distinguishes_usage_lock_and_failure() {
        let usage = anyhow::Error::new(UsageError("bad value".to_owned()));
        assert_eq!(exit_code_for(&usage), exit_code::USAGE);
        let lock = anyhow::Error::new(LockBusy("held".to_owned()));
        assert_eq!(exit_code_for(&lock), exit_code::LOCK_BUSY);
        let generic = anyhow::anyhow!("boom");
        assert_eq!(exit_code_for(&generic), exit_code::FAILURE);
    }

    #[test]
    fn malformed_retention_spec_is_a_usage_error() {
        let err = parse_policy("all", "7x").unwrap_err();
        assert_eq!(exit_code_for(&err), exit_code::USAGE);
    }

    #[test]
    fn parse_target_routes_local_and_remote_and_rejects_bad_ssh() {
        assert!(matches!(
            parse_target(Path::new("/mnt/backup/host")).unwrap(),
            Endpoint::Local(_)
        ));
        assert!(matches!(
            parse_target(Path::new("ssh://isard@apolo/mnt/btrfs-test")).unwrap(),
            Endpoint::Remote { .. }
        ));
        // A malformed ssh:// spec (no path) is a usage error → exit 2.
        let err = parse_target(Path::new("ssh://apolo")).unwrap_err();
        assert_eq!(exit_code_for(&err), exit_code::USAGE);
    }

    #[test]
    fn routing_delete_port_sends_remote_paths_over_ssh_and_keeps_the_rest_local() {
        use std::cell::RefCell;
        struct Rec(RefCell<Vec<PathBuf>>);
        impl DeletePort for Rec {
            fn delete(&self, path: &Path, _commit: DeleteCommit) -> Result<(), PortError> {
                self.0.borrow_mut().push(path.to_path_buf());
                Ok(())
            }
        }
        let local = Rec(RefCell::new(Vec::new()));
        let remote = Rec(RefCell::new(Vec::new()));
        let routing = RoutingDeletePort::new(&local, &remote, PathBuf::from("/mnt/btrfs-test"));

        // A source snapshot deletes locally; a target backup (under the remote
        // prefix) deletes over ssh.
        routing
            .delete(Path::new("/pool/.snap/data.X"), DeleteCommit::Deferred)
            .unwrap();
        routing
            .delete(Path::new("/mnt/btrfs-test/data.X"), DeleteCommit::Deferred)
            .unwrap();

        assert_eq!(*local.0.borrow(), vec![PathBuf::from("/pool/.snap/data.X")]);
        assert_eq!(
            *remote.0.borrow(),
            vec![PathBuf::from("/mnt/btrfs-test/data.X")]
        );
    }

    #[test]
    fn only_mutating_commands_take_the_run_lock() {
        let mutates = |args: &[&str]| command_mutates(&Cli::try_parse_from(args).unwrap().command);
        // State-changing commands must serialize behind the lock.
        assert!(mutates(&["mybtrfs", "run", "/p/home", "/p/.snap", "home"]));
        assert!(mutates(&[
            "mybtrfs", "snapshot", "/p/home", "/p/.snap", "home"
        ]));
        assert!(mutates(&["mybtrfs", "resume", "/snap", "home", "/target"]));
        assert!(mutates(&["mybtrfs", "prune", "/snap", "/target"]));
        assert!(mutates(&["mybtrfs", "restore", "/backup", "/dest"]));
        // Dry runs change nothing, and read-only commands never contend.
        assert!(!mutates(&[
            "mybtrfs",
            "prune",
            "/snap",
            "/target",
            "--dry-run"
        ]));
        assert!(!mutates(&[
            "mybtrfs",
            "restore",
            "/backup",
            "/dest",
            "--dry-run"
        ]));
        assert!(!mutates(&["mybtrfs", "list", "/snap", "/target"]));
        assert!(!mutates(&["mybtrfs", "stats", "/snap", "/target"]));
        assert!(!mutates(&["mybtrfs", "list-drives"]));
    }

    #[test]
    fn acquire_lock_maps_a_held_lock_to_the_lock_busy_code() {
        let path = std::env::temp_dir().join(format!(
            "mybtrfs-cli-lock-{}.lock",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The first acquisition holds the lock for the rest of the test.
        let _held = acquire_lock(Some(path.as_path())).unwrap();
        // A concurrent acquisition is rejected as lock-busy (→ exit code 3).
        let err = acquire_lock(Some(path.as_path())).unwrap_err();
        assert_eq!(exit_code_for(&err), exit_code::LOCK_BUSY);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn default_lock_path_is_per_uid_and_under_temp() {
        // The override is honored verbatim.
        let explicit = PathBuf::from("/run/custom.lock");
        assert_eq!(lock_path(Some(&explicit)), explicit);
        // The default is `<tmpdir>/mybtrfs-<uid>.lock` — per-uid so a file owned by
        // another user (which `fs.protected_regular` would forbid opening in a
        // sticky dir, even for root) never collides.
        let default = lock_path(None);
        assert_eq!(default.parent(), Some(std::env::temp_dir().as_path()));
        let name = default.file_name().unwrap().to_string_lossy();
        assert_eq!(name, format!("mybtrfs-{}.lock", effective_uid()));
        // effective_uid agrees with the file the current process would own; for a
        // normal test run that's the invoking user's uid (non-panicking).
        let _ = effective_uid();
    }

    #[test]
    fn an_unopenable_lock_is_a_clean_lock_error_not_a_crash() {
        // A directory can't be opened as a lock file (EISDIR) — the same shape as
        // the EACCES a foreign-owned lock would give. It must surface as a clear
        // lock error (exit 3), never a panic.
        let err = acquire_lock(Some(std::env::temp_dir().as_path())).unwrap_err();
        assert_eq!(exit_code_for(&err), exit_code::LOCK_BUSY);
    }

    #[test]
    fn classify_does_not_mislabel_a_lock_permission_error_as_needs_root() {
        // A lock failure whose cause is a literal "Permission denied" must stay a
        // lock error (exit 3) — not be hijacked into "needs root" (exit 4).
        let lock = anyhow::Error::new(LockBusy(
            "could not open the run lock /tmp/mybtrfs-0.lock: Permission denied".to_owned(),
        ));
        assert_eq!(exit_code_for(&classify(lock)), exit_code::LOCK_BUSY);
        // A genuine btrfs-command permission failure (unclassified) still becomes
        // the friendly needs-root error (exit 4).
        let btrfs = anyhow::Error::new(PortError::Command(
            "`btrfs` exited unsuccessfully (1): ERROR: ... Permission denied".to_owned(),
        ));
        assert_eq!(
            exit_code_for(&classify(btrfs)),
            exit_code::PERMISSION_DENIED
        );
    }

    #[test]
    fn describe_command_summarizes_the_invocation() {
        let run = Cli::try_parse_from(["mybtrfs", "run", "/p/home", "/p/.snap", "home", "/d/host"])
            .unwrap();
        assert_eq!(describe_command(&run.command), "run /p/home -> /d/host");
        // An omitted target is shown as the auto-drive placeholder.
        let auto = Cli::try_parse_from(["mybtrfs", "run", "/p/home", "/p/.snap", "home"]).unwrap();
        assert_eq!(
            describe_command(&auto.command),
            "run /p/home -> <auto-drive>"
        );
        let prune =
            Cli::try_parse_from(["mybtrfs", "prune", "/snap", "/target", "--dry-run"]).unwrap();
        assert_eq!(
            describe_command(&prune.command),
            "prune (dry-run) /snap /target"
        );
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
                set,
            } => {
                assert_eq!(source, Some(PathBuf::from("/mnt/pool/home")));
                assert_eq!(snapshot_dir, Some(PathBuf::from("/mnt/pool/.snapshots")));
                assert_eq!(basename, Some("home".to_string()));
                assert_eq!(target_dir, Some(PathBuf::from("/mnt/drive/host")));
                assert_eq!(set, None);
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

    #[test]
    fn parses_run_with_backup_set_file() {
        let cli = Cli::try_parse_from(["mybtrfs", "run", "--set", "/etc/mybtrfs.backup-set.toml"])
            .unwrap();
        match cli.command {
            Command::Run {
                source,
                snapshot_dir,
                basename,
                target_dir,
                set,
                incremental,
                ..
            } => {
                // When --set is provided, the positional args are optional.
                assert_eq!(source, None);
                assert_eq!(snapshot_dir, None);
                assert_eq!(basename, None);
                assert_eq!(target_dir, None);
                assert_eq!(set, Some(PathBuf::from("/etc/mybtrfs.backup-set.toml")));
                assert_eq!(incremental, IncrementalArg::Yes);
            }
            _ => panic!("expected a Run command"),
        }
    }

    #[test]
    fn run_with_set_flag_overrides_positional_args() {
        // When --set is provided with positional args, the positional args are ignored.
        let cli = Cli::try_parse_from([
            "mybtrfs",
            "run",
            "/source",
            "/snapshots",
            "base",
            "--set",
            "/etc/mybtrfs.backup-set.toml",
        ])
        .unwrap();
        match cli.command {
            Command::Run {
                source,
                snapshot_dir,
                basename,
                set,
                ..
            } => {
                // Positional args are parsed but set takes precedence in dispatch.
                assert_eq!(source, Some(PathBuf::from("/source")));
                assert_eq!(snapshot_dir, Some(PathBuf::from("/snapshots")));
                assert_eq!(basename, Some("base".to_string()));
                assert_eq!(set, Some(PathBuf::from("/etc/mybtrfs.backup-set.toml")));
            }
            _ => panic!("expected a Run command"),
        }
    }
}
