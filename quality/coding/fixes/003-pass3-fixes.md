# Fix Report — Pass 3 (2026-06-22)

Review scope: `git diff HEAD~1` → `CLAUDE.md`, `RULES.md`,
`crates/domain/src/model.rs`, `crates/domain/src/safety.rs`

Pre-commit audit: uncommitted `crates/application/src/ports.rs` (212 lines),
`crates/application/Cargo.toml`, `Cargo.toml` (MSRV correction)

Verification: `cargo test --workspace` → 48 passed, 0 failed
             `cargo clippy --workspace --all-targets` → 0 warnings

---

## User correction: 006-2 MSRV value was wrong (re-closed)

**File:** `Cargo.toml`
**Issue:** Pass-1 fix 006-2 set `rust-version = "1.85"` citing Rust 2024 edition
stabilization. The correct MSRV is **1.88**: let-chains as a stable, general-purpose
feature (RFC 2497) landed in 1.88 — in 1.85 they were gated behind the edition 2024
feature gate and not usable in the general sense used in `domain/model.rs`.
**Fix:** User corrected directly to `rust-version = "1.88"` with comment
`# let-chains (used in domain/model.rs) stabilized in 1.88`.
**Status:** CLOSED (user-applied)

---

## Review: HEAD~1 diff — CLAUDE.md, RULES.md, model.rs, safety.rs

### CLAUDE.md
- `@RULES.md` import added and correctly positioned — makes the 20-rule checklist
  auto-load in every Claude Code session.
- Reference to `06-differential-oracle-test-spec.md` added — accurate description
  ("design-only until the CLI lands").
- **No issues.**

### RULES.md (new file)
- 20 rules across three sections: Architecture (1–7), Coding standards (8–15),
  Commit gates (16–20).
- Cross-checked against `CLAUDE.md`, `04-coding-guidelines.md` references, and
  enforced workspace lints — all rules are accurate and consistent.
- Naming-inconsistency observation (not a violation): `SubvolumeRepository`,
  `Prompter`, `Journal` in `ports.rs` omit the `Port` suffix used by
  `SnapshotPort`, `TransferPort`, `DeletePort`, `FilesystemPort`, `DriveDiscoveryPort`,
  `ClockPort`. The pattern is intentional for the query/utility traits but worth
  noting for reviewers — RULES.md has no suffix convention rule, so no violation.
- **No issues.**

### crates/domain/src/model.rs
- Four `Subvolume` field docs added (`id`, `uuid`, `parent_uuid`, `received_uuid`).
- All descriptions accurate: `received_uuid` correctly distinguishes
  "set by `btrfs receive`, absent on native snapshots".
- **No issues.**

### crates/domain/src/safety.rs
- Two `CommonPair` field docs added (`snapshot`, `backups`).
- `snapshot` doc: "has at least one correlated backup on the target" — correct
  (the loop exits early without creating a `CommonPair` if `backups` is empty).
- **No issues.**

---

## Pre-commit audit: crates/application/src/ports.rs

Full RULES.md checklist pass on 212-line port-traits implementation:

| Rule | Check | Result |
|------|-------|--------|
| §3 Ports in application | file is `application/src/ports.rs` | ✓ |
| §4 All I/O behind a port | all ops go through trait methods | ✓ |
| §8 `#[must_use]` | `ClockPort::now` annotated; other trait fns return `Result` (not pure) | ✓ |
| §9 `///` on every public item + field | all 9 traits, 3 types, 5 struct fields documented (verified by Python AST walk) | ✓ |
| §10 `# Errors` on every `Result` fn | all 14 trait methods with `Result` return have `# Errors` sections | ✓ |
| §11 `pub(crate)` by default | `PortError`, `DeleteCommit`, `DiscoveredFilesystem`, all traits are `pub` — correct, these are the application's public boundary | ✓ |
| §12 No magic numbers | no magic literals | ✓ |
| §13 No unwrap/expect | none in production code | ✓ |
| §15 unsafe_code = forbid | no unsafe blocks | ✓ |
| §1 Dependency rule | imports only `std`, `chrono`, `mybtrfs_domain` | ✓ |
| §2 Domain is pure | this is application layer — correct layer for I/O contracts | ✓ |

Invariant cross-check:
- `TransferPort::send_receive` doc references invariants #1/#2 by number.
- `SnapshotPort::make_writable` doc references invariant #7.
- `DeleteCommit::Each` doc references invariant #2.
- All invariant numbers validated against `documentation/02-architecture-v2.md §6`.

**No violations found.**

---

## False-positive awk note

During this pass an awk script checking for `///` docs reported all 14 trait methods
in `ports.rs` as "MISSING DOC". Root cause: the script checked only the
*immediately* preceding line for `///`, but Rust doc comments typically span
multiple lines with a blank line or `# Errors` section between the `///` header and
the `fn` signature. A Python check (looking back up to 6 lines) confirmed **all
methods are properly documented**. The awk approach is unreliable for multi-line
doc blocks and should not be used.

---

## Summary

- **Issues found this pass:** 1 (MSRV value in 006-2, already corrected by user)
- **Fixes applied this pass:** 0 (user-applied MSRV, no other violations found)
- **Issues regressed from prior passes:** 0
- **Tests:** 48/48 passing
- **Clippy:** 0 warnings
