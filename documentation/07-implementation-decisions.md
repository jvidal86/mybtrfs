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

---

## ID-4 — The run lock is a single advisory `flock` at the composition root

**Date:** 2026-06-23 · **Source:** implementing E2E-CC-09 (concurrency lock).

**Context.** Two overlapping `mybtrfs` runs could interleave snapshot creation /
prune on the same data. btrbk guards against this with a global lockfile. The
design left the mechanism open ("pending lock decision").

**Decision.** A **process-level advisory lock** (`std::fs::File::try_lock`, an
exclusive `flock(2)`), acquired at the **CLI composition root** — *not* a port.
Rationale: it guards the whole invocation, not a single dangerous operation, so it
doesn't fit the narrow-port model; and `flock` is released by the OS on process
exit, so a crash never leaves a stale lock (no pidfile/staleness logic needed —
leaner than the alternative). Scope is a single global lock file (`--lock <PATH>`,
default `<tmpdir>/mybtrfs.lock`), mirroring btrbk's single-instance model. Only
**state-changing** commands take it (`run`/`snapshot`/`resume`/committing
`prune`/`restore`); read-only commands and dry runs (which mutate nothing —
invariant #8) never contend. A lock already held maps to `LockBusy` → exit code
`3`; the second run makes no changes.

**Enforcement.** `FileLock` (`crates/adapters/src/lock.rs`, RAII guard) +
`command_mutates`/`acquire_lock` in `crates/cli/src/cli.rs`; the guard is held for
the lifetime of `dispatch`. Requires Rust **1.89** (`File::try_lock`). Tested
in-process (two OFDs on one path conflict) and via `exit_code_for`.

---

## ID-5 — Restore transfer-back: mountpoint-prefix detection, full send, verify-then-clean

**Date:** 2026-06-23 · **Source:** implementing Phase 4 transfer-back (item 7).

**Context.** A backup that lives only on a different filesystem (the external
drive) cannot be made writable in place — `btrfs subvolume snapshot` works within
one filesystem. Such a backup must be sent/received onto the destination
filesystem first. `RestoreService` had to decide *whether* a transfer-back is
needed, *how* to transfer, and *what* to clean up.

**Decisions.**
1. **Local-vs-remote by mountpoint prefix, not fs UUID.** `restore` resolves the
   backup (`repo.show`) and treats `dest` as same-filesystem iff it falls under
   the backup's mountpoint (`dest.starts_with(backup.mountpoint)`). This needs no
   new port and no `stat` of the not-yet-existing `dest`. Both misjudgements are
   **safe**: a wrong "same-fs" makes the cross-filesystem `btrfs snapshot` fail
   cleanly (no corruption), and a wrong "remote" does a correct, if needless,
   transfer. (A literal-UUID compare would be marginally more precise but adds a
   port for no safety gain.)
2. **Full send, not incremental (for now).** The transfer-back is always a full
   `send/receive` (`ParentSelection::default()`). Restores are infrequent, so the
   delta optimization (E2E-P4-06: reuse a common parent already on the pool) isn't
   worth the graph-building/parent-resolution complexity yet — deferred, tracked
   by P4-06. Full send is always correct and applicable.
3. **Verify, then clean up.** The received intermediate copy on the destination
   filesystem is deleted only **after** the writable result passes its clean-write
   verification (#7). If verification fails, the staging copy is **kept** so the
   data stays recoverable (parallel to how `NotCleanWritable` surfaces the
   move-aside path).

**Enforcement.** `RestoreService` (`crates/application/src/restore.rs`) now takes
repository + transfer + delete ports; the CLI wires the `btrfs` adapter for all
three and routes the staging cleanup through the logging deleter (ID-1). Tested
with fakes: remote restore transfers → makes writable → deletes; dry-run plans
without executing; a non-clean result keeps the staging copy.

---

## ID-6 — Exit code 4 for "needs root" (a divergence over the original sketch)

**Date:** 2026-06-23 · **Source:** real-world hand-testing — running the binary
unprivileged surfaced btrfs "Permission denied" as an opaque generic failure.

**Context.** btrfs ioctls/commands require root; an unprivileged run fails deep in
the `btrfs` adapter. The original exit-code sketch (`01`, success/generic/usage/
partial-abort) and btrbk itself fold this into the generic failure code, so a
cron/script can't tell "you forgot sudo" from a real backup error.

**Decision.** Add a dedicated **exit code `4` (`PermissionDenied`)** — an
intentional divergence from btrbk, consistent with `01`'s "privilege requirements
are explicit, never auto-escalated". `is_permission_error` scans the error chain
for an `io::ErrorKind::PermissionDenied` or a `"Permission denied"` substring
(catching `PortError::Command` that wraps raw btrfs stderr); when found, dispatch
re-wraps the error as `PermissionDenied`, which `exit_code_for` maps to `4`. The
user message is actionable ("re-run with sudo").

**Enforcement / status.** `crates/cli/src/cli.rs` (`exit_code::PERMISSION_DENIED`,
`struct PermissionDenied`, `is_permission_error`, the `dispatch` re-wrap). Verified
manually against an unprivileged real run; **not yet covered by an automated test**
(the string/kind classification in `is_permission_error` and the `exit_code_for`
mapping are the natural unit-test targets — a follow-up).
