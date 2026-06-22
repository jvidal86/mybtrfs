//! Driven adapters — concrete port implementations, wired at the composition
//! root (the CLI). Production adapters live here; test fakes (`FakeBtrfs`,
//! `FixedClock`, `ScriptedPrompter`) go behind `#[cfg(test)]` / a test-support
//! module. See `documentation/02-architecture-v2.md` §3.

pub mod btrfs_cli;
pub mod clock;
pub mod drive_discovery;
pub mod journal;
pub mod local_fs;
pub mod prompter;
