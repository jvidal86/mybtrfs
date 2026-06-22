//! `RestoreService` — powers `restore` (a mybtrfs addition that automates
//! btrbk's documented manual procedure). Transfers the backup back if needed,
//! then makes a **writable** snapshot (never `property set ro=false`), and
//! verifies the result's `received_uuid` is empty so future incrementals stay
//! intact. See `documentation/01-phases-design-v2.md` Phase 4.
//
// TODO (Phase 4).
