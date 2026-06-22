//! Application use cases (orchestrators). Depend only on `ports` + `domain`;
//! never on a concrete adapter. See `documentation/02-architecture-v2.md` §3.

pub mod backup;
pub mod inventory;
pub mod restore;
pub mod retention;
