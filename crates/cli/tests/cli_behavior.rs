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

/// Return the first mounted btrfs filesystem path, or `None` if:
/// - The process is root (uid 0 — permission would not be denied), or
/// - No btrfs filesystem is mounted (the error would not be "Permission denied")
///
/// Used to conditionally skip permission-denied tests on machines where they can't run.
fn btrfs_path_if_not_root() -> Option<String> {
    // Read effective UID from /proc/self/status — avoids unsafe libc calls.
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let uid: u32 = status
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    if uid == 0 {
        return None; // root — btrfs will not deny permission; test would be wrong
    }
    // findmnt is universally available on modern Linux; simpler than parsing lsblk JSON.
    let out = Command::new("findmnt")
        .args([
            "--types",
            "btrfs",
            "--first-only",
            "--noheadings",
            "--output",
            "TARGET",
        ])
        .output()
        .ok()?;
    let mp = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    if mp.is_empty() { None } else { Some(mp) }
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

#[test]
fn run_without_root_exits_code_4_with_friendly_message() {
    // E2E-CC-10: permission-denied error classification. When a `run` command invokes
    // btrfs without root, the "Permission denied" error is classified as exit code 4
    // (PERMISSION_DENIED) and the stderr message is friendly: "mybtrfs requires root
    // privileges — re-run with sudo". This test verifies both the exit code and the
    // message text.
    //
    // Skipped if root or no btrfs filesystem (the error would not occur).
    let Some(btrfs) = btrfs_path_if_not_root() else {
        return;
    };

    let snap_dir = temp_path("run-snap");
    let target_dir = temp_path("run-target");
    let lock = temp_path("run-no-root-lock");
    std::fs::create_dir_all(&snap_dir).ok();
    std::fs::create_dir_all(&target_dir).ok();

    let out = Command::new(BIN)
        .args([
            "run",
            &btrfs,
            snap_dir.to_str().unwrap(),
            "home",
            target_dir.to_str().unwrap(),
            "--yes",
            "--lock",
            lock.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    std::fs::remove_file(&lock).ok();

    assert_eq!(out.status.code(), Some(4), "no-root run → exit 4");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mybtrfs requires root privileges"),
        "expected 'mybtrfs requires root privileges' in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("re-run with sudo"),
        "expected 're-run with sudo' in stderr, got: {stderr}"
    );
}

#[test]
fn snapshot_without_root_exits_code_4() {
    // E2E-CC-10: `snapshot` is a mutating btrfs command. Without root, it fails
    // with "Permission denied", which is classified as exit code 4.
    let Some(btrfs) = btrfs_path_if_not_root() else {
        return;
    };

    let snap_dir = temp_path("snap-no-root");
    let lock = temp_path("snap-no-root-lock");
    std::fs::create_dir_all(&snap_dir).ok();

    let status = Command::new(BIN)
        .args([
            "snapshot",
            &btrfs,
            snap_dir.to_str().unwrap(),
            "home",
            "--yes",
            "--lock",
            lock.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(4), "no-root snapshot → exit 4");
    std::fs::remove_file(&lock).ok();
}

#[test]
fn list_without_root_exits_code_4() {
    // E2E-CC-10: `list` is read-only but still invokes `btrfs subvolume list`,
    // which denies permission without root. Must exit 4, same as mutating commands.
    let Some(btrfs) = btrfs_path_if_not_root() else {
        return;
    };

    let lock = temp_path("list-no-root-lock");
    let status = Command::new(BIN)
        .args(["list", &btrfs, &btrfs, "--lock", lock.to_str().unwrap()])
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(4), "no-root list → exit 4");
    std::fs::remove_file(&lock).ok();
}

#[test]
fn list_drives_does_not_require_root() {
    // E2E-CC-10: negative test — `list-drives` uses `lsblk` (not `btrfs`), so it
    // must **not** require root and must **not** exit 4. This guards against
    // inadvertently gating a user-friendly discovery command behind sudo.
    //
    // This test runs unconditionally (no skip); it works even on machines with no btrfs.
    let status = Command::new(BIN).arg("list-drives").status().unwrap();

    assert_ne!(
        status.code(),
        Some(4),
        "list-drives must not require root (uses lsblk, not btrfs)"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "list-drives must succeed without root"
    );
}
