//! Driven adapters — concrete port implementations, wired at the composition
//! root (the CLI). Production adapters live here; test fakes (`FakeBtrfs`,
//! `FixedClock`, `ScriptedPrompter`) go behind `#[cfg(test)]` / a test-support
//! module. See `documentation/02-architecture-v2.md` §3.

pub(crate) mod btrfs_cli;
pub(crate) mod clock;
pub(crate) mod command;
pub(crate) mod drive_discovery;
pub(crate) mod journal;
pub(crate) mod local_fs;
pub(crate) mod lock;
pub(crate) mod mounts;
pub(crate) mod prompter;

/// The btrfs-CLI-backed adapter (subvolume / snapshot / transfer / delete ports);
/// resolves each path's filesystem from the mount table, so one instance serves
/// both source and target.
pub use btrfs_cli::BtrfsCliAdapter;

/// Clock adapters: [`SystemClock`] (real wall clock) and [`FixedClock`]
/// (deterministic, for tests / reproducible runs).
pub use clock::{FixedClock, SystemClock};

/// Drive discovery: [`LsblkDriveDiscovery`] enumerates mounted btrfs filesystems.
pub use drive_discovery::LsblkDriveDiscovery;

/// Filesystem operations: [`LocalFsAdapter`] (`std::fs`) — existence / mkdir / rename.
pub use local_fs::LocalFsAdapter;

/// Prompters: [`StdioPrompter`] (interactive stdin/stdout) and [`AutoPrompter`]
/// (`--yes`/non-interactive — auto-confirm, auto-resolve a single choice).
pub use prompter::{AutoPrompter, StdioPrompter};

/// Journals: [`FileJournal`] (append-only audit file) and [`NullJournal`] (no-op
/// default when no journal is configured).
pub use journal::{FileJournal, NullJournal};

/// Concurrency guard: [`FileLock`] — an advisory `flock` serializing mutating
/// runs (released on drop / process exit).
pub use lock::FileLock;

/// Initialize `env_logger` once for unit tests (idempotent; safe to call from
/// every `#[test]`). Logs go through the test harness and appear only for
/// failing tests unless `--nocapture` is passed.
#[cfg(test)]
pub(crate) fn init_test_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = env_logger::builder().is_test(true).try_init();
    });
}
