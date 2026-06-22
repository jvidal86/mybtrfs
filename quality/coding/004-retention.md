# Quality Report 004 — crates/domain/src/retention.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo test -p mybtrfs-domain && cargo clippy -p mybtrfs-domain --all-targets`.

---

## Issue 004-1
**File:** `crates/domain/src/retention.rs`
**Lines:** 57–67 (the fields of `RetentionPolicy`)
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
pub struct RetentionPolicy {
    pub preserve_min: PreserveMin,
    pub hourly: Option<TierCount>,
    pub daily: Option<TierCount>,
    pub weekly: Option<TierCount>,
    pub monthly: Option<TierCount>,
    pub yearly: Option<TierCount>,
    // hour_of_day and day_of_week ARE documented — those are fine
```

**What is wrong:**
The fields `preserve_min`, `hourly`, `daily`, `weekly`, `monthly`, and
`yearly` are public but carry no `///` doc comments. The `hour_of_day` and
`day_of_week` fields below them are documented; these six are not. Add a
`///` comment on each explaining the semantics (especially why they are
`Option` — `None` means "not scheduled").

---

## Issue 004-2
**File:** `crates/domain/src/retention.rs`
**Lines:** 88–94 (the fields of `DatedEntry<T>`)
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
pub struct DatedEntry<T> {
    pub instant: i64,
    pub local: NaiveDateTime,
    pub has_exact_time: bool,
    pub nn: u32,
    pub payload: T,
}
```

**What is wrong:**
All five fields are public but undocumented. The struct-level doc explains
`instant` and `local` at a high level, but the individual fields still need
`///` comments. In particular `nn` (collision counter) and `has_exact_time`
are non-obvious from their names alone. Add a `///` above each field.

---

## Issue 004-3
**File:** `crates/domain/src/retention.rs`
**Lines:** 98–101 (the fields of `Schedule<T>`)
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
pub struct Schedule<T> {
    pub preserve: Vec<T>,
    pub delete: Vec<T>,
}
```

**What is wrong:**
`Schedule<T>` has a struct-level doc comment but neither field has a `///`
comment. The distinction between "entries to preserve" and "entries to
delete" and what the caller should do with each vec should be stated
explicitly. Add a `///` above each field, e.g.:
```rust
/// Entries that must be kept (sorted oldest-first).
pub preserve: Vec<T>,
/// Entries that are safe to delete (sorted oldest-first).
pub delete: Vec<T>,
```
