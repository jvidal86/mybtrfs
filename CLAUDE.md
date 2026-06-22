# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

`mybtrfs` is a ground-up **Rust reimagining of [btrbk]** — a backup tool for
btrfs subvolumes (atomic read-only snapshots + incremental `btrfs send/receive`
+ retention). It is intended to **shell out to the `btrfs` CLI** (not
libbtrfsutil/ioctls) and stay **stateless**: every run re-derives all subvolume
relationships from live btrfs metadata (UUIDs), never from a side database.

[btrbk]: https://github.com/digint/btrbk

## Current state — read this first

The repository currently contains **only `documentation/`**. There is no Rust
code, no `Cargo.toml`, and no git repo in the tree yet. The project is in the
**design-documentation stage**: the design is fully specified and reviewed, but
implementation has not started. Do not assume any build/test tooling exists
until `Cargo.toml` and `src/` are created.

## Source of truth — the design lives in `documentation/`

These three documents are the authoritative spec. Read them before implementing
anything; they encode decisions that span many future files.

- **`documentation/01-phases-design.md`** — functional design, organized into
  the four delivery phases (use cases, prerequisites, error control, security,
  robustness per phase).
- **`documentation/02-architecture.md`** — the module architecture: hexagonal
  (ports & adapters) + SOLID, with Mermaid sequence diagrams and a fail-safe
  verification table.
- **`documentation/03-review-and-corrections.md`** — a correctness review of the
  above against the btrbk source, listing verified-faithful items, accuracy
  fixes still to apply, and intentional divergences from btrbk.

## Reference implementation

The original btrbk Perl program sits at **`../btrbk/btrbk/btrbk`** (one script,
~7000 lines) with its man pages and FAQ under **`../btrbk/btrbk/doc/`**. The
mybtrfs design was verified line-by-line against it. When implementing a
mechanism, consult the corresponding btrbk logic — `03-review-and-corrections.md`
cites the exact line ranges (e.g. the retention scheduler at `sub schedule`
4541–4752; send/receive verification 1518–1597; correlation `_is_correlated`
2585–2589). The goal is to be **parallel to btrbk's proven logic**.

## Architecture (the big picture)

Hexagonal / ports & adapters, dependencies pointing **inward**:

- **Domain core (pure, no I/O):** `ParentResolver` (UUID correlation + parent
  selection), `RetentionScheduler` (h/d/w/m/y cascade), `SafetyPolicy` (the
  delete/restore safety rules), and the model (`Subvolume`, three UUID indexes,
  `RetentionPolicy`, `Schedule`). This is the riskiest logic and must be unit
  testable with zero I/O.
- **Application use cases:** `BackupService`, `RetentionService`,
  `RestoreService`, `InventoryService` — orchestrate via ports only.
- **Driven ports (traits):** `SubvolumeRepository`, `SnapshotPort`,
  `TransferPort` (send/receive **and** verify), `DeletePort`,
  `DriveDiscoveryPort`, `ClockPort`, `Prompter`, `Journal`.
- **Adapters:** prod (`BtrfsCliAdapter`, `ProcMounts/LsblkAdapter`, `SystemClock`,
  `StdioPrompter`) and test (`FakeBtrfs`, `FixedClock`, `ScriptedPrompter`,
  `LoopbackBtrfs`). The **CLI is the composition root** that wires concrete
  adapters.

Key structural intent: the dangerous operations (delete, make-writable, transfer)
are reachable only through narrow ports whose contracts *embed* the safety
checks, so the fail-safe properties are architectural, not conventional.

## Delivery phases (roadmap)

1. **Pick a drive & full backup** — drive auto-detect, read-only snapshot,
   `send | receive`, verify received subvolume.
2. **Incremental backups** — UUID relationship graph, parent/clone-source
   resolution, `send -p`.
3. **Manage** — list/stats + the retention scheduler and safe prune.
4. **Safe restore** — transfer back + writable snapshot, guarding the
   received-uuid trap.

Phase 5+ (config file, remote/ssh, raw/encrypted targets, scheduling) is out of
scope until the four phases land.

## Invariants any implementation MUST preserve

These are the non-obvious correctness rules carried over from btrbk (details +
source citations in the docs):

- **Never trust a `send|receive` by exit code.** After receive, verify the target
  is readonly, has a `received_uuid`, and has the correct `parent_uuid`
  (unset for a full backup, set for an incremental). Detect a garbled result
  (writable + no received_uuid) and delete it (`subvolume delete --commit-each`).
- **Delete-safety anchors** (in `SafetyPolicy`, applied before any delete):
  always preserve the just-created snapshot/backup and the latest common
  snapshot/backup pair; **skip snapshot deletion entirely if any target was
  unreachable/aborted**; never delete a subvolume another preserved backup needs
  as a parent.
- **Restore never flips read-only via `btrfs property set`** (it poisons
  `received_uuid` and silently breaks all future incrementals); the only path to
  a writable subvolume is `btrfs subvolume snapshot` without `-r`.
- **Stateless & idempotent:** re-derive truth from the filesystem each run;
  timestamped names with `_N` collision counter; leave non-matching names
  untouched.

## Intentional divergences from btrbk

Flagged in `03-review-and-corrections.md` (these are additions/improvements, not
oversights): mybtrfs is **CLI-first with interactive drive auto-detection**
(btrbk is config-file-driven with explicit paths); mybtrfs **automates restore**
(btrbk leaves it a manual README procedure); mybtrfs **hard-refuses on duplicate
UUIDs** (btrbk only warns).

## Planned tooling (once `src/` exists)

Per `01-phases-design.md` (§Verification) and `02-architecture.md`: a Rust binary
crate; `cargo build` / `cargo test`. Pure logic (naming/timestamp parsing,
retention scheduler, parent ranking against synthetic UUID graphs) is unit-tested
with no I/O via the fake adapters. End-to-end behavior is exercised against
**loopback btrfs images** (sparse file + `mkfs.btrfs` + loop-mount) — these
integration tests require root/sudo and should be gated behind a feature/env
flag so plain `cargo test` stays runnable by non-root.
