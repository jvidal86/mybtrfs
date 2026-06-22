//! mybtrfs — a backup tool for btrfs subvolumes (a Rust reimagining of btrbk).
//!
//! **Architecture: hexagonal (ports & adapters).** Dependencies point inward:
//! `adapters` → `ports` ← `app` → `domain`. The `domain` is pure (no I/O,
//! deterministic given an injected clock + timezone). See
//! `documentation/02-architecture-v2.md`.
//!
//! **This is a SCAFFOLD.** The modules below are laid out per the architecture
//! but not yet implemented. Behavior is specified in
//! `documentation/01-phases-design-v2.md`; the executable spec is
//! `documentation/05-e2e-test-spec.md`. Per SDD/TDD, write the test first.

pub mod error;

/// Pure domain core (no I/O): model, naming, parent resolution, retention, safety.
pub mod domain;

/// Driven port traits — the abstractions the application depends on.
pub mod ports;

/// Application use cases (orchestrators) — depend only on `ports` + `domain`.
pub mod app;

/// Driven adapters — concrete implementations of the ports.
pub mod adapters;

/// Driving adapter: the command-line interface and composition root.
pub mod cli;
