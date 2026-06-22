# Quality Report 003 — crates/domain/src/parent.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo test -p mybtrfs-domain && cargo clippy -p mybtrfs-domain --all-targets`.

---

## Issue 003-1
**File:** `crates/domain/src/parent.rs`
**Lines:** 38–41
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParentSelection {
    pub parent: Option<Subvolume>,
    pub clone_sources: Vec<Subvolume>,
}
```

**What is wrong:**
Both fields of `ParentSelection` are public but lack `///` doc comments.
The semantics of `parent` vs `clone_sources` map directly to `btrfs send`
flags (`-p` and `-c` respectively); callers need to know this to use the
struct correctly. Add doc comments, e.g.:
```rust
/// The incremental parent passed to `btrfs send -p`; `None` for a full send.
pub parent: Option<Subvolume>,
/// Additional clone sources passed to `btrfs send -c` (may be empty).
pub clone_sources: Vec<Subvolume>,
```

---

## Issue 003-2
**File:** `crates/domain/src/parent.rs`
**Lines:** ~132–148 (the `best_parent` body — locate the `unwrap_or(candidates.len() - 1)` call)
**Guideline violated:** §1 Error handling / §3 Guard clauses — non-obvious invariants that prevent a panic must be documented inline.

**Offending code (approximate):**
```rust
let parent_pos = candidates
    .iter()
    .rposition(|c| c.generation <= snap_gen)
    .unwrap_or(candidates.len() - 1);
let parent = candidates.remove(parent_pos);
```

**What is wrong:**
`unwrap_or(candidates.len() - 1)` will underflow (subtract 1 from 0) if
`candidates` is empty, causing a panic. The function's `candidates.is_empty()`
guard earlier in the body is the load-bearing precondition that makes this safe.
That guard is not adjacent to this call; if the guard is ever moved or refactored,
this becomes a silent underflow panic.

Add an inline comment at the `unwrap_or` line documenting the invariant:
```rust
// SAFETY: `candidates` is non-empty — the early-return above guarantees this.
let parent_pos = candidates
    .iter()
    .rposition(|c| c.generation <= snap_gen)
    .unwrap_or(candidates.len() - 1);
```

Alternatively, replace the pattern with one that is safe without relying on
a distant guard, e.g. `candidates.len().saturating_sub(1)`.
