# mybtrfs — Phased Design (v2, development)

> **Development reference.** Functional design for Phases 1–4: per phase, the
> **use cases**, **what's needed**, **behavior**, **error control**,
> **security**, and **robustness**. This v2 folds in the corrections from the
> documentation review (accuracy fixes, divergence notes, consistency wording);
> it supersedes `01-phases-design.md`. Companion: `02-architecture-v2.md`.
> Grounded in — and verified against — the btrbk Perl source
> (`../btrbk/btrbk/btrbk`); key line citations are inline.

## Foundational decisions

- **CLI-first** — driven by per-invocation commands and flags; no config file yet
  (deferred to Phase 5+).
- **Stateless** — every run re-derives all relationships from live btrfs metadata
  (subvolume UUIDs). No side database that could drift from reality.
- **Shell out to `btrfs`** — orchestrate the standard `btrfs` CLI (spawn the
  `btrfs` process directly with an argv array — never via a shell) rather than
  binding to libbtrfsutil/ioctls.
- **Scope** — a single local source and a single local target (e.g. an external
  drive) per invocation. Multiple targets/sources, remote/ssh, raw/encrypted
  targets, and scheduling are out of scope until Phase 5+. (The delete-safety
  rules are written for "a/any target" so they extend unchanged when multiple
  targets arrive.)

### Relationship to btrbk (faithful vs. divergent)

mybtrfs reuses btrbk's **proven core logic** (snapshot/transfer mechanics, UUID
relationship tracking, parent resolution, the retention scheduler, the
delete-safety rules) but **diverges deliberately** in three places — called out
again in the relevant phases:

- **Interface (Phase 1):** btrbk is config-file-driven with explicit paths and
  has *no drive discovery*. mybtrfs is CLI-first with interactive **drive
  auto-detection**. *(divergence)*
- **Restore (Phase 4):** btrbk has *no restore command* — restore is a manual
  procedure in its README/FAQ. mybtrfs **automates** that documented procedure.
  *(addition)*
- **Duplicate UUIDs (Phase 2):** btrbk treats UUIDs as globally unique and only
  *warns* on collisions. mybtrfs **hard-refuses**. *(robustness improvement)*

### CLI surface (decided)

**Commands** — btrbk-style: a full-cycle command plus granular ones.

| Command | Does | Service(s) |
|---------|------|-----------|
| `run` | full cycle: snapshot → send/receive → prune (snapshots + backups) | BackupService + RetentionService |
| `snapshot` | create snapshot, then prune snapshots only | BackupService + RetentionService |
| `resume` | send/receive + prune, **no new snapshot** | BackupService + RetentionService |
| `prune` | delete per policy only | RetentionService |
| `restore` | restore a backup to a working subvolume | RestoreService |
| `list` / `stats` / `list-drives` | read-only inventory | InventoryService |
| `list-subvolumes` | read-only: every btrfs subvolume on the local system (all filesystems) — pick a backup source | LocalSubvolumesService (DriveDiscovery + SubvolumeRepository) |

**Retention — keep-all by default (btrbk-faithful):** nothing is deleted unless a
policy is supplied via `--snapshot-preserve[-min]` and/or `--target-preserve[-min]`
(default `preserve_min = all`). The two policies are independent (snapshots vs
backups).

**Directories — auto-create with defaults:** when not given explicitly,
- the snapshot dir defaults to `<source-parent>/.mybtrfs_snapshots`,
- backups go to `<target-mountpoint>/<hostname>/`.

Missing **default** dirs are created on first run **after confirmation** (`--yes`
for non-interactive/cron). An **explicitly given** `--snapshot-dir`/`--target`
that doesn't exist is an *error* (never silently created), so a typo can't spawn a
stray backup tree.

---

## Phase 1 — Pick a drive & make a (full) backup

### Use cases
- "Plug in my external btrfs drive, pick it from a list, and back up a subvolume
  to it." First-ever backup of a source (no prior history).
- Discover which mounted filesystems are valid backup targets.

### What's needed
- A **source** btrfs subvolume to protect.
- A **target**: chosen interactively from auto-detected btrfs filesystems, or
  given explicitly with `--target`. Backups default to
  `<target-mountpoint>/<hostname>/`. *(Drive auto-detection is a mybtrfs addition
  over btrbk — see foundational note.)*
- A **snapshot location** on the source side (default
  `<source-parent>/.mybtrfs_snapshots`); default dirs are auto-created on first
  run after confirmation (see CLI surface).
- `btrfs-progs` installed; privileges to run btrfs operations (root, or a
  privilege helper later).

### Behavior
1. Resolve and validate the target (is it btrfs? mounted? writable?).
2. Create a **read-only snapshot** of the source
   (`btrfs subvolume snapshot -r <src> <dst>`; btrbk 1368–1390).
3. Transfer it **in full** to the target — `btrfs send <snap>` (no `-p`, no `-c`)
   piped into `btrfs receive <target>/` — then **verify** the received copy.

### Error control
- Validate inputs up front: source exists and is a subvolume; target is a
  mounted, writable btrfs filesystem. Missing **default** snapshot/target dirs are
  created after confirmation (`--yes` to skip the prompt); an **explicitly given**
  `--snapshot-dir`/`--target` that does not exist is an error — never silently
  created.
- The send→receive transfer cannot be trusted by exit code alone (a classic
  btrfs/pipe pitfall: a failed `send` can still yield `receive` exit 0). After
  receiving, **independently verify** the received subvolume's metadata
  (btrbk 1518–1597):
  - it is **read-only**;
  - its **received_uuid is set**;
  - its **parent_uuid is *unset*** (a full backup must have no parent_uuid; this
    catches a wrong/partial transfer).
- Detect a **garbled/incomplete** backup (writable, no received_uuid) and
  **clean it up** (`btrfs subvolume delete --commit-each`) so it can't later be
  mistaken for a valid parent.
- Surface the underlying `btrfs` stderr verbatim on failure; classify `ERROR:`
  lines as fatal.

### Security
- Treat all paths as untrusted: require absolute paths, reject option-injection
  (a path that looks like a flag), and pass arguments directly — never via a
  shell string.
- Be explicit about the privilege requirement; never silently escalate.
- Drive auto-detection only *reads* system metadata; never act on a device
  without explicit user selection.

### Robustness
- A read-only btrfs snapshot is **filesystem-atomic / crash-consistent** — the
  safe basis for transfer. Application-level consistency (open databases, etc.)
  requires the application to flush first; mybtrfs does not quiesce or `sync`
  before snapshotting (matching btrbk).
- Idempotent naming: `<basename>.<timestamp>[_N]` with a collision counter so
  repeated runs never clobber each other.
- Leave anything not matching mybtrfs's naming scheme untouched.
- Verify-and-cleanup guarantees the target is left clean even if the transfer is
  interrupted (network/disk/kill).

---

## Phase 2 — Incremental backups

### Use cases
- "Back up again — send only what changed." The everyday case after Phase 1.
- Fast, low-bandwidth backups suitable for frequent runs.

### What's needed
- A **common ancestor** present on *both* the source (snapshot) and the target
  (backup) — what makes an incremental transfer possible.
- The full **UUID relationship picture** of source and target subvolumes (own
  UUID, parent relationship, received-from relationship; from
  `btrfs subvolume list -a -c -u -q -R` plus a `-r` pass for the readonly flag).

### Behavior
- Read all relevant subvolumes and build the relationship graph.
- **Correlate** source snapshots with their target copies via the UUID
  predicate (both read-only AND any of: `a.uuid == b.received_uuid`,
  `b.uuid == a.received_uuid`, `a.received_uuid == b.received_uuid`; btrbk
  `_is_correlated` 2585–2589).
- **Select the best parent** — the most recent common ancestor to transfer
  against — optionally adding correlated subvolumes as "clone sources" to share
  more data. Transfer the delta with `btrfs send -p <parent> [-c <clone>...]`.
- If **no** common parent exists: fall back to a full backup. In **strict** mode
  mybtrfs instead **refuses**, and additionally restricts the parent to one with
  an actual `parent_uuid` relationship — rejecting parents matched only by
  timestamp correlation (btrbk 3699–3704).

### Error control
- If parent resolution is ambiguous or the chain is broken, prefer a correct full
  backup over a wrong incremental — never produce an invalid chain.
- Guard against pathological/cyclic relationship data (bounded traversal).
- Verify the received incremental as in Phase 1, plus the inverse parent check:
  an incremental result **must have parent_uuid set** (btrbk 1562–1565).
- **Refuse to operate on a duplicate `uuid`.** The subvolume `uuid` is the
  one-to-one key the relationship graph is built on; two subvolumes sharing a
  `uuid` (e.g. a disk cloned with `dd`) break the model, so mybtrfs treats it as a
  hard error (btrbk only *warns*). This is **not** the expected
  `received_uuid`/`parent_uuid` *links* (one-to-many), which are how backups
  correlate to their sources. *(improvement)*

### Security
- Same path-handling rules as Phase 1.
- Relationship decisions come from authoritative btrfs metadata, not filenames —
  names are only a secondary hint, never the sole basis for trusting that two
  subvolumes share data.

### Robustness
- The relationship chain can **break after pruning** (deleting an intermediate
  subvolume can orphan descendants). Parent selection therefore considers
  candidates by **two independent means** — by recorded `parent_uuid`
  relationship *and* by matching timestamped siblings in the snapshot location —
  so a prune doesn't silently force a costly full resend.
- Parents must be reachable on the same mountpoint as the source, or an
  incremental send is impossible — so each subvolume carries its owning
  filesystem-UUID/mountpoint, and the (pure) resolver filters candidates on it and
  reports when none are reachable.

---

## Phase 3 — Manage backups (list, stats, retention/prune)

### Use cases
- "Show me what snapshots and backups exist and how much space they use."
- "Apply my retention policy — keep N hourly/daily/weekly/monthly/yearly, delete
  the rest — without breaking incremental backups."
- Reclaim disk after changing the policy.

### What's needed
- **Two independent retention policies** (as in btrbk): one for **source
  snapshots** and one for **backups on the target** — each a minimum-keep window
  (`preserve_min`) plus how many of each tier to preserve, with the day/hour
  boundaries that define the tiers. (E.g. keep few local snapshots but many
  backups.)
- The existing snapshots/backups with their **creation times recovered from
  their names**.

### Behavior
- **Scheduler** (pure, deterministic; btrbk `sub schedule` 4541–4752): run
  **once per set** — source snapshots with the snapshot policy, target backups
  with the backup policy — classifying each entry as *preserve* or *delete*.
  Tiers cascade — the **first (earliest) of each period** is the
  representative and rolls up into the next tier (first-of-day → first-of-week →
  first weekly of month → first weekly of year). Day boundary = `preserve_hour_of_day`;
  week/month/year anchored on `preserve_day_of_week`. `short`-format timestamps
  (no time-of-day) use a 00:00 boundary; `short`/`long` are local time, `long-iso`
  is absolute.
- **Delete** the non-preserved subvolumes
  (`btrfs subvolume delete [--commit-each]`).

> Default: with no policy supplied, `preserve_min = all` for both sets, so
> `prune`/`run` delete nothing until you set a policy (btrbk-faithful).

### Error control
- The scheduler is side-effect-free, so it is exhaustively unit-testable
  independent of any filesystem.
- Time-zone correctness matters (see the format note above); the boundary math
  must account for it.
- If a deletion fails, report it and stop rather than continuing blindly.

### Security
- Deletion is the highest-risk operation. Only ever delete subvolumes that match
  mybtrfs's own naming scheme and live where mybtrfs expects them — never
  arbitrary paths.
- A dry-run mode must show exactly what *would* be deleted before anything is
  removed.

### Robustness — the delete-safety anchors (all required)
1. **The snapshot/backup created in the current run is always preserved** (on
   `run`/`snapshot`) — guarantees a fresh restore point exists regardless of
   policy (btrbk `'preserve forced: created just now'`, 6706).
2. **The latest common snapshot/backup pair is always preserved**, on both `run`
   and `prune` — the anchor that lets the *next* incremental backup find a parent
   on both ends; pruning must never sever it (btrbk 6884–6897).
3. **Skip snapshot deletion entirely if any target was unreachable/aborted** — a
   missing destination must not cause the source to lose the only resumable copy
   (btrbk 6930–6936).
4. **Never delete a subvolume that a preserved backup still needs as a parent.**

> Note: on a *standalone* `prune` the absolute newest is preserved only via
> `preserve_min` (`all`/`latest`) or anchor #2 — there is no unconditional
> "keep newest" rule beyond those (this matches btrbk).

---

## Phase 4 — Safe restore

> **Divergence/addition:** btrbk has **no** restore command — restoring is a
> manual procedure in its README ("Restoring Backups") and FAQ ("Received
> UUID"). mybtrfs **automates that documented procedure**, faithful to its steps
> and guard rails.

### Use cases
- "Restore a backup to a working subvolume after data loss or a bad change."
- Bring a backup from the external drive back onto the main pool as a usable,
  writable subvolume.

### What's needed
- The **backup** to restore and a **destination** for the restored working copy.
- Enough free space; and (when restoring from the backup drive) the ability to
  transfer back to the source pool.

### Behavior
1. If the backup lives on a separate location, transfer it back to the pool
   (incrementally when a common parent exists, otherwise in full).
2. Create a **read-write working subvolume** from the restored read-only copy
   (`btrfs subvolume snapshot <restored_ro> <dest>` — **without `-r`**).

### Error control
- Refuse to overwrite an existing destination unless explicitly forced; on force,
  move the existing one aside (preserved, not destroyed) and say so.
- Provide a dry-run that prints the exact intended operations.
- After restoring, verify the working subvolume is in the expected state.

### Security
- Restore writes to the live system — confirm destructive aspects explicitly.
- Same strict path handling as the other phases.

### Robustness — the critical correctness rule
- **Never** flip a restored subvolume to read-write via `btrfs property set`.
  Doing so leaves a stale "received" marker that **silently breaks all future
  incremental backups**. The only safe way to a writable copy is a fresh
  (non-read-only) snapshot of the restored read-only subvolume.
- After restore, the working subvolume must have an **empty received marker** —
  verify this, and keep the restored read-only copy until a fresh backup exists,
  so the incremental chain stays intact.

---

## Cross-cutting concerns

### Error control (global)
- One central place builds and runs every `btrfs` invocation, so error
  classification, captured output, and logging are uniform.
- Distinguish *expected* failures (unreachable target, no common parent, missing
  snapshot dir) — reported with actionable messages and meaningful exit codes —
  from *unexpected* ones.
- Define a small, stable set of exit codes for scripting: success `0` / generic
  error `1` / usage error `2` / lock held `3` / needs-root `4` / partial-abort
  `10`. (`3`/`4` are mybtrfs additions over the original sketch — decisions
  ID-4/ID-6; see `07-implementation-decisions.md`.)

### Security (global)
- No shell interpolation; arguments passed directly. Absolute-path validation and
  rejection of flag-like paths everywhere.
- Privilege requirements are explicit and never auto-escalated.
- Read-only discovery (drives, subvolumes, stats) is strictly separated from
  state-changing actions (snapshot, send/receive, delete).

### Robustness (global)
- Stateless and idempotent: safe to re-run; re-derives truth from the filesystem
  each time.
- The just-created snapshot, the latest common pair, and any subvolume needed as
  a parent are protected from deletion by construction.
- Interrupted transfers are detected and cleaned up; UUID-uniqueness is checked
  rather than assumed.

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
