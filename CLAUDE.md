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

A **Cargo workspace** built Spec-Driven / Test-Driven. **All four delivery phases
are implemented end to end** — the pure `domain` core, the `application` use cases
and ports, the concrete `adapters`, and the `cli` composition root — each via
red→green TDD.

- `domain`: `naming`, `model`, `retention` (scheduler), `parent` (resolution),
  `safety` (`SafetyPolicy`, applied before any delete).
- `application`: `backup` (run/resume), `prune`, `restore` (incl. transfer-back),
  `inventory` (list/stats), `retention`, and the `ports`.
- `adapters`: `btrfs_cli` (subvolume/snapshot/transfer/delete + mount-table
  resolution), `ssh` (remote `ssh://` endpoints — runner + mount table), `local_fs`,
  `drive_discovery`, `clock`, `prompter`, `journal`, `lock`.
- `cli`: the full command set + global flags (`--yes`/`--journal`/`--lock`),
  exit-code taxonomy, and the run lock.
- **~196 unit/integration tests**, all passing; `clippy` (pedantic subset) and
  `fmt` clean; MSRV 1.89.

**What remains:** the loopback-btrfs e2e suite and the differential-oracle harness
(`crates/cli/tests/{e2e,diff_btrbk_schedule}.rs`) are written but **`#[ignore]`d** —
they need root/loopback (and `faketime` + a real btrbk for the oracle), so they are
validated on a real host/CI, not in the sandbox. **Phase 5 §2 (remote/ssh) is
implemented** — backup, incremental, prune, and restore all work over `ssh://`
endpoints, validated end-to-end against a real host
(`contrib/test/mybtrfs-ssh-smoke.sh`). **Scheduling is shipped** in `contrib/`. The
remaining Phase 5 items (raw/encrypted targets, an optional backup-set file) are
design-only (`08`/`09`), needing real infra (GPG) to validate.

**Manual testing:** a persistent local btrfs environment is available via
`contrib/setup-local-backup-env.sh` — creates two loopback filesystems (source +
backup) with real btrfs subvolumes, survives reboots, and can be torn down cleanly.
Use this to test backup/restore workflows interactively without requiring CI or a
real external drive. See `contrib/README.md` for usage.

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
  surface (`run`/`snapshot`/`resume`/`prune`/`restore`/`list`/`stats`/`status`/`diff`/`list-drives`/`list-subvolumes`,
  keep-all-by-default retention, auto-create dirs).
- **`02-architecture-v2.md`** — hexagonal architecture, sequence diagrams, and the
  numbered fail-safe invariants (§6).
- **`04-coding-guidelines.md`** — Rust + clean-code rules to follow.
- **`05-e2e-test-spec.md`** — the end-to-end behavioral spec (black-box, SDD/TDD),
  with a traceability matrix back to the §6 invariants.
- **`06-differential-oracle-test-spec.md`** — differential ("back-to-back")
  conformance test that runs btrbk (the reference oracle) and mybtrfs over the same
  loopback fixture and compares resulting btrfs state (harness in
  `crates/cli/tests/diff_btrbk_schedule.rs`, gated).
- **`07-implementation-decisions.md`** — the ADR-style decision log (ID-1…ID-7).
- **`08-phase5-design.md`** — Phase 5+ design (scheduling, SSH, raw/encrypted,
  backup-set file); each slots behind the existing ports. Scheduling is shipped in
  `contrib/`; SSH §2 is implemented and validated end-to-end; the rest is
  design-only (needs real infra to validate).
- **`09-roadmap.md`** — post–Phase-5 roadmap & competitive positioning: maps an
  external btrbk-replacement roadmap onto what mybtrfs already ships, settles the
  config-vs-CLI-first conflict (backup-set sugar, not a DSL), and recommends the
  next-version slice (status view, snapshot diff, retention-preview polish) with
  each item tied to its port and sandbox-validatability.
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

The four phases are implemented. **Phase 5 §2 (remote/ssh) is also implemented and
validated end-to-end** — backup/incremental/prune/restore over `ssh://` endpoints
(the per-uid run lock it exposed is decision ID-7). The remaining Phase 5 items
(raw/encrypted targets, an optional backup-set file) are designed in
`08-phase5-design.md` / `09-roadmap.md` but unbuilt — they need real infrastructure
(GPG) to validate, so they are not done in-sandbox. **Scheduling is shipped** in
`contrib/` (systemd timer + cron drop-ins that invoke the CLI; no daemon, no config).

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

## Logging

Diagnostic logging uses the `log` facade in `application` and `adapters` only
(`domain` stays log-free to preserve purity). The CLI composition root initializes
dual-target logging: **errors/warnings go to stderr (with color), info/debug go to
a log file**. This matches standard backup-tool UX (btrbk, rsync, borg): critical
issues are always visible, even with `--quiet`, while progress/info output can be
suppressed for cron/scripting.

**Output routing:**
- `error` (red) & `warn` (yellow) → stderr (always, even with `--quiet`)
- `info` & `debug` → file (suppressed with `--quiet`)
- Default file: `/var/log/mybtrfs.log` or `~/.local/share/mybtrfs/logs/mybtrfs.log`
- Override with `--log-file <PATH>`; use `/dev/null` to discard

**Level convention:**
- `error` — invariant violated, operation cannot continue
- `warn` — garbled receive detected, path skipped, safety anchor triggered, cleanup failed
- `info` — each major step (snapshot, transfer, prune, delete, restore)
- `debug` — btrfs commands spawned, name collision resolution, decisions within a step
- `trace` — per-item iteration, path filtering

Every adapter method spawning a btrfs command must emit `log::debug!` before the
spawn. Every `PortError::Verification` / `PortError::Command` returned must have a
`log::error!` or `log::warn!` at the detection site. Capture the full trace with
`RUST_LOG=debug mybtrfs …` (stderr for warnings/errors, file for info/debug).

## Intentional divergences from btrbk

Additions/improvements, not oversights: **CLI-first with interactive drive
auto-detection** (btrbk is config-file-driven); **automated restore** (btrbk
leaves it manual, and mybtrfs additionally *transfers a remote backup back* before
making it writable — decision ID-5); **hard-refuse on duplicate UUIDs** (btrbk only
warns); a dedicated **exit code 4 for "needs root"** (`PermissionDenied`, decision
ID-6) so cron/scripts can tell a privilege problem from a generic failure (btrbk
has no such code).

Beyond the decided CLI surface, three **global flags** were added: `--yes`
(non-interactive confirm, for cron), `--journal <PATH>` (append a timestamped audit
line per invocation — wires the `Journal` port), and `--lock <PATH>` (the run lock,
decision ID-4).

## Working style

Strict TDD per increment: write the failing test first (red), implement to green,
then refactor; keep `clippy`/`fmt` clean. End-to-end behavior is exercised against
**loopback btrfs images** (root/sudo, gated behind a feature/env flag) per
`05-e2e-test-spec.md`; the pure-logic unit tests are the fast always-on layer.
