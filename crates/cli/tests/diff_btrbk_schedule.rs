//! Differential conformance test, **Tier T1 (scheduler diff)**: mybtrfs's
//! retention scheduler (`mybtrfs_domain::retention`) vs the original **btrbk**'s
//! scheduler, compared on the *same* synthetic snapshot set + reference time.
//! See `documentation/06-differential-oracle-test-spec.md`.
//!
//! Two layers, by what can be verified without a live oracle:
//!
//!  * **Always-on unit tests** for the pure, verifiable pieces — the parser for
//!    btrbk's `--format raw` schedule output (grounded in btrbk's source: schedule
//!    raw columns `topic action url host port path hod dow min h d w m y`, emitted
//!    as space-separated `key=value` pairs) and mybtrfs's survivor extraction.
//!    These run under plain `cargo test`.
//!
//!  * A **gated `#[ignore]`d oracle diff** (`oracle_schedule_diff_against_btrbk`)
//!    that actually runs btrbk. btrbk has no injectable clock and schedules from
//!    live btrfs, so a deterministic run needs root/loopback (a controlled set) and
//!    a real btrbk binary (`MYBTRFS_BTRBK`); it mirrors the e2e gate and, like it,
//!    is written-but-unvalidated in the sandbox. It is faketime-free: snapshots are
//!    placed at whole-day offsets from a single `now` that both schedulers share,
//!    so sub-second wall-clock skew never flips a day/week/month decision.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use chrono::NaiveDateTime;
use mybtrfs_domain::naming::parse_name;
use mybtrfs_domain::retention::{DatedEntry, RetentionPolicy, schedule};

// ===== Oracle side: parse btrbk `--format raw` schedule output =====

/// The subvolume **basenames** btrbk's schedule marks `action=preserve`. btrbk
/// emits one space-separated `key=value` row per item; we read `action` + `path`.
fn btrbk_schedule_survivors(raw: &str) -> BTreeSet<String> {
    raw.lines()
        .filter_map(parse_btrbk_raw_row)
        .filter(|(action, _)| action == "preserve")
        .map(|(_, path)| leaf(&path))
        .collect()
}

/// Parse one btrbk raw row (`k=v k=v …`) into `(action, path)` when both appear.
fn parse_btrbk_raw_row(line: &str) -> Option<(String, String)> {
    let mut action = None;
    let mut path = None;
    for field in line.split_whitespace() {
        if let Some((key, value)) = field.split_once('=') {
            match key {
                "action" => action = Some(unquote(value)),
                "path" => path = Some(unquote(value)),
                _ => {}
            }
        }
    }
    Some((action?, path?))
}

/// Strip btrbk's `quoteshell` single-quoting if present (`'…'`, with `'\''`
/// standing for an embedded quote).
fn unquote(value: &str) -> String {
    value
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .map_or_else(|| value.to_owned(), |inner| inner.replace("'\\''", "'"))
}

/// The final path component (btrbk reports absolute `path`; we compare leaves).
fn leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

// ===== Subject side: mybtrfs's retention scheduler =====

/// mybtrfs's survivor set for `names` under `policy` at `now`: parse each name's
/// timestamp, run the domain scheduler, and collect the preserved names. Names
/// that don't match the scheme are skipped (mybtrfs leaves them untouched).
fn mybtrfs_survivors(
    names: &[&str],
    policy: &RetentionPolicy,
    now: NaiveDateTime,
) -> BTreeSet<String> {
    let entries: Vec<DatedEntry<String>> = names
        .iter()
        .filter_map(|name| {
            let parsed = parse_name(name)?;
            Some(DatedEntry {
                instant: parsed.naive.and_utc().timestamp(),
                local: parsed.naive,
                has_exact_time: parsed.has_exact_time,
                nn: parsed.nn,
                payload: (*name).to_owned(),
            })
        })
        .collect();
    schedule(entries, policy, now)
        .preserve
        .into_iter()
        .collect()
}

// ===== Always-on unit tests (the verifiable halves of the differential) =====

#[test]
fn parses_btrbk_raw_schedule_preserve_and_delete() {
    // Grounded in btrbk's `--format raw` schedule format.
    let raw = "\
topic=snapshot action=preserve url=/mnt/pool/home.20240103 host=- port=- path=/mnt/pool/home.20240103 hod=0 dow=monday min=0 h=- d=14 w=- m=- y=-
topic=snapshot action=delete url=/mnt/pool/home.20240102 host=- port=- path=/mnt/pool/home.20240102 hod=0 dow=monday min=0 h=- d=- w=- m=- y=-
topic=snapshot action=preserve url=/mnt/pool/home.20240101 host=- port=- path=/mnt/pool/home.20240101 hod=0 dow=monday min=0 h=- d=- w=- m=- y=-";
    assert_eq!(
        btrbk_schedule_survivors(raw),
        BTreeSet::from(["home.20240103".to_owned(), "home.20240101".to_owned()])
    );
}

#[test]
fn parser_skips_blank_lines_and_rows_missing_action_or_path() {
    let raw = "\n\
topic=snapshot d=1\n\
topic=snapshot action=preserve path=/x/home.20240101 d=1\n";
    assert_eq!(
        btrbk_schedule_survivors(raw),
        BTreeSet::from(["home.20240101".to_owned()])
    );
}

#[test]
fn parser_unquotes_a_shell_quoted_path() {
    // btrbk `quoteshell`s a value only when it has shell-special characters; the
    // fields stay space-separated, so we strip surrounding quotes but assume the
    // value itself has no spaces (true for scheme leaf names like `home.<ts>`).
    let raw = "action=preserve path='/mnt/pool/home.20240101'";
    assert_eq!(
        btrbk_schedule_survivors(raw),
        BTreeSet::from(["home.20240101".to_owned()])
    );
}

#[test]
fn mybtrfs_keep_all_preserves_every_named_snapshot() {
    let names = ["home.20240101", "home.20240102", "home.20240103"];
    let policy = RetentionPolicy::parse("all", "").unwrap();
    let now = "2024-01-10T12:00:00".parse::<NaiveDateTime>().unwrap();
    assert_eq!(
        mybtrfs_survivors(&names, &policy, now),
        names.iter().map(|n| (*n).to_owned()).collect()
    );
}

#[test]
fn mybtrfs_preserve_min_latest_keeps_only_the_newest() {
    let names = ["home.20240101", "home.20240102", "home.20240103"];
    let policy = RetentionPolicy::parse("latest", "").unwrap();
    let now = "2024-01-10T12:00:00".parse::<NaiveDateTime>().unwrap();
    assert_eq!(
        mybtrfs_survivors(&names, &policy, now),
        BTreeSet::from(["home.20240103".to_owned()])
    );
}

// ===== Gated live oracle diff (root + loopback + a real btrbk) =====

/// A loopback btrfs filesystem mounted at a temp dir, torn down on `Drop` (so a
/// panicking assertion never leaks a loop device or mount).
struct Loopback {
    image: std::path::PathBuf,
    mnt: std::path::PathBuf,
    loop_dev: String,
}

impl Loopback {
    fn create() -> Self {
        let image = std::path::PathBuf::from("/tmp/mybtrfs-diff.img");
        let mnt = std::path::PathBuf::from("/tmp/mybtrfs-diff-mnt");
        sh("truncate", &["-s", "400M", image.to_str().unwrap()]);
        sh("mkfs.btrfs", &["-q", image.to_str().unwrap()]);
        std::fs::create_dir_all(&mnt).unwrap();
        let loop_dev = sh("losetup", &["--find", "--show", image.to_str().unwrap()])
            .trim()
            .to_owned();
        sh("mount", &[&loop_dev, mnt.to_str().unwrap()]);
        Self {
            image,
            mnt,
            loop_dev,
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        let _ = std::process::Command::new("umount").arg(&self.mnt).status();
        let _ = std::process::Command::new("losetup")
            .args(["-d", &self.loop_dev])
            .status();
        let _ = std::fs::remove_dir_all(&self.mnt);
        let _ = std::fs::remove_file(&self.image);
    }
}

/// Run `program args…`, asserting success; returns captured stdout.
fn sh(program: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn `{program}`: {e}"));
    assert!(
        out.status.success(),
        "`{program} {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "needs root/loopback + a real btrbk via MYBTRFS_BTRBK; see documentation/06-differential-oracle-test-spec.md. Written but unvalidated in the CI sandbox."]
fn oracle_schedule_diff_against_btrbk() {
    use chrono::{Duration, Local, Timelike};

    // Absolute path to the reference btrbk (the 06 spec invokes it by path, not on
    // PATH). Absent → skip cleanly: this gate cannot run without it.
    let Ok(btrbk) = std::env::var("MYBTRFS_BTRBK") else {
        eprintln!("skipping: set MYBTRFS_BTRBK=/abs/path/to/btrbk to run the diff");
        return;
    };
    assert!(
        std::path::Path::new(&btrbk).is_file(),
        "MYBTRFS_BTRBK does not point at a file: {btrbk}"
    );

    let lo = Loopback::create();

    // Faketime-free determinism: place snapshots at whole-day offsets from one
    // shared `now` pinned to noon (well off the midnight boundary), so the
    // sub-second skew between btrbk's wall clock and our injected clock can never
    // flip a day-tier decision.
    let now = Local::now()
        .with_hour(12)
        .and_then(|t| t.with_minute(0))
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("noon is a valid time")
        .naive_local();

    // Three daily snapshots (now, now-1d, now-2d) named in btrbk's `long` format.
    let snap_dir = lo.mnt.join("btrbk_snapshots");
    std::fs::create_dir_all(&snap_dir).unwrap();
    let mut names = Vec::new();
    for days in 0..3i64 {
        let ts = now - Duration::days(days);
        let name = format!("home.{}", ts.format("%Y%m%dT%H%M"));
        let path = snap_dir.join(&name);
        sh("btrfs", &["subvolume", "create", path.to_str().unwrap()]);
        sh(
            "btrfs",
            &["property", "set", path.to_str().unwrap(), "ro", "true"],
        );
        names.push(name);
    }
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    // Retention: keep the two most recent daily snapshots (prune the oldest). The
    // SAME policy strings feed both schedulers.
    let (preserve_min, preserve) = ("no", "2d");
    let conf = lo.mnt.join("btrbk.conf");
    std::fs::write(
        &conf,
        format!(
            "timestamp_format long\n\
             snapshot_dir btrbk_snapshots\n\
             snapshot_preserve_min {preserve_min}\n\
             snapshot_preserve {preserve}\n\
             volume {}\n  \
               subvolume home\n",
            lo.mnt.display()
        ),
    )
    .unwrap();

    // Oracle: btrbk's schedule decision (`-n -S` = dry-run, schedule-only).
    let raw = sh(
        &btrbk,
        &[
            "-c",
            conf.to_str().unwrap(),
            "-n",
            "-S",
            "--format",
            "raw",
            "run",
        ],
    );
    let btrbk_keep = btrbk_schedule_survivors(&raw);

    // Subject: mybtrfs's domain scheduler over the same names + policy + now.
    let policy = RetentionPolicy::parse(preserve_min, preserve).unwrap();
    let mybtrfs_keep = mybtrfs_survivors(&name_refs, &policy, now);

    assert_eq!(
        btrbk_keep, mybtrfs_keep,
        "scheduler survivor sets must match\n  btrbk:   {btrbk_keep:?}\n  mybtrfs: {mybtrfs_keep:?}"
    );
}
