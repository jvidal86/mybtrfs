# 06 — Differential Conformance Testing: btrbk as Reference Oracle

**Status:** design-only spec. The harness is **not** implemented; the `mybtrfs` CLI is still a
stub, so the full binary-vs-binary tier cannot run until Phase 1+ lands. One tier (the
retention-scheduler diff) is buildable against the already-implemented `domain::retention`.

**Scope:** defines a test that runs the original **btrbk** (the reference oracle) and **mybtrfs**
(the subject) over the same small btrfs setup and asserts they reach **equivalent observable
state**, modulo a whitelisted set of deliberate divergences.

**Relationship to the other docs:**
- Complements `05-e2e-test-spec.md` (black-box *specification* testing against hand-written
  expected outcomes). This spec instead **derives the expected outcome from the oracle at
  runtime** and reuses the same `two_filesystems` loopback fixture + helpers.
- Provides a second, independent line of evidence for the fail-safe invariants in
  `02-architecture-v2.md` §6 (see Traceability below).

---

## Name & classification (the SE term for this test)

This is **differential testing** — a.k.a. **back-to-back testing** — where two independent
implementations are fed the same input and their outputs compared. Because one side is a
trusted, known-good program (the original **btrbk**), it serves as a **reference oracle** (the
"golden"/pseudo-oracle), so the technique is more fully **Differential Conformance Testing
against a Reference Oracle**: we assert mybtrfs *conforms to* btrbk's observable behavior,
modulo a whitelisted set of deliberate divergences.

---

## Test shape (the whole idea in one picture)

**One scenario → two invocations → compare resulting filesystem state.**

```
  scenario (the single source of truth)
  ├─ logical command:   run | snapshot | resume | prune
  ├─ test data/folders: identical loopback fixtures (POOL, DRIVE), seeded the same
  ├─ clock instant T + TZ
  └─ knobs:             retention policy, source mutations
                 │
     ┌───────────┴────────────┐         (translated, because the two tools
     ▼                        ▼           take input differently)
  SUBJECT                  ORACLE
  mybtrfs <cmd> --flags    btrbk -c <generated.conf> <cmd>
  injected clock = T       TZ=… faketime 'T' …
     │                        │
     ▼                        ▼
  run on fixture_b         run on identical fixture_a   (separate clones, so neither
     │                        │                          pollutes the other's _N/parents)
     └──────────┬─────────────┘
                ▼
   COMPARE OUTPUTS = resulting btrfs state (not console text):
     per produced subvolume, read via a neutral `btrfs subvolume show` probe —
       • name           basename.<ts>[_N]        → exact match
       • readonly / received_uuid / parent_uuid  → set-vs-empty match
       • content_hash(subvol)                    → exact match
     + exit-code class  + retention survivor set → exact match
   NORMALIZE away the deliberate diffs (snapshot-dir & <host>/ layout, literal UUIDs).
   WHITELIST the deliberate divergences (drive-detect, restore, duplicate-UUID).
```

Key points this encodes:
- **"A user CLI command as input"** — yes, but it can't be the *same string* for both: the
  scenario is translated into mybtrfs CLI flags **and** a generated btrbk `.conf` (btrbk has no
  equivalent CLI; it is config-file-driven).
- **"Compare outputs"** — for a backup tool the meaningful output is the **filesystem state**,
  read through an impl-independent probe so the comparator never trusts either tool about
  itself. Console stdout is only a secondary, normalized check.

---

## Feasibility verdict

**Yes, and it is high-value** — split into two tiers by cost/prerequisites:

| Tier | What it diffs | Needs root/loopback? | Needs mybtrfs CLI? | Buildable when |
|------|---------------|----------------------|--------------------|----------------|
| **T1 — Scheduler diff** | Retention *survivor/prune sets* | No | No (uses `domain::retention` directly) | **Now** |
| **T2 — Full cycle diff** | snapshot→send/recv→prune on real btrfs | Yes | Yes (`run`/`snapshot`/`prune`/`resume`) | After Phase 1–3 |

T1 is the cheapest, most-deterministic, most-shared logic (btrbk's `sub schedule` 4541–4660 ↔
`domain::retention`), so it is the recommended **first** implementation target. T2 is the
headline "side-by-side backup/restore" test.

---

## Harness architecture

Three replaceable pieces behind one comparator, reusing the e2e fixture in
`05-e2e-test-spec.md` §1.

```
                 ┌───────────────── Fixture (loopback btrfs) ─────────────────┐
                 │  two_filesystems:  POOL=/mnt/pool   DRIVE=/mnt/drive        │
                 │  sparse → mkfs.btrfs → loop-mount; RAII teardown + leakcheck│
                 └───────────────┬───────────────────────────┬────────────────┘
   ┌──────────────────┐          │                           │       ┌──────────────────┐
   │  Oracle driver   │   faketime+TZ                  injected clock │  Subject driver  │
   │  (btrbk)         │──────────┤                           ├────────│  (mybtrfs)       │
   │  -c <gen-config> │          ▼                           ▼        │  CLI flags       │
   └──────────────────┘   run on a CLONE of the fixture, independently └──────────────────┘
            │                                                                   │
            ▼  state probe (impl-independent: `btrfs subvolume show` + content_hash)
        ┌───────────────────────────  Comparator  ───────────────────────────────┐
        │ normalize names/dirs → assert equal {props, hashes, survivor sets, rc}  │
        └────────────────────────────────────────────────────────────────────────┘
```

### A. Fixture (shared, from the e2e spec)
- Reuse the `two_filesystems` design verbatim (`05-e2e-test-spec.md` §1): `POOL` btrfs at
  `/mnt/pool` (top-level, subvolid=5), `DRIVE` btrfs at `/mnt/drive`; RAII teardown that
  unmounts, detaches loop devices, deletes images even on failure, leak-checked.
- **Critical:** the two tools must **not** run on the same filesystem state, or the first run
  pollutes the second (it would see the other's snapshots when computing `_N`, parents, and
  survivors). Each side runs against an **independent identical clone**: two separately-built
  images seeded by the same `make_subvol(path, dataset)` recipe (same content → identical
  `content_hash`), mounted at distinct paths (`/mnt/pool_a`+`/mnt/drive_a` for btrbk,
  `…_b` for mybtrfs).
- Helpers to reuse exactly as the e2e spec names them: `make_subvol`, `mutate`, `content_hash`,
  `subvol(path)` (reads exists/readonly/received_uuid/parent_uuid/uuid).

### B. Oracle driver (btrbk)
- Invoke `../btrbk/btrbk/btrbk -c <generated.conf> -q <command>` by **absolute path** (not on
  PATH). One generated config per scenario; pin `snapshot_dir`, `timestamp_format`, and
  `*_preserve*`. Use `--override KEY=VALUE` (`btrbk:399`) to force options identically.
- **Determinism:** wrap every btrbk call in `TZ=<fixed> faketime '<instant>' …` — btrbk reads
  wall-clock once at startup (`btrbk:5212`) and has no `--now`. One run ⇒ one timestamp shared
  by all its snapshots (`btrbk:6675`).
- **State capture (machine-readable):**
  - `btrbk -c … --format raw list all|backups|snapshots` and `… --format raw stats`
    (raw branch `btrbk:4970`; column sets `%table_formats` `btrbk:173-282`).
  - the `transaction_log <file>` table (`tlog` format, `btrbk:250`) for the action sequence.
  - For T1 without disk: `btrbk -c … -n -S --format raw <action>` emits the **schedule** rows
    (`topic action … hod dow min h d w m y`, `btrbk:234-238`) — pure scheduler decisions.

### C. Subject driver (mybtrfs)
- Invoke the compiled `mybtrfs` binary with the flags from `01-phases-design-v2.md`
  (`run`/`snapshot`/`resume`/`prune`/`restore`; `-n`, `--yes`, `--snapshot-preserve[-min]`,
  `--target-preserve`, `--incremental`).
- **Determinism:** drive the injected `ClockPort` to the same instant faketime gave btrbk
  (`02-architecture-v2.md` already injects time+timezone; a test-only entry point — e.g.
  `--clock-at <RFC3339>` or env — must be added when the CLI is built; see Prerequisites).
- **State capture:** use the same **impl-independent probe** (`btrfs subvolume show` +
  `content_hash`) directly on the fixture, so the comparator never depends on mybtrfs being
  correct about itself. mybtrfs's stdout/exit-code are *additionally* asserted, not the source
  of truth.

### D. Comparator
- Probes both sides through the **same** reader: for every subvolume under each snapshot dir /
  target, collect `{name, readonly, received_uuid(set?), parent_uuid(set?), uuid,
  content_hash}`. (btrbk's `lsbtr`/`fs_list` raw, `btrbk:272`, is a convenient cross-check, but
  the canonical probe is plain `btrfs subvolume show`.)
- Apply **normalization** (below), then assert set-equality on the normalized records plus
  equality of: exit-code class, retention survivor set, and `content_hash` chains.

---

## Test isolation & sandboxing (no damage to the dev disk)

This suite is uniquely dangerous: it runs **as root** and issues `mkfs.btrfs`, `losetup`,
`mount`, `btrfs subvolume delete`, and `send | receive`. A harness bug must not be able to
reach the developer's real disk. Containment is therefore a first-class requirement, not an
afterthought.

### Threat model — three ways the host disk could die
1. **A destructive op targeting a host path** — a delete/mkfs whose path resolves outside the
   intended sandbox (empty variable → `/`, a `..`, or a mountpoint colliding with a real
   `/mnt/...`).
2. **mkfs on a real block device** — `/dev/sdX` instead of the intended loop image.
3. **Leaked kernel resources** — loop devices and mounts are *host-global*; a crashed test can
   leave `/dev/loopN` and stale mounts behind.

Loopback images already isolate **data** (operations act on a file-backed loop device, not a
partition). Sandboxing adds the missing ring: isolating **paths, mounts, and ideally the
kernel** so that even a root-privileged harness bug cannot escape.

### Three rings of defense (use all three)

**Ring 1 — harness guards (always on, in code):**
- **Sandbox gate:** the destructive suite `panic!`s at startup unless `MYBTRFS_TEST_SANDBOX=1`
  is set (only the container/VM sets it) → it can never run on a bare dev box by accident.
- **Containment assertion:** everything lives under one `mktemp -d` root; every mkfs/mount/
  delete target is asserted to be a descendant of that root *before* the command is issued;
  absolute paths outside it and flag-like paths are rejected. (Reuse the `BtrfsCliAdapter`
  contract: absolute, non-flag, no-shell paths — `02-architecture-v2.md`.)
- `losetup --find --show` (never a hardcoded `/dev/loop0`); mkfs only ever names an **image
  file**, never `/dev/sd*`.
- RAII teardown + leak check (already in `05`/`06`): unmount, `losetup -d`, delete images even
  on panic; assert no loop device / mount leaked.

**Ring 2 — container (isolates paths + mounts):**
- A `Containerfile` with `btrfs-progs`, `perl`, `libfaketime`, Rust, and the btrbk checkout;
  entrypoint runs the gated suite.
- **No host bind-mounts** of the working dir or host devices — build loop images on the
  container's ephemeral layer or a `tmpfs`. If the host disk isn't visible, a stray `rm -rf`
  has nothing to hit.
- Its own **mount namespace** means leaked mounts die with the container, and `/mnt/pool_a`
  cannot collide with a real host mount.

**Ring 3 — disposable VM (isolates the kernel — strongest, the choice for T2):**
- **Hypervisor: QEMU with KVM** acceleration — real root + real kernel + *virtual* block
  devices, so a bug that runs `mkfs /dev/vda` destroys only a virtual disk. This is the only
  ring that neutralizes "wrong `/dev/sdX`" and host loop-device leakage, because the devices and
  the kernel are virtualized.
- **Driver: `virtme-ng` (`vng`)** — the tool the btrfs/`fstests` community uses; boots a
  throwaway guest in ~1–2 s, exposes the working dir over virtiofs, runs one command, and
  returns its exit code to the harness. The btrfs loop images are built on a **scratch virtio
  disk** (or tmpfs) *inside* the guest, so even the guest rootfs stays clean.
- **Local vs CI:** local iteration reuses the dev's host kernel via `vng` (near-instant boot);
  CI pins a **kernel + minimal rootfs** under QEMU/KVM (or `vng` with a pinned kernel) for
  reproducibility independent of the host kernel.
- Alternatives considered and rejected: `vmtest` (fine equivalent), Vagrant/VirtualBox (heavy,
  stateful), Firecracker (awkward for arbitrary loop/mount setup), `systemd-nspawn --ephemeral`
  (not a real VM — shares the host kernel, so loop/mkfs accidents still hit host resources).

### The gotcha that drives the design: btrfs is not userns-mountable
A rootless/unprivileged container **cannot** `mount -t btrfs` a loop image — btrfs does not set
`FS_USERNS_MOUNT`, so mounting it needs real `CAP_SYS_ADMIN`. "Rootless container" therefore
does **not** solve this on its own. The two honest options for the destructive tier:

| Approach | Can mount btrfs? | Blast radius if the harness has a bug |
|----------|------------------|----------------------------------------|
| Rootless container alone | ❌ no | n/a (can't run T2) |
| **Privileged container** (`--cap-add SYS_ADMIN` + loop access, **no host bind-mounts**) | ✅ | shares host kernel + host loop devices; safe *only because* the host fs is not mounted in |
| **Disposable VM** | ✅ | virtual disk only — host untouched ✅ |

### Per-tier recommendation
- **T1 (scheduler diff)** — little/no btrfs; runs in a **plain container**, or the Perl
  `sub schedule` shim at zero privilege. Low risk.
- **T2 (full cycle: send/receive/delete)** — run in a **disposable VM** in CI (bulletproof);
  offer a **privileged-container** path for fast local iteration, with Ring-1 guards
  **mandatory**.
- **Ring 1 is on regardless** — it is the line of defense that survives a misconfigured
  container.

---

## Comparison surface & normalization (semantic equivalence)

**Assert EQUAL, after normalization:**
1. **Subvolume identity name** → `basename.<timestamp>[_N]`. Timestamp formats (`short`
   `YYYYMMDD`, `long` `YYYYMMDDThhmm`, `long-iso` `YYYYMMDDThhmmss±hhmm`) and the `_N` collision
   counter are byte-identical by design (`btrbk:4816-4851`, naming/collision `:6674-6691` ↔
   `domain::naming`). Compare exactly.
2. **btrfs properties:** `readonly`, `received_uuid` set-vs-empty, `parent_uuid` set-vs-empty.
   - full backup ⇒ readonly + received_uuid set + parent_uuid empty;
   - incremental ⇒ + parent_uuid set;
   - restore target ⇒ read-write + received_uuid empty.
   (own-UUID values differ per run — compare **set/empty + chain shape**, never literal UUIDs.)
3. **Data:** `content_hash(subvol)` equal across source ↔ backup ↔ restore on both tools.
4. **Retention survivor set:** the exact set of names that survive prune, and the exact
   complement deleted (re-runnable, order-independent).
5. **Exit-code class** (success / usage / partial-abort) — see Risks (codes not yet finalized).

**Normalize away (the deliberate, expected differences):**
- **Snapshot dir prefix:** btrbk `snapshot_dir` (config-relative) vs mybtrfs default
  `.mybtrfs_snapshots` → strip the dir, compare leaf names.
- **Target layout:** btrbk's config-driven target dir vs mybtrfs's `<target>/<hostname>/` →
  strip the host/dir prefix, compare leaf names.
- **Literal UUID/ctime/transid values** and per-run wall-time fields.

---

## Whitelisted divergences (compare NOTHING here — by design)

Per `01-phases-design-v2.md` lines 26–40, these are intentional and excluded from the diff
(asserted to *differ*, not match):
- **Drive auto-detection / interactive `list-drives`** — mybtrfs-only interface (btrbk is
  config-driven). Diff the backup *result*, not the selection UX.
- **`restore`** — btrbk has **no** restore command. mybtrfs restore (Phase 4) is validated
  against the e2e spec's own assertions (`05 §6`, esp. the received_uuid-poison trap P4-02),
  **not** against btrbk.
- **Duplicate-UUID handling** — btrbk only *warns*; mybtrfs **hard-refuses**. The cloned-disk
  scenario (CC-03) asserts mybtrfs exits non-zero where btrbk proceeds — an inverted
  expectation.

---

## Scenario matrix to diff

Each maps to existing e2e IDs (`05-e2e-test-spec.md`) and btrbk source anchors. Both tools are
driven from one scenario definition (clock instant, source mutations, retention flags).

| # | Scenario | Tier | e2e id | What must match |
|---|----------|------|--------|-----------------|
| D1 | Full backup of one subvolume | T2 | P1-02/03 | snapshot+backup leaf names & timestamp; ro+recv-set+parent-empty; content_hash |
| D2 | Same-minute re-run | T2 | P1-09 | `_N=1` collision suffix on second; first untouched |
| D3 | Incremental after `mutate` | T2 | P2-01/02 | new backup parent_uuid set + chains to prior; delta ≪ full; content_hash |
| D4 | No-common-parent → full fallback | T2 | P2-03 | both fall back to full (parent_uuid empty) |
| D5 | Foreign (non-scheme) subvols present | T2 | P3-10 | both leave them untouched |
| **D6** | **Retention survivor set**, daily/weekly/monthly tiers | **T1** | P3-02/03 | identical survivors + complement; re-runnable; TZ-independent |
| D7 | `preserve_min latest` + keep-all default | T1 | P3-01/.. | identical survivors |
| D8 | Force-preserve anchors (just-created; latest common pair; parent-of-preserved) | T1+T2 | P3-05/06/08 | both preserve the same anchors (`btrbk` FORCE_PRESERVE `:6706`) |
| D9 | Dry-run mutates nothing | T1/T2 | CC-01/P3-09 | both no-op; plans align after normalization |

D6/D7 and the schedule part of D8 are the **buildable-first** core (no root; `domain::retention`
vs `btrbk -S --format raw`).

---

## Determinism approach

- **btrbk:** `TZ=<fixed> faketime '<instant>' btrbk -c <conf> -q <cmd>`. Advance the instant
  between scenario steps to place snapshots at chosen times. Pin `TZ` (short/long are
  local-time; long-iso embeds the offset — `conf.5:586-591`).
- **mybtrfs:** set the injected clock to the **same** instant + TZ.
- **`_N` counter** is deterministic given identical on-disk names → equal if D1/D2 ran
  identically.
- **Independent fixtures per tool** so neither side sees the other's snapshots.

---

## Prerequisites in mybtrfs before T2 can run (gating)

This spec is design-only because T2 needs, at minimum:
- a working `clap` command tree + dispatch (`crates/cli`) for `run`/`snapshot`/`prune`/`resume`;
- the application ports + `BtrfsCliAdapter` (`crates/adapters/src/btrfs_cli.rs`) implementing
  snapshot / send|receive / verify / delete;
- a **test-only injected-clock entry point** (e.g. `--clock-at <RFC3339>` or env) so the
  subject is as deterministic as faketime makes the oracle;
- dev-dependencies not yet present: a process runner + tempfile + (optionally) `nix`/`libc`
  for loop devices; a `faketime` binary in the test/CI image.

T1 needs none of the above — only a small harness calling `domain::retention` and parsing
`btrbk -S --format raw`.

---

## Implementation layout (when coding resumes)

- This document (the spec) — done.
- `crates/cli/tests/diff_btrbk.rs` (or a dedicated `crates/diff-tests/`) — the harness, behind
  the same root/loopback feature/env gate as `crates/cli/tests/e2e.rs`, with the Ring-1
  `MYBTRFS_TEST_SANDBOX` gate + containment assertions at startup.
- Shared fixture/probe helpers co-located with the e2e harness, reusing the `05 §1` helper
  names so both suites share one fixture layer.
- **Sandbox (see Test isolation & sandboxing):**
  - `quality/ci/Containerfile` — image with `btrfs-progs`, `perl`, `libfaketime`, Rust, and the
    btrbk checkout; sets `MYBTRFS_TEST_SANDBOX=1`; runs the gated suite with no host
    bind-mounts.
  - `quality/ci/run-e2e-vm.sh` — boots a disposable QEMU/KVM guest via `virtme-ng` and runs the
    destructive (T2) suite inside it, e.g.
    `vng --run . --memory 2G --disk scratch.img -- env MYBTRFS_TEST_SANDBOX=1 cargo test --features e2e`.
  - `make test-e2e` — wrapper that dispatches to the container (T1 / fast local) or the VM (T2 /
    CI).

### btrbk config / command crib

```
# generated test.conf (per scenario)
timestamp_format  long
snapshot_dir      btrbk_snapshots          # pre-create it; btrbk won't (conf.5:124)
snapshot_preserve_min  all                  # keep-all default; override per retention scenario
target_preserve_min    all
volume /mnt/pool_a
  target    /mnt/drive_a/host
  subvolume home
```
```bash
# T1 scheduler oracle (no disk):
TZ=UTC faketime '2026-06-22 15:31:00' \
  ../btrbk/btrbk/btrbk -c test.conf -n -S --format raw run
# T2 real run + machine-readable state:
TZ=UTC faketime '2026-06-22 15:31:00' ../btrbk/btrbk/btrbk -c test.conf -q run
../btrbk/btrbk/btrbk -c test.conf --format raw list all
# impl-independent probe (both tools): btrfs subvolume show <each subvol> + content_hash
```

---

## Implementation notes — confirmed from btrbk source (T1 buildability)

Confirmed by reading `../btrbk/btrbk/btrbk` (line refs below); these tighten the
spec above into something directly implementable.

- **`btrbk --format raw` line shape** (`print_formatted` raw branch, btrbk:4970-4977):
  each row is one self-describing line
  `format="<table_key>" col0='v0' col1='v1' …`. The `format=` value uses *double*
  quotes; every column value is *single*-quoted via `quoteshell` (btrbk:761-764):
  internal `'` → `'\''`, and empty/undef → `''` (the table/long `-` empty-cell char
  does **not** apply to raw). A parser must handle both quote styles and the
  `'\''` escape.
- **Schedule raw columns** (btrbk:237): `topic action url host port path hod dow min h d w m y`.
  Beware: `h`/`d`/`w`/`m`/`y` are the hourly/daily/weekly/monthly/yearly **counts**,
  `hod` is hour-of-day, `dow` is day-of-week (e.g. `'sunday'`), and `min` is the
  preserve-min **string** (`'all'`/`'latest'`/`'no'`/`'2d'`). Schedule rows are only
  emitted by the run/clean/archive path and are gated behind `unless($quiet)`
  (btrbk:6965) — so use `-S` **without** `-q`; `list`/`stats` never emit them.
- **GOTCHA — `-n -S` still touches btrfs.** `-n` only skips *destructive* commands
  (`run_cmd` btrbk:921-924); snapshot discovery (`btrfs subvolume list -a -c -u -q -R`,
  btrbk:1236), `subvolume show`, and `filesystem show` are `non_destructive => 1` and
  run even under dry-run. There is **no** hook to feed snapshot names into the
  scheduler — it always reads live subvolumes. So the T1 oracle needs **either** a
  real btrfs mount **or** a **PATH-shimmed fake `btrfs`** returning canned
  `subvolume list`/`show`/`filesystem show` output. The exact set of subcommands the
  shim must satisfy (and the formats btrbk's parser expects from each) must be pinned
  against a live btrbk — this is the main remaining unknown and why T1 is *not* a
  zero-cost, sandbox-only build.
- **Determinism:** wrap btrbk in `TZ=<fixed> faketime '<instant>' …` (`libfaketime`);
  btrbk reads wall-clock once at startup and has no `--now`. `--override KEY=VALUE`
  (btrbk:5476-5486) forces config options from the CLI.
- **mybtrfs side is library-direct and sandbox-verifiable:** the subject's survivor
  set comes from `mybtrfs_domain::retention::{schedule, RetentionPolicy}` (no CLI, no
  btrfs), so that half can be unit-tested today; only the btrbk-oracle half needs the
  shim + faketime + root-ish setup.

A runner scaffold lives at `scripts/run-diff-btrbk.sh` (gated by
`MYBTRFS_TEST_SANDBOX`, points at the local btrbk checkout, and is where the
fake-`btrfs` shim is wired). The Rust harness itself is deferred until it can be
developed against a live btrbk (writing the shim blind risks silently mismatching
btrbk's expected `subvolume list` format).

---

## Verification (how we'll know the harness itself is right)

1. **Oracle self-consistency:** run btrbk twice on identical clones under the same faketime
   instant → comparator reports zero diff (proves the probe + normalization are stable and
   UUID/ctime noise is excluded).
2. **Sensitivity (negative control):** perturb one side (skip a `mutate`, change a `*_preserve`
   tier) → comparator must flag exactly that difference (proves it isn't trivially passing).
3. **T1 against the implemented scheduler:** feed a known subvolume-set + policy to both
   `domain::retention` and `btrbk -S --format raw`; survivor sets must match for D6/D7. First
   concrete milestone.
4. **T2 smoke (post-CLI):** D1 full-backup green end-to-end on loopback under root,
   leak-checked teardown confirmed (no stray loop device/mount).
5. **Sandbox self-test (Ring 1):** with `MYBTRFS_TEST_SANDBOX` unset the suite must refuse to
   run; a fault-injected target path *outside* the `mktemp -d` root must trip the containment
   assertion *before* any mkfs/mount/delete is issued (proves the guards fail closed).

---

## Traceability — invariants this method additionally exercises

Beyond `05`'s hand-written assertions, the oracle independently corroborates these
`02-architecture-v2.md` §6 invariants (the survivor-set/property comparison must agree with
btrbk's own proven behavior):

| §6 invariant | Differential evidence |
|--------------|------------------------|
| §6-1 transfer verified, not by exit code | D1/D3 property probe (ro+received_uuid+parent) matches btrbk's post-receive check (`btrbk:1546-1573`) |
| §6-3 just-created preserved | D8 vs btrbk FORCE_PRESERVE (`:6706`) |
| §6-4 latest common pair preserved | D8 survivor set |
| §6-6 parent of preserved backup kept | D8 dependency-closure survivor set |
| §6-9 re-runs non-destructive | D2 `_N` collision behavior |
| §6-11 deterministic scheduling | D6/D7 survivor set equal under fixed clock+TZ |

Excluded by design (whitelisted divergences): §6-7 restore (btrbk has none) and §6-10
duplicate-UUID (btrbk warns, mybtrfs refuses) — covered only by `05`, not by the oracle.

---

## Risks / open issues

- **mybtrfs exit codes not finalized** (`05` CC-08 "proposed to mirror btrbk"). Compare *code
  class* (success/usage/partial-abort), not raw integers, until pinned.
- **`faketime` and btrfs `creation_time`:** btrfs may stamp subvol ctime from the kernel, not
  the faked libc clock; never compare ctimes — rely on the name timestamp and on set/empty
  property checks.
- **btrbk needs `snapshot_dir` to pre-exist** (`conf.5:124`) while mybtrfs auto-creates its
  default — a normalized, not asserted, difference (already whitelisted).
- **Send-stream byte comparison is out of scope** (UUID/ctime-laden); D3 compares delta *size*
  ≪ full and parent-chain shape, not stream bytes.
- **CI needs `/dev/kvm`** (nested virtualization) for the T2 VM. GitHub-hosted Ubuntu runners
  expose KVM; on a runner without it, QEMU falls back to TCG software emulation — correct but
  much slower. Verify KVM availability when wiring the pipeline.

---

## Implementation status (T1)

`crates/cli/tests/diff_btrbk_schedule.rs` implements the **verifiable halves** of the
T1 scheduler diff as always-on unit tests:

- **Oracle parser** — `btrbk_schedule_survivors` reads btrbk's `--format raw`
  schedule output (space-separated `key=value` rows; columns
  `topic action url host port path hod dow min h d w m y` per btrbk source), and
  returns the leaf names with `action=preserve`. Assumes space-free fields (true
  for scheme leaf names).
- **Subject side** — `mybtrfs_survivors` parses each name's timestamp and runs
  `mybtrfs_domain::retention::schedule`, returning the preserved leaf names.

The **live oracle diff** (`oracle_schedule_diff_against_btrbk`) is `#[ignore]`d.
Why it cannot run in the current sandbox, and what it needs:

- **No `faketime`** here, and btrbk has no injectable clock. The test sidesteps
  this by placing snapshots at whole-day offsets from one shared `now`, so a sub-
  second skew between btrbk's wall clock and the injected domain clock never flips
  a day/week/month decision — no `faketime` required for coarse-tier scenarios.
- **btrbk schedules from live btrfs** and its `MAIN:` block runs on load (it is not
  cleanly `require`-able to call `sub schedule` in isolation, and it ships no test
  harness). So a controlled snapshot set means **root + loopback btrfs** (as in the
  `e2e` gate), plus a real btrbk via the `MYBTRFS_BTRBK` env var.

Run it where those exist with:
`sudo -E env "PATH=$PATH" MYBTRFS_BTRBK=/abs/btrbk cargo test --test diff_btrbk_schedule -- --ignored`.
