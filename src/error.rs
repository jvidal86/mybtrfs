//! Error taxonomy.
//!
//! Domain/library code uses typed errors (to be defined with `thiserror`); the
//! CLI boundary attaches context with `anyhow`. Errors map to a central
//! exit-code table — proposed to mirror btrbk (`0`/`1`/`2`/`3`/`10`/`255`),
//! pending final confirmation. See `documentation/04-coding-guidelines.md` §1.
//
// TODO (Phase 1): define `Error` enum + `Result<T>` alias and the exit-code map.
