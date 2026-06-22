//! `SafetyPolicy` — the non-negotiable delete/restore safety rules.
//!
//! Applied as a **monotonic** step over scheduler output (only ever moves items
//! delete → preserve, never the reverse):
//! - keep the just-created snapshot/backup;
//! - keep the latest common snapshot/backup pair (run **and** prune);
//! - keep any parent a preserved backup still needs;
//! - skip **all** snapshot deletion if a target was unreachable/aborted.
//!
//! See `documentation/02-architecture-v2.md` §6 (the fail-safe invariants).
//
// TODO (Phase 3).
