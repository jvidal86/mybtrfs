//! End-to-end test suite (loopback btrfs; root-gated).
//!
//! Scenarios are specified in `documentation/05-e2e-test-spec.md`. They will be
//! implemented here (TDD) behind a feature/env gate so plain `cargo test` stays
//! runnable by non-root. The fast, always-on layer is the pure-logic unit tests
//! living next to the domain modules.
//!
//! SCAFFOLD ONLY — no tests yet. Write the scenario test first (red), then make
//! it pass (green), then refactor.
