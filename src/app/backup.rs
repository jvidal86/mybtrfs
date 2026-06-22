//! `BackupService` — powers `run` (snapshot → send/receive → prune),
//! `snapshot`, and `resume` (send without a new snapshot). Resolves the
//! incremental parent via the domain, transfers via `TransferPort` (which
//! verifies), and delegates deletion to `RetentionService`.
//! See `documentation/01-phases-design-v2.md` Phases 1–2.
//
// TODO (Phase 1 full backup; Phase 2 incremental).
