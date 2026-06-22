//! `RetentionService` — powers `prune`. Runs the scheduler **separately** for
//! snapshots (snapshot policy) and backups (target policy), then applies the
//! `SafetyPolicy` anchors before any deletion. Keep-all by default: deletes
//! nothing unless a policy is supplied. See `documentation/01` Phase 3.

use chrono::{FixedOffset, TimeZone};

use mybtrfs_domain::model::Subvolume;
use mybtrfs_domain::naming::parse_name;
use mybtrfs_domain::retention::{DatedEntry, RetentionPolicy, Schedule, schedule};
use mybtrfs_domain::safety::{SafetyContext, enforce};

use crate::ports::{ClockPort, DeleteCommit, DeletePort, PortError};

/// Orchestrates retention: schedule → safety anchors → delete, over one set of
/// candidate subvolumes (call once for snapshots, once for backups).
pub struct RetentionService<'a> {
    clock: &'a dyn ClockPort,
    deleter: &'a dyn DeletePort,
}

impl<'a> RetentionService<'a> {
    /// Construct a service over the injected clock and delete port.
    #[must_use]
    pub fn new(clock: &'a dyn ClockPort, deleter: &'a dyn DeletePort) -> Self {
        Self { clock, deleter }
    }

    /// Prune `candidates` per `policy`, honoring the `SafetyPolicy` anchors in
    /// `ctx`, deleting the resulting complement via the [`DeletePort`]. Returns
    /// the final preserve/delete partition.
    ///
    /// Subvolumes whose names don't match the mybtrfs scheme don't parse and are
    /// therefore never scheduled or deleted — foreign subvolumes are left
    /// untouched. The reference clock supplies both "now" and the timezone used
    /// to resolve `short`/`long` (local-time) names.
    ///
    /// # Errors
    /// Propagates any [`PortError`] from the delete port.
    pub fn prune(
        &self,
        candidates: &[Subvolume],
        policy: &RetentionPolicy,
        ctx: &SafetyContext,
        commit: DeleteCommit,
    ) -> Result<Schedule<Subvolume>, PortError> {
        let now = self.clock.now();
        let reference = *now.offset();

        let entries: Vec<DatedEntry<Subvolume>> = candidates
            .iter()
            .filter_map(|subvol| dated_entry(subvol, reference))
            .collect();

        let scheduled = schedule(entries, policy, now.naive_local());
        let safe = enforce(scheduled, ctx);

        for subvol in &safe.delete {
            let path = subvol.mountpoint.join(&subvol.path);
            self.deleter.delete(&path, commit)?;
        }
        Ok(safe)
    }
}

/// Build a [`DatedEntry`] from a subvolume by parsing its name. `None` when the
/// name doesn't match the mybtrfs scheme (so foreign subvolumes are skipped).
/// `long-iso` names carry their own offset (absolute); `short`/`long` names are
/// resolved against the `reference` timezone.
fn dated_entry(subvol: &Subvolume, reference: FixedOffset) -> Option<DatedEntry<Subvolume>> {
    let leaf = subvol.path.file_name()?.to_str()?;
    let parsed = parse_name(leaf)?;

    let (instant, local) = match parsed.offset {
        Some(offset) => {
            let dt = offset.from_local_datetime(&parsed.naive).single()?;
            (dt.timestamp(), dt.with_timezone(&reference).naive_local())
        }
        None => {
            let dt = reference.from_local_datetime(&parsed.naive).single()?;
            (dt.timestamp(), parsed.naive)
        }
    };

    Some(DatedEntry {
        instant,
        local,
        has_exact_time: parsed.has_exact_time,
        nn: parsed.nn,
        payload: subvol.clone(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, FixedOffset, NaiveDate};

    use mybtrfs_domain::model::{Subvolume, Uuid};
    use mybtrfs_domain::retention::{PreserveMin, RetentionPolicy};
    use mybtrfs_domain::safety::SafetyContext;

    use crate::ports::{ClockPort, DeleteCommit, DeletePort, PortError};
    use crate::retention::RetentionService;

    struct FixedClock(DateTime<FixedOffset>);
    impl FixedClock {
        fn at(rfc3339: &str) -> Self {
            Self(DateTime::parse_from_rfc3339(rfc3339).expect("valid rfc3339"))
        }
    }
    impl ClockPort for FixedClock {
        fn now(&self) -> DateTime<FixedOffset> {
            self.0
        }
    }

    /// A `DeletePort` that records the paths (and commit mode) it was asked to delete.
    #[derive(Default)]
    struct RecordingDeleter {
        deleted: RefCell<Vec<(PathBuf, DeleteCommit)>>,
    }
    impl RecordingDeleter {
        fn deleted(&self) -> Vec<(PathBuf, DeleteCommit)> {
            self.deleted.borrow().clone()
        }
    }
    impl DeletePort for RecordingDeleter {
        fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
            self.deleted.borrow_mut().push((path.to_path_buf(), commit));
            Ok(())
        }
    }

    /// A canonical UUID derived from a small integer tag.
    fn uuid_for(tag: u64) -> Uuid {
        Uuid::parse(&format!("{tag:08x}-0000-0000-0000-000000000000")).expect("valid uuid")
    }

    /// A read-only snapshot named `name` in the pool's snapshot dir. A `name`
    /// that doesn't match the scheme stands in for a foreign subvolume.
    fn snap(id: u64, name: &str) -> Subvolume {
        Subvolume {
            id,
            uuid: Some(uuid_for(id)),
            parent_uuid: None,
            received_uuid: None,
            generation: 100,
            cgen: 100,
            readonly: true,
            path: PathBuf::from(format!(".mybtrfs_snapshots/{name}")),
            fs_uuid: uuid_for(0),
            mountpoint: PathBuf::from("/mnt/pool"),
        }
    }

    fn prune_ids(
        schedule: &mybtrfs_domain::retention::Schedule<Subvolume>,
    ) -> (Vec<u64>, Vec<u64>) {
        let mut preserve: Vec<u64> = schedule.preserve.iter().map(|s| s.id).collect();
        let mut delete: Vec<u64> = schedule.delete.iter().map(|s| s.id).collect();
        preserve.sort_unstable();
        delete.sort_unstable();
        (preserve, delete)
    }

    #[test]
    fn keep_all_default_deletes_nothing() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let service = RetentionService::new(&clock, &deleter);

        let candidates = vec![snap(1, "home.20240101T1200"), snap(2, "home.20240102T1200")];
        let out = service
            .prune(
                &candidates,
                &RetentionPolicy::default(), // preserve_min = All
                &SafetyContext::default(),
                DeleteCommit::Deferred,
            )
            .expect("prune succeeds");

        assert!(deleter.deleted().is_empty());
        let (preserve, delete) = prune_ids(&out);
        assert_eq!(preserve, vec![1, 2]);
        assert!(delete.is_empty());
    }

    #[test]
    fn no_floor_deletes_all_via_port_with_absolute_paths() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let service = RetentionService::new(&clock, &deleter);

        let candidates = vec![snap(1, "home.20240101T1200"), snap(2, "home.20240102T1200")];
        let policy = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let out = service
            .prune(
                &candidates,
                &policy,
                &SafetyContext::default(),
                DeleteCommit::Each,
            )
            .expect("prune succeeds");

        let (_, delete) = prune_ids(&out);
        assert_eq!(delete, vec![1, 2]);

        let mut deleted = deleter.deleted();
        deleted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            deleted,
            vec![
                (
                    PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240101T1200"),
                    DeleteCommit::Each
                ),
                (
                    PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240102T1200"),
                    DeleteCommit::Each
                ),
            ]
        );
    }

    #[test]
    fn force_preserve_anchor_rescues_from_deletion() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let service = RetentionService::new(&clock, &deleter);

        let candidates = vec![snap(1, "home.20240101T1200"), snap(2, "home.20240102T1200")];
        let policy = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let ctx = SafetyContext {
            force_preserve_ids: HashSet::from([2]),
            target_aborted: false,
        };
        let out = service
            .prune(&candidates, &policy, &ctx, DeleteCommit::Deferred)
            .expect("prune succeeds");

        let (preserve, delete) = prune_ids(&out);
        assert_eq!(preserve, vec![2]);
        assert_eq!(delete, vec![1]);
        assert_eq!(
            deleter.deleted(),
            vec![(
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240101T1200"),
                DeleteCommit::Deferred
            )]
        );
    }

    #[test]
    fn foreign_named_subvolume_is_never_scheduled_or_deleted() {
        let clock = FixedClock::at("2024-02-01T00:00:00+00:00");
        let deleter = RecordingDeleter::default();
        let service = RetentionService::new(&clock, &deleter);

        let candidates = vec![snap(1, "home.20240101T1200"), snap(9, "random-data")];
        let policy = RetentionPolicy {
            preserve_min: PreserveMin::None,
            ..Default::default()
        };
        let out = service
            .prune(
                &candidates,
                &policy,
                &SafetyContext::default(),
                DeleteCommit::Deferred,
            )
            .expect("prune succeeds");

        // Only the scheme-matching snapshot is scheduled; the foreign one is absent.
        let (_, delete) = prune_ids(&out);
        assert_eq!(delete, vec![1]);
        assert_eq!(
            deleter.deleted(),
            vec![(
                PathBuf::from("/mnt/pool/.mybtrfs_snapshots/home.20240101T1200"),
                DeleteCommit::Deferred
            )]
        );
    }

    // --- dated_entry timezone resolution ---

    #[test]
    fn dated_entry_resolves_long_name_against_reference_timezone() {
        let plus2 = FixedOffset::east_opt(2 * 3600).expect("valid offset");
        let sv = snap(1, "home.20240102T1200"); // long format, no embedded offset
        let entry = super::dated_entry(&sv, plus2).expect("name parses");

        // 12:00 wall-clock interpreted at +02:00 is 10:00 UTC.
        assert_eq!(
            entry.instant,
            NaiveDate::from_ymd_opt(2024, 1, 2)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        );
        // The reference-tz wall-clock used for calendar math stays 12:00.
        assert_eq!(
            entry.local,
            NaiveDate::from_ymd_opt(2024, 1, 2)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
        );
        assert!(entry.has_exact_time);
    }

    #[test]
    fn dated_entry_uses_embedded_offset_for_long_iso_names() {
        let reference = FixedOffset::east_opt(0).expect("valid offset"); // UTC reference
        let sv = snap(2, "home.20240102T120000+0200"); // long-iso, +02:00
        let entry = super::dated_entry(&sv, reference).expect("name parses");

        // 12:00+0200 is the absolute instant 10:00 UTC, regardless of reference tz.
        assert_eq!(
            entry.instant,
            NaiveDate::from_ymd_opt(2024, 1, 2)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        );
        // Expressed in the reference tz (UTC), that instant's wall-clock is 10:00.
        assert_eq!(
            entry.local,
            NaiveDate::from_ymd_opt(2024, 1, 2)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap()
        );
    }
}
