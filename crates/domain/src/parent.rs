//! `ParentResolver` — choose the incremental parent (+ clone sources).
//!
//! Pure, over the source/target [`RelationshipGraph`]s. Three pieces:
//! - [`is_correlated`] — btrbk's `_is_correlated` (two read-only subvolumes hold
//!   the same content), used to confirm a source snapshot has a copy on the target.
//! - [`related`] — the `parent_uuid` lineage of a snapshot (climb to the chain
//!   top, then walk down), i.e. its ancestors/descendants/siblings.
//! - [`best_parent`] — pick the parent for `btrfs send -p` plus `-c` clone
//!   sources, honoring reachability (same filesystem) and the `Incremental` mode.
//!
//! Selection is a faithful simplification of btrbk's `incremental_prefs`: the
//! parent is the newest correlated candidate not newer than the snapshot (falling
//! back to the oldest newer one), and the remaining correlated candidates become
//! clone sources. The hard correctness rule — a parent must have a correlated
//! copy on the target — is exact.
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use crate::model::{RelationshipGraph, Subvolume, Uuid};
use std::collections::HashSet;
use std::collections::VecDeque;

/// `btrfs send -p` strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incremental {
    /// Use a parent if one is found, else fall back to a full backup.
    Yes,
    /// Require a `parent_uuid`-related parent; never fall back to full.
    Strict,
    /// Always full (no `-p`).
    No,
}

/// The chosen parent (+ clone sources) for an incremental send. `parent` and
/// `clone_sources` are **source-side** subvolumes, each guaranteed to have a
/// correlated copy on the target.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParentSelection {
    /// Incremental parent passed to `btrfs send -p`; `None` means a full send.
    pub parent: Option<Subvolume>,
    /// Additional clone sources passed to `btrfs send -c`; may be empty.
    pub clone_sources: Vec<Subvolume>,
}

/// Why a parent could not be resolved. The only failure mode is a [`Incremental::Strict`]
/// request with no eligible parent — Strict **refuses** rather than falling back to
/// a full send (the orchestrator must abort, never transfer a full backup).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParentError {
    /// Strict mode required a `parent_uuid`-related, correlated parent but none
    /// qualified. A full fallback is forbidden, so the operation must abort.
    #[error(
        "strict incremental requires a parent_uuid-related parent with a correlated copy on the target, but none was found"
    )]
    NoStrictParent,
}

/// Whether two read-only subvolumes are correlated (hold the same content).
/// Mirrors btrbk `_is_correlated`.
#[must_use]
pub fn is_correlated(a: &Subvolume, b: &Subvolume) -> bool {
    a.readonly
        && b.readonly
        && (same(&a.uuid, &b.received_uuid)
            || same(&b.uuid, &a.received_uuid)
            || same(&a.received_uuid, &b.received_uuid))
}

/// Equal only when both sides are present (so `None == None` never matches).
fn same(x: &Option<Uuid>, y: &Option<Uuid>) -> bool {
    matches!((x, y), (Some(a), Some(b)) if a == b)
}

/// The read-only subvolumes in `graph` connected to `snapshot` through the
/// `parent_uuid` lineage (climb to the chain top, then walk down), excluding
/// `snapshot` itself.
#[must_use]
pub fn related<'a>(graph: &'a RelationshipGraph, snapshot: &Subvolume) -> Vec<&'a Subvolume> {
    const MAX_DEPTH: usize = 4096;

    // Climb to the top of the parent_uuid chain.
    let mut top = snapshot;
    let mut steps = 0;
    while let Some(parent_uuid) = &top.parent_uuid {
        if steps >= MAX_DEPTH {
            break;
        }
        match graph.get(parent_uuid) {
            Some(parent) => {
                top = parent;
                steps += 1;
            }
            None => break,
        }
    }

    // Walk down from the top through parent_uuid children.
    let mut result: Vec<&Subvolume> = Vec::new();
    let mut seen_ids: HashSet<u64> = HashSet::new();
    let mut visited_uuids: HashSet<Uuid> = HashSet::new();
    let mut queue: VecDeque<Uuid> = VecDeque::new();
    if let Some(top_uuid) = &top.uuid {
        queue.push_back(top_uuid.clone());
    }
    // The chain-top's own parent_uuid bucket holds its SIBLINGS. In production
    // every read-only snapshot of one source shares that live source's uuid as
    // parent_uuid, but the live source is never inside snapshot_dir, so the
    // common parent node is absent from the graph and the climb stops at the
    // snapshot itself. Seeding from the parent_uuid bucket recovers the siblings
    // (subvolumes sharing the chain-top's parent_uuid) that the uuid-descendant
    // walk alone would miss.
    if let Some(top_parent_uuid) = &top.parent_uuid {
        queue.push_back(top_parent_uuid.clone());
    }
    while let Some(uuid) = queue.pop_front() {
        if !visited_uuids.insert(uuid.clone()) {
            continue;
        }
        for child in graph.children_of(&uuid) {
            if child.id != snapshot.id && child.readonly && seen_ids.insert(child.id) {
                result.push(child);
            }
            if let Some(child_uuid) = &child.uuid {
                queue.push_back(child_uuid.clone());
            }
        }
    }
    result
}

/// Resolve the incremental parent and clone sources for `snapshot`.
///
/// # Errors
/// [`ParentError::NoStrictParent`] when `mode` is [`Incremental::Strict`] and no
/// `parent_uuid`-related candidate has a correlated copy on the target. Strict
/// **refuses** rather than falling back to a full send. [`Incremental::Yes`] and
/// [`Incremental::No`] never error (they return a full-send selection instead).
pub fn best_parent(
    snapshot: &Subvolume,
    source: &RelationshipGraph,
    target: &RelationshipGraph,
    mode: Incremental,
) -> Result<ParentSelection, ParentError> {
    if mode == Incremental::No {
        return Ok(ParentSelection::default());
    }

    // Candidate parents: source-side, read-only, not the snapshot, reachable on
    // the same filesystem, and with a correlated copy on the target.
    let related_ids: HashSet<u64> = if mode == Incremental::Strict {
        related(source, snapshot).iter().map(|s| s.id).collect()
    } else {
        HashSet::new()
    };

    let mut candidates: Vec<&Subvolume> = source
        .all()
        .iter()
        .filter(|s| s.id != snapshot.id && s.readonly)
        .filter(|s| s.fs_uuid == snapshot.fs_uuid)
        .filter(|s| !target_correlates(s, target).is_empty())
        .filter(|s| mode != Incremental::Strict || related_ids.contains(&s.id))
        .collect();

    if candidates.is_empty() {
        // Strict must REFUSE — never silently fall back to a full send.
        if mode == Incremental::Strict {
            return Err(ParentError::NoStrictParent);
        }
        return Ok(ParentSelection::default());
    }

    // Parent = newest candidate not newer than the snapshot; else oldest newer.
    let snap_gen = snapshot.reference_generation();
    candidates.sort_by(|a, b| {
        b.reference_generation()
            .cmp(&a.reference_generation())
            .then(b.id.cmp(&a.id))
    });
    // SAFETY: `candidates` is non-empty — the early-return above guarantees this,
    // so `candidates.len() - 1` cannot underflow.
    let parent_pos = candidates
        .iter()
        .position(|s| s.reference_generation() <= snap_gen)
        .unwrap_or(candidates.len() - 1);
    let parent = candidates.remove(parent_pos);

    Ok(ParentSelection {
        parent: Some(parent.clone()),
        clone_sources: candidates.into_iter().cloned().collect(),
    })
}

/// The correlated copies of `subvol` present on `target` (deduplicated by id).
#[must_use]
pub fn target_correlates<'a>(
    subvol: &Subvolume,
    target: &'a RelationshipGraph,
) -> Vec<&'a Subvolume> {
    let mut candidates: Vec<&Subvolume> = Vec::new();
    if let Some(uuid) = &subvol.uuid {
        candidates.extend(target.received_from(uuid));
    }
    if let Some(received) = &subvol.received_uuid {
        if let Some(node) = target.get(received) {
            candidates.push(node);
        }
        candidates.extend(target.received_from(received));
    }
    candidates.retain(|t| is_correlated(subvol, t));
    candidates.sort_by_key(|t| t.id);
    candidates.dedup_by_key(|t| t.id);
    candidates
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::RelationshipGraph;
    use std::path::PathBuf;

    fn u(tag: &str) -> Uuid {
        let t = tag.repeat(32);
        Uuid::parse(&format!(
            "{}-{}-{}-{}-{}",
            &t[0..8],
            &t[8..12],
            &t[12..16],
            &t[16..20],
            &t[20..32]
        ))
        .expect("valid test uuid")
    }

    #[allow(clippy::too_many_arguments)]
    fn sv(
        id: u64,
        uuid: &str,
        parent: Option<&str>,
        received: Option<&str>,
        readonly: bool,
        cgen: u64,
        fs: &str,
    ) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(u(uuid)),
            parent_uuid: parent.map(u),
            received_uuid: received.map(u),
            generation: cgen,
            cgen,
            readonly,
            path: PathBuf::from(format!("/mnt/{fs}/sv{id}")),
            fs_uuid: u(fs),
            mountpoint: PathBuf::from(format!("/mnt/{fs}")),
        }
    }

    // --- is_correlated ---

    #[test]
    fn correlated_when_target_received_from_source() {
        let s = sv(1, "1", Some("d"), None, true, 10, "f");
        let t = sv(2, "a", None, Some("1"), true, 5, "e");
        assert!(is_correlated(&s, &t));
    }

    #[test]
    fn correlated_when_sharing_received_uuid() {
        let a = sv(1, "1", None, Some("9"), true, 10, "f");
        let b = sv(2, "2", None, Some("9"), true, 10, "e");
        assert!(is_correlated(&a, &b));
    }

    #[test]
    fn not_correlated_when_writable() {
        let s = sv(1, "1", Some("d"), None, false, 10, "f");
        let t = sv(2, "a", None, Some("1"), true, 5, "e");
        assert!(!is_correlated(&s, &t));
    }

    #[test]
    fn not_correlated_unrelated_and_no_none_false_match() {
        let a = sv(1, "1", None, None, true, 10, "f");
        let b = sv(2, "2", None, None, true, 10, "e");
        assert!(!is_correlated(&a, &b));
    }

    // --- related ---

    #[test]
    fn related_returns_parent_uuid_siblings() {
        let g = RelationshipGraph::build(vec![
            sv(5, "d", None, None, false, 1, "f"),
            sv(1, "1", Some("d"), None, true, 10, "f"),
            sv(2, "2", Some("d"), None, true, 20, "f"),
            sv(3, "3", Some("d"), None, true, 30, "f"),
        ])
        .unwrap();
        let s1 = sv(1, "1", Some("d"), None, true, 10, "f");
        let mut ids: Vec<u64> = related(&g, &s1).iter().map(|s| s.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn related_excludes_unrelated_lineage() {
        let g = RelationshipGraph::build(vec![
            sv(5, "d", None, None, false, 1, "f"),
            sv(1, "1", Some("d"), None, true, 10, "f"),
            sv(2, "2", Some("d"), None, true, 20, "f"),
            sv(4, "4", Some("e"), None, true, 40, "f"),
        ])
        .unwrap();
        let s1 = sv(1, "1", Some("d"), None, true, 10, "f");
        let ids: Vec<u64> = related(&g, &s1).iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![2]);
        assert!(!ids.contains(&4));
    }

    #[test]
    fn related_finds_siblings_when_common_parent_absent_from_graph() {
        // Production reality: every read-only snapshot of one source shares that
        // LIVE source's uuid as its parent_uuid, but the live source itself is
        // never inside snapshot_dir, so it is NEVER in the graph. The siblings
        // must still be found via the shared (absent) parent_uuid bucket.
        let g = RelationshipGraph::build(vec![
            sv(1, "1", Some("d"), None, true, 10, "f"),
            sv(2, "2", Some("d"), None, true, 20, "f"),
            sv(3, "3", Some("d"), None, true, 30, "f"),
        ])
        .unwrap();
        let s1 = sv(1, "1", Some("d"), None, true, 10, "f");
        let mut ids: Vec<u64> = related(&g, &s1).iter().map(|s| s.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![2, 3]);
    }

    // --- best_parent ---

    fn source_with_three_snaps() -> RelationshipGraph {
        RelationshipGraph::build(vec![
            sv(5, "d", None, None, false, 1, "f"),
            sv(1, "1", Some("d"), None, true, 10, "f"),
            sv(2, "2", Some("d"), None, true, 20, "f"),
            sv(3, "3", Some("d"), None, true, 30, "f"),
        ])
        .unwrap()
    }

    #[test]
    fn best_parent_picks_newest_older_correlated() {
        let source = source_with_three_snaps();
        let target = RelationshipGraph::build(vec![
            sv(11, "a", None, Some("1"), true, 5, "c"),
            sv(12, "b", None, Some("2"), true, 6, "c"),
        ])
        .unwrap();
        let snap = sv(3, "3", Some("d"), None, true, 30, "f");

        let sel = best_parent(&snap, &source, &target, Incremental::Yes).unwrap();
        assert_eq!(sel.parent.map(|p| p.id), Some(2));
        let mut clones: Vec<u64> = sel.clone_sources.iter().map(|s| s.id).collect();
        clones.sort_unstable();
        assert_eq!(clones, vec![1]);
    }

    #[test]
    fn best_parent_full_fallback_when_no_correlate() {
        let source = source_with_three_snaps();
        let target = RelationshipGraph::build(vec![]).unwrap();
        let snap = sv(3, "3", Some("d"), None, true, 30, "f");

        let sel = best_parent(&snap, &source, &target, Incremental::Yes).unwrap();
        assert!(sel.parent.is_none());
        assert!(sel.clone_sources.is_empty());
    }

    #[test]
    fn best_parent_mode_no_never_uses_parent() {
        let source = source_with_three_snaps();
        let target =
            RelationshipGraph::build(vec![sv(11, "a", None, Some("2"), true, 5, "c")]).unwrap();
        let snap = sv(3, "3", Some("d"), None, true, 30, "f");

        let sel = best_parent(&snap, &source, &target, Incremental::No).unwrap();
        assert!(sel.parent.is_none());
        assert!(sel.clone_sources.is_empty());
    }

    #[test]
    fn best_parent_excludes_other_filesystem() {
        let source = RelationshipGraph::build(vec![
            sv(3, "3", Some("d"), None, true, 30, "f"),
            sv(8, "8", Some("d"), None, true, 20, "0"),
        ])
        .unwrap();
        let target =
            RelationshipGraph::build(vec![sv(11, "a", None, Some("8"), true, 5, "c")]).unwrap();
        let snap = sv(3, "3", Some("d"), None, true, 30, "f");

        let sel = best_parent(&snap, &source, &target, Incremental::Yes).unwrap();
        assert!(sel.parent.is_none());
    }

    #[test]
    fn strict_refuses_non_parent_related_but_yes_accepts() {
        let source = RelationshipGraph::build(vec![
            sv(5, "d", None, None, false, 1, "f"),
            sv(3, "3", Some("d"), None, true, 30, "f"),
            sv(9, "9", Some("e"), None, true, 20, "f"),
        ])
        .unwrap();
        let target =
            RelationshipGraph::build(vec![sv(11, "a", None, Some("9"), true, 5, "c")]).unwrap();
        let snap = sv(3, "3", Some("d"), None, true, 30, "f");

        let yes = best_parent(&snap, &source, &target, Incremental::Yes).unwrap();
        assert_eq!(yes.parent.map(|p| p.id), Some(9));

        // The correlated candidate (9) is NOT parent_uuid-related to the snapshot
        // (different parent_uuid), so Strict refuses rather than falling back.
        let strict = best_parent(&snap, &source, &target, Incremental::Strict);
        assert_eq!(strict, Err(ParentError::NoStrictParent));
    }
}
