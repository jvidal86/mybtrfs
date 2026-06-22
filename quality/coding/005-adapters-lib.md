# Quality Report 005 — crates/adapters/src/lib.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo build --workspace && cargo clippy --workspace --all-targets`.

---

## Issue 005-1
**File:** `crates/adapters/src/lib.rs`
**Lines:** 6–11
**Guideline violated:** §4 API & module design — `pub(crate)` by default; only the genuinely-public API is `pub`.

**Offending code:**
```rust
pub mod btrfs_cli;
pub mod clock;
pub mod drive_discovery;
pub mod journal;
pub mod local_fs;
pub mod prompter;
```

**What is wrong:**
All six adapter submodules are declared `pub`. The `mybtrfs-adapters` crate is
consumed exclusively by the `mybtrfs` (cli) composition-root crate; no other
crate in the workspace depends on it, and the workspace is not a library
intended for external consumers. Per §4, `pub(crate)` is the correct
visibility; `pub` leaks the adapter API to any future crate that takes a
dependency on `mybtrfs-adapters`.

Change each `pub mod` to `pub(crate) mod`:
```rust
pub(crate) mod btrfs_cli;
pub(crate) mod clock;
pub(crate) mod drive_discovery;
pub(crate) mod journal;
pub(crate) mod local_fs;
pub(crate) mod prompter;
```

Note: if any re-exports in `lib.rs` use `pub use`, update those to
`pub(crate) use` as well.
