//! Driven port traits — the secondary interfaces the application needs from the
//! outside world. Small and focused (ISP): a use case depends only on the ports
//! it actually uses. Concrete adapters (production and test) implement them and
//! are wired at the CLI composition root.
//!
//! The dangerous operations (transfer, delete, make-writable, move-aside) are
//! reachable only through these narrow contracts, several of which *embed* a
//! safety check — so the fail-safe properties are architectural, not
//! conventional. See `documentation/02-architecture-v2.md` §3/§6.

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};

use mybtrfs_domain::model::{Subvolume, Uuid};
use mybtrfs_domain::parent::ParentSelection;

/// The failure modes a driven port can surface to the application layer.
///
/// A deliberately small taxonomy (invariant #12): each adapter maps its concrete
/// failures onto these variants so orchestrators can branch on the *kind* of
/// failure without knowing the underlying technology.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// An external command (e.g. `btrfs`) could not be run, or it exited non-zero.
    #[error("command failed: {0}")]
    Command(String),
    /// Command output could not be parsed into a model object.
    #[error("parse error: {0}")]
    Parse(String),
    /// A post-condition check failed — e.g. a received subvolume was not
    /// read-only, lacked a `received_uuid`, or had an implausible `parent_uuid`.
    #[error("verification failed: {0}")]
    Verification(String),
    /// An underlying filesystem / OS I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Whether `btrfs subvolume delete` should force a transaction commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteCommit {
    /// No explicit commit flag; rely on btrfs's normal commit interval.
    Deferred,
    /// `--commit-each`: commit the transaction after the deletion — used when a
    /// garbled subvolume must be gone before anything else proceeds (invariant #2).
    Each,
}

/// A mounted btrfs filesystem offered as a backup-target candidate (Phase 1
/// drive selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFilesystem {
    /// Backing block device (e.g. `/dev/sdb1`).
    pub device: PathBuf,
    /// Where the filesystem is currently mounted.
    pub mountpoint: PathBuf,
    /// The filesystem UUID (matches `Subvolume::fs_uuid`).
    pub fs_uuid: Uuid,
    /// Filesystem label, if one is set.
    pub label: Option<String>,
    /// Kernel "removable" hint (typically true for USB/external drives).
    pub removable: bool,
}

/// Read-only queries against btrfs metadata (`btrfs subvolume show` / `list`),
/// mapped into domain [`Subvolume`]s.
pub trait SubvolumeRepository {
    /// Show a single subvolume's metadata (`btrfs subvolume show <path>`).
    ///
    /// # Errors
    /// [`PortError::Command`] if btrfs fails (e.g. `path` is not a subvolume);
    /// [`PortError::Parse`] if the output cannot be parsed.
    fn show(&self, path: &Path) -> Result<Subvolume, PortError>;

    /// List every subvolume of the btrfs filesystem containing `filesystem` (any
    /// path on that filesystem), as needed to build a `RelationshipGraph`.
    ///
    /// # Errors
    /// [`PortError::Command`] if btrfs fails; [`PortError::Parse`] on malformed output.
    fn list(&self, filesystem: &Path) -> Result<Vec<Subvolume>, PortError>;
}

/// Snapshot creation. Read-only snapshots are the unit of backup; the writable
/// variant is the **only** sanctioned path to a writable subvolume on restore
/// (invariant #7 — never `btrfs property set ro=false`, which poisons
/// `received_uuid` and silently breaks future incrementals).
pub trait SnapshotPort {
    /// Create a read-only snapshot of `source` at `dest`
    /// (`btrfs subvolume snapshot -r`), returning the created subvolume.
    ///
    /// # Errors
    /// [`PortError::Command`] if the snapshot fails; [`PortError::Parse`] or
    /// [`PortError::Verification`] if the result cannot be confirmed.
    fn create_readonly(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError>;

    /// Create a **writable** snapshot of `source` at `dest` (`btrfs subvolume
    /// snapshot`, no `-r`) — the restore path. Returns the created subvolume.
    ///
    /// # Errors
    /// [`PortError::Command`] if the snapshot fails; [`PortError::Parse`] or
    /// [`PortError::Verification`] if the result cannot be confirmed.
    fn make_writable(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError>;
}

/// Incremental/full transfer with **built-in verification** (invariants #1/#2):
/// the implementation must confirm the received subvolume and clean up a garbled
/// result, so a successful return is a trustworthy backup — never a bare exit code.
pub trait TransferPort {
    /// Send `source` and receive it into `target_dir`
    /// (`btrfs send [-p parent] [-c clone...] | btrfs receive`), using `selection`
    /// for the incremental parent and clone sources (a full send when
    /// `selection.parent` is `None`).
    ///
    /// On success the returned subvolume has been **verified**: read-only, with a
    /// `received_uuid`, and a `parent_uuid` consistent with full vs incremental.
    /// A garbled result is deleted before returning.
    ///
    /// # Errors
    /// [`PortError::Command`] if send/receive fails; [`PortError::Verification`]
    /// if the received subvolume fails its post-conditions (after cleanup);
    /// [`PortError::Parse`] if its metadata cannot be read.
    fn send_receive(
        &self,
        source: &Subvolume,
        selection: &ParentSelection,
        target_dir: &Path,
    ) -> Result<Subvolume, PortError>;
}

/// Subvolume deletion — the single sanctioned destructive btrfs operation, kept
/// behind its own narrow port so the domain safety policy decides *what* may be
/// deleted before this is ever reached.
pub trait DeletePort {
    /// Delete the subvolume at `path` (`btrfs subvolume delete`), honoring `commit`.
    ///
    /// # Errors
    /// [`PortError::Command`] if btrfs fails to delete the subvolume.
    fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError>;
}

/// Plain filesystem operations (not btrfs): existence checks, default-directory
/// creation, and the restore move-aside (`D → D.broken`). Backed by `std::fs`.
pub trait FilesystemPort {
    /// Whether `path` exists.
    ///
    /// # Errors
    /// [`PortError::Io`] if existence cannot be determined (e.g. a permission
    /// error on a parent component).
    fn exists(&self, path: &Path) -> Result<bool, PortError>;

    /// Create `path` and any missing parents (used for default snapshot/target dirs).
    ///
    /// # Errors
    /// [`PortError::Io`] if the directory cannot be created.
    fn create_dir_all(&self, path: &Path) -> Result<(), PortError>;

    /// Rename/move `from` to `to` (the restore move-aside).
    ///
    /// # Errors
    /// [`PortError::Io`] if the rename fails.
    fn rename(&self, from: &Path, to: &Path) -> Result<(), PortError>;
}

/// Enumerate candidate backup targets — mounted btrfs filesystems plus removable
/// hints — for interactive drive selection (Phase 1).
pub trait DriveDiscoveryPort {
    /// Detect the currently mounted btrfs filesystems.
    ///
    /// # Errors
    /// [`PortError::Command`] or [`PortError::Io`] if the mount table / device
    /// metadata cannot be read; [`PortError::Parse`] on malformed output.
    fn detect(&self) -> Result<Vec<DiscoveredFilesystem>, PortError>;
}

/// The injected clock **and timezone** — the sole source of "now", so naming and
/// retention are deterministic and testable (invariant #11).
pub trait ClockPort {
    /// The current instant carrying the configured local UTC offset. The offset
    /// is significant: `short`/`long` snapshot names are local-time, so the
    /// timezone is an explicit input, not an ambient default.
    #[must_use]
    fn now(&self) -> DateTime<FixedOffset>;
}

/// Interactive prompts — drive selection and destructive/dir-creation
/// confirmation. Kept generic (no knowledge of filesystems) so callers format
/// their own choices and this port stays substitutable.
pub trait Prompter {
    /// Ask a yes/no question; returns the user's answer.
    ///
    /// # Errors
    /// [`PortError::Io`] if the prompt cannot be read or written.
    fn confirm(&self, prompt: &str) -> Result<bool, PortError>;

    /// Offer `options` and return the chosen index, or `None` if the user made no
    /// selection (cancelled).
    ///
    /// # Errors
    /// [`PortError::Io`] if the prompt cannot be read or written.
    fn choose(&self, prompt: &str, options: &[String]) -> Result<Option<usize>, PortError>;
}

/// Append-only audit log of what mybtrfs did (created / sent / deleted), for
/// after-the-fact inspection.
pub trait Journal {
    /// Append `message` to the log.
    ///
    /// # Errors
    /// [`PortError::Io`] if the entry cannot be written.
    fn record(&self, message: &str) -> Result<(), PortError>;
}
