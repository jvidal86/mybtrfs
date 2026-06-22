//! Pure domain core — no I/O, deterministic given an injected clock + timezone.
//! See `documentation/02-architecture-v2.md` §3.

pub mod model;
pub mod naming;
pub mod parent;
pub mod retention;
pub mod safety;
