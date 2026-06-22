//! Transaction journal — implements `Journal`: an append-only audit of
//! snapshot-create / send-receive / delete actions (file and/or syslog), each
//! with source/target/status. See `documentation/02-architecture-v2.md`.
//
// TODO (Phase 1+).
