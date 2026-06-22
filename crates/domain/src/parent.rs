//! `ParentResolver` — choose the incremental parent (+ clone sources).
//!
//! Pure: by UUID correlation, related-walk over `parent_uuid`, and ranked
//! strategy selection (parent_uuid-relation **and** timestamp-sibling candidates,
//! so a pruned chain doesn't force a full resend). Reachability ("same mountpoint
//! as the source") is computed from each subvolume's injected
//! filesystem-UUID/mountpoint. Parallels btrbk `get_best_parent` /
//! `_is_correlated`.
//
// TODO (Phase 2).
