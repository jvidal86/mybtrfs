# mybtrfs — Phased Design (Phases 1–4)

> Design document, **no code**. One section per phase, each describing: the
> **use cases**, **what's needed** (inputs/prerequisites), **error control**,
> **security**, and **robustness**. Cross-cutting concerns are collected at the
> end. Grounded in the proven mechanics of btrbk.

## Foundational decisions

- **CLI-first** — driven by per-invocation commands and flags; no config file
  yet (deferred to Phase 5+).
- **Stateless** — every run re-derives all relationships from live btrfs
  metadata (subvolume UUIDs). No side database that could drift from reality.
- **Shell out to `btrfs`** — orchestrate the standard `btrfs` CLI rather than
  binding to libbtrfsutil/ioctls. Simpler, portable, and the behavior is well
  understood.
- **Scope** — local source and local target (e.g. an external drive). Remote/ssh,
  raw/encrypted targets, and scheduling are out of scope until Phase 5+.

---

## Phase 1 — Pick a drive & make a (full) backup

### Use cases
- "Plug in my external btrfs drive, pick it from a list, and back up a subvolume
  to it." First-ever backup of a source (no prior history).
- Discover which mounted filesystems are valid backup targets.

### What's needed
- A **source** btrfs subvolume to protect.
- A **target**: either chosen interactively from auto-detected btrfs
  filesystems, or given explicitly as a mounted path.
- A **snapshot location** on the source side to hold the read-only snapshot.
- `btrfs-progs` installed; privileges to run btrfs operations (root, or a
  privilege helper later).
- Drive detection reads the system mount table and block-device metadata to
  present candidates (with removable/USB/size hints).

### Behavior (conceptual)
1. Resolve and validate the target (is it btrfs? mounted? writable?).
2. Create a **read-only snapshot** of the source.
3. Transfer it **in full** to the target (no parent), then **verify** the
   received copy.

### Error control
- Validate inputs up front: source exists and is a subvolume; target is a
  mounted, writable btrfs filesystem; snapshot location exists (do **not**
  silently create it — fail with a clear message).
- The send→receive transfer cannot be trusted by exit code alone (a classic
  btrfs/pipe pitfall). After receiving, **independently verify** the result by
  reading the received subvolume's metadata.
- Detect a **garbled/incomplete** backup (writable, no received-UUID) and
  **clean it up** so it can't be mistaken for a valid parent later.
- Surface the underlying `btrfs` stderr verbatim on failure; classify lines that
  signal real errors as fatal.

### Security
- Treat all paths as untrusted input: require absolute paths, reject
  option-injection (a path that looks like a flag), and avoid shell
  interpolation entirely (pass arguments directly, never via a shell string).
- Be explicit about the privilege requirement; never silently escalate.
- Drive auto-detection only *reads* system metadata; it must never act on a
  device without the user's explicit selection/confirmation.

### Robustness
- A read-only snapshot is an atomic, consistent point-in-time copy — the safe
  basis for transfer.
- Idempotent naming: timestamped names with a collision counter so repeated runs
  never clobber each other.
- Leave anything not matching mybtrfs's naming scheme untouched.
- The verify-and-cleanup step guarantees the target is left in a clean state even
  if the transfer is interrupted (network/disk/kill).

---

## Phase 2 — Incremental backups

### Use cases
- "Back up again — only send what changed since last time." The everyday case
  after Phase 1 has established a baseline.
- Fast, low-bandwidth backups suitable for frequent runs.

### What's needed
- An existing **common ancestor** present on *both* the source (snapshot) and the
  target (backup) — this is what makes an incremental transfer possible.
- The full **UUID relationship picture** of source and target subvolumes
  (each subvolume's own UUID, its parent relationship, and its received-from
  relationship).

### Behavior (conceptual)
- Read all relevant subvolumes and build the relationship graph.
- **Correlate**: find, on the target, the copy that corresponds to a given source
  snapshot (same content, established via received-from / parent links).
- **Select the best parent**: choose the most recent common ancestor to transfer
  *against*; optionally add extra correlated subvolumes as "clone sources" to
  share even more data.
- Transfer only the delta. If **no** common parent exists, fall back to a full
  backup. A future "strict" mode refuses rather than sending a full backup.

### Error control
- If parent resolution is ambiguous or the chain is broken, prefer a correct
  full backup over a wrong/abORTED incremental — never produce an invalid chain.
- Guard against pathological/cyclic relationship data (bounded traversal).
- Verify the received incremental the same way as a full backup, and additionally
  check that the parent relationship is correctly recorded on the result.

### Security
- Same path-handling rules as Phase 1.
- Relationship decisions are made from authoritative btrfs metadata, not from
  filenames alone — names are only a secondary hint, never the sole basis for
  trusting that two subvolumes share data.

### Robustness
- The relationship chain can **break after pruning** (deleting an intermediate
  subvolume can orphan its descendants). The parent-selection strategy therefore
  considers candidates by **two independent means** — by recorded parent
  relationship *and* by matching timestamped siblings in the snapshot location —
  so a prune doesn't silently force a costly full resend.
- Treat UUIDs as globally unique identifiers; refuse to operate when duplicate
  UUIDs are detected (e.g. from a cloned disk), since that breaks the model.
- Parents must be reachable on the same filesystem/mountpoint as the source, or
  an incremental send is impossible — detect and report this rather than failing
  opaquely.

---

## Phase 3 — Manage backups (list, stats, retention/prune)

### Use cases
- "Show me what snapshots and backups exist, and how much space they use."
- "Apply my retention policy — keep N hourly/daily/weekly/monthly/yearly,
  delete the rest — without breaking my ability to back up incrementally."
- Clean up disk space after changing the policy.

### What's needed
- A **retention policy**: a minimum-keep window plus how many of each tier
  (hourly/daily/weekly/monthly/yearly) to preserve, with the day/hour boundaries
  that define those tiers.
- The set of existing snapshots/backups with their **creation times recovered
  from their names**.

### Behavior (conceptual)
- **Scheduler** (pure, deterministic): given the timestamps and the policy,
  classify every snapshot/backup into *preserve* or *delete*. The tiers cascade
  (the first of each period rolls up into the next), with a well-defined
  "first-in-bucket wins" rule and explicit day/hour boundaries.
- **Delete** the non-preserved subvolumes.

### Error control
- The scheduler is isolated and side-effect-free so it can be exhaustively
  unit-tested and reasoned about independently of any filesystem.
- Time-zone correctness matters: timestamp formats without a time-of-day are
  interpreted in local time; the format with an explicit offset is absolute.
  The boundary math must account for this.
- If a deletion fails, report it and stop rather than continuing blindly.

### Security
- Deletion is the highest-risk operation. Only ever delete subvolumes that match
  mybtrfs's own naming scheme and live where mybtrfs expects them — never
  arbitrary paths.
- A dry-run mode must show exactly what *would* be deleted before anything is
  removed; deletion of many items warrants explicit intent.

### Robustness — the delete-safety anchors (all three required)
1. **Always keep the newest** snapshot/backup, regardless of policy.
2. **Always keep the latest common snapshot/backup pair** — the anchor that lets
   the *next* incremental backup find a parent on both ends. Pruning must never
   sever this.
3. **Skip snapshot deletion entirely if a target was unreachable/aborted** — a
   missing backup destination must not cause the source to lose the only copy
   needed to resume later.
- Never delete a subvolume that another preserved backup still depends on as a
  parent.

---

## Phase 4 — Safe restore

### Use cases
- "Restore a backup back to a working subvolume after data loss or a bad change."
- Bring a backup from the external drive back onto the main pool as a usable,
  writable subvolume.

### What's needed
- The **backup** to restore and a **destination** for the restored working copy.
- Enough free space, and (when restoring from the backup drive) the ability to
  transfer back to the source pool.

### Behavior (conceptual)
1. If the backup lives on a separate location, transfer it back to the pool
   (incrementally when a common parent exists, otherwise in full).
2. Create a **read-write working subvolume** from the restored read-only copy.

### Error control
- Refuse to overwrite an existing destination unless explicitly forced; on force,
  move the existing one aside (preserved, not destroyed) and say so.
- Provide a dry-run that prints the exact intended operations.
- After restoring, verify the working subvolume is in the expected state.

### Security
- Restore writes to the live system — confirm destructive aspects explicitly.
- Same strict path handling as the other phases.

### Robustness — the critical correctness rule
- **Never** flip a restored subvolume to read-write via the low-level property
  command. Doing so leaves a stale "received" marker that **silently breaks all
  future incremental backups**. The only safe way to get a writable copy is to
  make a fresh (non-read-only) snapshot of the restored read-only subvolume.
- After restore, the working subvolume must have an **empty received marker** —
  verify this, and keep the restored read-only copy until a fresh backup exists,
  so the incremental chain stays intact.

---

## Cross-cutting concerns

### Error control (global)
- One central place builds and runs every `btrfs` invocation, so error
  classification, captured output, and (later) logging are uniform.
- Distinguish *expected* failures (unreachable target, no common parent, missing
  snapshot dir) — reported with actionable messages and meaningful exit codes —
  from *unexpected* ones.
- Define a small, stable set of exit codes (success / generic error / usage error
  / partial-abort) for scripting.

### Security (global)
- No shell interpolation; arguments passed directly. Absolute-path validation and
  rejection of flag-like paths everywhere.
- Privilege requirements are explicit and never auto-escalated; a future
  non-root mode (via a privilege helper) is an additive Phase 5+ concern.
- Read-only discovery (drives, subvolumes, stats) is strictly separated from
  state-changing actions (snapshot, send/receive, delete).

### Robustness (global)
- Stateless and idempotent: safe to re-run; re-derives truth from the filesystem
  each time.
- The latest snapshot, the latest common pair, and any subvolume needed as a
  parent are protected from deletion by construction.
- Interrupted transfers are detected and cleaned up; UUID-uniqueness assumptions
  are checked rather than assumed.

### Testing & verification (planned)
- Pure logic (name/timestamp parsing, the retention scheduler, parent ranking) is
  unit-tested in isolation.
- End-to-end behavior is exercised against **loopback btrfs images** (a sparse
  file formatted as btrfs and loop-mounted) so no physical drive is required;
  these tests need elevated privileges and are gated accordingly.

---

## Out of scope until Phase 5+
Config-file support · remote/ssh sources & targets · raw + compressed/encrypted
targets · scheduling/automation · richer output formats · non-root backends.
