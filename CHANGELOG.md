# Changelog

All notable changes to mybtrfs are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Planned for v1.x+

- Snapshot diff parsing tuning for all btrfs versions
- Backup-set file support (multi-subvolume cron sugar, if needed)
- Raw/encrypted targets (design-only, needs GPG infrastructure)

See `documentation/09-roadmap.md` §6 for detailed prioritization and validatability gates.

## [1.1] — 2026-06-24

### Observability Complete — Backup Health Tracking & Progress Feedback

#### Phase 1: Retention Preview
- `mybtrfs prune --dry-run` shows what snapshots/backups would be deleted
- Tab-separated output (action, name, age) — scriptable via grep/awk
- GFS retention policies (hourly/daily/weekly/monthly/yearly)
- No mutations with `--dry-run` flag

#### Phase 2: Status View with Journal-Backed Health
- `mybtrfs status <snapshots> <backups>` shows backup health (counts, sync status)
- Journal-backed operational history: `last_run` and `last_command` tracked
- Journal defaults to `/var/log/mybtrfs.journal` (user-readable, persists)
- Parent directory auto-creation for journal file

#### Progress Indicators
- Spinners for scanning/metadata operations (all commands: run, prune, restore, list, inventory)
- Progress bars with percentage for deletion loops
- Transfer speed and byte reporting for send/receive
- Clears before final output (no UI noise)

#### CLI Output Standardized
- Tab-separated format across all commands (scriptable with grep/awk)
- Removed emojis and decorative elements
- Consistent format: `action\tname\tdetail`

#### Phase 3: Snapshot Diff (Early Access)
- `mybtrfs diff <older_snap> <newer_snap>` estimates byte changes
- Uses `btrfs subvolume find-new` for actual changed bytes
- Useful for predicting incremental backup size
- Output: `older_path	older_size	newer_path	newer_size	changed_size`

#### Validation & Quality
- ✓ 221 unit tests passing
- ✓ 4 E2E tests passing (full backup, incremental, prune safety, restore)
- ✓ All safety invariants verified (UUID tracking, send/receive verification, delete-safety anchors)

#### Bug Fixes
- Fixed journal directory auto-creation (FileJournal now `mkdir -p` parent dirs)
- Fixed journal default path to `/var/log/mybtrfs.journal` (root-accessible when run under sudo)
- Optimized E2E tests: reduced loopback image size (400M → 150M)
- Added auto-sudo for privileged E2E commands (losetup, mkfs.btrfs, mount, btrfs)

## [0.2.0] — 2026-06-24

Feature-complete btrfs backup tool. All delivery phases (1–4) and Phase 5 §2 (remote/SSH) implemented and validated end-to-end.

### Added

#### Core features (Phases 1–4)
- **Phase 1:** Drive auto-detection, full backup to remote, read-only snapshot, `send | receive`, verification of received subvolume
- **Phase 2:** Incremental backups via UUID relationship graph, parent/clone-source resolution, `send -p`
- **Phase 3:** List/stats inventory, GFS retention scheduler (keep N hourly/daily/weekly/monthly/yearly), safe prune with delete-safety anchors
- **Phase 4:** Safe restore with transfer-back from remote, writable snapshot creation, guarding against the received-uuid trap

#### Phase 5 §2 (Remote/SSH)
- Backup, incremental backup, prune, and restore over `ssh://` endpoints
- Per-UID run lock to prevent concurrent execution (`--lock` global flag)
- Mount-table resolution over SSH for automatic target discovery
- End-to-end validation against real hosts

#### CLI & safety
- Command set: `run`, `snapshot`, `resume`, `prune`, `restore`, `list`, `stats`, `list-drives`
- Global flags: `--yes` (non-interactive), `--journal <PATH>` (audit log), `--lock <PATH>` (run lock)
- Exit code taxonomy with dedicated code 4 for "needs root" (decision ID-6)
- Comprehensive logging (`RUST_LOG` support with `info`/`debug`/`trace` levels)

#### Delivery & robustness
- Hexagonal architecture with compiler-enforced dependency rules (`cli → adapters → application → domain`)
- Pure domain logic (parent resolution, retention, safety) with zero I/O, unit-tested in isolation
- ~196 unit/integration tests, all passing
- `clippy` (pedantic subset) and `fmt` clean
- MSRV 1.89 (File::try_lock stability)
- Scheduling contrib (systemd timer + cron drop-ins, no daemon, no config)

### Safety invariants

All core invariants preserved from btrbk:
- Send/receive verification: readonly check, received_uuid validation, parent_uuid correctness
- Delete-safety anchors: preserve just-created and latest common snapshots; skip deletion on any target unreachable
- Restore read-only trap: never use `btrfs property set` (poisons received_uuid); only path to writable is snapshot without `-r`
- Stateless & idempotent: re-derive all truth from filesystem each run; timestamped names with `_N` collision counter

### Known limitations

- Loopback e2e (`crates/cli/tests/e2e.rs`) and differential-oracle (`diff_btrbk_schedule.rs`) test suites are `#[ignore]`d — they require root and loopback fixtures, validated on real hosts / CI only
- Phase 5 remaining items (raw/encrypted targets, backup-set DSL) are designed but not implemented — they need real infrastructure (GPG) to validate
- No TUI / snapshot browser (scope decision, kept separate; see `09-roadmap.md` §4)

### Documentation

- `documentation/01-phases-design-v2.md` — functional design (Phases 1–4) and CLI surface
- `documentation/02-architecture-v2.md` — hexagonal architecture, sequence diagrams, fail-safe invariants
- `documentation/04-coding-guidelines.md` — Rust + clean-code rules
- `documentation/05-e2e-test-spec.md` — black-box behavioral spec and traceability matrix
- `documentation/06-differential-oracle-test-spec.md` — btrbk conformance testing harness
- `documentation/07-implementation-decisions.md` — ADR-style decision log (ID-1…ID-7)
- `documentation/08-phase5-design.md` — Phase 5+ design (scheduling, SSH, raw/encrypted)
- `documentation/09-roadmap.md` — post–Phase-5 roadmap and competitive positioning

### Technical details

- Rust 2024 edition, single static binary (no Perl/Python/Ruby runtime)
- Workspace layout: `domain` (pure core) → `application` (use cases + ports) → `adapters` (I/O) → `cli` (composition root)
- Dependencies: `anyhow`, `chrono`, `clap`, `env_logger`, `log`, `regex`, `serde`, `serde_json`, `thiserror`
- Repository: https://github.com/jvidal86/mybtrfs
- License: GPL-3.0-or-later

[Unreleased]: https://github.com/jvidal86/mybtrfs/compare/v1.1...HEAD
[1.1]: https://github.com/jvidal86/mybtrfs/compare/v0.2.0...v1.1
[0.2.0]: https://github.com/jvidal86/mybtrfs/releases/tag/v0.2.0
