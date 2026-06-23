//! Application use cases (orchestrators). Depend only on `ports` + `domain`;
//! never on a concrete adapter. See `documentation/02-architecture-v2.md` §3.

pub mod ports;

pub mod backup;
pub mod inventory;
pub mod prune;
pub mod restore;
pub mod retention;

/// Initialize `env_logger` once for unit tests (idempotent; safe to call from
/// every `#[test]`). Logs go through the test harness and appear only for
/// failing tests unless `--nocapture` is passed.
#[cfg(test)]
pub(crate) fn init_test_logger() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = env_logger::builder().is_test(true).try_init();
    });
}
