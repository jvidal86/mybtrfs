//! `BtrfsCliAdapter` — spawns `btrfs` directly (argv array, **never** a shell);
//! implements `SubvolumeRepository`, `SnapshotPort`, `TransferPort`, `DeletePort`.
//! Verification (readonly + received_uuid + plausible parent_uuid) and
//! garbled-receive cleanup are part of the transfer contract — exit codes are
//! never trusted alone. See `documentation/04-coding-guidelines.md` §5.

pub(crate) mod parse;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use mybtrfs_application::ports::{PortError, SubvolumeRepository};
use mybtrfs_domain::model::{Subvolume, Uuid};

use crate::command::{CommandRunner, SystemCommandRunner};

/// External program name (spawned as an argv array, never via a shell).
const BTRFS: &str = "btrfs";

/// [`SubvolumeRepository`] over the real `btrfs` CLI, scoped to a single
/// filesystem — its `fs_uuid` and `mountpoint`, as discovered by the
/// drive-discovery adapter. Those identify the owning filesystem for every
/// parsed [`Subvolume`], which the btrfs output itself does not carry.
pub struct BtrfsCliAdapter {
    runner: Box<dyn CommandRunner>,
    fs_uuid: Uuid,
    mountpoint: PathBuf,
}

impl BtrfsCliAdapter {
    /// Create an adapter for the filesystem identified by `fs_uuid` /
    /// `mountpoint`, spawning the real `btrfs` binary.
    #[must_use]
    pub fn new(fs_uuid: Uuid, mountpoint: PathBuf) -> Self {
        Self {
            runner: Box::new(SystemCommandRunner),
            fs_uuid,
            mountpoint,
        }
    }

    /// The subvolume path relative to the bound mountpoint (as stamped onto a
    /// `Subvolume`); falls back to the input path if it is not under the mountpoint.
    fn relative_path(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.mountpoint)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

impl SubvolumeRepository for BtrfsCliAdapter {
    fn show(&self, path: &Path) -> Result<Subvolume, PortError> {
        let output = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("show"),
                path.as_os_str(),
            ],
        )?;
        parse::parse_show(
            &output,
            self.relative_path(path),
            &self.fs_uuid,
            &self.mountpoint,
        )
    }

    fn list(&self, filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
        // Display flags match btrbk: -a (all) -c (cgen) -u (uuid) -q (parent_uuid)
        // -R (received_uuid). The read-only flag is only available via -r, so a
        // second call provides it and `parse_list` merges the two.
        let listing = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("list"),
                OsStr::new("-a"),
                OsStr::new("-c"),
                OsStr::new("-u"),
                OsStr::new("-q"),
                OsStr::new("-R"),
                filesystem.as_os_str(),
            ],
        )?;
        let readonly = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("list"),
                OsStr::new("-a"),
                OsStr::new("-r"),
                filesystem.as_os_str(),
            ],
        )?;
        parse::parse_list(&listing, &readonly, &self.fs_uuid, &self.mountpoint)
    }
}

#[cfg(test)]
impl BtrfsCliAdapter {
    /// Test constructor injecting a fake command runner in place of `btrfs`.
    fn with_runner(runner: Box<dyn CommandRunner>, fs_uuid: Uuid, mountpoint: PathBuf) -> Self {
        Self {
            runner,
            fs_uuid,
            mountpoint,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn fs() -> Uuid {
        Uuid::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap()
    }

    fn mountpoint() -> PathBuf {
        PathBuf::from("/mnt/pool")
    }

    const SHOW: &str = "\
@data
    UUID:               a1a1a1a1-1111-4111-8111-111111111111
    Parent UUID:        -
    Received UUID:      -
    Subvolume ID:       256
    Generation:         120
    Gen at creation:    95
    Flags:              -
";

    const LIST: &str = "\
ID 256 gen 120 cgen 95 top level 5 parent_uuid - received_uuid - uuid a1a1a1a1-1111-4111-8111-111111111111 path @data
ID 260 gen 130 cgen 130 top level 5 parent_uuid b2b2b2b2-2222-4222-8222-222222222222 received_uuid a1a1a1a1-1111-4111-8111-111111111111 uuid c3c3c3c3-3333-4333-8333-333333333333 path <FS_TREE>/backups/@data.20260622T1900
";

    const READONLY: &str =
        "ID 260 gen 130 top level 5 path <FS_TREE>/backups/@data.20260622T1900\n";

    struct FakeBtrfs {
        show: String,
        list: String,
        readonly: String,
        fail: bool,
    }

    impl Default for FakeBtrfs {
        fn default() -> Self {
            Self {
                show: SHOW.to_owned(),
                list: LIST.to_owned(),
                readonly: READONLY.to_owned(),
                fail: false,
            }
        }
    }

    impl CommandRunner for FakeBtrfs {
        fn run(&self, _program: &str, args: &[&OsStr]) -> Result<String, PortError> {
            if self.fail {
                return Err(PortError::Command("simulated btrfs failure".to_owned()));
            }
            let has = |needle: &str| args.iter().any(|arg| *arg == OsStr::new(needle));
            // Routing doubles as a flag assertion: wrong flags fall through to Err.
            if has("show") {
                Ok(self.show.clone())
            } else if has("list") && has("-r") {
                Ok(self.readonly.clone())
            } else if has("list") && has("-c") && has("-u") && has("-q") && has("-R") {
                Ok(self.list.clone())
            } else {
                Err(PortError::Command(format!(
                    "unexpected btrfs invocation: {args:?}"
                )))
            }
        }
    }

    fn repo(runner: FakeBtrfs) -> BtrfsCliAdapter {
        BtrfsCliAdapter::with_runner(Box::new(runner), fs(), mountpoint())
    }

    #[test]
    fn show_returns_subvolume_tagged_with_filesystem() {
        let sv = repo(FakeBtrfs::default())
            .show(Path::new("/mnt/pool/@data"))
            .unwrap();
        assert_eq!(sv.id, 256);
        assert_eq!(sv.uuid, Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111"));
        assert!(!sv.readonly);
        assert_eq!(sv.path, PathBuf::from("@data")); // mountpoint stripped from the queried path
        assert_eq!(sv.fs_uuid, fs());
        assert_eq!(sv.mountpoint, mountpoint());
    }

    #[test]
    fn list_merges_readonly_from_second_call() {
        let subs = repo(FakeBtrfs::default())
            .list(Path::new("/mnt/pool"))
            .unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].id, 256);
        assert!(!subs[0].readonly);
        assert_eq!(subs[1].id, 260);
        assert!(subs[1].readonly);
        assert_eq!(subs[1].fs_uuid, fs());
    }

    #[test]
    fn command_failure_propagates() {
        let err = repo(FakeBtrfs {
            fail: true,
            ..FakeBtrfs::default()
        })
        .show(Path::new("/mnt/pool/@data"))
        .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }
}
