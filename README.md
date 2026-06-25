# mybtrfs

[![CI](https://img.shields.io/github/actions/workflow/status/jvidal86/mybtrfs/ci.yml?label=CI)](https://github.com/jvidal86/mybtrfs/actions/workflows/ci.yml)
[![Coverage](https://img.shields.io/codecov/c/github/jvidal86/mybtrfs)](https://codecov.io/gh/jvidal86/mybtrfs)
[![Tests](https://img.shields.io/github/actions/workflow/status/jvidal86/mybtrfs/ci.yml?label=tests)](https://github.com/jvidal86/mybtrfs/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/jvidal86/mybtrfs)](LICENSE)
[![Built with Claude Code](https://img.shields.io/badge/built%20with-Claude%20Code-d97757?logo=anthropic&logoColor=white)](https://claude.ai/code)

A backup tool for **btrfs subvolumes**, written in Rust — a ground-up reimagining of [btrbk](https://github.com/digint/btrbk).

Atomic read-only snapshots, incremental `btrfs send`/`receive`, flexible retention, and automated restore — all from a single binary, with no config file required.

## Features

- **Snapshot + backup in one command** — creates a read-only snapshot, sends it incrementally to a local or remote target, verifies the result, and prunes old copies per a retention policy.
- **Incremental transfers** — UUID-based parent resolution; falls back to a full send automatically (`--incremental yes|strict|no`).
- **Retention scheduling** — Grandfather-Father-Son (GFS) tiers: `24h 7d 4w 12m` keeps hourly for a day, daily for a week, and so on.
- **Safe restore** — transfers a backup back from a remote target, then creates a writable snapshot via `btrfs subvolume snapshot` (never `btrfs property set ro=false`, which silently poisons future incrementals).
- **SSH remotes** — any command works against `ssh://[user@]host[:port]/path` endpoints.
- **Pre/post snapshot hooks** — quiesce a database around the snapshot window; the post-hook always runs even if a subsequent step fails.
- **Backup-set files** — TOML file with multiple `[[backup]]` entries for multi-subvolume automation, each with its own retention and hooks.
- **Fail-safe by design** — every `btrfs send | receive` is verified (read-only, correct `received_uuid`, correct `parent_uuid`); a garbled result is deleted before reporting an error.
- **Stateless** — re-derives all subvolume relationships from live btrfs metadata on every run; no side database.
- **Exit code taxonomy** — `0` success / `1` failure / `2` usage error / `3` lock held / `4` needs root — cron and scripts can tell a privilege problem from a backup failure.

## Requirements

- Linux with btrfs-progs (`btrfs` CLI on `$PATH`)
- Root privileges (btrfs operations require root)
- Rust 1.89+ to build from source

## Installation

```bash
cargo install --path crates/cli
```

Or build and copy manually:

```bash
cargo build --release
sudo cp target/release/mybtrfs /usr/local/bin/
```

Man pages are in `man/`. Install with:

```bash
sudo cp man/*.1 /usr/local/share/man/man1/
```

## Quick start

```bash
# Full backup — snapshot /home, send to /mnt/backup, pick retention defaults
sudo mybtrfs run /home /home/.snapshots home /mnt/backup

# Incremental backup to an SSH remote
sudo mybtrfs run /home /home/.snapshots home ssh://user@nas.local/backups/myhost

# Snapshot only (no transfer)
sudo mybtrfs snapshot /home /home/.snapshots home

# List snapshots and backups
sudo mybtrfs list /home/.snapshots /mnt/backup

# Prune old copies (dry run first)
sudo mybtrfs prune --dry-run /home/.snapshots /mnt/backup home

# Restore a backup to /mnt/restore
sudo mybtrfs restore /mnt/backup/home.20260625 /mnt/restore
```

## Retention policy

Retention uses GFS (Grandfather-Father-Son) scheduling. Specify tiers with time-unit suffixes:

```bash
sudo mybtrfs run /home /home/.snapshots home /mnt/backup \
  --snapshot-preserve "24h 7d 4w 12m" \
  --target-preserve   "7d 4w 12m 3y"
```

`--snapshot-preserve-min` / `--target-preserve-min` set a floor: `all` (keep everything, the default), `latest` (always keep the newest), or a duration like `3d`.

## Backup-set files

For multi-subvolume backups, use a TOML backup-set file instead of positional arguments:

```toml
[[backup]]
source       = "/home"
snapshot_dir = "/home/.snapshots"
basename     = "home"
target_dir   = "/mnt/backup/home"
incremental  = "yes"
snapshot_preserve_daily = 7
target_preserve_daily   = 30
pre_snapshot_hook  = "systemctl stop mydb && sync"
post_snapshot_hook = "systemctl start mydb"

[[backup]]
source       = "/var/lib/data"
snapshot_dir = "/var/lib/data/.snapshots"
basename     = "data"
target_dir   = "ssh://nas.local/backups/data"
target_preserve_daily   = 90
target_preserve_monthly = 12
```

```bash
sudo mybtrfs run --set /etc/mybtrfs.backup-set.toml --yes --quiet
```

## Scheduling

### systemd timer

```bash
sudo cp contrib/systemd/mybtrfs-backup.service /etc/systemd/system/
sudo cp contrib/systemd/mybtrfs-backup.timer   /etc/systemd/system/
sudo systemctl enable --now mybtrfs-backup.timer
```

Edit the `.service` file to set your source, target, and retention flags. The timer runs daily by default and catches up on missed runs at next boot.

### cron

```bash
sudo cp contrib/cron/mybtrfs.crontab /etc/cron.d/mybtrfs
```

```cron
# m  h  dom mon dow  user  command
  17 2  *   *   *    root  mybtrfs run /home /home/.snapshots home /mnt/backup \
                             --yes --lock /run/mybtrfs.lock --journal /var/log/mybtrfs.journal
```

## Commands

| Command | Description |
|---|---|
| `mybtrfs run` | Full workflow: snapshot → transfer → verify → prune |
| `mybtrfs snapshot` | Create a read-only snapshot only |
| `mybtrfs resume` | Re-send the latest unbacked snapshot (retry after a failure) |
| `mybtrfs prune` | Apply retention policy and delete expired copies |
| `mybtrfs restore` | Restore a backup to a writable subvolume |
| `mybtrfs list` | List snapshots and backups with pairing |
| `mybtrfs stats` | Storage stats (referenced bytes, shared data) |
| `mybtrfs status` | Health summary: latest snapshot/backup ages and lag |
| `mybtrfs diff` | Show what changed between two snapshots |
| `mybtrfs list-drives` | Discover mounted btrfs filesystems |
| `mybtrfs list-subvolumes` | List all subvolumes on a filesystem |

See `man mybtrfs` or `mybtrfs <command> --help` for full option reference.

## Global flags

| Flag | Description |
|---|---|
| `--yes` | Non-interactive; skip all confirmation prompts (safe for cron) |
| `--quiet` / `-q` | Suppress progress output; errors still go to stderr |
| `--log-file PATH` | Write diagnostic logs to a file (default: `/var/log/mybtrfs.log`) |
| `--journal PATH` | Append a timestamped audit line per invocation |
| `--lock PATH` | Path to run-lock file; a held lock exits with code 3 |

## Architecture

Hexagonal layering enforced by Rust's crate boundaries (`cli → adapters → application → domain`). The riskiest logic — parent UUID resolution, GFS retention scheduling, and the delete-safety policy — lives in the pure `domain` crate with zero I/O and is unit-tested independently of btrfs.

```
crates/
  domain/       # Pure core: model, naming, parent resolution, retention, safety
  application/  # Use cases + ports (traits); depends only on domain
  adapters/     # btrfs CLI, SSH, local FS, lock, journal, prompter, progress
  cli/          # Composition root: argument parsing, wiring, exit codes
```

~300 unit and integration tests; `clippy` (pedantic subset) and `fmt` clean; MSRV 1.89.

## Local test environment

A persistent loopback btrfs environment is available for interactive testing:

```bash
sudo contrib/setup-local-backup-env.sh setup    # create source + backup loop images
sudo contrib/setup-local-backup-env.sh teardown # clean up
```

See `contrib/README.md` for details.

## Differences from btrbk

| | btrbk | mybtrfs |
|---|---|---|
| Configuration | Config file required | CLI-first; backup-set file optional |
| Drive selection | Manual config | Auto-detects mounted btrfs filesystems |
| Restore | Manual process | Automated, including remote transfer-back |
| Duplicate UUID guard | Warning only | Hard refusal |
| Needs-root exit code | Generic failure | Exit code 4 (distinct, scriptable) |
| Remote targets | Via config | `ssh://` URL on any command |

## License

GPL-3.0-or-later — matching the license of the original [btrbk](https://github.com/digint/btrbk).
