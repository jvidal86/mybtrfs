# mybtrfs — End-to-End Test Specification (descriptive)

> **Spec-Driven / Test-Driven.** This document is the **behavioral specification
> written before the code**. Each scenario is descriptive (Given / When / Then),
> **black-box** (drives the `mybtrfs` CLI and inspects observable state only), and
> **deterministic**. No implementation yet — these will later be interpreted and
> turned into executable tests, and the code is written to satisfy them.
>
> Companion specs: `01-phases-design-v2.md` (behavior), `02-architecture-v2.md`
> (architecture + the §6 fail-safe invariants), `04-coding-guidelines.md`.
> Each test cites the spec/invariant it validates ("**Spec:** …").

---

## 1. Test environment & fixtures

All E2E tests run against **loopback btrfs images** (sparse file →
`mkfs.btrfs` → loop-mount). They require root/sudo and are gated behind a
feature/env flag so plain unit `cargo test` stays runnable by non-root.

**Standard fixture (`two_filesystems`):**
- `POOL` — a btrfs "source pool" mounted at `/mnt/pool` (top-level, `subvolid=5`).
- `DRIVE` — a separate btrfs "external drive" mounted at `/mnt/drive`.
- Both are freshly formatted and empty unless a scenario states otherwise.

**Helpers (provided by the harness):**
- `make_subvol(path, dataset)` — create a btrfs subvolume and populate it with a
  known dataset (files whose **content hashes** are recorded).
- `mutate(path)` — add/modify files so the subvolume's generation advances.
- `content_hash(path)` — stable digest of a subvolume's file tree, for equality
  assertions.
- `clock_at(T, TZ)` — the harness **fixes the reference clock and timezone** the
  tool sees, so timestamped names and retention math are reproducible. Advancing
  the clock between steps lets a test create snapshots with chosen timestamps.
- `subvol(path)` — read observable btrfs metadata: *exists*, *readonly*,
  *received_uuid* (set/empty), *parent_uuid* (set/empty), *uuid*.

**Determinism:** every time-dependent test pins `clock_at(...)`; no test depends
on the ambient wall clock, host timezone, or network.

**Teardown:** unmount, detach loop devices, delete images — even on failure
(harness RAII). Leak-checked: no loop device or mount is left behind.

**Observable outcomes a test may assert:** process **exit code**;
**stdout/stderr** content (messages, tables, dry-run plans); **subvolume state**
on POOL/DRIVE (existence by name, readonly flag, received_uuid, parent_uuid);
**data equality** via `content_hash`; **absence** of stray/garbled subvolumes.

**Command vocabulary** (per the decided CLI surface): `run` (snapshot → send →
prune), `snapshot`, `resume`, `prune`, `restore`, `list`, `stats`,
`list-drives`; global `-n/--dry-run`, `--yes`.

---

## 2. Phase 1 — Drive selection & full backup

### E2E-P1-01 — list-drives shows the mounted btrfs drive
**Spec:** 01 Phase 1 (drive auto-detection).
- **Given** the `two_filesystems` fixture.
- **When** `mybtrfs list-drives` runs.
- **Then** exit code 0, and the output lists `/mnt/drive` (and `/mnt/pool`) as
  btrfs targets; if the loop device is marked removable, it is tagged as such.

### E2E-P1-02 — full backup, happy path
**Spec:** 01 Phase 1 behavior; 02 §6 rows 1–2.
- **Given** `POOL/home` exists with dataset A; `DRIVE` empty.
- **When** `mybtrfs run --source /mnt/pool/home --target /mnt/drive --yes`.
- **Then** exit 0; a backup subvolume `…/home.<ts>` exists under
  `/mnt/drive/<hostname>/`; it is **readonly**, **received_uuid set**,
  **parent_uuid empty**; and `content_hash(backup) == content_hash(POOL/home)`.

### E2E-P1-03 — a read-only source snapshot is created
**Spec:** 01 Phase 1 step 2; naming.
- **Given** the result of E2E-P1-02.
- **Then** a **readonly** snapshot `home.<ts>` exists in the default snapshot dir
  `/mnt/pool/.mybtrfs_snapshots/`, with the **same timestamp** as the backup.

### E2E-P1-04 — default directories auto-created (with confirmation / `--yes`)
**Spec:** 01 "CLI surface" (auto-create defaults).
- **Given** neither `/mnt/pool/.mybtrfs_snapshots` nor `/mnt/drive/<hostname>`
  exists.
- **When** `mybtrfs run --source /mnt/pool/home --target /mnt/drive --yes`.
- **Then** both directories are created; the backup succeeds. **And** without
  `--yes` in an interactive run, the tool *prompts before creating* (scripted
  prompter: declining aborts with no changes).

### E2E-P1-05 — explicit, non-existent target is an error (never created)
**Spec:** 01 Phase 1 error control.
- **When** `mybtrfs run --source /mnt/pool/home --target /mnt/drive/nope`
  where `nope` does not exist.
- **Then** non-zero exit, a clear message, and `nope` is **not** created; no
  snapshot is left behind.

### E2E-P1-06 — target is not btrfs → rejected
**Spec:** 01 Phase 1 validation.
- **Given** a non-btrfs directory (e.g. tmpfs) as target.
- **Then** non-zero exit with a "target is not btrfs" message; no snapshot, no
  partial state.

### E2E-P1-07 — source is not a subvolume → rejected
**Spec:** 01 Phase 1 validation.
- **Given** `--source` pointing at a plain directory (not a subvolume).
- **Then** non-zero exit with a clear message; nothing created.

### E2E-P1-08 — interactive drive pick
**Spec:** 01 Phase 1 (interactive selection); 02 `Prompter`.
- **Given** no `--target`; scripted prompter selects `/mnt/drive`.
- **Then** the backup lands on `/mnt/drive`; selecting an out-of-range option
  errors cleanly with no changes.

### E2E-P1-09 — same-minute re-run uses a collision counter
**Spec:** 01 Phase 1 robustness (idempotent naming).
- **Given** clock pinned so two runs share a timestamp.
- **When** `run` executes twice.
- **Then** the second snapshot/backup is named `home.<ts>_1`; the first is **not**
  overwritten; both are intact.

---

## 3. Phase 2 — Incremental backups

### E2E-P2-01 — incremental after a change
**Spec:** 01 Phase 2; 02 §6 rows 1–2, 4.
- **Given** a completed full backup (E2E-P1-02); then `mutate(POOL/home)`.
- **When** `mybtrfs run --source /mnt/pool/home --target /mnt/drive --yes`.
- **Then** a new backup `home.<ts2>` exists, **readonly**, **received_uuid set**,
  **parent_uuid set**; `content_hash(new backup) == content_hash(POOL/home)`
  (post-mutation); the previous backup still exists.

### E2E-P2-02 — the second transfer is a delta, not a full resend
**Spec:** 01 Phase 2 (incremental efficiency).
- **Given** E2E-P2-01 with a small mutation relative to a large dataset.
- **Then** the reported/transferred send-stream size is **much smaller** than the
  initial full transfer, and the new backup's `parent_uuid` chains to the prior
  backup (observable proof a parent was used).

### E2E-P2-03 — no common parent → full fallback
**Spec:** 01 Phase 2 (fallback).
- **Given** a prior backup, then the common backup on `DRIVE` is removed.
- **When** `run` executes again (default `--incremental=yes`).
- **Then** a **full** backup is created (new backup has **parent_uuid empty**),
  exit 0, data matches.

### E2E-P2-04 — strict mode refuses a timestamp-only parent
**Spec:** 01 Phase 2 (strict restricts to parent_uuid-related parents).
- **Given** a setup where a correlated candidate exists **only** by name/timestamp
  (the `parent_uuid` chain is broken), with `--incremental=strict`.
- **Then** non-zero exit, an explanatory message (suggesting `--incremental=yes`),
  and **no** full backup is created.

### E2E-P2-05 — strict mode refuses when no parent exists at all
**Spec:** 01 Phase 2 (strict never does full).
- **Given** an empty target and `--incremental=strict`.
- **Then** non-zero exit; nothing transferred.

### E2E-P2-06 — incremental survives a pruned intermediate snapshot
**Spec:** 01 Phase 2 robustness ("two independent means").
- **Given** a chain of backups; an **intermediate** source snapshot is pruned.
- **When** `run` executes again.
- **Then** the backup is still **incremental** (resolved via the timestamp-sibling
  strategy) — no costly full resend; chain remains valid.

---

## 4. Phase 3 — Retention / prune & the safety anchors

All Phase 3 tests pin `clock_at(...)` and seed snapshots/backups with chosen
timestamps.

### E2E-P3-01 — keep-all by default
**Spec:** 01 "CLI surface" (keep-all default); 01 Phase 3 note.
- **Given** several snapshots and backups.
- **When** `mybtrfs prune --source … --target …` with **no** policy flags.
- **Then** exit 0 and **nothing is deleted**.

### E2E-P3-02 — `preserve_min latest`
**Spec:** 01 Phase 3 (preserve_min).
- **Given** N snapshots; policy `--snapshot-preserve-min latest`.
- **Then** only the newest snapshot remains (plus any safety-anchored ones); the
  rest are deleted.

### E2E-P3-03 — daily/weekly/monthly tiers select an exact survivor set
**Spec:** 01 Phase 3 scheduler (the cascade); 02 §6 row 11 (determinism).
- **Given** a fixed list of backups with known dates and `clock_at(T, TZ)`, and a
  policy such as `--target-preserve "7d 4w 6m"`.
- **Then** the surviving set is **exactly** the enumerated expected list (the
  first-of-day/week/month representatives), and the deleted set is its exact
  complement. Re-running yields the identical result.

### E2E-P3-04 — snapshot and backup policies are independent
**Spec:** 01 Phase 3 (two policies).
- **Given** identical timestamp sets on POOL (snapshots) and DRIVE (backups).
- **When** `prune` runs with a *tight* `--snapshot-preserve` and a *loose*
  `--target-preserve`.
- **Then** the surviving counts differ accordingly (few snapshots, many backups).

### E2E-P3-05 — anchor: the just-created snapshot is always preserved
**Spec:** 02 §6 row 3.
- **Given** a policy that, on its own, would delete the newest.
- **When** `mybtrfs run …` (which snapshots then prunes).
- **Then** the snapshot created this run **survives** regardless of policy.

### E2E-P3-06 — anchor: the latest common pair is preserved (chain intact)
**Spec:** 02 §6 row 4.
- **Given** an aggressive policy on both sets.
- **When** `prune` runs.
- **Then** the latest common snapshot (POOL) **and** its backup (DRIVE) both
  survive; **and** a subsequent `run` is still **incremental** (proving the
  parent on both ends was kept).

### E2E-P3-07 — anchor: snapshots are not pruned if the target is unreachable
**Spec:** 02 §6 row 5; 01 Phase 3 anchor 3.
- **Given** snapshots + backups; then `DRIVE` is **unmounted**.
- **When** `mybtrfs run …` (or `prune …`) targeting the now-missing drive.
- **Then** source snapshots are **not** deleted; a warning is emitted; the
  partial-abort exit code is returned.

### E2E-P3-08 — anchor: a parent needed by a preserved backup is not deleted
**Spec:** 02 §6 row 6 (dependency closure).
- **Given** a policy that would delete a subvolume that a preserved backup still
  depends on as a parent.
- **Then** that dependency is **force-preserved** (not deleted).

### E2E-P3-09 — prune dry-run mutates nothing
**Spec:** 02 §6 row 8.
- **When** `mybtrfs prune … --dry-run` with a deleting policy.
- **Then** the would-delete list is printed and the filesystem is **unchanged**
  (same subvolumes before/after).

### E2E-P3-10 — prune ignores non-mybtrfs subvolumes
**Spec:** 01 Phase 3 security; 01 Phase 1 robustness.
- **Given** a manually-named subvolume (not matching `<name>.<ts>`) in the
  snapshot dir.
- **When** `prune` runs with any policy.
- **Then** the foreign subvolume is **left untouched**.

---

## 5. Phase 4 — Safe restore

### E2E-P4-01 — restore a backup to a new writable subvolume
**Spec:** 01 Phase 4 behavior; 02 §6 row 7.
- **Given** a backup `home.<ts>` on `DRIVE`; `DEST = /mnt/pool/home_restored`
  does not exist.
- **When** `mybtrfs restore --backup /mnt/drive/<host>/home.<ts> --dest DEST`.
- **Then** exit 0; `DEST` exists, is **read-write**, **received_uuid empty**;
  `content_hash(DEST) == content_hash(backup)`.

### E2E-P4-02 — restore does not break future incrementals (the critical rule)
**Spec:** 01 Phase 4 robustness; 02 §6 row 7.
- **Given** the restored `DEST` from E2E-P4-01.
- **When** `mybtrfs run --source DEST --target /mnt/drive --yes`.
- **Then** the backup **succeeds** (incrementally where a parent exists); i.e. the
  restore left no poisoned received marker. *(This is the test that would have
  caught the `btrfs property set` trap.)*

### E2E-P4-03 — restore refuses to overwrite an existing dest
**Spec:** 01 Phase 4 error control.
- **Given** `DEST` already exists.
- **When** `restore` runs **without** `--force`.
- **Then** non-zero exit, a clear refusal; `DEST` unchanged.

### E2E-P4-04 — `--force` moves the old dest aside (preserved, not destroyed)
**Spec:** 01 Phase 4 error control; 02 `FilesystemPort` rename.
- **Given** `DEST` exists with dataset X.
- **When** `restore … --force`.
- **Then** the prior `DEST` is renamed to `DEST.broken` (dataset X intact) and the
  restore proceeds; nothing is silently deleted.

### E2E-P4-05 — restore dry-run prints the plan, changes nothing
**Spec:** 02 §6 row 8.
- **When** `restore … --dry-run`.
- **Then** the exact intended operations are printed; no subvolume is created,
  moved, or deleted.

### E2E-P4-06 — restore is incremental when a common parent exists on the pool *(deferred refinement)*
**Spec:** 01 Phase 4 behavior. *Transfer-back is currently a **full** send/receive
(decision ID-5) — correct and always applicable, but not delta-optimized. Restores
are infrequent, so the full send is an acceptable simplification; this scenario
tracks the future incremental refinement.*
- **Given** an older snapshot of the data still present on POOL.
- **When** `restore` runs.
- **Then** the transfer back uses that parent (delta), and the result is correct.

---

## 6. Cross-cutting

### E2E-CC-01 — backup dry-run creates nothing
**Spec:** 02 §6 row 8.
- **When** `mybtrfs run … --dry-run`.
- **Then** no snapshot and no backup are created; a plan is printed; exit 0.

### E2E-CC-02 — idempotent re-runs
**Spec:** 02 §6 row 9.
- **When** `run` executes twice (keep-all).
- **Then** two snapshot/backup generations exist, no clobber, no error.

### E2E-CC-03 — duplicate `uuid` is refused
**Spec:** 02 §6 row 10; 01 Phase 2.
- **Given** `DRIVE` is a **block-level clone** of POOL (so a subvolume `uuid`
  collides), and both are mounted.
- **When** any relationship-using command runs.
- **Then** the tool **refuses** with a clear "duplicate UUID / cloned filesystem"
  error and performs no destructive action.

### E2E-CC-04 — option-injection via a flag-like path is rejected
**Spec:** 01/04 security (no option injection).
- **When** a path argument begins with `-` (e.g. `--source -rf`).
- **Then** it is rejected as invalid input; nothing runs.

### E2E-CC-05 — relative paths are rejected
**Spec:** 04 §5 (absolute paths).
- **When** a relative `--source`/`--target` is given.
- **Then** non-zero exit with a clear message.

### E2E-CC-06 — no shell interpretation of paths
**Spec:** 01/04 security (argv, never a shell).
- **Given** a subvolume whose path contains spaces and shell metacharacters
  (e.g. `/mnt/pool/we ird;$(touch x)`).
- **When** `run` backs it up.
- **Then** it works correctly and **no** side effect occurs (no file `x` created);
  proves arguments are passed as argv, not via a shell.

### E2E-CC-07 — garbled subvolume is cleaned up on interrupted transfer
**Spec:** 01 Phase 1 robustness; 02 §6 rows 1–2.
- **Given** `DRIVE` sized so the transfer hits **ENOSPC** mid-receive (or the
  process is killed mid-transfer).
- **Then** non-zero exit; **no** writable/garbled leftover subvolume remains on
  `DRIVE`; a later retry can succeed cleanly.

### E2E-CC-08 — exit codes are stable and scriptable
**Spec:** 01 cross-cutting (exit codes). *(Codes proposed to mirror btrbk —
pending final confirmation.)*
- **Then:** success → `0`; usage/parse error → `2`; lock held → `3`; insufficient
  privileges (needs root) → `4` *(a mybtrfs addition, decision ID-6 — no btrbk
  equivalent)*; at least one backup task aborted → `10`; other generic failure →
  `1`. Each asserted in its relevant scenario above.

### E2E-CC-09 — concurrency lock prevents overlapping runs
**Spec:** robustness (run lock; decision ID-4). Implemented as an advisory
`flock` on `--lock <PATH>` (default `<tmpdir>/mybtrfs.lock`), held only by
state-changing commands (`run`/`snapshot`/`resume`/committing `prune`/`restore`).
- **Given** one `mybtrfs` run holding the lock.
- **When** a second `mybtrfs` starts.
- **Then** the second exits immediately with the lock-busy code (`3`) and makes
  no changes.

### E2E-CC-10 — timezone-independent determinism
**Spec:** 02 §6 row 11; 01 Phase 3 (TZ as input).
- **Given** identical fixtures and `clock_at(T, TZ)` run under two different host
  timezones.
- **Then** the prune decisions are **identical** (timezone is an injected input,
  not ambient state).

### E2E-CC-11 — the transaction journal records actions *(if Journal enabled)*
**Spec:** 02 `Journal`; 01 cross-cutting (logging).
- **When** a `run` completes.
- **Then** the journal contains entries for snapshot-create, send/receive, and any
  delete, each with source/target/status.

---

## 7. Traceability matrix (spec/invariant → tests)

Confirms every fail-safe invariant in `02` §6 and every phase behavior is covered.

| Spec / invariant | Covered by |
|------------------|-----------|
| 02 §6-1 transfer verified (not by exit code) | P1-02, P2-01, CC-07 |
| 02 §6-2 garbled cleanup | CC-07 |
| 02 §6-3 just-created preserved | P3-05 |
| 02 §6-4 latest common pair preserved | P3-06 |
| 02 §6-5 skip prune if target aborted | P3-07 |
| 02 §6-6 parent of preserved backup kept | P3-08 |
| 02 §6-7 restore cannot poison received_uuid | P4-01, **P4-02** |
| 02 §6-8 dry-run never mutates | P3-09, P4-05, CC-01 |
| 02 §6-9 re-runs non-destructive | P1-09, CC-02 |
| 02 §6-10 duplicate-uuid refused | CC-03 |
| 02 §6-11 deterministic scheduling | P3-03, CC-10 |
| 02 §6-12 partial failure degrades safely | P3-07, CC-08 |
| Phase 1 full backup + drive select | P1-01..P1-09 |
| Phase 2 incremental + parent resolution | P2-01..P2-06 |
| Phase 3 retention + two policies | P3-01..P3-10 |
| Phase 4 safe restore | P4-01..P4-06 |
| Security (argv, absolute paths, no shell) | CC-04, CC-05, CC-06 |

---

## 8. Notes for turning these into executable tests (later)

- Keep them **black-box**: drive the compiled binary; assert on exit code,
  stdout/stderr, and btrfs state — not on internal calls.
- Each scenario should be **independent** and start from a fresh fixture.
- Time-dependent scenarios **must** pin `clock_at(...)`; never read the host clock.
- Gate the whole suite behind the root/loopback feature flag; in CI, run it in a
  privileged job, and keep the pure-logic unit tests (scheduler, parser, parent
  ranking) as the fast, always-on layer beneath this one.
- Write the test for a behavior **before** implementing it (TDD): red → green →
  refactor, with these scenarios as the source of "red".
