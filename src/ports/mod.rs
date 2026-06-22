//! Driven port traits — small and focused (ISP). To be defined as the
//! implementation needs them:
//!
//! - `SubvolumeRepository` — query `show`/`list` → model objects.
//! - `SnapshotPort` — create read-only snapshot; create **writable** working snapshot.
//! - `TransferPort` — send/receive **and verify** the received subvolume.
//! - `DeletePort` — delete a subvolume (with commit option).
//! - `FilesystemPort` — exists / **mkdir** (default dirs) / **rename** (move-aside).
//! - `DriveDiscoveryPort` — enumerate mounted btrfs filesystems + removable hints.
//! - `ClockPort` — current time **and timezone** (injected ⇒ deterministic).
//! - `Prompter` — drive selection / directory-creation & destructive confirmation.
//! - `Journal` — append-only transaction log.
//!
//! See `documentation/02-architecture-v2.md` §3.
//
// TODO (Phase 1+): define the port traits.
