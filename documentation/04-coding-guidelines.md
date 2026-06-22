# mybtrfs — Coding Guidelines (Rust + Clean Code)

> Rules to follow when implementing mybtrfs. These complement
> `01-phases-design-v2.md` (what to build) and `02-architecture-v2.md` (how it's
> structured). Where this conflicts with personal style, **this wins**; where it
> conflicts with a correctness/safety invariant from `02` §6, **the invariant
> wins**.

---

## 0. Principles that override style

1. **Correctness and safety first.** The fail-safe invariants in `02` §6
   (verify-after-receive, the delete-safety anchors, the restore received-uuid
   rule) are non-negotiable and take priority over brevity or cleverness.
2. **Make illegal states unrepresentable.** Encode invariants in the type system
   so the compiler rejects misuse, instead of checking at runtime.
3. **Parse, don't validate.** Convert untrusted input into a precise type once,
   at the boundary; the rest of the code receives already-valid values.
4. **Keep the domain pure.** Core logic has no I/O and no `btrfs`/OS knowledge;
   side effects happen only through ports.
5. **Determinism.** No wall-clock, randomness, or environment reads inside
   logic — inject them (`ClockPort`) so behavior is reproducible and testable.

---

## 1. Error handling

- **Domain & library code:** typed errors via `thiserror`; one error enum per
  module boundary, variants carry context (paths, command, stderr).
- **Application boundary (CLI/`main`):** `anyhow` with `.context("…")` to attach
  human-readable context as errors bubble up.
- **No `unwrap()` / `expect()` in non-test code** except where a failure is
  provably impossible — and then `expect("why it cannot fail")` documents the
  reason. Deny it with clippy (§9).
- **Never use panics for control flow.** Panics are for bugs/contract violations
  (`debug_assert!`), not for expected failures (unreachable target, no parent,
  missing dir) — those return `Result`.
- Propagate with `?`; don't match-and-rewrap unless adding real context.
- Map all error categories to the **central exit-code table** in one place
  (success / generic / usage / partial-abort), not ad-hoc `std::process::exit`.

---

## 2. Types & domain modeling

- **Newtypes over primitives.** Wrap `Uuid`, subvolume paths, timestamps, sizes —
  no primitive obsession, no stringly-typing. `fn parent(p: &SubvolPath)` not
  `fn parent(p: &str)`.
- **Enums for states and choices.** Replace boolean parameters with intent-named
  enums — `snapshot(Readonly::Yes)` reads better and resists call-site mistakes
  than `snapshot(true)`.
- **`Option`, not sentinels.** Mirror btrfs's `"-"` (and an absent field) as
  `Option<Uuid>` at the parse boundary; logic never compares against `"-"`. But a
  *present-but-malformed* field is a parse **error**, never silently coerced to
  `None`/default — silently dropping a garbled `received_uuid` would forge the
  "garbled receive" signal that invariant #1 depends on.
- **Builders** for constructors with many optional fields; avoid long positional
  argument lists.
- Derive deliberately (`Debug` almost always; `Clone`/`PartialEq` when needed).
  Don't derive `Copy` on non-trivial types. Keep `Debug` output non-secret.

---

## 3. Functions & control flow

- Small, single-purpose functions. If a function does two things, split it.
- **Guard clauses / early returns**; keep nesting shallow (avoid the rightward
  drift of nested `if`/`match`).
- **Command–Query Separation:** a function either returns data *or* causes an
  effect, not both surprisingly.
- Prefer **iterators and combinators** (`map`/`filter`/`collect`) over manual
  index loops; they're clearer and bounds-safe.
- **Immutability by default:** `let` over `let mut`; introduce mutation only when
  it simplifies real logic.
- Exhaustive `match`; avoid a catch-all `_` arm when you want the compiler to
  force you to handle a new enum variant later.

---

## 4. API & module design (uphold the hexagon)

- **Minimal visibility:** `pub(crate)` by default; only the genuinely-public API
  is `pub`. A small surface is easier to keep correct.
- **Accept borrowed/generic inputs, return owned outputs:** `&str`,
  `impl AsRef<Path>`, `&[T]` in parameters; return `String`/`PathBuf`/owned
  structs.
- **Ports are small focused traits** (ISP). Inject them as `&dyn Port` (or a
  generic) chosen at the composition root; use cases never name a concrete
  adapter.
- **Enforce the dependency rule:** domain modules must not `use std::process`,
  `std::fs`, `std::net`, or any adapter module. If the core needs an effect, it's
  a missing port. A module-level `//!` comment should state each module's
  responsibility and (for the core) that it is I/O-free.

---

## 5. External commands & I/O (project-critical)

- **One runner for all `btrfs` calls.** Build an **argv array**; never assemble a
  shell command string and never invoke a shell — this removes injection and
  quoting hazards entirely.
- **Validate every path** before passing it out: absolute, and not flag-like
  (reject a leading `-`).
- Capture stdout/stderr; **classify `ERROR:` lines centrally**; surface stderr
  verbatim on failure.
- **Never trust exit codes for side effects** — verification is part of the port
  contract (e.g. `TransferPort` re-reads the received subvolume). See `02` §6.
- Use **RAII / `Drop` guards** for resource cleanup (loop devices and temp mounts
  in integration tests, partial-transfer cleanup), so cleanup runs even on the
  error path.

---

## 6. Documentation & comments

- `///` doc comments on every public item; include `# Errors` (what makes it
  return `Err`), and `# Panics` / `# Safety` where applicable.
- **Comments explain *why*, not *what*.** The code says what; reserve prose for
  rationale, invariants, and links to the btrbk behavior being mirrored
  (cite the doc/line, e.g. "mirrors btrbk `_is_correlated`").
- Keep comments truthful and current — a stale comment is worse than none.

---

## 7. Testing

- **Pure logic** (timestamp parsing, retention scheduler, parent ranking): unit
  tests in `#[cfg(test)] mod tests`, no I/O. These should be exhaustive and fast.
- **Orchestrators:** test against **fakes** (`FakeBtrfs`, `FixedClock`,
  `ScriptedPrompter`) — the hexagon exists precisely to enable this.
- **Integration:** real `btrfs` over **loopback images**, gated behind a
  feature/env flag (root required) so plain `cargo test` runs for non-root devs.
- Prefer **table-driven** tests; consider **property tests** for the scheduler
  and the name parser (round-trip, boundary dates).
- Test names describe the behavior under test; each test asserts one idea.
- Tests are deterministic: **clock *and timezone* are injected** (the retention
  scheduler interprets `short`/`long` timestamps in local time, so timezone is a
  real input, not ambient state), no network, no reliance on the host TZ.

---

## 8. Concurrency & resources

- Avoid shared mutable state; pass data and ownership instead of sharing.
- The send→receive pipe must be handled to **avoid deadlock** (drain the reader;
  wait on both children) — document the ordering in the transfer adapter.
- No `unsafe`. Crate root carries `#![forbid(unsafe_code)]`; none of the planned
  work needs it.

---

## 9. Tooling & gates (must pass before a change is "done")

- **`cargo fmt`** — rustfmt is authoritative; no manual formatting debates.
- **`cargo clippy --all-targets --all-features -- -D warnings`** — clean. Enable
  at least `clippy::unwrap_used` / `clippy::expect_used` (warn) and a curated
  subset of `clippy::pedantic`; allow specific lints locally with a justification
  comment, never blanket.
- **`cargo test`** green (unit + doc tests) before commit; integration suite run
  when touching adapters.
- `#![forbid(unsafe_code)]` at the crate root; pin the Rust **edition** and a
  declared **MSRV**.
- Optional but encouraged: `cargo deny` / `cargo audit` for the dependency set.

---

## 10. Dependencies

- **Prefer `std`.** Add a crate only for real value; justify each one.
- Agreed baseline set: `clap` (CLI), `thiserror` (domain errors), `anyhow` (app
  boundary), `chrono` (calendar math), `serde`/`serde_json` (lsblk parsing),
  `regex` (name parsing). New dependencies need a one-line rationale.
- Keep adapters as the only place third-party I/O crates appear; the domain core
  depends on as little as possible (ideally only `std` + small pure crates).

---

## 11. Clean-code general (kept brief — the above is the project-specific part)

- **No magic numbers or repeated literals.** Give meaningful values a named
  `const` (e.g. `HOURS_PER_DAY`, `MONTHS_PER_YEAR`, `UUID_GROUP_LENGTHS`, the
  strftime patterns) and name function parameters/locals for intent — avoid bare
  literals and single-letter names in non-trivial code (idiomatic short binders
  like a loop `idx` or a `|s| s.id` closure are fine).
- **DRY**, but prefer a little duplication over the *wrong* abstraction; extract
  only when the shared shape is real and stable. **YAGNI** — build for the four
  phases, not imagined futures.
- **Intention-revealing, consistent names.** Reuse the project vocabulary exactly
  as defined in `01` (*source*, *subvolume*, *snapshot*, *backup*, *target*); do
  not coin synonyms.
- **Composition over inheritance** (natural in Rust): small types and traits
  combined, not deep hierarchies.
- Refactor when a unit grows a second responsibility; keep files and functions a
  size you can hold in your head.
- Don't optimize prematurely — measure first; clarity beats micro-tuning in all
  the non-hot paths (which is nearly all of this tool).
- Match the surrounding code's idioms and density when editing existing files.
