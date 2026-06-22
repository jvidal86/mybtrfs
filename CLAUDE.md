# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

`mybtrfs` is a ground-up **Rust reimagining of [btrbk]** — a backup tool for
btrfs subvolumes (atomic read-only snapshots + incremental `btrfs send/receive`
+ retention). It **shells out to the `btrfs` CLI** (not libbtrfsutil/ioctls) and
stays **stateless**: every run re-derives all subvolume relationships from live
btrfs metadata (UUIDs), never from a side database.

[btrbk]: https://github.com/digint/btrbk

## Binding rules (auto-imported)

The enforced coding & architecture rules live in **`RULES.md`**, imported below so
they are always in context. Read them before writing code; the full rationale is in
`documentation/04-coding-guidelines.md` and `02-architecture-v2.md`.

@RULES.md

## Current state

A **Cargo workspace** under active **Spec-Driven / Test-Driven** development. The
pure `domain` modules are implemented with tests; everything outward (ports,
application use cases, adapters, CLI) is a doc-commented **stub** awaiting its TDD
increment.

- Implemented (in `crates/domain`): `naming` (timestamp parse/format), `model`
  (`Uuid`, `Subvolume`, `RelationshipGraph`), `retention` (the scheduler).
- ~32 unit tests, all passing; `clippy`/`fmt` clean.
- Still stubs: `domain/{parent,safety}`, all of `application`, `adapters`, `cli`.

## Workspace layout — the dependency rule is compiler-enforced

Hexagonal layering as separate crates; dependencies point **inward**
(`cli → adapters → application → domain`). An inner crate cannot compile against
an outer one — e.g. `mybtrfs-domain` has no internal dependencies.

```
crates/
  domain/        # mybtrfs-domain      — pure core (model, naming, parent, retention, safety)
  application/   # mybtrfs-application  — use cases + ports;  deps: domain
  adapters/      # mybtrfs-adapters     — port impls;         deps: application, domain
  cli/           # mybtrfs (the binary) — composition root + CLI; deps: all three
```

Shared `version`/`edition`/deps/lints are centralized in the root `Cargo.toml`
(`[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`); each
crate opts in with `dep.workspace = true` and `[lints] workspace = true`.

## Common commands

- Build everything: `cargo build --workspace`
- Test everything: `cargo test --workspace`
- Test one crate: `cargo test -p mybtrfs-domain`
- Run one test: `cargo test -p mybtrfs-domain naming::tests::parses_long`
- Lint (must be clean): `cargo clippy --workspace --all-targets`
- Format: `cargo fmt` (check with `cargo fmt --check`)
- Confirm the dependency rule: `cargo tree -p mybtrfs-domain` (no `mybtrfs-*` deps)

Lints in force: `unsafe_code = "forbid"`; clippy `unwrap_used`/`expect_used` warn
(allow locally with a justification only, e.g. in `#[cfg(test)]` modules).

## Source of truth — `documentation/`

The design is the spec; read it before implementing. **The `-v2` files supersede
their originals.**

- **`01-phases-design-v2.md`** — functional design (Phases 1–4) + the decided CLI
  surface (`run`/`snapshot`/`resume`/`prune`/`restore`/`list`/`stats`/`list-drives`,
  keep-all-by-default retention, auto-create dirs).
- **`02-architecture-v2.md`** — hexagonal architecture, sequence diagrams, and the
  numbered fail-safe invariants (§6).
- **`04-coding-guidelines.md`** — Rust + clean-code rules to follow.
- **`05-e2e-test-spec.md`** — the end-to-end behavioral spec (black-box, SDD/TDD),
  with a traceability matrix back to the §6 invariants.
- **`06-differential-oracle-test-spec.md`** — differential ("back-to-back")
  conformance test that runs btrbk (the reference oracle) and mybtrfs over the same
  loopback fixture and compares resulting btrfs state (design-only until the CLI lands).
- `03-review-and-corrections.md` — the review trail (history).

## Reference implementation

The original btrbk Perl program sits at **`../btrbk/btrbk/btrbk`** (one script,
~7000 lines) with man pages/FAQ under **`../btrbk/btrbk/doc/`**. The design was
verified line-by-line against it; when implementing a mechanism, consult the
cited btrbk logic (e.g. the retention scheduler at `sub schedule` 4541–4752;
send/receive verification 1518–1597; correlation `_is_correlated` 2585–2589).
Goal: **parallel to btrbk's proven logic.**

## Architecture intent

The riskiest decisions live in the **pure domain** (`ParentResolver`,
`RetentionScheduler` — `domain/retention.rs`, `SafetyPolicy`, and the model) and
are unit-tested with zero I/O. Use cases orchestrate via **ports** only; concrete
**adapters** (`BtrfsCliAdapter`, `LocalFsAdapter`, drive discovery, clock,
prompter, journal) are wired at the CLI **composition root**. Dangerous
operations (delete, make-writable, transfer) are reachable only through narrow
ports whose contracts *embed* the safety checks — so the fail-safe properties are
architectural, not conventional. Determinism: the clock **and timezone** are
injected (`ClockPort`), since `short`/`long` timestamps are local-time.

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

Non-obvious correctness rules carried from btrbk (details + citations in the docs):

- **Never trust a `send|receive` by exit code.** After receive, verify the target
  is readonly, has a `received_uuid`, and has the correct `parent_uuid` (unset for
  a full backup, set for an incremental). Detect a garbled result (writable + no
  received_uuid) and delete it (`subvolume delete --commit-each`).
- **Delete-safety anchors** (`SafetyPolicy`, applied before any delete): always
  preserve the just-created snapshot/backup and the latest common snapshot/backup
  pair; **skip snapshot deletion entirely if any target was unreachable/aborted**;
  never delete a subvolume another preserved backup needs as a parent.
- **Restore never flips read-only via `btrfs property set`** (poisons
  `received_uuid`, silently breaking future incrementals); the only path to a
  writable subvolume is `btrfs subvolume snapshot` without `-r`.
- **Stateless & idempotent:** re-derive truth from the filesystem each run;
  timestamped names with `_N` collision counter; leave non-matching names
  untouched; reject duplicate `uuid` (cloned-disk guard).

## Intentional divergences from btrbk

Additions/improvements, not oversights: **CLI-first with interactive drive
auto-detection** (btrbk is config-file-driven); **automated restore** (btrbk
leaves it manual); **hard-refuse on duplicate UUIDs** (btrbk only warns).

## Working style

Strict TDD per increment: write the failing test first (red), implement to green,
then refactor; keep `clippy`/`fmt` clean. End-to-end behavior is exercised against
**loopback btrfs images** (root/sudo, gated behind a feature/env flag) per
`05-e2e-test-spec.md`; the pure-logic unit tests are the fast always-on layer.
