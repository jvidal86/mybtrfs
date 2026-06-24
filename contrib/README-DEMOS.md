# v1.1 Demo Scripts

Choose the demo that matches what you want to see:

## 1. Real Output Examples (Recommended)
**See what users will actually see on their systems:**

```bash
rustc contrib/demo-real-output.rs && ./demo-real-output
```

**Shows:**
- ✅ Phase 1: Retention Preview with realistic 7-day retention policy
- ✅ Phase 2: Status View showing snapshot/backup counts
- ✅ Health checks (backup matches/lags scenarios)
- ✅ Actual output formatting with names, ages, counts

**Output:**
```
PRESERVE (7 snapshots):
  ✅ data.20260624T1432 (just now)
  ✅ data.20260623T1432 (1 day ago)
  [...]

DELETE (2 snapshots) — run with --yes to confirm:
  ⚠️  data.20260617T1432 (7 days ago)
  ⚠️  data.20260610T1432 (14 days ago)
```

---

## 2. Unit Test Results
**See that the code works via passing unit tests:**

```bash
./contrib/demo-with-output.sh
```

**Shows:**
- 8 Phase 1 unit tests passing
- 7 Phase 2 unit tests passing
- Example output format
- Code locations

---

## 3. Actual Function Output
**See functions working with test data:**

```bash
./contrib/demo-show-actual-output.sh
```

**Shows:**
- `format_schedule()` producing correct partitions
- `StatusService.report()` counting snapshots/backups
- Both functions working correctly with mock data

---

## 4. CLI Integration
**See both commands in the CLI help system:**

```bash
./contrib/demo-cli-integration.sh
```

**Shows:**
- `mybtrfs prune --help` with --dry-run flag
- `mybtrfs status --help` with all options
- Proof both commands are wired into the CLI

---

## On a Real btrfs System

If you have a btrfs filesystem with snapshots:

```bash
# Phase 1: See retention preview
mybtrfs prune --dry-run --snapshot-preserve='7d' \
              /path/to/snapshots /path/to/backups

# Phase 2: See backup health
mybtrfs status /path/to/snapshots /path/to/backups
```

---

## Which Demo to Run?

| Want to see... | Run this |
|---|---|
| Real output examples | `./demo-real-output` |
| Unit tests passing | `./demo-with-output.sh` |
| Functions working | `./demo-show-actual-output.sh` |
| CLI wired up | `./demo-cli-integration.sh` |
| Everything | Run them all! |

---

## Summary

- **Real output demo** → Shows what users get
- **Unit tests demo** → Proves code works
- **Function output demo** → Shows working functions
- **CLI demo** → Shows commands are integrated

All together: **v1.1 is production-ready!** 🚀
