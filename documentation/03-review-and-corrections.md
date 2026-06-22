# mybtrfs — Documentation Review & Corrections

> A correctness review of `01-phases-design.md` and `02-architecture.md`,
> checking that the descriptions are accurate, the logic sound and robust, and
> **parallel to the original btrbk Perl program**. Each finding cites the
> verified location in the btrbk source
> (`btrbk` script in the `btrbk/` reference repo).
>
> **Overall verdict:** the two documents are largely accurate and faithful to
> btrbk. The items below are precision fixes and explicit notes about where
> mybtrfs intentionally *diverges from* or *extends* btrbk — recorded so the
> "parallel to the original" claim stays honest. **No code involved.**

---

## 1. Verified correct (faithful to btrbk — no change required)

| Topic | btrbk source | Status |
|-------|--------------|--------|
| Retention cascade: first/oldest-in-period wins; hourly→daily→weekly→monthly→yearly roll-up; "first weekly of month/year" | `sub schedule` 4664–4699 | ✓ matches |
| Boundaries: `preserve_hour_of_day` (day), `preserve_day_of_week` (week), first dow of period (month/year); `short` forces 00:00 boundary | 4578–4614 | ✓ matches |
| Time zones: `short`/`long` interpreted local, `long-iso` absolute | `localtime(...)` 4587 | ✓ matches |
| Correlation predicate (both readonly + the three UUID equalities; roots excluded) | `_is_correlated` 2585–2589 | ✓ matches |
| Garbled detection (`!readonly && received_uuid == "-"`) + cleanup via `subvolume delete --commit-each` | 1573, 1591 | ✓ matches |
| Skip snapshot deletion if any target aborted/unreachable | 6930–6936 | ✓ matches |
| Never delete a parent of a preserved backup (dependency closure) | 4701–4709 | ✓ matches |
| Parent-resolution strategies (parent_uuid-related **and** timestamp-matched candidates) survive a pruned chain | `get_best_parent` region | ✓ matches |

---

## 2. Accuracy fixes (to be precisely parallel to btrbk)

### A1 — "Always keep the newest" is imprecise
- **Where:** `01` Phase 3 robustness, anchor #1; `02` §6 fail-safe table, row 3.
- **btrbk reality:** there is no blanket "newest is always kept" on a standalone
  prune. The actual guarantees are:
  1. the snapshot/backup **created in the current run** is force-preserved
     (`'preserve forced: created just now'`, line 6706);
  2. the **latest common snapshot/backup pair** is force-preserved, on both
     `run` and `prune` (lines 6884–6897).
  Outside those, the newest is preserved only if `preserve_min` (`all`/`latest`)
  or a retention tier covers it.
- **Correction:** reword anchor #1 to "the just-created snapshot/backup is always
  preserved (during a backup run)"; the latest-common-pair anchor remains the
  prune-time guarantee. Update table row 3 to match.

### A2 — send/receive verification omits the `parent_uuid` plausibility check
- **Where:** `01` Phase 1 error control; `02` §6 table row 1.
- **btrbk reality (1552–1569):** after receive, in addition to *readonly* and
  *received_uuid set*, btrbk checks the parent relationship:
  - **full** backup ⇒ received subvolume must have **no** `parent_uuid`;
  - **incremental** ⇒ `parent_uuid` **must** be set.
- **Correction:** add this parent_uuid plausibility check to the verify
  description in both docs.

### A3 — strict incremental mode is under-described
- **Where:** `01` Phase 2 (behavior + error control).
- **btrbk reality (3699–3704):** strict mode does **two** things — it never falls
  back to a full backup, **and** it restricts the parent to one with an actual
  `parent_uuid` relationship (rejecting parents matched only by timestamp
  correlation). The doc currently states only the first.
- **Correction:** add the second clause.

---

## 3. Divergences from btrbk (intentional — state them explicitly)

These are places where mybtrfs deliberately *adds to* or *improves on* btrbk.
They are not errors, but the docs should name them so "parallel to the original"
is not misread as "identical to."

### D1 — Restore is a mybtrfs addition
btrbk has **no** restore command; restoring is a manual procedure documented in
btrbk's README ("Restoring Backups") and FAQ ("Received UUID"). mybtrfs Phase 4
*automates that documented procedure* — faithful to the steps and guard rails,
but new functionality. Note this in `01` Phase 4 and `02`.

### D2 — Drive auto-detection and CLI-first are mybtrfs additions
btrbk is **config-file-driven** with explicit paths and has no drive discovery.
mybtrfs's interactive drive auto-detection and CLI-first model are intentional
divergences (chosen for the early phases; config deferred to Phase 5+). Add a
short "Relationship to btrbk" note in `01`'s foundational decisions.

### D3 — Hard-refuse on duplicate UUIDs is a mybtrfs improvement
btrbk treats subvolume UUIDs as globally unique and *warns* (e.g. "Assuming same
filesystem") but does not hard-refuse on duplicates. mybtrfs's plan to **refuse**
is a deliberate robustness improvement. Reword `01` Phase 2 / `02` §6 row 10 so
this reads as an improvement, not as btrbk behavior.

---

## 4. Quality & diagram notes

### Q1 — Snapshot consistency wording
`01` Phase 1 calls a snapshot "atomic, consistent." Qualify as
**filesystem-atomic / crash-consistent**; application-level consistency requires
the application to flush. btrbk does **not** quiesce or `sync` before
snapshotting, so this caveat is itself parallel to btrbk.

### Q2 — Mermaid robustness in `02`
- Replace the multi-target labeled dotted edge
  `BtrfsCliAdapter -. implements .-> REPO & SNAP & XFER & DEL` with separate
  edges (more reliable rendering across Mermaid versions).
- Declare the `SNAP` participant explicitly in sequence diagram 5.2 (it is
  currently auto-created on first use).
- The restore "move `D` → `D.broken`" step (5.4) is a filesystem rename; it is
  more accurately a `FilesystemPort` / `DeletePort.rename` responsibility than
  `SnapshotPort`.

### Q3 — Safety-anchor monotonicity in `02`
Clarify (§5/§6) that the safety anchors only ever move items from *delete* →
*preserve*, never the reverse — mirroring how btrbk seeds `FORCE_PRESERVE`
*before* the scheduler runs, so a tier rule can never un-preserve an anchored
subvolume.

---

## 5. Suggested follow-up

The fixes above are localized edits to the two existing documents. Recommended
order: apply the **accuracy fixes (§2)** first (they affect correctness), then
the **divergence notes (§3)**, then the **quality/diagram polish (§4)**. After
editing, re-read `01` and `02` together to confirm no contradiction, and
sanity-check each Mermaid block (balanced subgraphs, declared participants, no
multi-target labeled edges).
