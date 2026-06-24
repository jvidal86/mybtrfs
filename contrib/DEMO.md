# v1.1 Feature Demos: Phase 1 & Phase 2

This directory contains demonstration scripts for the v1.1 release features: **Retention Preview** (Phase 1) and **Status View** (Phase 2).

## Quick Start

```bash
# Phase 1: Retention Preview (what would be deleted)
./contrib/demo-phase1-retention-preview.sh

# Phase 2: Status View (backup health snapshot)
./contrib/demo-phase2-status-view.sh
```

Both scripts are **safe to run** (Phase 1 uses `--dry-run`, Phase 2 is read-only).

---

## Phase 1: Retention Preview

**What it is:**  
`mybtrfs prune --dry-run` shows what snapshots/backups would be deleted before actually deleting them.

**Expected output:**
```
Retention Policy Preview — Snapshot Side
─────────────────────────────────────────────────────────
PRESERVE (7 snapshots):
  ✅ data.20260624T1432 (just now)
  ✅ data.20260623T1432 (1 day ago)
  ✅ data.20260622T1432 (2 days ago)
  ✅ data.20260621T1432 (3 days ago)
  ✅ data.20260620T1432 (4 days ago)
  ✅ data.20260619T1432 (5 days ago)
  ✅ data.20260618T1432 (6 days ago)

DELETE (2 snapshots) — run with --yes to confirm:
  ⚠️  data.20260617T1432 (7 days ago)
  ⚠️  data.20260610T1432 (14 days ago)

Retention Policy Preview — Backup Side
─────────────────────────────────────────────────────────
PRESERVE (4 backups):
  ✅ data.20260624T1432 (just now)
  ✅ data.20260623T1432 (1 day ago)
  ✅ data.20260622T1432 (2 days ago)
  ✅ data.20260615T1432 (9 days ago)

DELETE (1 backup) — run with --yes to confirm:
  ⚠️  data.20260603T1432 (21 days ago)
```

**What to look for:**
- ✅ Green checkmarks for snapshots/backups being **kept**
- ⚠️ Warning icons for snapshots/backups being **deleted**
- Snapshot/backup **names** (with timestamps)
- **Age** calculations ("just now", "7 days ago", etc.)
- **Counts** in parentheses
- Disclaimer: "run with --yes to confirm" (only in DELETE section)

**Code location:**
- `crates/application/src/retention_preview.rs` — formatting logic
- `crates/cli/src/cli.rs:print_prune_report()` — integration

**Tests:**
- 8 unit tests in `retention_preview.rs`: name display, partition separation, edge cases, determinism

---

## Phase 2: Status View

**What it is:**  
`mybtrfs status <source> <target>` shows backup health at a glance: snapshot/backup counts, latest ages, health checks.

**Expected output (current v1.1):**
```
Status Report
────────────────────────────────────────────
Source:      /mnt/data/.snapshots
Target:      /backup/daily

Snapshot count:  5 snapshots
Backup count:    4 backups
```

**What you're seeing:**
- Status Report header
- Source and target directories
- Snapshot count
- Backup count

**Future v1.1+ enhancements (infrastructure ready):**
```
Status Report
────────────────────────────────────────────
Source:      /mnt/data/.snapshots
Target:      /backup/daily

Latest snapshot:  data.20260624T1432 (just now)
Latest backup:    data.20260624T1432 (just now)

Snapshot count:  5 snapshots  [retention policy: keep 7 daily]
Backup count:    4 backups    [retention policy: keep 4 daily, 4 weekly]

Health check:
  ✅ Backup matches latest snapshot (incremental parent OK)
  ✅ No orphaned snapshots (all have backups or within policy)
```

**Code location:**
- `crates/application/src/status.rs` — StatusService, StatusReport
- `crates/cli/src/cli.rs` — Status command, print_status()

**Tests:**
- 7 unit tests in `status.rs`: counts, latest identification, health checks, edge cases
- 10 E2E test stubs in `crates/cli/tests/status_e2e.rs` (ready for loopback validation)

---

## Running the Demos

### Prerequisites

1. **Build mybtrfs:**
   ```bash
   cargo build -p mybtrfs
   ```

2. **Have a btrfs filesystem** with snapshots and backups:
   - Real btrfs mount point, OR
   - Loopback fixture created by `contrib/test/mybtrfs-loopback.sh`

### Option A: Demo on Real btrfs Filesystem

```bash
# Set the paths and run
SOURCE_SNAP=/path/to/snapshots TARGET_BACKUP=/path/to/backups \
  ./contrib/demo-phase1-retention-preview.sh

SOURCE_SNAP=/path/to/snapshots TARGET_BACKUP=/path/to/backups \
  ./contrib/demo-phase2-status-view.sh
```

### Option B: Demo on Loopback Fixture

If you have a loopback test fixture (requires root):

```bash
# Create fixture (one-time)
sudo ./contrib/test/mybtrfs-loopback.sh setup

# Run demos
sudo SOURCE_SNAP=/tmp/mybtrfs-test/.snapshots \
      TARGET_BACKUP=/tmp/mybtrfs-test/.backups \
      ./contrib/demo-phase1-retention-preview.sh

sudo SOURCE_SNAP=/tmp/mybtrfs-test/.snapshots \
      TARGET_BACKUP=/tmp/mybtrfs-test/.backups \
      ./contrib/demo-phase2-status-view.sh

# Clean up
sudo ./contrib/test/mybtrfs-loopback.sh teardown
```

### Option C: No btrfs Available?

The demo scripts will still run and show:
- Expected output format (documented in the script)
- Code locations and test references
- Explanation of what features do

---

## Architecture & Testing

### Phase 1: Retention Preview
- **Module:** `crates/application/src/retention_preview.rs`
- **Public functions:**
  - `format_schedule(schedule: &Schedule<Subvolume>) -> String`
  - `compute_age(name: &str, now: &DateTime<Local>) -> String`
- **Tests:** 8 unit tests (green ✅)
- **E2E stubs:** 10 test cases in `crates/cli/tests/retention_preview_e2e.rs`

### Phase 2: Status View
- **Module:** `crates/application/src/status.rs`
- **Public types:**
  - `StatusReport` — holds snapshot/backup lists
  - `StatusService<'a>` — orchestrates queries via SubvolumeRepository
- **Public methods:**
  - `StatusService::report(source_dir, target_dir) -> Result<StatusReport>`
- **Tests:** 7 unit tests (green ✅)
- **E2E stubs:** 10 test cases in `crates/cli/tests/status_e2e.rs`

### Design Principles
- **Stateless:** Re-derives truth from btrfs metadata each run
- **Read-only:** No mutations, safe to run anytime
- **Pure domain:** Business logic stays in `domain` crate (purity enforced)
- **Hexagonal:** All I/O through ports (SubvolumeRepository, ClockPort)
- **TDD:** Tests written first, then implementation

---

## What's Next?

### Phase 3: Snapshot Diff (Optional for v1.1)
- Estimate changed bytes between two snapshots
- Uses `btrfs subvolume find-new` (estimate-only, not exact)
- Design complete; implementation deferred to v1.2 (or ship as estimate in v1.1)

### Post–v1.1 Roadmap
- Byte-level space accounting (requires sizing port)
- Per-file diff breakdown (requires FIEMAP, slow)
- Encryption support
- Backup-set configuration file (sugar over CLI)
- TUI dashboard

---

## Files

```
contrib/
  demo-phase1-retention-preview.sh  ← Run Phase 1 demo
  demo-phase2-status-view.sh        ← Run Phase 2 demo
  DEMO.md                           ← This file
```

---

## Troubleshooting

**Error: "mybtrfs binary not found"**
- Build first: `cargo build -p mybtrfs`
- Or set `MYBTRFS=./target/debug/mybtrfs` before running

**Error: "cannot open /dev/nvme0n1p6: Permission denied"**
- btrfs requires root for some operations
- Try: `sudo ./contrib/demo-phase1-retention-preview.sh`

**Error: "not a valid btrfs filesystem"**
- Check that `SOURCE_SNAP` and `TARGET_BACKUP` point to valid btrfs mounts
- Example: `SOURCE_SNAP=/mnt/data/.snapshots TARGET_BACKUP=/mnt/backup/daily ./contrib/demo-phase1-retention-preview.sh`

---

## Questions?

See the design documentation:
- **Phase 1 & 2 design:** `documentation/10-v1x-plan.md`
- **Architecture:** `documentation/02-architecture-v2.md`
- **Coding guidelines:** `documentation/04-coding-guidelines.md`
