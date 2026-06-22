//! Drive discovery — implements `DriveDiscoveryPort` by enumerating mounted btrfs
//! filesystems via `lsblk --json` (one source for device / mountpoint / label /
//! uuid / removable). Read-only; never acts on a device. Parsing is pure (tested
//! against fixtures); the `lsblk` call goes through the [`CommandRunner`] seam.
//! See `documentation/01` Phase 1.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::PathBuf;

use serde::Deserialize;

use mybtrfs_application::ports::{DiscoveredFilesystem, DriveDiscoveryPort, PortError};
use mybtrfs_domain::model::Uuid;

use crate::command::{CommandRunner, SystemCommandRunner};

/// External program (spawned as an argv array, never via a shell).
const LSBLK: &str = "lsblk";
/// The btrfs filesystem type as `lsblk` reports it.
const BTRFS_FSTYPE: &str = "btrfs";

/// Enumerates btrfs backup-target candidates via `lsblk --json`.
pub struct LsblkDriveDiscovery {
    runner: Box<dyn CommandRunner>,
}

impl LsblkDriveDiscovery {
    /// Create a discovery adapter that runs the real `lsblk`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: Box::new(SystemCommandRunner),
        }
    }
}

impl Default for LsblkDriveDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

impl DriveDiscoveryPort for LsblkDriveDiscovery {
    fn detect(&self) -> Result<Vec<DiscoveredFilesystem>, PortError> {
        let json = self.runner.run(
            LSBLK,
            &[
                OsStr::new("--json"),
                OsStr::new("-o"),
                OsStr::new("PATH,FSTYPE,MOUNTPOINT,LABEL,UUID,RM"),
            ],
        )?;
        parse_lsblk(&json)
    }
}

/// `lsblk --json` top-level object.
#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<LsblkDevice>,
}

/// One lsblk node (a disk or a partition; partitions appear under `children`).
#[derive(Deserialize)]
struct LsblkDevice {
    path: Option<String>,
    fstype: Option<String>,
    mountpoint: Option<String>,
    label: Option<String>,
    uuid: Option<String>,
    #[serde(default)]
    rm: bool,
    #[serde(default)]
    children: Vec<LsblkDevice>,
}

/// Parse `lsblk --json` output into the mounted btrfs filesystems — one entry per
/// filesystem uuid (extra subvolume mounts of the same fs are deduplicated).
///
/// # Errors
/// [`PortError::Parse`] if the output is not valid lsblk json.
fn parse_lsblk(json: &str) -> Result<Vec<DiscoveredFilesystem>, PortError> {
    let parsed: Lsblk = serde_json::from_str(json)
        .map_err(|err| PortError::Parse(format!("invalid lsblk json: {err}")))?;

    let mut found = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut stack: Vec<LsblkDevice> = parsed.blockdevices;
    while let Some(device) = stack.pop() {
        let candidate = btrfs_filesystem(&device);
        stack.extend(device.children);
        if let Some(fs) = candidate
            && seen.insert(fs.fs_uuid.clone())
        {
            found.push(fs);
        }
    }
    Ok(found)
}

/// Map a mounted btrfs device to a [`DiscoveredFilesystem`] (else `None`).
fn btrfs_filesystem(device: &LsblkDevice) -> Option<DiscoveredFilesystem> {
    if device.fstype.as_deref() != Some(BTRFS_FSTYPE) {
        return None;
    }
    let mountpoint = device.mountpoint.as_deref()?;
    let fs_uuid = device.uuid.as_deref().and_then(Uuid::parse)?;
    Some(DiscoveredFilesystem {
        device: device
            .path
            .as_deref()
            .map_or_else(PathBuf::new, PathBuf::from),
        mountpoint: PathBuf::from(mountpoint),
        fs_uuid,
        label: device.label.clone().filter(|label| !label.is_empty()),
        removable: device.rm,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A `CommandRunner` returning canned `lsblk` json.
    struct FakeLsblk(String);
    impl CommandRunner for FakeLsblk {
        fn run(&self, _program: &str, _args: &[&OsStr]) -> Result<String, PortError> {
            Ok(self.0.clone())
        }
        fn pipe(
            &self,
            _producer: (&str, &[&OsStr]),
            _consumer: (&str, &[&OsStr]),
        ) -> Result<(), PortError> {
            Err(PortError::Command(
                "drive discovery does not pipe".to_owned(),
            ))
        }
    }

    fn discovery(json: &str) -> LsblkDriveDiscovery {
        LsblkDriveDiscovery {
            runner: Box::new(FakeLsblk(json.to_owned())),
        }
    }

    const LSBLK_JSON: &str = r#"{ "blockdevices": [
        { "path": "/dev/sda", "fstype": null, "mountpoint": null, "label": null, "uuid": null, "rm": false,
          "children": [ { "path": "/dev/sda1", "fstype": "btrfs", "mountpoint": "/mnt/pool", "label": "pool", "uuid": "11111111-1111-4111-8111-111111111111", "rm": false } ] },
        { "path": "/dev/sdb", "fstype": null, "mountpoint": null, "label": null, "uuid": null, "rm": true,
          "children": [ { "path": "/dev/sdb1", "fstype": "btrfs", "mountpoint": "/mnt/drive", "label": "backup", "uuid": "22222222-2222-4222-8222-222222222222", "rm": true } ] },
        { "path": "/dev/sdc1", "fstype": "ext4", "mountpoint": "/", "label": null, "uuid": "33333333-3333-4333-8333-333333333333", "rm": false }
    ] }"#;

    #[test]
    fn detects_mounted_btrfs_filesystems_with_hints() {
        let found = discovery(LSBLK_JSON).detect().unwrap();
        assert_eq!(found.len(), 2);

        let drive = found
            .iter()
            .find(|f| f.mountpoint.to_str() == Some("/mnt/drive"))
            .unwrap();
        assert_eq!(drive.device.to_str(), Some("/dev/sdb1"));
        assert_eq!(drive.label.as_deref(), Some("backup"));
        assert!(drive.removable);
        assert_eq!(
            drive.fs_uuid,
            Uuid::parse("22222222-2222-4222-8222-222222222222").unwrap()
        );

        let pool = found
            .iter()
            .find(|f| f.mountpoint.to_str() == Some("/mnt/pool"))
            .unwrap();
        assert!(!pool.removable);

        // The ext4 root filesystem is excluded.
        assert!(found.iter().all(|f| f.mountpoint.to_str() != Some("/")));
    }

    #[test]
    fn deduplicates_multiple_mounts_of_one_filesystem() {
        let json = r#"{ "blockdevices": [
            { "path": "/dev/sdb1", "fstype": "btrfs", "mountpoint": "/mnt/a", "label": null, "uuid": "22222222-2222-4222-8222-222222222222", "rm": false },
            { "path": "/dev/sdb1", "fstype": "btrfs", "mountpoint": "/mnt/b", "label": null, "uuid": "22222222-2222-4222-8222-222222222222", "rm": false }
        ] }"#;
        let found = discovery(json).detect().unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn rejects_invalid_json() {
        let err = discovery("not json").detect().unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }
}
