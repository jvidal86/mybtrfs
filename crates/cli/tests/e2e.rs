//! End-to-end test of `mybtrfs run` against a real (loopback) btrfs filesystem.
//!
//! GATED + root-required: this test is `#[ignore]`d, so plain `cargo test` skips
//! it. It creates loopback btrfs images (`truncate` + `mkfs.btrfs` + `losetup` +
//! `mount`), so it needs **root** and `btrfs-progs`/`util-linux`. Run it with:
//!
//! ```text
//! sudo -E env "PATH=$PATH" cargo test --test e2e -- --ignored --nocapture
//! ```
//!
//! Scenarios from `documentation/05-e2e-test-spec.md`, each on its own loopback
//! fixture:
//!   * **P1 + P4** — a full backup (snapshot → send/receive → verify) plus a
//!     restore leg (P4-01..04: writable + no received_uuid, refuse-without-force,
//!     `--force` move-aside);
//!   * **P2** — an incremental backup chains to a parent (full vs incremental by
//!     the received subvolume's Parent UUID, invariant #1);
//!   * **P3** — prune safety: keep-all (the default) deletes nothing, and a
//!     `--dry-run` prune under an aggressive policy mutates nothing (#8).
//!
//! NOTE: written but unvalidated in the CI sandbox (no root / no user namespaces);
//! the first real run is the actual proof. Process-level behavior that needs no
//! btrfs (exit codes, the concurrency lock) is covered — and actually run — in
//! `cli_behavior.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `program args…`, asserting success; returns the captured stdout.
/// Automatically uses sudo for commands that require root.
fn sh(program: &str, args: &[&str]) -> String {
    // Commands that need root
    let needs_root = matches!(
        program,
        "losetup" | "mkfs.btrfs" | "mount" | "umount" | "btrfs"
    );

    let (cmd, final_args) = if needs_root {
        ("sudo", {
            let mut v = vec![program];
            v.extend_from_slice(args);
            v
        })
    } else {
        (program, args.to_vec())
    };

    let out = Command::new(cmd)
        .args(&final_args)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn `{cmd}`: {err}"));
    assert!(
        out.status.success(),
        "`{program} {}` failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A loopback btrfs filesystem mounted at a temp dir, torn down on `Drop`.
struct LoopbackBtrfs {
    image: PathBuf,
    mountpoint: PathBuf,
    loop_dev: String,
}

impl LoopbackBtrfs {
    fn create(tag: &str) -> Self {
        let image = PathBuf::from(format!("/tmp/mybtrfs-e2e-{tag}.img"));
        let mountpoint = PathBuf::from(format!("/tmp/mybtrfs-e2e-{tag}-mnt"));
        sh("truncate", &["-s", "150M", image.to_str().unwrap()]);
        sh("mkfs.btrfs", &["-q", image.to_str().unwrap()]);
        fs::create_dir_all(&mountpoint).unwrap();
        let loop_dev = sh("losetup", &["--find", "--show", image.to_str().unwrap()])
            .trim()
            .to_owned();
        sh("mount", &[&loop_dev, mountpoint.to_str().unwrap()]);
        Self {
            image,
            mountpoint,
            loop_dev,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.mountpoint.join(rel)
    }
}

impl Drop for LoopbackBtrfs {
    fn drop(&mut self) {
        // Best-effort teardown — runs even if the test panicked.
        let _ = Command::new("sudo")
            .args(&["umount", self.mountpoint.to_str().unwrap()])
            .status();
        let _ = Command::new("sudo")
            .args(&["losetup", "-d", &self.loop_dev])
            .status();
        let _ = fs::remove_dir_all(&self.mountpoint);
        let _ = fs::remove_file(&self.image);
    }
}

#[test]
#[ignore = "requires root + btrfs-progs; run: sudo -E env PATH=$PATH cargo test --test e2e -- --ignored"]
fn run_full_backup_against_real_btrfs() {
    let pool = LoopbackBtrfs::create("pool");
    let drive = LoopbackBtrfs::create("drive");

    // A source subvolume with some data on the pool.
    let source = pool.path("home");
    sh("btrfs", &["subvolume", "create", source.to_str().unwrap()]);
    fs::write(source.join("data.txt"), b"hello mybtrfs").unwrap();

    // Snapshot dir (on the pool) and target dir (on the drive).
    let snapshot_dir = pool.path("snapshots");
    fs::create_dir_all(&snapshot_dir).unwrap();
    let target_dir = drive.path("host");
    fs::create_dir_all(&target_dir).unwrap();

    // Run the actual binary: snapshot → send/receive → prune (keep-all default).
    let status = Command::new(env!("CARGO_BIN_EXE_mybtrfs"))
        .args([
            "run",
            source.to_str().unwrap(),
            snapshot_dir.to_str().unwrap(),
            "home",
            target_dir.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "`mybtrfs run` exited non-zero");

    // Exactly one source snapshot was created.
    let snapshots = subdir_entries(&snapshot_dir);
    assert_eq!(
        snapshots.len(),
        1,
        "expected one source snapshot, got {snapshots:?}"
    );

    // Exactly one backup landed on the target, read-only with a received_uuid.
    let backups = subdir_entries(&target_dir);
    assert_eq!(
        backups.len(),
        1,
        "expected one backup on the target, got {backups:?}"
    );
    let show = sh(
        "btrfs",
        &["subvolume", "show", backups[0].to_str().unwrap()],
    );
    assert!(
        show.contains("readonly"),
        "backup must be read-only:\n{show}"
    );
    let received = show
        .lines()
        .find_map(|line| line.trim().strip_prefix("Received UUID:"))
        .map(str::trim);
    assert!(
        matches!(received, Some(uuid) if uuid != "-" && !uuid.is_empty()),
        "backup must carry a received_uuid:\n{show}"
    );

    // --- Restore leg (documentation/05 §6, P4-01..04) ----------------------
    // Restore the backup to a fresh writable subvolume. `make_writable` is a
    // local `btrfs subvolume snapshot` (same-filesystem only), so the dest must
    // live on the DRIVE — the same filesystem as the backup, not the pool.
    let backup = &backups[0];
    let dest = drive.path("home_restored");
    let status = Command::new(env!("CARGO_BIN_EXE_mybtrfs"))
        .args(["restore", backup.to_str().unwrap(), dest.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "`mybtrfs restore` exited non-zero");

    // The restored subvol must be READ-WRITE with an EMPTY received_uuid — the
    // `make_writable` trap (P4-01/02, invariant #7): a plain `snapshot` without
    // `-r`, never `property set ro=false` (which would forge a received_uuid).
    let restored_show = sh("btrfs", &["subvolume", "show", dest.to_str().unwrap()]);
    assert!(
        !restored_show.contains("readonly"),
        "restored subvolume must be writable:\n{restored_show}"
    );
    let restored_received = restored_show
        .lines()
        .find_map(|line| line.trim().strip_prefix("Received UUID:"))
        .map(str::trim);
    assert!(
        matches!(restored_received, None | Some("-") | Some("")),
        "restored subvolume must NOT carry a received_uuid:\n{restored_show}"
    );
    // Content survived the restore.
    assert_eq!(
        fs::read(dest.join("data.txt")).unwrap(),
        b"hello mybtrfs",
        "restored content must match the source"
    );

    // P4-03: restoring onto an existing dest WITHOUT --force is refused.
    let refused = Command::new(env!("CARGO_BIN_EXE_mybtrfs"))
        .args(["restore", backup.to_str().unwrap(), dest.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(
        !refused.success(),
        "restore onto an existing dest without --force must fail"
    );

    // P4-04: --force moves the existing dest aside to `<dest>.broken`, succeeds.
    let forced = Command::new(env!("CARGO_BIN_EXE_mybtrfs"))
        .args([
            "restore",
            backup.to_str().unwrap(),
            dest.to_str().unwrap(),
            "--force",
        ])
        .status()
        .unwrap();
    assert!(
        forced.success(),
        "`mybtrfs restore --force` exited non-zero"
    );
    let moved_aside = drive.path("home_restored.broken");
    assert!(
        moved_aside.exists(),
        "the displaced destination should be at {}",
        moved_aside.display()
    );
}

// ===== Phase 2 (incremental) and Phase 3 (prune) scenarios =====

#[test]
#[ignore = "requires root + btrfs-progs; run: sudo -E env PATH=$PATH cargo test --test e2e -- --ignored"]
fn run_incremental_backup_chains_to_a_parent() {
    // E2E-P2-01/02: after a full backup, a mutation + a second run produces an
    // *incremental* backup. Invariant #1's marker: a full receive has no Parent
    // UUID, an incremental one does — so of the two backups exactly one carries it.
    let pool = LoopbackBtrfs::create("p2-pool");
    let drive = LoopbackBtrfs::create("p2-drive");
    let (_snapshot_dir, target_dir) = two_backups(&pool, &drive);

    let backups = subdir_entries(&target_dir);
    assert_eq!(backups.len(), 2, "expected two backups, got {backups:?}");
    let incremental = backups.iter().filter(|b| parent_uuid(b).is_some()).count();
    assert_eq!(
        incremental, 1,
        "exactly one backup should be incremental (Parent UUID set)"
    );
}

#[test]
#[ignore = "requires root + btrfs-progs; run: sudo -E env PATH=$PATH cargo test --test e2e -- --ignored"]
fn prune_keep_all_default_deletes_nothing() {
    // E2E-P3-01: the default retention is keep-all, so a plain prune is a no-op
    // (no over-deletion) — both snapshots and both backups remain.
    let pool = LoopbackBtrfs::create("p3a-pool");
    let drive = LoopbackBtrfs::create("p3a-drive");
    let (snapshot_dir, target_dir) = two_backups(&pool, &drive);

    assert!(
        mybtrfs(&[
            "prune",
            snapshot_dir.to_str().unwrap(),
            target_dir.to_str().unwrap(),
        ])
        .success(),
        "`mybtrfs prune` (keep-all) exited non-zero"
    );
    assert_eq!(subdir_entries(&snapshot_dir).len(), 2, "kept all snapshots");
    assert_eq!(subdir_entries(&target_dir).len(), 2, "kept all backups");
}

#[test]
#[ignore = "requires root + btrfs-progs; run: sudo -E env PATH=$PATH cargo test --test e2e -- --ignored"]
fn prune_dry_run_changes_nothing() {
    // E2E-P3-09 / CC-01: even an aggressive policy under `--dry-run` mutates
    // nothing (invariant #8) — both snapshots and backups survive.
    let pool = LoopbackBtrfs::create("p3b-pool");
    let drive = LoopbackBtrfs::create("p3b-drive");
    let (snapshot_dir, target_dir) = two_backups(&pool, &drive);

    assert!(
        mybtrfs(&[
            "prune",
            snapshot_dir.to_str().unwrap(),
            target_dir.to_str().unwrap(),
            "--dry-run",
            "--snapshot-preserve-min",
            "latest",
        ])
        .success(),
        "`mybtrfs prune --dry-run` exited non-zero"
    );
    assert_eq!(
        subdir_entries(&snapshot_dir).len(),
        2,
        "dry-run kept snapshots"
    );
    assert_eq!(subdir_entries(&target_dir).len(), 2, "dry-run kept backups");
}

/// Run the `mybtrfs` binary with `args`, returning its exit status.
fn mybtrfs(args: &[&str]) -> std::process::ExitStatus {
    Command::new(env!("CARGO_BIN_EXE_mybtrfs"))
        .args(args)
        .status()
        .unwrap()
}

/// A source subvolume on `pool` backed up to `drive` **twice** — a full backup,
/// then an incremental one after a mutation. Returns `(snapshot_dir, target_dir)`.
/// Shared by the Phase 2/3 scenarios above.
fn two_backups(pool: &LoopbackBtrfs, drive: &LoopbackBtrfs) -> (PathBuf, PathBuf) {
    let source = pool.path("home");
    sh("btrfs", &["subvolume", "create", source.to_str().unwrap()]);
    fs::write(source.join("a.txt"), b"one").unwrap();
    let snapshot_dir = pool.path("snapshots");
    fs::create_dir_all(&snapshot_dir).unwrap();
    let target_dir = drive.path("host");
    fs::create_dir_all(&target_dir).unwrap();

    let run = |label: &str| {
        assert!(
            mybtrfs(&[
                "run",
                source.to_str().unwrap(),
                snapshot_dir.to_str().unwrap(),
                "home",
                target_dir.to_str().unwrap(),
            ])
            .success(),
            "`mybtrfs run` ({label}) exited non-zero"
        );
    };
    run("full");
    fs::write(source.join("b.txt"), b"two").unwrap();
    run("incremental");
    (snapshot_dir, target_dir)
}

/// The "Parent UUID" of a subvolume (`btrfs subvolume show`); `None` when unset
/// (`-`/empty) — i.e. a full backup; `Some` for an incremental one.
fn parent_uuid(path: &Path) -> Option<String> {
    sh("btrfs", &["subvolume", "show", path.to_str().unwrap()])
        .lines()
        .find_map(|line| line.trim().strip_prefix("Parent UUID:"))
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "-")
        .map(str::to_owned)
}

/// The entries directly inside `dir` (the created subvolumes), as full paths.
fn subdir_entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect()
}
