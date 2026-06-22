//! `SafetyPolicy` — the non-negotiable delete-safety anchors.
//!
//! Applied as a **monotonic** step over the retention [`Schedule`]: it only ever
//! moves subvolumes from *delete* to *preserve*, never the reverse. Anchors:
//! - keep the just-created snapshot/backup, and the latest common snapshot/backup
//!   pair (both fed in via [`SafetyContext::force_preserve_ids`]);
//! - skip **all** deletion if a target was unreachable/aborted.
//!
//! [`latest_common_pair`] computes the second anchor (newest source snapshot that
//! still has a correlated backup on the target). The dependency-closure anchor
//! (parents of preserved *raw* backups) is a Phase 5+ concern.
//!
//! See `documentation/02-architecture-v2.md` §6 (the fail-safe invariants).
//!
//! TDD: the tests below are the spec, written first. Implementation follows.

use crate::model::{RelationshipGraph, Subvolume};
use crate::parent::target_correlates;
use crate::retention::Schedule;
use std::collections::HashSet;

/// A source snapshot and its correlated backups on the target.
#[derive(Debug)]
pub struct CommonPair<'a> {
    /// The source-side snapshot; has at least one correlated backup on the target.
    pub snapshot: &'a Subvolume,
    /// Correlated backup copies of `snapshot` present on the target filesystem.
    pub backups: Vec<&'a Subvolume>,
}

/// Overrides that force-preserve subvolumes regardless of the retention schedule.
#[derive(Debug, Clone, Default)]
pub struct SafetyContext {
    /// Subvolume ids that must be kept (just-created + latest common pair + …).
    pub force_preserve_ids: HashSet<u64>,
    /// If a target was unreachable/aborted, skip all deletion.
    pub target_aborted: bool,
}

/// Apply the safety anchors to a schedule. Monotonic: subvolumes only ever move
/// from `delete` to `preserve`, never the reverse.
#[must_use]
pub fn enforce(schedule: Schedule<Subvolume>, ctx: &SafetyContext) -> Schedule<Subvolume> {
    let Schedule {
        mut preserve,
        delete,
    } = schedule;

    if ctx.target_aborted {
        preserve.extend(delete);
        return Schedule {
            preserve,
            delete: Vec::new(),
        };
    }

    let (rescued, deletable): (Vec<Subvolume>, Vec<Subvolume>) = delete
        .into_iter()
        .partition(|subvol| ctx.force_preserve_ids.contains(&subvol.id));
    preserve.extend(rescued);
    Schedule {
        preserve,
        delete: deletable,
    }
}

/// The newest source snapshot that still has a correlated backup on the target,
/// together with those backups. Both halves should be force-preserved so the next
/// incremental backup keeps a common parent on both ends.
#[must_use]
pub fn latest_common_pair<'a>(
    snapshots: &'a [Subvolume],
    target: &'a RelationshipGraph,
) -> Option<CommonPair<'a>> {
    let mut ordered: Vec<&Subvolume> = snapshots.iter().collect();
    ordered.sort_by(|a, b| {
        b.reference_generation()
            .cmp(&a.reference_generation())
            .then(b.id.cmp(&a.id))
    });
    for snapshot in ordered {
        let backups = target_correlates(snapshot, target);
        if !backups.is_empty() {
            return Some(CommonPair { snapshot, backups });
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::{RelationshipGraph, Subvolume, Uuid};
    use crate::retention::Schedule;
    use std::collections::HashSet;
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

    fn sv(id: u64, uuid: &str, received: Option<&str>, cgen: u64) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(u(uuid)),
            parent_uuid: None,
            received_uuid: received.map(u),
            generation: cgen,
            cgen,
            readonly: true,
            path: PathBuf::from(format!("/mnt/f/sv{id}")),
            fs_uuid: u("f"),
            mountpoint: PathBuf::from("/mnt/f"),
        }
    }

    fn ids(subvols: &[Subvolume]) -> Vec<u64> {
        let mut v: Vec<u64> = subvols.iter().map(|s| s.id).collect();
        v.sort_unstable();
        v
    }

    // --- enforce ---

    #[test]
    fn target_aborted_rescues_every_deletion() {
        let schedule = Schedule {
            preserve: vec![sv(1, "1", None, 10)],
            delete: vec![sv(2, "2", None, 20), sv(3, "3", None, 30)],
        };
        let ctx = SafetyContext {
            force_preserve_ids: HashSet::new(),
            target_aborted: true,
        };
        let out = enforce(schedule, &ctx);
        assert_eq!(ids(&out.preserve), vec![1, 2, 3]);
        assert!(out.delete.is_empty());
    }

    #[test]
    fn force_preserved_ids_move_from_delete_to_preserve() {
        let schedule = Schedule {
            preserve: vec![sv(1, "1", None, 10)],
            delete: vec![sv(2, "2", None, 20), sv(3, "3", None, 30)],
        };
        let ctx = SafetyContext {
            force_preserve_ids: HashSet::from([3]),
            target_aborted: false,
        };
        let out = enforce(schedule, &ctx);
        assert_eq!(ids(&out.preserve), vec![1, 3]);
        assert_eq!(ids(&out.delete), vec![2]);
    }

    #[test]
    fn no_op_when_nothing_forced_and_not_aborted() {
        let schedule = Schedule {
            preserve: vec![sv(1, "1", None, 10)],
            delete: vec![sv(2, "2", None, 20)],
        };
        let ctx = SafetyContext::default();
        let out = enforce(schedule, &ctx);
        assert_eq!(ids(&out.preserve), vec![1]);
        assert_eq!(ids(&out.delete), vec![2]);
    }

    // --- latest_common_pair ---

    #[test]
    fn latest_common_pair_picks_newest_correlated_and_its_backups() {
        let snapshots = vec![
            sv(1, "1", None, 10),
            sv(2, "2", None, 20),
            sv(3, "3", None, 30), // newest, but not yet backed up
        ];
        let target = RelationshipGraph::build(vec![
            sv(11, "a", Some("1"), 5), // backup of snap 1
            sv(12, "b", Some("2"), 6), // backup of snap 2
        ])
        .unwrap();

        let pair = latest_common_pair(&snapshots, &target).unwrap();
        assert_eq!(pair.snapshot.id, 2); // newest snapshot with a target copy
        assert_eq!(
            pair.backups.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![12]
        );
    }

    #[test]
    fn latest_common_pair_none_when_no_correlate() {
        let snapshots = vec![sv(1, "1", None, 10), sv(2, "2", None, 20)];
        let target = RelationshipGraph::build(vec![]).unwrap();
        assert!(latest_common_pair(&snapshots, &target).is_none());
    }
}
