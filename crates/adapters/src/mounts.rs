//! Mount-table resolution: which btrfs filesystem (mountpoint) contains a path.
//!
//! Used to tag each parsed `Subvolume` with the filesystem it lives on (the
//! btrfs output carries neither the mountpoint nor the fs UUID), and the
//! groundwork for drive discovery. The parser is pure; reading
//! `/proc/self/mounts` is behind the [`MountTable`] seam so the adapter stays
//! unit-testable.

use std::path::{Path, PathBuf};

use mybtrfs_application::ports::PortError;

/// The kernel mount table.
const PROC_MOUNTS: &str = "/proc/self/mounts";
/// The btrfs filesystem type as it appears in the mount table.
const BTRFS_FSTYPE: &str = "btrfs";

/// One filesystem from the kernel mount table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountEntry {
    /// Where the filesystem is mounted.
    pub mountpoint: PathBuf,
    /// Filesystem type (e.g. `btrfs`).
    pub fstype: String,
    /// The mounted subvolume's path from the filesystem root, from the `subvol=`
    /// mount option (e.g. `/@pool`); `/` for a top-level (subvolid 5) mount. Used
    /// to re-base `btrfs subvolume list` paths (which are fs-root-relative) to be
    /// mountpoint-relative.
    pub subvol: PathBuf,
}

/// Reads the kernel mount table (injectable so the adapter stays testable).
pub(crate) trait MountTable {
    /// Read and parse the current mount table.
    ///
    /// # Errors
    /// [`PortError::Io`] if it cannot be read; [`PortError::Parse`] on a malformed line.
    fn entries(&self) -> Result<Vec<MountEntry>, PortError>;
}

/// Production [`MountTable`] over `/proc/self/mounts`.
pub(crate) struct ProcMounts;

impl MountTable for ProcMounts {
    fn entries(&self) -> Result<Vec<MountEntry>, PortError> {
        let content = std::fs::read_to_string(PROC_MOUNTS)?;
        parse_mounts(&content)
    }
}

/// Parse mount-table content (`device mountpoint fstype …` per line, with the
/// kernel's octal-escaped whitespace in the path fields).
pub(crate) fn parse_mounts(content: &str) -> Result<Vec<MountEntry>, PortError> {
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (_device, Some(mountpoint), Some(fstype), options) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(PortError::Parse(format!("malformed mount line: {line}")));
        };
        entries.push(MountEntry {
            mountpoint: PathBuf::from(unescape_octal(mountpoint)),
            fstype: fstype.to_owned(),
            subvol: subvol_from_options(options),
        });
    }
    Ok(entries)
}

/// The mounted subvolume's path from the comma-separated mount options
/// (`subvol=/@pool`), octal-unescaped. Defaults to `/` (top level) when no
/// `subvol=` option is present.
fn subvol_from_options(options: Option<&str>) -> PathBuf {
    options
        .and_then(|opts| opts.split(',').find_map(|opt| opt.strip_prefix("subvol=")))
        .map_or_else(
            || PathBuf::from("/"),
            |raw| PathBuf::from(unescape_octal(raw)),
        )
}

/// The btrfs mount whose mountpoint is the longest prefix of `path` — the
/// filesystem that actually contains `path`.
pub(crate) fn containing_btrfs_mount<'a>(
    entries: &'a [MountEntry],
    path: &Path,
) -> Option<&'a MountEntry> {
    entries
        .iter()
        .filter(|entry| entry.fstype == BTRFS_FSTYPE && path.starts_with(&entry.mountpoint))
        .max_by_key(|entry| entry.mountpoint.as_os_str().len())
}

/// Decode the kernel's octal escapes in a mount path (`\040` space, `\011` tab,
/// `\012` newline, `\134` backslash).
fn unescape_octal(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        if digits.len() == 3
            && digits.bytes().all(|b| (b'0'..=b'7').contains(&b))
            && let Ok(byte) = u8::from_str_radix(&digits, 8)
        {
            out.push(byte as char);
            chars.nth(2); // consume the three octal digits
            continue;
        }
        out.push('\\');
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const MOUNTS: &str = "\
/dev/sda1 / ext4 rw 0 0
/dev/sdb1 /mnt/pool btrfs rw,relatime 0 0
/dev/sdc1 /mnt/drive btrfs rw 0 0
/dev/sdb1 /mnt/pool/nested btrfs rw 0 0
tmpfs /run tmpfs rw 0 0
";

    #[test]
    fn parses_mount_entries() {
        crate::init_test_logger();
        let entries = parse_mounts(MOUNTS).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[1].mountpoint, PathBuf::from("/mnt/pool"));
        assert_eq!(entries[1].fstype, "btrfs");
    }

    #[test]
    fn parses_the_mount_subvol_option() {
        crate::init_test_logger();
        let entries = parse_mounts(
            "/dev/sdb1 /mnt/pool btrfs rw,relatime,subvolid=256,subvol=/@pool 0 0\n/dev/sdc1 /mnt/drive btrfs rw 0 0\n",
        )
        .unwrap();
        assert_eq!(entries[0].subvol, PathBuf::from("/@pool"));
        // No `subvol=` option → defaults to the top level.
        assert_eq!(entries[1].subvol, PathBuf::from("/"));
    }

    #[test]
    fn finds_longest_containing_btrfs_mount() {
        crate::init_test_logger();
        let entries = parse_mounts(MOUNTS).unwrap();
        let nested = containing_btrfs_mount(&entries, Path::new("/mnt/pool/nested/sub")).unwrap();
        assert_eq!(nested.mountpoint, PathBuf::from("/mnt/pool/nested"));
        let pool = containing_btrfs_mount(&entries, Path::new("/mnt/pool/home")).unwrap();
        assert_eq!(pool.mountpoint, PathBuf::from("/mnt/pool"));
    }

    #[test]
    fn ignores_non_btrfs_filesystems() {
        crate::init_test_logger();
        let entries = parse_mounts(MOUNTS).unwrap();
        // `/` is ext4, so a path only under `/` has no containing btrfs mount.
        assert!(containing_btrfs_mount(&entries, Path::new("/etc/fstab")).is_none());
    }

    #[test]
    fn unescapes_octal_whitespace_in_mountpoints() {
        crate::init_test_logger();
        let entries = parse_mounts("/dev/sdb1 /mnt/my\\040pool btrfs rw 0 0\n").unwrap();
        assert_eq!(entries[0].mountpoint, PathBuf::from("/mnt/my pool"));
    }

    #[test]
    fn rejects_a_malformed_line() {
        crate::init_test_logger();
        let err = parse_mounts("only-one-field\n").unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }
}
