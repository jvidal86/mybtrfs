//! Driving adapter: the command-line interface (clap).
//!
//! Parses the command set — `run`, `snapshot`, `resume`, `prune`, `restore`,
//! `list`, `stats`, `list-drives` — plus global flags (`-n/--dry-run`, `--yes`,
//! retention flags). Acts as the **composition root**: wires concrete adapters
//! into the use cases. See `documentation/01` (CLI surface) and `02` §3.
//
// TODO (Phase 1+): define the clap command tree and dispatch.
