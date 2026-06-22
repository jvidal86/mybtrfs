# Quality Report 006 — crates/cli/src/main.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo build --workspace && cargo clippy --workspace --all-targets`.

---

## Issue 006-1
**File:** `crates/cli/src/main.rs`
**Lines:** ~12
**Guideline violated:** §1 Error handling — all exit codes must flow through a single central mapping table; no ad-hoc `std::process::exit(N)` with a raw numeric literal.

**Offending code:**
```rust
    std::process::exit(1);
```

**What is wrong:**
The scaffold `main` calls `std::process::exit(1)` directly with a raw literal.
The coding guidelines require all exit codes to be defined in one central place
(e.g. a `ExitCode` enum or module) that maps error categories to exit codes
(success / generic / usage / partial-abort). Even at scaffold stage, using a
raw `1` establishes a pattern that must be refactored later.

Introduce a minimal exit-code constant or enum and use it here, even if the
enum only has two variants right now:
```rust
mod exit_code {
    pub const SUCCESS: i32 = 0;
    pub const ERROR: i32 = 1;
}
// ...
std::process::exit(exit_code::ERROR);
```

---

## Issue 006-2
**File:** workspace `Cargo.toml` (root)
**Lines:** `[workspace.package]` section
**Guideline violated:** §9 Tooling — a declared MSRV (`rust-version`) is required.

**Offending code (absence):**
The `[workspace.package]` table does not contain a `rust-version` key.

**What is wrong:**
Guideline §9 requires pinning the Rust edition and a declared MSRV so that CI
and contributors know the minimum supported toolchain. Without it, the project
silently starts depending on whatever the developer's local toolchain provides.

Add to the root `Cargo.toml`:
```toml
[workspace.package]
rust-version = "1.XX"   # set to the oldest toolchain you intend to support
```

A reasonable starting point is the stable release that introduced the newest
language features already in use (check with `cargo +<version> check`).
