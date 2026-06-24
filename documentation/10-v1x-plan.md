# 10 — v1.x Implementation Plan ("Make it visible")

**Status:** Design phase (pre-implementation)  
**Target:** v1.1 release — observability, snapshots diff, retention preview  
**Philosophy:** Spec-before-code; stay true to hexagonal architecture, SOLID, and clean practices.

---

## §1 — Scope & sequencing

This plan selects the three highest-leverage v1.x features from `09-roadmap.md` §6, ranked by
impact and integration simplicity:

1. **Status view** (backed by journal) — the headline wedge; btrbk is weakest here
2. **Snapshot diff** — cheap, differentiating, unit-testable
3. **Retention preview** — ~90% done via dry-run; needs presentation polish

**Deliberately deferred to v2:** native encryption, restorability check, TUI, backup-set file
(each needs design-first work or infra we lack).

**Why this order:**
- Status view lands first because it's the most-cited pain point (roadmap users + btrbk
  switcher feedback).
- Snapshot diff is self-contained and validates the "new query" pattern.
- Retention preview is quick polish with no new ports or adapters.
- Each is orthogonal; can ship independently.

---

## §2 — Architecture decisions (no domain changes)

### 2.1 — Dependency rule compliance

All three features are **read-only query operations**. They do NOT modify state, do NOT add
new dangerous operations, and do NOT touch the domain core (`naming`, `model`, `retention`,
`parent`, `safety`).

```
Dependency rule: cli → adapters → application → domain
                  ↑
              (new CLI commands here)
                  ↑
            (new application services here: read-only)
```

Each feature:
- **Adds a new CLI command** (`status`, `diff`, `prune --preview`)
- **Adds new application service(s)** (e.g., `StatusService`) with ports for reads
- **Reuses existing ports** (`SubvolumeRepository`, `ClockPort`, `JournalPort`)
- **No new dangerous ports** (no delete, no transfer, no write)

### 2.2 — Port reuse vs. new ports

| Feature | Existing ports | New port? |
|---------|---|---|
| Status view | `SubvolumeRepository`, `ClockPort`, `JournalPort` | No — journal is the backing |
| Snapshot diff | `SubvolumeRepository` | No — pure diff logic |
| Retention preview | `SubvolumeRepository`, `RetentionPolicy` | No — re-use from `RetentionService` |

**Principle:** New queries go through existing ports; only dangerous ops get new ports.

### 2.3 — Service layer additions

New application services (in `crates/application/src/`):

```rust
// status.rs
pub struct StatusService<'a> {
    source_repo: &'a dyn SubvolumeRepository,
    target_repo: &'a dyn SubvolumeRepository,
    clock: &'a dyn ClockPort,
    journal: &'a dyn Journal,  // backing for last-run health
}

impl<'a> StatusService<'a> {
    /// Compute the status report: last successful run, last failure, space used, next scheduled run.
    /// Pure read; re-derives truth from btrfs metadata + audit log.
    pub fn report(&self, source_dir: &Path, target_dir: &Path) -> Result<StatusReport, PortError> { ... }
}

// diff.rs
pub struct DiffService;

impl DiffService {
    /// Return a summary of what changed between two snapshots: added, removed, modified bytes.
    /// Uses btrfs `send --check-parent` metadata (or post-transfer inspection).
    pub fn diff(older: &Subvolume, newer: &Subvolume) -> DiffSummary { ... }
}

// retention_preview.rs (in existing prune.rs or new file)
impl PruneService<'_> {
    /// Compute and pretty-print what the retention policy would delete (already ~90% done).
    pub fn preview(&self, ...) -> Result<Vec<(Subvolume, &'static str)>, PortError> { ... }
}
```

---

## §3 — Feature designs (high-level)

### 3.1 — Status view

**Goal:** Show last-run health without querying a side database (stateless, re-derived).

**Input:**
- Source path (where snapshots live)
- Target path (where backups live)
- Optional: `--all-targets` to report across multiple targets

**Output (human-readable + JSON for scripting):**
```
Status Report
─────────────────────────────────────────
Target: /backup/myhost
  Last successful backup:  2026-06-24 14:32:10 (41 minutes ago)
  Last run status:         ✅ success
  Latest snapshot:         data.20260624T143210 (readonly, 1.2 GB)
  Latest backup:           data.20260624T143210 (readonly, received, 1.2 GB)
  Next scheduled run:      2026-06-24 15:00:00 (systemd timer)
  
Space summary:
  Source snapshots:        2.4 GB (3 snapshots) [policy: keep 7 daily]
  Target backups:          4.8 GB (6 backups)   [policy: keep 4 daily, 4 weekly]
  
Health check:
  ⚠️ Warning: 1 orphaned snapshot (delete failed on 2026-06-23)
  ⚠️ Warning: Last backup is 41 minutes old (target unreachable for 6 hours yesterday)
```

**Architecture:**
- `StatusService` reads snapshots + backups (via repos), journal (last N runs), current time.
- Formats output via adapter (new `OutputFormatter` port? or just in CLI).
- **Pure:** no I/O except reads; re-derives health from existing data.

**Integration points:**
- **Journal port:** provides audit trail (timestamp, exit code, error if any).
- **ClockPort:** for "how long ago was the last run" calculations.
- **SubvolumeRepository:** for space used (already has `list`).

**Testing:**
- Unit: mock repos + journal, verify status computation.
- E2E: loopback fixture with known snapshots/backups, check reported health.

### 3.2 — Snapshot diff

**Goal:** Show what changed between two snapshots (added/removed/modified bytes).

**Input:** Two snapshot paths (can be on same source or different sources via SSH).

**Output:**
```
Difference between data.20260618T120000 and data.20260624T143210
───────────────────────────────────────────────────────────────
  Files added:      142 files, +850 MB
  Files deleted:    8 files, -12 MB
  Files modified:   24 files, +120 MB (net +85 MB in those 24 files)
  Directories:      +3 new dirs, -1 deleted
  
Total delta:        +943 MB
```

**Implementation approach:**
- **Option A (cheaper):** Use btrfs `send --check-parent` metadata inspection — parse the send-stream header to extract delta size without actually transferring.
- **Option B (fallback):** For offline snapshots, use `btrfs filesystem usage` or metadata comparison on each snapshot.
- Start with Option B (guaranteed to work on any two snapshots, pure metadata read).

**Architecture:**
- `DiffService` (pure, stateless) takes two `Subvolume` references, computes delta.
- New adapter `SnapshotDiffAdapter` spawns btrfs commands to inspect each snapshot.
- **No new ports** — reuse `SubvolumeRepository` for metadata reads.

**Integration:**
- CLI command: `mybtrfs diff <source1> <source2>` (paths, not subvolume objects; discovery in CLI).
- Works over SSH via existing `Endpoint` parsing (e.g., `mybtrfs diff /local/snap ssh://host/snap`).

**Testing:**
- Unit: mock snapshots with known deltas, verify computation.
- E2E: create two loopback snapshots with known file changes, measure delta.

### 3.3 — Retention preview polish

**Goal:** Pretty-print what `prune` would delete *before* it runs (already ~90% done).

**Current state:** `prune --dry-run` computes the schedule but prints in debug format:
```
Schedule { preserve: [...], delete: [...] }
```

**Target output:**
```
Retention Policy: GFS (keep 7 daily, 4 weekly, 12 monthly)
Source snapshots: /path/.snapshots/
──────────────────────────────────────────────────────────
PRESERVE (7 snapshots, 3.2 GB):
  ✅ data.20260624T143210 (today)
  ✅ data.20260623T143210 (1 day ago)
  ✅ data.20260622T143210 (2 days ago)
  [...]

DELETE (2 snapshots, 240 MB) — run with --yes to confirm:
  ⚠️  data.20260617T143210 (7 days ago, 120 MB)
  ⚠️  data.20260610T143210 (14 days ago, 120 MB)
```

**Implementation:**
- Move the pretty-printer from `BackupService::run` into `PruneService::preview()`.
- Add `--preview` flag to `prune` (or keep `--dry-run` and auto-format).
- **No new logic** — just a view over the existing `Schedule` struct.

**Testing:**
- Unit: mock schedule, verify output format.
- E2E: run `prune --dry-run` on loopback, verify output matches expected.

---

## §4 — Integration with existing code

### 4.1 — CLI dispatch (crates/cli/src/cli.rs)

New subcommands:

```rust
#[derive(Subcommand)]
enum Command {
    // ... existing commands ...
    
    /// Show status: last backup health, space, warnings.
    #[command(about = "Show backup health and observability")]
    Status {
        /// Source directory (where snapshots live)
        source_dir: PathBuf,
        /// Target directory (where backups live)
        target_dir: PathBuf,
        /// Show detailed space breakdown (default: summary only)
        #[arg(long)]
        detailed: bool,
        /// Output as JSON for scripting
        #[arg(long)]
        json: bool,
    },
    
    /// Show what changed between two snapshots.
    #[command(about = "Compare snapshots")]
    Diff {
        /// First snapshot (local path or ssh://host/path)
        snapshot1: String,
        /// Second snapshot (local path or ssh://host/path)
        snapshot2: String,
        /// Show file-by-file changes (default: summary)
        #[arg(long)]
        detailed: bool,
    },
    
    /// (Update existing)
    Prune {
        // ... existing args ...
        /// Preview what would be deleted (don't delete)
        #[arg(long)]
        preview: bool,
    },
}
```

**Dispatch in `run()`:**
```rust
fn dispatch(cli: &Cli) -> Result<()> {
    // ... existing setup (repos, adapters, services) ...
    
    match &cli.command {
        // ... existing arms ...
        
        Command::Status { source_dir, target_dir, detailed, json } => {
            let source_repo = /* BtrfsCliAdapter as SubvolumeRepository */;
            let target_repo = /* separate BtrfsCliAdapter for target */;
            let clock = &SystemClock;
            let journal = &/* FileJournal or NullJournal */;
            
            let service = StatusService::new(source_repo, target_repo, clock, journal);
            let report = service.report(source_dir, target_dir)?;
            
            if *json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_status_report(&report);
            }
            Ok(())
        }
        
        Command::Diff { snapshot1, snapshot2, detailed } => {
            let snap1 = parse_endpoint(snapshot1)?; // local or ssh://
            let snap2 = parse_endpoint(snapshot2)?;
            
            let diff = DiffService::diff(&snap1, &snap2)?;
            print_diff(&diff, *detailed);
            Ok(())
        }
        
        Command::Prune { .. } => {
            // ... existing, but add:
            if cli.preview {
                println!("{}", service.preview(&snapshots, &backups, policy)?);
                return Ok(());  // don't delete
            }
            // ... existing delete logic ...
        }
    }
}
```

### 4.2 — Application layer

**New files:**
- `crates/application/src/status.rs` — `StatusService`, `StatusReport` (struct)
- `crates/application/src/diff.rs` — `DiffService`, `DiffSummary` (struct)

**Reused:**
- `crates/application/src/ports.rs` — no new ports needed
- `crates/application/src/prune.rs` — extend `PruneService::preview()`

### 4.3 — Adapter layer

**Minimal changes:**
- `crates/adapters/src/btrfs_cli.rs` — extend `SubvolumeRepository` queries if needed (e.g., `get_subvolume` by path for diff, not just `list`).
- No new adapters for status or diff (both are pure queries).

### 4.4 — Domain layer

**No changes.** Status/diff are views over existing data; domain stays log-free and pure.

---

## §5 — SOLID & clean-code compliance checklist

### S — Single Responsibility
- ✅ `StatusService` → compute & format status; formatting details → CLI/adapter
- ✅ `DiffService` → compute delta; output → CLI/adapter
- ✅ `PruneService::preview()` → return formatted text; caller decides what to print

### O — Open/Closed
- ✅ New features extend via new services/commands; don't modify existing logic
- ✅ New CLI subcommands don't touch dispatch arms for existing commands

### L — Liskov Substitution
- ✅ No new trait implementations that break existing contracts

### I — Interface Segregation
- ✅ No new omnibus traits; reuse focused `SubvolumeRepository`, `ClockPort`, `JournalPort`

### D — Dependency Inversion
- ✅ Services depend on port abstractions, not concrete adapters
- ✅ CLI composition root wires adapters → services

### Clean Code Practices
- ✅ No `unwrap`/`expect` outside tests
- ✅ `#[must_use]` on query functions
- ✅ `///` docs on all public items
- ✅ No magic numbers — named `const` for retention limits, space thresholds
- ✅ Boundary parsers (e.g., endpoint parsing) reject malformed input (parse, don't validate)
- ✅ TDD: failing test → green → refactor for each feature

### Testing Strategy
- **Unit:** mock repos, clock, journal; test computation in isolation
- **Integration:** real loopback fixtures; test end-to-end with real btrfs
- **Property-based:** retention policy computation (if applicable)
- **No e2e at sandbox boundary:** journal reads, snapshot diffs work locally

---

## §6 — Validation & acceptability criteria

### Status view
- ✅ Computes health from journal + metadata (no side effects)
- ✅ Warns on known issues (orphaned snapshots, old backups)
- ✅ JSON output is parseable
- ✅ Works over SSH (reads remote metadata via existing adapters)

### Snapshot diff
- ✅ Computes delta accurately (matches manual `btrfs send --check-parent`)
- ✅ Works for snapshots on same source or different sources
- ✅ Handles SSH endpoints
- ✅ Output is human-readable + machine-parseable

### Retention preview
- ✅ Output matches what `prune --yes` would actually delete
- ✅ Formatting is clear (preserve vs. delete, reasons)
- ✅ `--preview` flag suppresses deletion (dryrun mode)

---

## §7 — Phasing & timeline

**Phase 1 (week 1):** Spec and unit tests (TDD)
- Write failing tests for `StatusService`, `DiffService`, `PruneService::preview()`
- Implement to green

**Phase 2 (week 2):** Integration and CLI
- Wire services into CLI dispatch
- Add new commands (`status`, `diff`, update `prune --preview`)
- Integration tests with loopback fixtures

**Phase 3 (week 3):** Polish & documentation
- Output formatting, error messages
- Update CLAUDE.md with new commands
- Man page entries

**Phase 4 (release):** Tag v1.1, ship

---

## §8 — Known risks & mitigations

| Risk | Mitigation |
|------|-----------|
| Journal port absent or empty (no audit trail) | `StatusService` gracefully handles missing journal; shows "no audit history" |
| Diff computation on large snapshots is slow | Start with metadata-only diff; add streaming option later if needed |
| CLI argument parsing for endpoints (local/ssh) is error-prone | Reuse existing `parse_endpoint()` from adapters; boundary parser rejects malformed |
| Retention logic drift between `schedule()` and `preview()` | Both use same `Schedule` struct & policy; unit tests verify consistency |

---

## §9 — Deferred / future work

**v2.x candidates** (not in scope for v1.x):
- Backup-set file (needs more design, not blocking v1.x)
- Native encryption (needs infrastructure; designed in `08 §3`)
- TUI snapshot browser (separate product decision)
- Restorability check (`scrub`-aware verification)

---

## Appendix: Code structure sketch

```
crates/application/src/
├── status.rs          [NEW: StatusService, StatusReport struct]
├── diff.rs            [NEW: DiffService, DiffSummary struct]
├── prune.rs           [MODIFY: add preview() method]
└── ports.rs           [NO CHANGE: reuse existing ports]

crates/adapters/src/
└── btrfs_cli.rs       [MINIMAL: extend queries if needed for diff]

crates/cli/src/
├── cli.rs             [MODIFY: add Status/Diff commands, update Prune]
└── output.rs          [NEW (optional): shared formatting for status/diff/prune]

documentation/
└── 10-v1x-plan.md    [THIS FILE]
```

---

## Signoff

**This plan:**
- ✅ Stays true to hexagonal architecture (new read-only services, no domain changes)
- ✅ Follows SOLID principles (single responsibility, dependency inversion)
- ✅ Adheres to clean-code practices (no unwrap, docs, type safety)
- ✅ Integrates cleanly with existing ports/adapters/services
- ✅ Validates in-sandbox (no new infra needed)
- ✅ Aligns with v1.x roadmap (observability + visibility)

**Ready to implement:** Yes, pending review feedback on prioritization or design changes.
