//! Domain model.
//!
//! To define (prefer newtypes over primitives — Uuid, paths, timestamps):
//! - `Subvolume` — id, three UUIDs (`uuid`/`parent_uuid`/`received_uuid`),
//!   `gen`/`cgen`, `readonly`, `path`, **owning filesystem UUID**, **mountpoint**.
//! - `RelationshipGraph` — the three UUID indexes, built **per filesystem**
//!   (`uuid` one-to-one; `parent_uuid`/`received_uuid` one-to-many).
//! - `RetentionPolicy`, `Schedule { preserve, delete }`,
//!   `ParentSelection { parent, clone_sources }` (source-side, target-correlated).
//!
//! See `documentation/02-architecture-v2.md` §3 and `01-phases-design-v2.md`.
//
// TODO (Phase 1–2): define the model types.
