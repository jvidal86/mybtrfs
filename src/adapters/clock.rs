//! `SystemClock` — implements `ClockPort` (current time + timezone). Injected so
//! naming and the retention scheduler are deterministic; tests substitute a
//! `FixedClock`. See `documentation/02-architecture-v2.md` §6 (determinism).
//
// TODO (Phase 1).
