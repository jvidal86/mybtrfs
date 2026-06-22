//! Domain model: the subvolume and its relationship graph.
//!
//! - `Uuid` — a validated, canonical-lowercase subvolume/filesystem UUID; the
//!   btrfs `"-"` sentinel maps to `None` at the boundary.
//! - `Subvolume` — id, three UUIDs, gen/cgen, readonly, path, owning filesystem
//!   UUID, mountpoint.
//! - `RelationshipGraph` — per-filesystem indexes: `uuid` one-to-one (duplicates
//!   are rejected — invariant #10), `parent_uuid`/`received_uuid` one-to-many.
//!
//! `RetentionPolicy`, `Schedule`, and `ParentSelection` are defined alongside
//! their consumers (the retention and parent increments) to avoid speculative
//! types. See `documentation/02-architecture-v2.md` §3.
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use std::collections::HashMap;
use std::path::PathBuf;

/// A btrfs subvolume/filesystem UUID in canonical lowercase form
/// (`8-4-4-4-12` hex).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Uuid(String);

impl Uuid {
    /// Parse a canonical UUID, normalizing to lowercase. `None` if malformed.
    pub fn parse(s: &str) -> Option<Uuid> {
        let s = s.trim();
        if is_canonical_uuid(s) {
            Some(Uuid(s.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// Interpret a UUID field from btrfs output: the `"-"` sentinel (and empty)
    /// map to `None`; anything else is parsed (malformed also yields `None`).
    pub fn from_btrfs(s: &str) -> Option<Uuid> {
        let t = s.trim();
        if t.is_empty() || t == "-" {
            None
        } else {
            Uuid::parse(t)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Uuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_canonical_uuid(s: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == groups.len()
        && parts
            .iter()
            .zip(groups)
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A btrfs subvolume as mybtrfs models it. The three UUID fields drive all
/// relationship tracking; the btrfs `"-"` sentinel is represented as `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subvolume {
    pub id: u64,
    pub uuid: Option<Uuid>,
    pub parent_uuid: Option<Uuid>,
    pub received_uuid: Option<Uuid>,
    /// Current generation.
    pub generation: u64,
    /// Generation at creation ("cgen").
    pub cgen: u64,
    pub readonly: bool,
    pub path: PathBuf,
    /// UUID of the filesystem this subvolume lives on (relationship indexes and
    /// reachability checks are per filesystem).
    pub fs_uuid: Uuid,
    /// Mountpoint of the owning filesystem (parent reachability is per mountpoint).
    pub mountpoint: PathBuf,
}

impl Subvolume {
    /// An incompletely received ("garbled") subvolume: writable with no
    /// received_uuid. btrfs leaves these behind on an interrupted receive.
    pub fn is_garbled(&self) -> bool {
        !self.readonly && self.received_uuid.is_none()
    }

    /// The generation used to order/reference this subvolume: `cgen` for
    /// read-only subvolumes, `generation` for read-write ones.
    pub fn reference_generation(&self) -> u64 {
        if self.readonly {
            self.cgen
        } else {
            self.generation
        }
    }
}

/// Errors from constructing a [`RelationshipGraph`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("duplicate subvolume uuid: {0}")]
    DuplicateUuid(Uuid),
}

/// Per-filesystem index of subvolumes by their UUID relationships.
///
/// `uuid` is a one-to-one key (duplicates are rejected — a cloned disk would
/// otherwise corrupt relationship tracking); `parent_uuid` and `received_uuid`
/// are one-to-many. Subvolumes without a `uuid` are retained in [`all`] but not
/// indexed by uuid.
#[derive(Debug)]
pub struct RelationshipGraph {
    subvols: Vec<Subvolume>,
    by_uuid: HashMap<Uuid, usize>,
    by_parent_uuid: HashMap<Uuid, Vec<usize>>,
    by_received_uuid: HashMap<Uuid, Vec<usize>>,
}

impl RelationshipGraph {
    /// Build the indexes, rejecting a duplicate `uuid`.
    pub fn build(subvols: Vec<Subvolume>) -> Result<Self, GraphError> {
        let mut by_uuid: HashMap<Uuid, usize> = HashMap::new();
        let mut by_parent_uuid: HashMap<Uuid, Vec<usize>> = HashMap::new();
        let mut by_received_uuid: HashMap<Uuid, Vec<usize>> = HashMap::new();

        for (i, sv) in subvols.iter().enumerate() {
            if let Some(uuid) = &sv.uuid
                && by_uuid.insert(uuid.clone(), i).is_some()
            {
                return Err(GraphError::DuplicateUuid(uuid.clone()));
            }
            if let Some(parent) = &sv.parent_uuid {
                by_parent_uuid.entry(parent.clone()).or_default().push(i);
            }
            if let Some(received) = &sv.received_uuid {
                by_received_uuid
                    .entry(received.clone())
                    .or_default()
                    .push(i);
            }
        }

        Ok(RelationshipGraph {
            subvols,
            by_uuid,
            by_parent_uuid,
            by_received_uuid,
        })
    }

    /// The subvolume with this `uuid`, if present.
    pub fn get(&self, uuid: &Uuid) -> Option<&Subvolume> {
        self.by_uuid.get(uuid).map(|&i| &self.subvols[i])
    }

    /// Subvolumes whose `parent_uuid` equals `parent` ("is-snapshot-of").
    pub fn children_of(&self, parent: &Uuid) -> Vec<&Subvolume> {
        self.indexed(self.by_parent_uuid.get(parent))
    }

    /// Subvolumes whose `received_uuid` equals `source` (received from it).
    pub fn received_from(&self, source: &Uuid) -> Vec<&Subvolume> {
        self.indexed(self.by_received_uuid.get(source))
    }

    /// All subvolumes (including any without a uuid).
    pub fn all(&self) -> &[Subvolume] {
        &self.subvols
    }

    fn indexed(&self, idx: Option<&Vec<usize>>) -> Vec<&Subvolume> {
        idx.map_or_else(Vec::new, |is| {
            is.iter().map(|&i| &self.subvols[i]).collect()
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a canonical test UUID from a single hex tag, e.g. `u("a")`.
    fn u(tag: &str) -> Uuid {
        let t = tag.repeat(32);
        let canonical = format!(
            "{}-{}-{}-{}-{}",
            &t[0..8],
            &t[8..12],
            &t[12..16],
            &t[16..20],
            &t[20..32]
        );
        Uuid::parse(&canonical).expect("valid test uuid")
    }

    /// Concise `Subvolume` builder for tests.
    fn sv(
        id: u64,
        uuid: Option<&str>,
        parent: Option<&str>,
        received: Option<&str>,
        readonly: bool,
    ) -> Subvolume {
        Subvolume {
            id,
            uuid: uuid.map(u),
            parent_uuid: parent.map(u),
            received_uuid: received.map(u),
            generation: 100,
            cgen: 50,
            readonly,
            path: PathBuf::from(format!("/mnt/pool/sv{id}")),
            fs_uuid: u("f"),
            mountpoint: PathBuf::from("/mnt/pool"),
        }
    }

    // --- Uuid ---

    #[test]
    fn uuid_parses_and_normalizes_case() {
        let parsed = Uuid::parse("AABBCCDD-EEFF-0011-2233-445566778899").unwrap();
        assert_eq!(parsed.as_str(), "aabbccdd-eeff-0011-2233-445566778899");
    }

    #[test]
    fn uuid_rejects_malformed() {
        assert!(Uuid::parse("not-a-uuid").is_none());
        assert!(Uuid::parse("").is_none());
        assert!(Uuid::parse("aabbccdd-eeff-0011-2233-44556677889").is_none()); // too short
    }

    #[test]
    fn uuid_from_btrfs_maps_sentinel_to_none() {
        assert!(Uuid::from_btrfs("-").is_none());
        assert!(Uuid::from_btrfs("   ").is_none());
        assert_eq!(
            Uuid::from_btrfs("aabbccdd-eeff-0011-2233-445566778899")
                .unwrap()
                .as_str(),
            "aabbccdd-eeff-0011-2233-445566778899"
        );
    }

    #[test]
    fn uuid_display_matches_as_str() {
        let x = u("a");
        assert_eq!(x.to_string(), x.as_str());
    }

    // --- Subvolume ---

    #[test]
    fn is_garbled_true_for_writable_without_received_uuid() {
        assert!(sv(256, Some("a"), None, None, false).is_garbled());
    }

    #[test]
    fn is_garbled_false_for_proper_backup() {
        assert!(!sv(256, Some("a"), None, Some("b"), true).is_garbled());
    }

    #[test]
    fn reference_generation_uses_cgen_when_readonly_else_gen() {
        let ro = sv(1, Some("a"), None, None, true);
        let rw = sv(2, Some("b"), None, None, false);
        assert_eq!(ro.reference_generation(), ro.cgen);
        assert_eq!(rw.reference_generation(), rw.generation);
    }

    // --- RelationshipGraph ---

    #[test]
    fn graph_indexes_and_gets_by_uuid() {
        let g = RelationshipGraph::build(vec![
            sv(1, Some("a"), None, None, true),
            sv(2, Some("b"), Some("a"), None, true),
        ])
        .unwrap();
        assert_eq!(g.get(&u("a")).unwrap().id, 1);
        assert_eq!(g.get(&u("b")).unwrap().id, 2);
        assert!(g.get(&u("e")).is_none());
    }

    #[test]
    fn graph_children_of_returns_parent_uuid_matches() {
        let g = RelationshipGraph::build(vec![
            sv(1, Some("a"), None, None, true),
            sv(2, Some("b"), Some("a"), None, true),
            sv(3, Some("c"), Some("a"), None, true),
            sv(4, Some("d"), Some("b"), None, true),
        ])
        .unwrap();
        let mut ids: Vec<u64> = g.children_of(&u("a")).iter().map(|s| s.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 3]);
        assert!(g.children_of(&u("e")).is_empty());
    }

    #[test]
    fn graph_received_from_returns_received_uuid_matches() {
        let g = RelationshipGraph::build(vec![
            sv(1, Some("a"), None, None, true),
            sv(2, Some("b"), None, Some("a"), true),
        ])
        .unwrap();
        let from_a = g.received_from(&u("a"));
        assert_eq!(from_a.len(), 1);
        assert_eq!(from_a[0].id, 2);
        assert!(g.received_from(&u("e")).is_empty());
    }

    #[test]
    fn graph_rejects_duplicate_uuid() {
        let err = RelationshipGraph::build(vec![
            sv(1, Some("a"), None, None, true),
            sv(2, Some("a"), None, None, true),
        ])
        .unwrap_err();
        assert_eq!(err, GraphError::DuplicateUuid(u("a")));
    }
}
