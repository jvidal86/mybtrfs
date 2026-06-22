//! Snapshot/backup naming: `<basename>.<timestamp>[_N]`.
//!
//! Pure parse + format for the `short` / `long` / `long-iso` timestamp formats;
//! `short`/`long` are local-time, `long-iso` is absolute. Parallels btrbk's name
//! regex. Names not matching the scheme are left untouched by mybtrfs.
//
// TODO (Phase 1): timestamp format/parse + collision-counter handling.
