# mybtrfs

[![CI](https://img.shields.io/github/actions/workflow/status/jvidal86/mybtrfs/ci.yml?label=CI)](https://github.com/jvidal86/mybtrfs/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/codecov/c/github/jvidal86/mybtrfs)](https://codecov.io/gh/jvidal86/mybtrfs)
[![Tests](https://img.shields.io/github/actions/workflow/status/jvidal86/mybtrfs/ci.yml?label=tests)](https://github.com/jvidal86/mybtrfs/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/jvidal86/mybtrfs)](LICENSE)
[![Built with Claude Code](https://img.shields.io/badge/built%20with-Claude%20Code-d97757?logo=anthropic&logoColor=white)](https://claude.ai/code)

`mybtrfs` is a backup tool for **btrfs subvolumes**, written in **Rust** — a
reimagining of [btrbk](https://github.com/digint/btrbk): atomic read-only
snapshots, incremental `btrfs send`/`receive`, and a flexible retention policy.

## Status

The crate is a **scaffold**: the hexagonal module structure is laid out but not
yet implemented. Development follows **Spec-Driven / Test-Driven Development** —
the specs below are the source of truth and are written before the code.

## Documentation

- `documentation/01-phases-design-v2.md` — functional design (Phases 1–4) + the
  decided CLI surface.
- `documentation/02-architecture-v2.md` — hexagonal architecture, sequence
  diagrams, and the fail-safe invariants.
- `documentation/04-coding-guidelines.md` — Rust + clean-code rules to follow.
- `documentation/05-e2e-test-spec.md` — the end-to-end behavioral spec (SDD/TDD).
- `documentation/03-review-and-corrections.md` — the documentation review trail.

See also `CLAUDE.md` for an orientation aimed at AI coding assistants.

## License

GPL-3.0-or-later (matching the original btrbk license).
