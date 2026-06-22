//! `RetentionScheduler` — the pure hourly→daily→weekly→monthly→yearly cascade.
//!
//! Inputs: timestamps, policy, reference time **and timezone** (because
//! `short`/`long` timestamps are local-time). Output: `(preserve, delete)`.
//! First/oldest-of-period wins and rolls up into the next tier. Parallels btrbk
//! `sub schedule`. Run once per set (snapshots vs backups) with its own policy.
//
// TODO (Phase 3).
