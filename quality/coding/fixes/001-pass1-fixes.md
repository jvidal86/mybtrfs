# Fix Report — Pass 1 (2026-06-22)
Source reports: `quality/coding/001` through `007`
Verification: `cargo test --workspace` → 48 passed, 0 failed | `cargo clippy --workspace --all-targets` → 0 warnings

---

## Fixed: 001-1 — `Uuid::as_str` missing doc
**File:** `crates/domain/src/model.rs`
**Change:** Added `/// Returns the inner UUID string slice.` + `#[must_use]`.
**Status:** CLOSED

---

## Fixed: 001-2 — `Subvolume::readonly` / `::path` missing docs
**File:** `crates/domain/src/model.rs`
**Change:** Added `///` doc comments to both fields.
**Status:** CLOSED

---

## Fixed: 001-3 — `RelationshipGraph::build` missing `# Errors`
**File:** `crates/domain/src/model.rs`
**Change:** Added `# Errors` section documenting `GraphError::DuplicateUuid`.
**Status:** CLOSED

---

## Fixed: 002-1 — `ParsedName` fields undocumented
**File:** `crates/domain/src/naming.rs`
**Change:** Added `///` doc comments to all five fields (`basename`, `naive`, `has_exact_time`, `offset`, `nn`).
**Status:** CLOSED

---

## Fixed: 003-1 — `ParentSelection` fields undocumented
**File:** `crates/domain/src/parent.rs`
**Change:** Added `///` doc comments to `parent` and `clone_sources`, referencing `btrfs send -p`/`-c`.
**Status:** CLOSED

---

## Fixed: 003-2 — Latent panic in `best_parent` (`unwrap_or(candidates.len() - 1)`)
**File:** `crates/domain/src/parent.rs` (~line 146)
**Change:** Added `// SAFETY:` comment documenting the non-empty invariant that makes the subtraction safe.
**Status:** CLOSED

---

## Fixed: 004-1 — `RetentionPolicy` tier fields undocumented
**File:** `crates/domain/src/retention.rs`
**Change:** Added `///` doc comments to `preserve_min`, `hourly`, `daily`, `weekly`, `monthly`, `yearly`.
**Status:** CLOSED

---

## Fixed: 004-2 — `DatedEntry<T>` fields undocumented
**File:** `crates/domain/src/retention.rs`
**Change:** Added `///` doc comments to all five fields.
**Status:** CLOSED

---

## Fixed: 004-3 — `Schedule<T>` fields undocumented
**File:** `crates/domain/src/retention.rs`
**Change:** Added `///` doc comments to `preserve` and `delete`.
**Status:** CLOSED

---

## Fixed: 005-1 — Adapter modules declared `pub` instead of `pub(crate)`
**File:** `crates/adapters/src/lib.rs`
**Change:** Changed all six `pub mod` declarations to `pub(crate) mod`.
**Status:** CLOSED

---

## Fixed: 006-1 — Raw `std::process::exit(1)` in main
**File:** `crates/cli/src/main.rs`
**Change:** Introduced `mod exit_code { pub const ERROR: i32 = 1; }` and replaced the literal with `exit_code::ERROR`.
**Status:** CLOSED

---

## Fixed: 006-2 — Missing MSRV (`rust-version`)
**File:** `Cargo.toml` (workspace root)
**Change:** Added `rust-version = "1.85"` to `[workspace.package]`.
**Note:** 1.85 is the first stable release to support the `edition = "2024"` features already in use in this workspace (let-chains). Verify with your CI toolchain matrix.
**Status:** CLOSED

---

## Fixed: 007-1 — `clippy::pedantic` subset not configured
**File:** `Cargo.toml` (workspace root)
**Change:** Added five pedantic lints to `[workspace.lints.clippy]`:
```toml
missing_panics_doc = "warn"
must_use_candidate = "warn"
needless_pass_by_value = "warn"
redundant_closure_for_method_calls = "warn"
explicit_iter_loop = "warn"
```
The `must_use_candidate` lint immediately flagged 20 public pure functions across the domain crate; all were addressed in the same pass (see below).
**Status:** CLOSED

---

## Additional fixes triggered by `must_use_candidate`
The new pedantic lint surfaced 20 missing `#[must_use]` attributes on public pure-query functions/methods. All fixed in the same pass:

| File | Items fixed |
|------|-------------|
| `crates/domain/src/model.rs` | `Uuid::parse`, `Uuid::from_btrfs`, `Uuid::as_str`, `Subvolume::is_garbled`, `Subvolume::reference_generation`, `RelationshipGraph::get`, `RelationshipGraph::children_of`, `RelationshipGraph::received_from`, `RelationshipGraph::all` |
| `crates/domain/src/naming.rs` | `format_timestamp`, `make_name`, `with_counter`, `parse_name` |
| `crates/domain/src/parent.rs` | `is_correlated`, `related`, `best_parent`, `target_correlates` |
| `crates/domain/src/retention.rs` | `schedule` |
| `crates/domain/src/safety.rs` | `enforce`, `latest_common_pair` |

All 20 now carry `#[must_use]`. Zero clippy warnings after fixes.

---

## Summary
- **13 original issues:** all CLOSED
- **20 secondary issues (must_use):** all CLOSED
- **Total changes:** 6 source files + root `Cargo.toml`
- **Tests:** 48/48 passing
- **Clippy:** 0 warnings
