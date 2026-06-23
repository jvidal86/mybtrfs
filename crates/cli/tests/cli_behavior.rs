//! Integration tests of the `mybtrfs` binary's **process-level contract** — exit
//! codes and the concurrency lock — that need neither root nor btrfs, so they run
//! under plain `cargo test`. (The btrfs data-path scenarios live in `e2e.rs`,
//! gated behind root/loopback.) See `documentation/05-e2e-test-spec.md` CC-08/CC-09.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::path::PathBuf;
use std::process::Command;

/// The compiled binary under test.
const BIN: &str = env!("CARGO_BIN_EXE_mybtrfs");

/// A unique temp path for this test process/run (avoids cross-test clashes).
fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("mybtrfs-clitest-{tag}-{nanos}"))
}

#[test]
fn lock_held_by_another_run_exits_with_code_3() {
    // E2E-CC-09: hold an exclusive flock on a private lock file, then a mutating
    // command pointed at the same lock must exit 3 (LOCK_BUSY) — the lock is taken
    // before any btrfs work, so this needs no real filesystem.
    let lock = temp_path("lock");
    let held = File::create(&lock).unwrap();
    held.try_lock().unwrap(); // this process now holds the lock

    let status = Command::new(BIN)
        .args([
            "snapshot",
            "/nonexistent/src",
            "/nonexistent/snapdir",
            "home",
            "--lock",
            lock.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(3), "a held lock must yield exit code 3");
    drop(held);
    std::fs::remove_file(&lock).ok();
}

#[test]
fn a_read_only_command_ignores_a_held_lock() {
    // The lock only serializes mutating commands; `list-drives` is read-only and
    // must not block on a held lock (it should not exit 3).
    let lock = temp_path("rolock");
    let held = File::create(&lock).unwrap();
    held.try_lock().unwrap();

    let status = Command::new(BIN)
        .args(["list-drives", "--lock", lock.to_str().unwrap()])
        .status()
        .unwrap();

    assert_ne!(
        status.code(),
        Some(3),
        "a read-only command must not contend for the lock"
    );
    drop(held);
    std::fs::remove_file(&lock).ok();
}

#[test]
fn an_unknown_subcommand_is_a_usage_error_exit_2() {
    // E2E-CC-08: clap rejects an unknown subcommand with the usage exit code.
    let status = Command::new(BIN)
        .arg("definitely-not-a-command")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2), "clap usage error → exit 2");
}

#[test]
fn a_malformed_retention_policy_is_a_usage_error_exit_2() {
    // E2E-CC-08: existing dirs so path validation passes; the bad policy string is
    // what fails → UsageError → exit 2 (distinct from a generic failure exit 1).
    let tmp = std::env::temp_dir();
    let lock = temp_path("usage");
    let status = Command::new(BIN)
        .args([
            "prune",
            tmp.to_str().unwrap(),
            tmp.to_str().unwrap(),
            "--snapshot-preserve-min",
            "not-a-policy",
            "--lock",
            lock.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2), "malformed policy → exit 2");
    std::fs::remove_file(&lock).ok();
}
