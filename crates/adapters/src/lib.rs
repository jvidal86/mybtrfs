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
pub(crate) mod prompter;

/// The btrfs-CLI-backed subvolume repository; the composition root constructs it
/// with a discovered filesystem's UUID and mountpoint.
pub use btrfs_cli::BtrfsCliAdapter;

/// Clock adapters: [`SystemClock`] (real wall clock) and [`FixedClock`]
/// (deterministic, for tests / reproducible runs).
pub use clock::{FixedClock, SystemClock};
