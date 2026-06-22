//! `RetentionService` — powers `prune`. Runs the scheduler **separately** for
//! snapshots (snapshot policy) and backups (target policy), then applies the
//! `SafetyPolicy` anchors before any deletion. Keep-all by default: deletes
//! nothing unless a policy is supplied. See `documentation/01` Phase 3.
//
// TODO (Phase 3).
