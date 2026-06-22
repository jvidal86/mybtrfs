# RULES.md — binding coding & architecture rules for `mybtrfs`

Auto-loaded into Claude Code via the `@RULES.md` import in `CLAUDE.md`. These are
the **enforced** rules in checklist form; the full rationale lives in
`documentation/04-coding-guidelines.md` (coding) and `documentation/02-architecture-v2.md`
(architecture). If a rule here ever disagrees with those docs, the docs win — fix
this file. Behavioral correctness invariants (send/receive verification,
delete-safety anchors, restore read-only trap) are **not** repeated here; they live
in `CLAUDE.md` § "Invariants any implementation MUST preserve" and `02` §6.

## Architecture rules

1. **Dependency rule (compiler-enforced):** `cli → adapters → application → domain`.
   Dependencies point inward; an inner crate must never depend on an outer one.
   `mybtrfs-domain` has zero internal deps (verify: `cargo tree -p mybtrfs-domain`).
2. **Domain is pure.** No I/O, no `std::process`, no filesystem, no ambient clock in
   `crates/domain`. The riskiest logic (parent resolution, retention, safety, model)
   lives here and is unit-tested with zero I/O.
3. **Ports live in `application`** (`application/src/ports.rs`). Use cases orchestrate
   through port traits only; they never name a concrete adapter.
4. **All I/O is behind a port.** Dangerous operations (delete, make-writable,
   send/receive transfer) are reachable only through narrow ports whose contracts
   embed the safety checks — fail-safe is architectural, not conventional.
5. **The CLI binary is the only composition root.** Concrete adapters are constructed
   and wired there, nowhere else.
6. **Determinism by injection.** The clock *and* timezone are injected (no ambient
   `now()`); `short`/`long` timestamps are local-time, so the tz is an input.
7. **Stateless.** Re-derive all truth from live btrfs metadata each run — never a side
   database.

## Coding standards

8. **`#[must_use]`** on every public pure/query fn & method (clippy `must_use_candidate`).
9. **`///` docs on every public item AND every public struct field** — no exceptions.
10. **`# Errors`** on every `Result`-returning public fn; **`# Panics`** wherever a fn
    can panic. Prefer removing the panic (`len().saturating_sub(1)`, not `len() - 1`);
    where a panic is genuinely impossible, document the invariant inline (`// SAFETY:`).
11. **`pub(crate)` by default.** Only the genuinely-public API is `pub`.
12. **No magic numbers / no cryptic names.** A named `const` for every literal that
    carries meaning; descriptive identifiers (single letters only as trivial loop or
    closure variables).
13. **No `unwrap`/`expect` outside `#[cfg(test)]`** (clippy `unwrap_used`/`expect_used`
    warn). If truly unavoidable in production code, `#[allow(...)]` locally with a
    one-line justification.
14. **No raw exit-code literals** — route through the central `exit_code` table.
15. **`unsafe_code = "forbid"`** (workspace lint); edition 2024.
16. **Boundary parsers reject malformed input.** At an adapter boundary, map a tool's
    sentinel/absence to `None`/default, but treat a *present-but-malformed* field as a
    parse **error** — never silently coerce it (e.g. a garbled `received_uuid` → `None`
    forges the garbled-receive signal invariant #1 depends on). "Parse, don't validate"
    (`04` §0.3).

## Gates before any commit

17. `cargo test --workspace` green.
18. `cargo clippy --workspace --all-targets` clean **with the pedantic subset**
    (`missing_panics_doc`, `must_use_candidate`, `needless_pass_by_value`,
    `redundant_closure_for_method_calls`, `explicit_iter_loop`).
19. `cargo fmt --check` clean.
20. Builds on the pinned **MSRV** (`rust-version` in `[workspace.package]`); bump it
    deliberately, never incidentally.
21. **TDD:** the increment started with a failing test (red → green → refactor).
