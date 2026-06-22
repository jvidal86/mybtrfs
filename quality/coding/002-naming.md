# Quality Report 002 — crates/domain/src/naming.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo test -p mybtrfs-domain && cargo clippy -p mybtrfs-domain --all-targets`.

---

## Issue 002-1
**File:** `crates/domain/src/naming.rs`
**Lines:** 33–39
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedName {
    pub basename: String,
    pub naive: NaiveDateTime,
    pub has_exact_time: bool,
    pub offset: Option<FixedOffset>,
    pub nn: u32,
}
```

**What is wrong:**
All five fields of `ParsedName` are public but none carry a `///` doc comment.
The struct-level comment explains the type at a high level; individual fields
still need inline documentation. Readers cannot determine from the field name
alone what `nn` is (collision counter), why `offset` is `Option` (absent on
short timestamps), or when `has_exact_time` differs from `offset.is_some()`.

Add a `///` comment above each field, e.g.:
```rust
/// Original subvolume base name (the part before the timestamp suffix).
pub basename: String,
/// Parsed local date-time without timezone offset (always present).
pub naive: NaiveDateTime,
/// `true` when the timestamp includes HH:MM:SS (long format).
pub has_exact_time: bool,
/// Timezone offset, present only for long-format timestamps.
pub offset: Option<FixedOffset>,
/// Collision counter (`_N` suffix); 0 means no suffix was present.
pub nn: u32,
```
