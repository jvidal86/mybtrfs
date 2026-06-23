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
//! Scenario from `documentation/05-e2e-test-spec.md`: a full backup
//! (snapshot → send/receive → verify) followed by a restore leg (P4-01..04:
//! writable + no received_uuid, refuse-without-force, `--force` move-aside),
//! proving the whole stack against real btrfs.
//!
//! NOTE: written but unvalidated in the CI sandbox (no root / no user namespaces);
//! the first real run is the actual proof of `run`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `program args…`, asserting success; returns the captured stdout.
fn sh(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("failed to spawn `{program}`: {err}"));
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
        sh("truncate", &["-s", "400M", image.to_str().unwrap()]);
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
        let _ = Command::new("umount").arg(&self.mountpoint).status();
        let _ = Command::new("losetup")
            .args(["-d", &self.loop_dev])
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
    // Restore the backup to a fresh writable subvolume on the pool.
    let backup = &backups[0];
    let dest = pool.path("home_restored");
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
    let moved_aside = pool.path("home_restored.broken");
    assert!(
        moved_aside.exists(),
        "the displaced destination should be at {}",
        moved_aside.display()
    );
}

/// The entries directly inside `dir` (the created subvolumes), as full paths.
fn subdir_entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect()
}
