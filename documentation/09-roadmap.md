# 09 — Roadmap & competitive positioning (post–Phase-5 §2)

Where mybtrfs goes after the four delivery phases and the SSH feature (Phase 5 §2)
landed. This is a **strategic / prioritization** document, not a functional spec:
it maps an external "btrbk-replacement" feature roadmap against what mybtrfs has
**already shipped**, settles the one philosophical conflict that roadmap forces
(declarative config vs. CLI-first identity), and recommends a concrete next-version
slice with each item tied to its **port** and to whether it is **validatable in the
sandbox** or needs real infrastructure (the same gating that governs the e2e/SSH
suites).

The numbered functional specs remain the source of truth for *how* a feature is
built (`01`/`02`/`05`/`08`); this doc decides *what* and *in what order*, and
**why**. When an item here graduates to implementation, write its design into the
appropriate `08`-style section (or a new spec) first — spec-before-code, the
project's habit.

Guiding principle (inherited from the source roadmap, and a perfect fit for the
hexagon): **CLI-first, TUI-optional. Every interactive action has a scriptable
equivalent. A view is a layer over the engine, never a gate in front of it.** In
hexagonal terms: every "view" feature is a new **driving front end** or a new
read-only **query** over re-derived state; the risky core (parent resolution,
retention, safety, model) is never touched.

---

## §1 — Verdict

The external roadmap is sound, and the useful finding is that **mybtrfs is already
through ~85% of its v1 "must-haves."** For this project the roadmap is therefore
**not** a from-scratch next version — it is a map showing that the *engine* is done
and the remaining work lives in **visibility and ergonomics**, not plumbing.

Of the 7 v1 table-stakes, **6 are implemented and validated**; the only gap is the
one mybtrfs **deliberately** rejected (declarative config), which is the single
real decision the roadmap forces (§3).

---

## §2 — Current state vs. the roadmap's v1 (table-stakes)

| v1 must-have | mybtrfs status | Where |
| --- | --- | --- |
| Incremental `send`/`receive` | ✅ done | Phase 2; `domain::parent`, `TransferPort` |
| Retention / GFS pruning | ✅ done | `domain::retention`, `RetentionService` |
| Dry-run / preview | ✅ done | invariant #8 — every mutating command |
| SSH push **and** pull | ✅ done | Phase 5 §2 — backup *and* restore-from-remote, validated e2e |
| Single static binary | ✅ done | Rust, no runtime (a headline edge over btrbk's Perl) |
| Clear exit codes + logging | ✅ done | exit taxonomy incl. code-4 (ID-6), `log` facade |
| **Declarative single-file config** | ❌ **by design** | intentional divergence (`CLAUDE.md`); see §3 |

**Implication:** the substantive roadmap content for mybtrfs is its **v1.x** (the
"differentiators") and **v2** (the "delight") tiers — almost all of which are
*views over state we already re-derive*, not new dangerous operations.

---

## §3 — The one conflict to settle: declarative config

**The tension.** The roadmap ranks "declarative single-file config" as a v1
table-stake ("btrbk's most-loved trait; win here = win switchers"). mybtrfs's
identity (`CLAUDE.md` § "Intentional divergences") is the opposite: **CLI-first**,
because btrbk being config-file-driven is precisely what mybtrfs set out to avoid.
`08-phase5-design.md` §4 already analyzed this exact fork.

**Decision (carried from `08` §4, reaffirmed).** Do **not** build a btrbk-style
config DSL. If multi-subvolume-from-cron is a real need, ship a thin **backup-set
file**: a list of CLI-invocation arg blocks parsed by a *new driving adapter* that
loops the existing use cases once per entry. The domain/application never learn
about a config; it is sugar at the composition root, exactly as `EnvironmentFile`
is for the systemd unit today. This closes the roadmap's "config" gap **without
becoming btrbk** — the roadmap's "config" and mybtrfs's "backup-set sugar" are the
same user-facing win under two names.

**Why this is also the pragmatic pick:** the parser is **pure and fully
unit-testable in-sandbox** (no I/O) — the rare Phase-5 item that needs no real
infrastructure to validate.

---

## §4 — Why the rest fits the hexagon cleanly

The roadmap's "a view is a layer over the engine" principle **is** the dependency
rule. Each visibility/ergonomics feature drops in as a driving front end or a
read-only query over re-derived btrfs metadata, leaving `domain` untouched:

- **Status / observability view** — the headline anti-btrbk wedge. mybtrfs already
  has `list`/`stats` (inventory). The missing piece is **last-run health**
  (success/failure, schedule, space). The **`--journal` port is the natural
  backing** for it *without breaking statelessness* — the journal is an explicit,
  opt-in audit log, not a side "truth" database; re-derivation of btrfs truth is
  unchanged.
- **Retention preview** — *largely already shipped* as dry-run `prune`, which
  prints exactly which snapshots a policy would delete. Needs presentation polish,
  not new logic.
- **Snapshot diff** ("what changed between two snapshots") — cheap on btrfs, a
  genuine 🎯 differentiator (rare even in restic/borg), and computable from
  metadata; unit-testable.
- **Pre/post hooks** — quiesce a DB / run scripts around snapshots. A composition-
  root concern (spawn around the use-case call); does not touch the domain.
- **Failure notifications** — first-classed via the scheduler contrib
  (`OnFailure=` systemd, mail/webhook wrapper); partly already reachable.
- **TUI / snapshot browser** — clean architectural fit (another driving front end
  over the same use cases), **but a whole new axis**: a new crate and a new
  dependency surface. Decide it consciously; do not half-build it (the roadmap's
  own "Later / optional — different product, different person" caveat).

---

## §5 — Validatability (the honest-scoping line)

Same discipline as the e2e/SSH/oracle suites: some items need infrastructure the
CI sandbox lacks (no root for some, no GPG, no second host). Surface that up front
rather than writing code that can only *pretend* to pass.

| Tier | Items | Validatable now? |
| --- | --- | --- |
| **Build & TDD in-sandbox now** | backup-set parser · snapshot diff · retention-preview polish · status-view formatting | **Yes** (pure / re-derived state) |
| **Needs real infra — design-first** | native encryption (GPG/openssl, designed in `08 §3`) · restorability / `scrub`-aware check · multi-host orchestration · live transfer progress | No — real-host / CI-with-VM, gated like the SSH suite |
| **Different product — decide, don't drift** | full GUI · cloud / object-storage targets | n/a (scope decision, not a build) |

---

## §6 — Recommended next version (v1.x = "make it visible")

A coherent, on-brand, mostly-sandbox-validatable slice:

1. **Backup-set file** — settles §3 as sugar, not a DSL; pure parser, TDD-able
   today. (Ship only if multi-subvolume cron is a real need; otherwise start at 2.)
2. **Status view** backed by the journal — the headline wedge btrbk is weakest at.
3. **Snapshot diff** — cheap, differentiating, unit-testable.
4. **Retention preview** polish — ~90% there via dry-run `prune`.

**Defer to a true v2, design-first behind existing ports:** **native encryption**
(highest-value of the deferred set; `08 §3` already sketches the `RawTargetPort` +
sidecar approach) and the **restorability / verification check**.

**Hold as a deliberate, separate yes/no:** the **TUI** and the **full GUI** — the
"different person, different product" call the source roadmap itself flags.

**The one decision that is genuinely the maintainer's**, gating item 1 vs. item 2
as the opener: *does multi-subvolume-from-cron matter enough to ship the backup-set
file first?* If yes → backup-set parser is the natural first increment (pure,
TDD-able now). If no → the status view is the better opener.

---

## §7 — Source roadmap (preserved verbatim, adapted to mybtrfs)

The external feature roadmap and competitive comparison this analysis responds to,
kept here so it is not lost. `mybtrfs (target)` columns show the **target** state
across v1 → v2, not necessarily today's state (see §2/§5 for what is actually
shipped). Legend: ✅ yes · ⚠️ partial / awkward · ❌ no · 🎯 differentiator.

### v1 — Must-have (the credible btrbk replacement)

| Feature | Why it's v1 |
| --- | --- |
| Incremental `send`/`receive` | The whole reason to be btrfs-native. Must be fast and flawless. |
| Declarative single-file config | btrbk's most-loved trait. Win here = win switchers. *(mybtrfs: as backup-set sugar — §3.)* |
| Retention / GFS pruning | Keep N hourly/daily/weekly/monthly/yearly. Core daily-use feature. |
| Dry-run / preview | High-stakes domain; users won't trust a backup tool without it. |
| SSH push **and** pull | Pull-based is prized for security (compromised client can't wipe backups). |
| Single static binary | No Perl/Ruby/Python runtime. A headline advantage over btrbk. |
| Clear exit codes + logging | Required for cron/systemd automation, which is how this crowd runs everything. |

### v1.x — High-leverage differentiators

| Feature | Why it matters |
| --- | --- |
| Status / observability view | Last-run success/failure, schedule, space used. btrbk is weak here. |
| Snapshot browser (TUI) | The demoable "oh, that's nicer" moment. Great for launch screen-recordings. |
| Retention **preview** | Show exactly which snapshots a policy would delete before it runs. |
| Failure notifications | Email/webhook on failure. Basic in btrbk; make it first-class. |
| Pre/post hooks | Quiesce a DB, flush, run scripts around snapshots. Power users depend on these. |

### v2 — Delight features

| Feature | Why it matters |
| --- | --- |
| Native encrypted destinations | restic/borg's headline advantage; btrbk punts to LUKS. A genuine wedge. |
| Easy single-file restore | "Restore last Tuesday's version of this file" as a one-liner or TUI action. |
| Verification / restorability check | A `scrub`-aware command confirming backups are actually recoverable. Builds trust. |
| Snapshot diff | "What changed between these two snapshots." Cheap on btrfs, rare in tooling. |
| Live transfer progress (TUI) | Visible throughput during long transfers. |
| Multi-host orchestration | Manage many sources/targets from one config + dashboard. |

### Later / optional

- Full GUI — **only** if deliberately targeting the desktop / Synology-refugee
  market. Different product, different person; decide consciously.
- Cloud / object-storage targets (to compete with restic on reach).

### Competitive comparison (target state)

| Feature | mybtrfs (target) | btrbk | restic | borg |
| --- | :---: | :---: | :---: | :---: |
| btrfs-native send/receive | ✅ | ✅ | ❌ | ❌ |
| Block-level incremental | ✅ | ✅ | ✅¹ | ✅¹ |
| Works on any filesystem | ❌² | ❌² | ✅ | ✅ |
| Single static binary | ✅ 🎯 | ❌ (Perl) | ✅ | ⚠️³ |
| Declarative single-file config | ✅ 🎯 | ✅ | ❌ | ❌ |
| GFS retention policies | ✅ | ✅ | ✅ | ✅ |
| Retention **preview** | ✅ 🎯 | ⚠️ | ⚠️ | ⚠️ |
| Dry-run | ✅ | ✅ | ✅ | ✅ |
| SSH push | ✅ | ✅ | ✅ | ✅ |
| SSH pull (server-initiated) | ✅ | ✅ | ⚠️ | ⚠️ |
| Native encryption | ✅ (v2) | ❌⁴ | ✅ | ✅ |
| Deduplication | ⚠️⁵ | ⚠️⁵ | ✅ | ✅ |
| Compression | ✅⁶ | ✅⁶ | ✅ | ✅ |
| Single-file restore | ✅ | ⚠️⁷ | ✅ | ✅ |
| Mount backup as filesystem | ✅⁸ | ✅⁸ | ✅ | ✅ |
| Integrity / restorability check | ✅ (v2) | ⚠️ | ✅ | ✅ |
| Snapshot diff | ✅ 🎯 | ❌ | ❌ | ❌ |
| Status / observability view | ✅ 🎯 | ⚠️ | ❌ | ❌ |
| TUI | ✅ 🎯 | ❌ | ❌ | ❌ |
| Pre/post hooks | ✅ | ✅ | ⚠️⁹ | ⚠️⁹ |
| Failure notifications | ✅ | ⚠️ | ❌ | ❌ |
| Cloud / object-storage targets | later | ❌ | ✅ | ⚠️¹⁰ |

**Notes.** 1. restic/borg do content-defined chunking rather than btrfs `send`;
incremental in a dedup sense, not block-delta. 2. By design — being btrfs-native is
the point. 3. borg's PyInstaller binary bundles a Python runtime, not a true static
binary. 4. btrbk relies on LUKS / encrypted btrfs underneath. 5. On btrfs, dedup is
a filesystem concern (`duperemove`); restic/borg dedup within the repo. 6. Via
btrfs transparent compression (zstd). 7. btrbk's snapshot is browsable but has no
first-class single-file restore. 8. The received snapshot *is* a mountable
subvolume natively. 9. Achievable via wrapper scripts, not a declared config
concept. 10. borg via rclone/append-only; not native.

### One-line pitch this table supports

> **mybtrfs** — btrbk's btrfs-native engine, rewritten in Rust as a single static
> binary, with a cleaner declarative (backup-set) front end, a status view you can
> actually *see*, and native encryption btrbk never had.

Highest-leverage trio for winning btrbk users specifically: **cleaner declarative
config (as backup-set sugar) · visible status/observability · rock-solid retention
with preview** — the exact friction points btrbk users complain about most.
