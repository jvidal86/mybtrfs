# Coding Quality Reports — Index
Last updated: 2026-06-22 | Pass: 1

Each report is self-contained and formatted for an agent to act on directly.
Reports are never modified in place — new passes append new files.

| Report | File / Scope | Issues | Status |
|--------|-------------|--------|--------|
| [001-model.md](001-model.md) | `crates/domain/src/model.rs` | 3 | OPEN |
| [002-naming.md](002-naming.md) | `crates/domain/src/naming.rs` | 1 | OPEN |
| [003-parent.md](003-parent.md) | `crates/domain/src/parent.rs` | 2 | OPEN |
| [004-retention.md](004-retention.md) | `crates/domain/src/retention.rs` | 3 | OPEN |
| [005-adapters-lib.md](005-adapters-lib.md) | `crates/adapters/src/lib.rs` | 1 | OPEN |
| [006-cli-main.md](006-cli-main.md) | `crates/cli/src/main.rs` + root `Cargo.toml` | 2 | OPEN |
| [007-workspace-clippy-pedantic.md](007-workspace-clippy-pedantic.md) | Workspace `Cargo.toml` | 1 | OPEN |

**Total open issues: 13**

## Issue severity summary

| Severity | Count | Issues |
|----------|-------|--------|
| Correctness risk | 1 | 003-2 (latent panic in `best_parent`) |
| Design / guidelines | 4 | 005-1 (visibility), 006-1 (exit code), 006-2 (MSRV), 007-1 (clippy::pedantic) |
| Documentation gaps | 8 | 001-1, 001-2, 001-3, 002-1, 003-1, 004-1, 004-2, 004-3 |

## Files with zero issues (pass 1)
- `crates/domain/src/lib.rs`
- `crates/domain/src/safety.rs`
- `crates/application/src/lib.rs`
- `crates/application/src/ports.rs`
- `crates/application/src/backup.rs`
- `crates/application/src/inventory.rs`
- `crates/application/src/restore.rs`
- `crates/application/src/retention.rs`
- `crates/adapters/src/btrfs_cli.rs`
- `crates/adapters/src/clock.rs`
- `crates/adapters/src/drive_discovery.rs`
- `crates/adapters/src/journal.rs`
- `crates/adapters/src/local_fs.rs`
- `crates/adapters/src/prompter.rs`
- `crates/cli/src/cli.rs`
- `crates/cli/tests/e2e.rs`
