# Quality Report 007 — Workspace: clippy::pedantic not configured
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
This is a workspace-level issue. Fix it in the root `Cargo.toml`, then verify
with `cargo clippy --workspace --all-targets`.

---

## Issue 007-1
**File:** `Cargo.toml` (workspace root)
**Lines:** `[workspace.lints.clippy]` section
**Guideline violated:** §9 Tooling — "enable a curated subset of `clippy::pedantic`"; allow specific lints locally with a justification comment, never a blanket allow.

**Offending code:**
```toml
[workspace.lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
```

**What is wrong:**
The workspace lint table enables only `unwrap_used` and `expect_used`. Guideline
§9 requires a curated subset of `clippy::pedantic` to be explicitly enabled.
Without this, an entire class of style and correctness lints (redundant clones,
needless passes, explicit iteration, etc.) are silently ignored.

Add the curated pedantic lints to the workspace `[workspace.lints.clippy]`
table. A minimal starting set aligned with the project's clean-code goals:

```toml
[workspace.lints.clippy]
# already present
unwrap_used  = "warn"
expect_used  = "warn"

# pedantic subset (add or remove as the team decides)
pedantic                  = { level = "warn", priority = -1 }
# then selectively allow noisy lints that don't apply to this project, e.g.:
# missing_errors_doc       = "allow"   # if you prefer to omit # Errors on simple fns
# module_name_repetitions  = "allow"   # common in Rust crates
```

Alternatively, list each pedantic lint individually rather than enabling the
whole group, which prevents future clippy releases from adding new warnings
unexpectedly.

**Suggested individual pedantic lints to start with:**
- `clippy::missing_panics_doc` — document `# Panics` sections
- `clippy::must_use_candidate` — flag return values that should be `#[must_use]`
- `clippy::redundant_closure_for_method_calls` — style
- `clippy::needless_pass_by_value` — performance/API
- `clippy::explicit_iter_loop` — style
- `clippy::items_after_statements` — readability
