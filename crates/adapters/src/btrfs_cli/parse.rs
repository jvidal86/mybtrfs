//! Parsing of `btrfs subvolume show` / `list` output into domain [`Subvolume`]s.
//!
//! Pure functions, separated from command execution so they are unit-testable
//! without spawning `btrfs`. Faithful to btrbk's parsing: `show` is a
//! `Key: value` block (`btrfs_subvolume_show`), `list -c -u -q -R` is one line
//! per subvolume (`btrfs_subvolume_list`), and the read-only flag comes from a
//! separate `list -a -r` call (`btrfs_subvolume_list_readonly_flag`) — so
//! [`parse_list`] takes both outputs and merges them.
//!
//! Context the btrfs output does not carry is supplied by the caller (the
//! adapter, which knows what it queried): `fs_uuid` and `mountpoint` for both,
//! and the subvolume `path` for [`parse_show`] — whose first output line is
//! version-dependent (absolute, relative, or an `is btrfs root` sentinel), so we
//! never scrape it; this targets modern btrfs-progs that emit the regular
//! `Key: value` block.
//!
//! Like btrbk, a present-but-malformed UUID or an implausible id (`< 5`) is a
//! parse **error**, never silently coerced — only the `-` sentinel or an absent
//! field maps to `None` (RULES.md rule 16).
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::{Captures, Regex};

use mybtrfs_application::ports::PortError;
use mybtrfs_domain::model::{Subvolume, Uuid};

/// Prefix btrfs prepends to `list` paths reachable within the queried path.
const FS_TREE_PREFIX: &str = "<FS_TREE>/";
/// btrfs's sentinel for an absent UUID field.
const BTRFS_NONE: &str = "-";
/// Smallest real btrfs subvolume id (5 is the filesystem root); btrbk validates this.
const MIN_SUBVOLUME_ID: u64 = 5;

/// `btrfs subvolume show` field labels (btrfs-progs >= 4.1).
const SHOW_KEY_ID: &str = "Subvolume ID";
const SHOW_KEY_GENERATION: &str = "Generation";
const SHOW_KEY_CGEN: &str = "Gen at creation";
const SHOW_KEY_FLAGS: &str = "Flags";
const SHOW_KEY_UUID: &str = "UUID";
const SHOW_KEY_PARENT_UUID: &str = "Parent UUID";
const SHOW_KEY_RECEIVED_UUID: &str = "Received UUID";
/// `Flags:` value marking a read-only subvolume (btrfs-progs >= 4.6.1).
const FLAG_READONLY: &str = "readonly";

/// Capture-group indices of [`list_line_regex`] (group 4 = "top level", unused —
/// [`Subvolume`] has no top-level field).
const LIST_GROUP_ID: usize = 1;
const LIST_GROUP_GENERATION: usize = 2;
const LIST_GROUP_CGEN: usize = 3;
const LIST_GROUP_PARENT_UUID: usize = 5;
const LIST_GROUP_RECEIVED_UUID: usize = 6;
const LIST_GROUP_UUID: usize = 7;
const LIST_GROUP_PATH: usize = 8;
/// Capture-group index of [`readonly_line_regex`].
const READONLY_GROUP_ID: usize = 1;

/// Parse one `btrfs subvolume show <path>` block into a [`Subvolume`].
///
/// `path` (relative to the fs root), `fs_uuid`, and `mountpoint` are supplied by
/// the caller; the indented `Key: value` lines carry the rest, and read-only is
/// derived from the `Flags` field. The first output line is ignored (see the
/// module docs).
///
/// # Errors
/// [`PortError::Parse`] if a required field (`Subvolume ID`, `Generation`,
/// `Gen at creation`, `Flags`) is missing or non-numeric, if the id is `< 5`, or
/// if a present UUID field is malformed.
pub(crate) fn parse_show(
    output: &str,
    path: PathBuf,
    fs_uuid: &Uuid,
    mountpoint: &Path,
) -> Result<Subvolume, PortError> {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in output.lines() {
        if !line.starts_with(char::is_whitespace) {
            continue; // the first (path) line and any non-field line
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim(), value.trim());
        }
    }

    let flags = fields
        .get(SHOW_KEY_FLAGS)
        .copied()
        .ok_or_else(|| PortError::Parse(format!("missing field: {SHOW_KEY_FLAGS}")))?;

    Ok(Subvolume {
        id: checked_subvolume_id(required_u64(&fields, SHOW_KEY_ID)?, SHOW_KEY_ID)?,
        uuid: parse_uuid_field(fields.get(SHOW_KEY_UUID).copied(), SHOW_KEY_UUID)?,
        parent_uuid: parse_uuid_field(
            fields.get(SHOW_KEY_PARENT_UUID).copied(),
            SHOW_KEY_PARENT_UUID,
        )?,
        received_uuid: parse_uuid_field(
            fields.get(SHOW_KEY_RECEIVED_UUID).copied(),
            SHOW_KEY_RECEIVED_UUID,
        )?,
        generation: required_u64(&fields, SHOW_KEY_GENERATION)?,
        cgen: required_u64(&fields, SHOW_KEY_CGEN)?,
        readonly: flags.contains(FLAG_READONLY),
        path,
        fs_uuid: fs_uuid.clone(),
        mountpoint: mountpoint.to_path_buf(),
    })
}

/// Parse `btrfs subvolume list -c -u -q -R` output, merging the read-only set
/// obtained from a separate `btrfs subvolume list -a -r` call (its only source).
///
/// # Errors
/// [`PortError::Parse`] if any line of either output does not match the expected
/// btrfs-progs format, a numeric field cannot be parsed, an id is `< 5`, or a
/// present UUID field is malformed.
pub(crate) fn parse_list(
    list_output: &str,
    readonly_output: &str,
    fs_uuid: &Uuid,
    mountpoint: &Path,
) -> Result<Vec<Subvolume>, PortError> {
    let readonly_ids = parse_readonly_ids(readonly_output)?;
    let regex = list_line_regex();
    let mut subvolumes = Vec::new();

    for line in list_output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let caps = regex
            .captures(line)
            .ok_or_else(|| PortError::Parse(format!("unparseable subvolume list line: {line}")))?;
        let id = checked_subvolume_id(
            parse_u64(group(&caps, LIST_GROUP_ID, line)?, "id", line)?,
            line,
        )?;

        subvolumes.push(Subvolume {
            id,
            uuid: parse_uuid_field(Some(group(&caps, LIST_GROUP_UUID, line)?), "uuid")?,
            parent_uuid: parse_uuid_field(
                Some(group(&caps, LIST_GROUP_PARENT_UUID, line)?),
                "parent_uuid",
            )?,
            received_uuid: parse_uuid_field(
                Some(group(&caps, LIST_GROUP_RECEIVED_UUID, line)?),
                "received_uuid",
            )?,
            generation: parse_u64(group(&caps, LIST_GROUP_GENERATION, line)?, "gen", line)?,
            cgen: parse_u64(group(&caps, LIST_GROUP_CGEN, line)?, "cgen", line)?,
            readonly: readonly_ids.contains(&id),
            path: strip_fs_tree(group(&caps, LIST_GROUP_PATH, line)?),
            fs_uuid: fs_uuid.clone(),
            mountpoint: mountpoint.to_path_buf(),
        });
    }
    Ok(subvolumes)
}

/// Parse the IDs from `btrfs subvolume list -a -r` (read-only subvolumes only).
fn parse_readonly_ids(output: &str) -> Result<HashSet<u64>, PortError> {
    let regex = readonly_line_regex();
    let mut ids = HashSet::new();
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let caps = regex.captures(line).ok_or_else(|| {
            PortError::Parse(format!("unparseable readonly subvolume list line: {line}"))
        })?;
        ids.insert(parse_u64(
            group(&caps, READONLY_GROUP_ID, line)?,
            "id",
            line,
        )?);
    }
    Ok(ids)
}

/// Strip btrfs's `<FS_TREE>/` prefix, yielding a path relative to the fs root.
fn strip_fs_tree(raw: &str) -> PathBuf {
    PathBuf::from(raw.strip_prefix(FS_TREE_PREFIX).unwrap_or(raw))
}

/// Re-base an fs-root-relative `list` path (relative to the btrfs top level,
/// subvolid 5) to be relative to the **mountpoint**, by stripping the mounted
/// subvolume's path-from-fs-root (`subvol`, taken from the `subvol=` mount
/// option). A top-level mount has `subvol = /`, so the path is returned
/// unchanged; a path not under the mounted subvolume is also returned as-is (it
/// simply won't match any directory under this mountpoint). This makes `list`
/// paths consistent with `show` (mountpoint-relative), so that
/// `mountpoint.join(path)` reconstructs the real on-disk path on any mount layout
/// — not only a subvolid-5 mount.
pub(crate) fn to_mountpoint_relative(fs_root_path: &Path, subvol: &Path) -> PathBuf {
    let prefix = subvol.strip_prefix("/").unwrap_or(subvol);
    fs_root_path
        .strip_prefix(prefix)
        .map_or_else(|_| fs_root_path.to_path_buf(), Path::to_path_buf)
}

/// Parse a btrfs UUID field. The `-` sentinel (and an absent field) map to
/// `None`; a present-but-malformed value is a parse **error**, not silently
/// dropped — btrbk rejects such subvolumes, and coercing a garbled
/// `received_uuid` to `None` would forge the "garbled receive" signal that the
/// transfer-verification invariant keys on (RULES.md rule 16).
fn parse_uuid_field(value: Option<&str>, field: &str) -> Result<Option<Uuid>, PortError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == BTRFS_NONE {
        return Ok(None);
    }
    Uuid::parse(trimmed)
        .map(Some)
        .ok_or_else(|| PortError::Parse(format!("malformed {field} UUID: {trimmed:?}")))
}

/// Reject an implausible subvolume id; btrbk requires `id >= 5` (5 is the root).
fn checked_subvolume_id(id: u64, context: &str) -> Result<u64, PortError> {
    if id < MIN_SUBVOLUME_ID {
        return Err(PortError::Parse(format!(
            "implausible subvolume id {id} (< {MIN_SUBVOLUME_ID}) in '{context}'"
        )));
    }
    Ok(id)
}

/// Look up a required `show` field and parse it as a `u64`.
fn required_u64(fields: &HashMap<&str, &str>, key: &str) -> Result<u64, PortError> {
    let raw = fields
        .get(key)
        .copied()
        .ok_or_else(|| PortError::Parse(format!("missing field: {key}")))?;
    raw.parse::<u64>()
        .map_err(|err| PortError::Parse(format!("invalid {key} '{raw}': {err}")))
}

/// Parse `value` as a `u64`, attributing failures to `field` within `context`.
fn parse_u64(value: &str, field: &str, context: &str) -> Result<u64, PortError> {
    value
        .parse::<u64>()
        .map_err(|err| PortError::Parse(format!("invalid {field} '{value}' in '{context}': {err}")))
}

/// Parse the filesystem UUID from `btrfs filesystem show <path>` output (a
/// summary line `Label: '…'  uuid: <fs-uuid>`).
///
/// # Errors
/// [`PortError::Parse`] if no `uuid:` field is present or it is malformed.
pub(crate) fn parse_filesystem_uuid(output: &str) -> Result<Uuid, PortError> {
    output
        .split_whitespace()
        .skip_while(|token| *token != "uuid:")
        .nth(1)
        .and_then(Uuid::parse)
        .ok_or_else(|| {
            PortError::Parse("no filesystem uuid in `btrfs filesystem show` output".to_owned())
        })
}

/// Parse the "Referenced:" byte count from `btrfs subvolume show` output.
///
/// The field looks like `    Referenced:         1.23GiB` (using btrfs-progs'
/// human-readable units: B, KiB, MiB, GiB, TiB, PiB, EiB). A missing or malformed
/// field is a parse **error** (rule 16): callers need a real byte count, not 0.
///
/// # Errors
/// [`PortError::Parse`] if the "Referenced:" field is absent or its value cannot
/// be decoded as `<number> <unit>`.
pub(crate) fn parse_referenced_bytes(show_output: &str) -> Result<u64, PortError> {
    const KEY: &str = "Referenced:";
    let raw = show_output
        .lines()
        .find_map(|l| l.trim().strip_prefix(KEY))
        .ok_or_else(|| {
            PortError::Parse("missing 'Referenced:' field in btrfs subvolume show output".into())
        })?
        .trim();
    parse_btrfs_size(raw)
        .ok_or_else(|| PortError::Parse(format!("malformed 'Referenced:' value: {raw:?}")))
}

/// Parse the total changed-bytes count from `btrfs subvolume find-new` output.
///
/// Each changed extent is reported as a line starting with `inode` containing a
/// `len <N>` field. This function sums all `len` values. The final
/// `transid marker was <N>` line is skipped. Returns `0` when no extents were
/// modified (the output is only the transid line).
///
/// # Errors
/// [`PortError::Parse`] if a line starts with `inode` but does not contain a
/// parseable `len <N>` field — a present-but-malformed value is an error, not a
/// silent skip (rule 16).
pub(crate) fn parse_find_new_changed_bytes(find_new_output: &str) -> Result<u64, PortError> {
    let mut total: u64 = 0;
    for line in find_new_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("transid") {
            continue;
        }
        if !trimmed.starts_with("inode") {
            return Err(PortError::Parse(format!(
                "unexpected line in btrfs subvolume find-new output: {trimmed:?}"
            )));
        }
        // Extract `len <N>` from the inode line.
        let len_bytes = trimmed
            .split_whitespace()
            .skip_while(|t| *t != "len")
            .nth(1)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                PortError::Parse(format!(
                    "missing or non-numeric 'len' field in find-new line: {trimmed:?}"
                ))
            })?;
        total = total.saturating_add(len_bytes);
    }
    Ok(total)
}

/// Parse a btrfs human-readable size string (e.g. `"1.23GiB"`, `"512.00MiB"`).
/// Returns `None` for unrecognised formats.
fn parse_btrfs_size(s: &str) -> Option<u64> {
    // Find the split between the numeric part and the unit suffix.
    let split = s.find(|c: char| c.is_alphabetic())?;
    let (num_str, unit) = s.split_at(split);
    let num: f64 = num_str.trim().parse().ok()?;
    let multiplier: f64 = match unit.trim() {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "PiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "EiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((num * multiplier) as u64)
}

/// Extract a (mandatory) capture group as a string slice.
fn group<'a>(caps: &Captures<'a>, index: usize, line: &str) -> Result<&'a str, PortError> {
    caps.get(index)
        .map(|m| m.as_str())
        .ok_or_else(|| PortError::Parse(format!("missing capture group {index} in line: {line}")))
}

#[allow(clippy::expect_used)] // compile-time-constant pattern; cannot fail at runtime
fn list_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            ^ ID            \s+ (\d+)  \s+
              gen           \s+ (\d+)  \s+
              cgen          \s+ (\d+)  \s+
              top\ level    \s+ (\d+)  \s+
              parent_uuid   \s+ (\S+)  \s+
              received_uuid \s+ (\S+)  \s+
              uuid          \s+ (\S+)  \s+
              path          \s+ (.+?)  \s* $
            ",
        )
        .expect("btrfs subvolume list line regex is valid")
    })
}

#[allow(clippy::expect_used)] // compile-time-constant pattern; cannot fail at runtime
fn readonly_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^ID\s+(\d+)\s+gen\s+\d+\s+top level\s+\d+\s+path\s")
            .expect("btrfs readonly subvolume list line regex is valid")
    })
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

    const SHOW_WRITABLE: &str = "\
@data
    Name:               @data
    UUID:               a1a1a1a1-1111-4111-8111-111111111111
    Parent UUID:        -
    Received UUID:      -
    Creation time:      2026-06-22 19:00:00 +0000
    Subvolume ID:       256
    Generation:         120
    Gen at creation:    95
    Parent ID:          5
    Top level ID:       5
    Flags:              -
    Snapshot(s):
";

    const SHOW_READONLY_RECEIVED: &str = "\
backups/@data.20260622T1900
    Name:               @data.20260622T1900
    UUID:               c3c3c3c3-3333-4333-8333-333333333333
    Parent UUID:        b2b2b2b2-2222-4222-8222-222222222222
    Received UUID:      a1a1a1a1-1111-4111-8111-111111111111
    Creation time:      2026-06-22 19:05:00 +0000
    Subvolume ID:       260
    Generation:         130
    Gen at creation:    130
    Parent ID:          5
    Top level ID:       5
    Flags:              readonly
    Snapshot(s):
";

    const SHOW_BAD_GENERATION: &str = "\
@data
    UUID:               a1a1a1a1-1111-4111-8111-111111111111
    Subvolume ID:       256
    Generation:         not-a-number
    Gen at creation:    95
    Flags:              -
";

    const LIST: &str = "\
ID 256 gen 120 cgen 95 top level 5 parent_uuid - received_uuid - uuid a1a1a1a1-1111-4111-8111-111111111111 path @data
ID 260 gen 130 cgen 130 top level 5 parent_uuid b2b2b2b2-2222-4222-8222-222222222222 received_uuid a1a1a1a1-1111-4111-8111-111111111111 uuid c3c3c3c3-3333-4333-8333-333333333333 path <FS_TREE>/backups/@data.20260622T1900
";

    const READONLY_LIST: &str = "\
ID 260 gen 130 top level 5 path <FS_TREE>/backups/@data.20260622T1900
";

    #[test]
    fn show_parses_writable_subvolume() {
        crate::init_test_logger();
        let sv = parse_show(SHOW_WRITABLE, PathBuf::from("@data"), &fs(), &mountpoint()).unwrap();
        assert_eq!(sv.id, 256);
        assert_eq!(sv.uuid, Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111"));
        assert_eq!(sv.parent_uuid, None);
        assert_eq!(sv.received_uuid, None);
        assert_eq!(sv.generation, 120);
        assert_eq!(sv.cgen, 95);
        assert!(!sv.readonly);
        assert_eq!(sv.path, PathBuf::from("@data"));
        assert_eq!(sv.fs_uuid, fs());
        assert_eq!(sv.mountpoint, mountpoint());
    }

    #[test]
    fn show_parses_readonly_received_snapshot() {
        crate::init_test_logger();
        let sv = parse_show(
            SHOW_READONLY_RECEIVED,
            PathBuf::from("backups/@data.20260622T1900"),
            &fs(),
            &mountpoint(),
        )
        .unwrap();
        assert_eq!(sv.id, 260);
        assert!(sv.readonly);
        assert_eq!(
            sv.parent_uuid,
            Uuid::parse("b2b2b2b2-2222-4222-8222-222222222222")
        );
        assert_eq!(
            sv.received_uuid,
            Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111")
        );
        assert_eq!(sv.cgen, 130);
    }

    #[test]
    fn show_missing_required_field_is_error() {
        crate::init_test_logger();
        let broken = SHOW_WRITABLE.replace(SHOW_KEY_ID, "Bogus Field");
        let err = parse_show(&broken, PathBuf::from("@data"), &fs(), &mountpoint()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn show_non_numeric_field_is_error() {
        crate::init_test_logger();
        let err = parse_show(
            SHOW_BAD_GENERATION,
            PathBuf::from("@data"),
            &fs(),
            &mountpoint(),
        )
        .unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn show_rejects_malformed_uuid() {
        crate::init_test_logger();
        let broken =
            SHOW_READONLY_RECEIVED.replace("a1a1a1a1-1111-4111-8111-111111111111", "not-a-uuid");
        let err = parse_show(
            &broken,
            PathBuf::from("backups/@data.20260622T1900"),
            &fs(),
            &mountpoint(),
        )
        .unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn show_rejects_implausible_id() {
        crate::init_test_logger();
        let broken = SHOW_WRITABLE.replace("256", "3"); // id 3 < MIN_SUBVOLUME_ID
        let err = parse_show(&broken, PathBuf::from("@data"), &fs(), &mountpoint()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn list_parses_and_merges_readonly_flag() {
        crate::init_test_logger();
        let subs = parse_list(LIST, READONLY_LIST, &fs(), &mountpoint()).unwrap();
        assert_eq!(subs.len(), 2);

        let data = &subs[0];
        assert_eq!(data.id, 256);
        assert_eq!(
            data.uuid,
            Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111")
        );
        assert_eq!(data.parent_uuid, None);
        assert_eq!(data.received_uuid, None);
        assert_eq!(data.generation, 120);
        assert_eq!(data.cgen, 95);
        assert!(!data.readonly); // absent from the `-r` list
        assert_eq!(data.path, PathBuf::from("@data"));
        assert_eq!(data.fs_uuid, fs());
        assert_eq!(data.mountpoint, mountpoint());

        let snapshot = &subs[1];
        assert_eq!(snapshot.id, 260);
        assert_eq!(
            snapshot.parent_uuid,
            Uuid::parse("b2b2b2b2-2222-4222-8222-222222222222")
        );
        assert_eq!(
            snapshot.received_uuid,
            Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111")
        );
        assert!(snapshot.readonly); // present in the `-r` list → merged
        assert_eq!(snapshot.path, PathBuf::from("backups/@data.20260622T1900")); // `<FS_TREE>/` stripped
    }

    #[test]
    fn to_mountpoint_relative_rebases_against_the_mount_subvol() {
        crate::init_test_logger();
        // Non-root mount `subvol=/@pool`: list yields fs-root-relative paths;
        // rebasing strips the mounted subvolume prefix → mountpoint-relative.
        assert_eq!(
            to_mountpoint_relative(Path::new("@pool/snapshots/home.X"), Path::new("/@pool")),
            PathBuf::from("snapshots/home.X")
        );
        // Top-level mount `subvol=/`: the path is unchanged.
        assert_eq!(
            to_mountpoint_relative(Path::new("snapshots/home.X"), Path::new("/")),
            PathBuf::from("snapshots/home.X")
        );
        // A subvolume elsewhere in the filesystem is left as-is.
        assert_eq!(
            to_mountpoint_relative(Path::new("@other/x"), Path::new("/@pool")),
            PathBuf::from("@other/x")
        );
    }

    #[test]
    fn list_empty_output_is_empty() {
        crate::init_test_logger();
        let subs = parse_list("", "", &fs(), &mountpoint()).unwrap();
        assert!(subs.is_empty());
    }

    #[test]
    fn list_malformed_line_is_error() {
        crate::init_test_logger();
        let err = parse_list("ID 256 gen 120 garbage\n", "", &fs(), &mountpoint()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn list_rejects_malformed_readonly_line() {
        crate::init_test_logger();
        let err =
            parse_list(LIST, "not a valid readonly line\n", &fs(), &mountpoint()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn list_rejects_malformed_uuid() {
        crate::init_test_logger();
        let line = "ID 256 gen 120 cgen 95 top level 5 parent_uuid - received_uuid - uuid GARBAGE path @data\n";
        let err = parse_list(line, "", &fs(), &mountpoint()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    // ── parse_referenced_bytes ─────────────────────────────────────────────

    const SHOW_WITH_REFERENCED: &str = "\
@data
    Name:                 @data
    UUID:                 a1a1a1a1-1111-4111-8111-111111111111
    Parent UUID:          -
    Received UUID:        -
    Creation time:        2026-06-22 19:00:00 +0000
    Subvolume ID:         256
    Generation:           120
    Gen at creation:      95
    Parent ID:            5
    Top level ID:         5
    Flags:                -
    Referenced:           1.23GiB
    Exclusive:            256.00MiB
    Snapshot(s):
";

    #[test]
    fn referenced_bytes_parses_gib() {
        let bytes = parse_referenced_bytes(SHOW_WITH_REFERENCED).unwrap();
        // 1.23 * 1024^3 = 1_320_702_443 bytes; allow ±1 for floating-point rounding
        assert!(
            (1_320_702_000..=1_321_000_000).contains(&bytes),
            "unexpected: {bytes}"
        );
    }

    #[test]
    fn referenced_bytes_parses_mib() {
        let output = "    Referenced:           512.00MiB\n";
        let bytes = parse_referenced_bytes(output).unwrap();
        assert_eq!(bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn referenced_bytes_parses_kib() {
        let output = "    Referenced:           4.00KiB\n";
        let bytes = parse_referenced_bytes(output).unwrap();
        assert_eq!(bytes, 4 * 1024);
    }

    #[test]
    fn referenced_bytes_parses_pib() {
        let output = "    Referenced:           1.00PiB\n";
        let bytes = parse_referenced_bytes(output).unwrap();
        assert_eq!(bytes, 1024u64 * 1024 * 1024 * 1024 * 1024);
    }

    #[test]
    fn referenced_bytes_parses_eib() {
        let output = "    Referenced:           1.00EiB\n";
        let bytes = parse_referenced_bytes(output).unwrap();
        assert_eq!(bytes, 1024u64 * 1024 * 1024 * 1024 * 1024 * 1024);
    }

    #[test]
    fn referenced_bytes_missing_field_is_error() {
        let output = "    UUID: a1a1a1a1-1111-4111-8111-111111111111\n";
        let err = parse_referenced_bytes(output).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn referenced_bytes_malformed_value_is_error() {
        let output = "    Referenced:           not-a-size\n";
        let err = parse_referenced_bytes(output).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    // ── parse_find_new_changed_bytes ───────────────────────────────────────

    const FIND_NEW_WITH_CHANGES: &str = "\
inode 258 file offset 0 len 131072 disk start 24117248 offset 0 gen 12 flags 0x1
inode 258 file offset 131072 len 65536 disk start 24248320 offset 0 gen 12 flags 0x1
inode 259 file offset 0 len 4096 disk start 0 offset 0 gen 12 flags 0x0
transid marker was 12
";

    #[test]
    fn find_new_sums_len_fields() {
        let bytes = parse_find_new_changed_bytes(FIND_NEW_WITH_CHANGES).unwrap();
        assert_eq!(bytes, 131072 + 65536 + 4096);
    }

    #[test]
    fn find_new_empty_output_is_zero() {
        // Only the transid line → nothing changed.
        let bytes = parse_find_new_changed_bytes("transid marker was 120\n").unwrap();
        assert_eq!(bytes, 0);
    }

    #[test]
    fn find_new_truly_empty_output_is_zero() {
        let bytes = parse_find_new_changed_bytes("").unwrap();
        assert_eq!(bytes, 0);
    }

    #[test]
    fn find_new_unexpected_line_is_error() {
        let output = "garbage line that is not inode or transid\n";
        let err = parse_find_new_changed_bytes(output).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn find_new_missing_len_field_is_error() {
        let output = "inode 258 file offset 0 disk start 0\n"; // no `len` token
        let err = parse_find_new_changed_bytes(output).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }
}
