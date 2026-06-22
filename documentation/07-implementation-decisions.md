# 07 — Implementation Decisions (decision log)

Short, dated decisions made *during* implementation (ADR-style) that aren't
captured by the design docs `01`/`02`. Each records the context, the decision,
and where it's enforced. The design docs remain the spec; this log explains
choices the spec left open.

---

## ID-1 — `prune` is fail-fast on a delete error (abort-and-report)

**Date:** 2026-06-22 · **Source:** code review of `RetentionService::prune`.

**Context.** `prune` deletes the scheduled complement via `DeletePort` in a loop
(`crates/application/src/retention.rs`). A delete failing mid-loop returns the
error immediately and drops the computed `Schedule`, so the caller doesn't learn
which subvolumes were already deleted.

**Decision.** Keep it **fail-fast**: abort on the first delete error and
propagate it. This is parallel to btrbk (a delete failure aborts the prune and is
reported). Partial progress — subvolumes deleted before the failure — is
acceptable and leaves the system **consistent**: each `btrfs subvolume delete` is
atomic, and the stateless design re-derives truth on the next run and re-attempts.
No rollback is needed or wanted.

**Enforcement / follow-up.** Observability is the **adapter/CLI**'s job: the
`BtrfsCliAdapter`/composition root must log each deletion (and the failure) so
partial progress is visible even though the in-memory `Schedule` is not returned
on error. (Wire per-deletion logging when building the CLI.)

---

## ID-2 — Path normalization is a composition-root responsibility (fail at the boundary)

**Date:** 2026-06-22 · **Source:** code review of `BackupService::snapshot`.

**Context.** Collision detection compares
`subvol.mountpoint.join(&subvol.path).parent() == Some(snapshot_dir)`
(`crates/application/src/backup.rs`), which only holds when `snapshot_dir` is a
canonical absolute path (no trailing slash, no `..`).

**Decision.** The **CLI composition root** normalizes and validates every
incoming path (absolute, canonical, no `..`/trailing slash) *before* calling the
application use cases; the application and domain layers **trust** them
("validate at boundaries, trust internals"). This composes with the
`BtrfsCliAdapter` path contract (absolute, non-flag, no-shell — `02 §3`).

**Enforcement / follow-up.** Add the canonicalization + validation step at the
composition root when wiring the CLI; the use cases stay free of defensive path
re-checks.

---

## ID-3 — Shared test doubles graduate to a test-support module on third use

**Date:** 2026-06-22 · **Source:** code review of the use-case test modules.

**Context.** `FixedClock` (and the recording fake ports) are duplicated across the
`backup.rs` and `retention.rs` `#[cfg(test)]` modules (~10 lines each).

**Decision.** Duplication across **two** use-case test modules is acceptable (avoids
premature abstraction). When a **third** use case needs the same double, lift the
shared doubles (`FixedClock`, recording ports) into a single `#[cfg(test)]`
test-support module rather than copying again.

**Enforcement / follow-up.** Apply at the next (third) use-case test that needs a
`FixedClock`/recording port — e.g. when `resume`/`restore`/CLI tests arrive.
