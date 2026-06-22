//! Driven adapters — concrete port implementations, wired at the composition
//! root (the CLI). Production adapters live here; test fakes (`FakeBtrfs`,
//! `FixedClock`, `ScriptedPrompter`) go behind `#[cfg(test)]` / a test-support
//! module. See `documentation/02-architecture-v2.md` §3.

pub(crate) mod btrfs_cli;
pub(crate) mod clock;
pub(crate) mod drive_discovery;
pub(crate) mod journal;
pub(crate) mod local_fs;
pub(crate) mod prompter;
