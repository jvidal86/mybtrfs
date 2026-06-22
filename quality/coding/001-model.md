# Quality Report 001 — crates/domain/src/model.rs
Generated: 2026-06-22 | Pass: 1 | Status: OPEN

## How to use this report
Each issue is self-contained. Fix the issue at the stated lines, verify with
`cargo test -p mybtrfs-domain && cargo clippy -p mybtrfs-domain --all-targets`.

---

## Issue 001-1
**File:** `crates/domain/src/model.rs`
**Lines:** 49–51
**Guideline violated:** §6 Documentation — `///` doc comments required on every public item.

**Offending code:**
```rust
    pub fn as_str(&self) -> &str {
        &self.0
    }
```

**What is wrong:**
`Uuid::as_str` is a public method with no `///` doc comment. Add a one-line doc
that states what the method returns, e.g.:
```rust
/// Returns the inner UUID string slice.
pub fn as_str(&self) -> &str {
```

---

## Issue 001-2
**File:** `crates/domain/src/model.rs`
**Lines:** 80–81
**Guideline violated:** §6 Documentation — `///` doc comments required on every public field.

**Offending code:**
```rust
    pub readonly: bool,
    pub path: PathBuf,
```

**What is wrong:**
The `readonly` and `path` fields of `Subvolume` are public but undocumented.
All other fields on the struct (`generation`, `cgen`, `fs_uuid`, `mountpoint`,
`uuid`, `parent_uuid`, `received_uuid`) carry `///` comments; these two do not.
Add a `///` doc comment above each field explaining its meaning.

---

## Issue 001-3
**File:** `crates/domain/src/model.rs`
**Lines:** 129–130
**Guideline violated:** §6 Documentation — fallible public functions must include a `# Errors` section.

**Offending code:**
```rust
    /// Build the indexes, rejecting a duplicate `uuid`.
    pub fn build(subvols: Vec<Subvolume>) -> Result<Self, GraphError> {
```

**What is wrong:**
`RelationshipGraph::build` returns `Result<_, GraphError>` but its doc comment
has no `# Errors` section. Add one, e.g.:
```rust
/// # Errors
/// Returns [`GraphError::DuplicateUuid`] if any two subvolumes share the same UUID.
```
