# Changelog

All notable changes to mybtrfs are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **`list-subvolumes` command:** lists every btrfs subvolume on the local system, across all mounted btrfs filesystems, for picking a backup source. Read-only; tab-separated output (`id  path  fs-mountpoint  uuid  ro|rw`). Complements `list-drives` (which lists the filesystems themselves). Implemented as a `LocalSubvolumesService` use case composing the existing `DriveDiscoveryPort` (lsblk) and `SubvolumeRepository` (`btrfs subvolume list`) ports — no new port. Requires root (runs `btrfs subvolume list`), so it exits with code 4 without it, like `list`.

### Planned for v1.x ("make it visible")

- Status view backed by journal (visibility into last-run health)
- Snapshot diff (show what changed between two snapshots)
- Retention preview polish (enhanced dry-run output)
- Backup-set file support (multi-subvolume cron sugar, if needed)

See `documentation/09-roadmap.md` §6 for detailed prioritization and validatability gates.

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

[Unreleased]: https://github.com/jvidal86/mybtrfs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jvidal86/mybtrfs/releases/tag/v0.2.0
