# 08 — Phase 5+ design (beyond the four delivery phases)

Phases 1–4 are implemented. This document designs the **Phase 5+** features that
`CLAUDE.md` lists as out-of-scope-until-the-four-phases-land: **scheduling**,
**remote source/target over SSH**, **raw & encrypted targets**, and an optional
**config / backup-set** front end. It is **design-only** (the spec, written before
the code — the project's habit), *except* scheduling, which is already shipped in
`contrib/`.

The unifying principle: **none of these change the hexagon.** The domain and the
application use cases already talk only to the ports in
`crates/application/src/ports.rs`; every Phase 5 feature is a new **driven
adapter** (or a new **driving** front end) wired at the CLI composition root. So
the riskiest, best-tested code — parent resolution, retention, the safety policy —
is untouched. Each section below names the port it implements and what real
infrastructure it needs to *validate* (none of which the CI sandbox has: no root,
no user namespaces, no SSH host, no GPG — so these are real-host / CI-with-VM work).

---

## §1 Scheduling — **implemented** (`contrib/`)

mybtrfs is CLI-first and stateless, so "scheduling" is just invoking `mybtrfs run`
from the system scheduler with the unattended flags (`--yes`, `--lock`,
`--journal`). Shipped as drop-ins, no new code:

- `contrib/systemd/mybtrfs-backup.{service,timer}` + `mybtrfs.env.example`,
- `contrib/cron/mybtrfs.crontab`,
- `contrib/README.md` (install + usage).

`RequiresMountsFor=` skips a run when the target drive is absent; the host lock
(`--lock /run/mybtrfs.lock`) serializes overlapping fires (exit 3). btrbk leans on
the same idea (`contrib/systemd`, `contrib/cron` in the reference tree); we keep it
config-free.

---

## §2 Remote source / target over SSH

**Goal.** Back up to (or from) a btrfs filesystem on another host:
`btrfs send … | ssh host "btrfs receive …"`, and read remote metadata via
`ssh host "btrfs subvolume show/list …"`.

**Hexagonal placement.** *No new ports.* Provide SSH-flavoured implementations of
the **existing** `SubvolumeRepository`, `SnapshotPort`, `TransferPort`, and
`DeletePort`, selected per endpoint at the composition root:

- An `SshCommandRunner` (sibling of the local `command` adapter) that prefixes the
  argv with `ssh [-p port] [-i identity] user@host --` and runs the *same* btrfs
  argv the local adapter builds. The `BtrfsCliAdapter` is already factored around a
  command runner + path/mount resolution; the SSH variant reuses its argv
  construction and verification (invariants #1/#2 hold identically — the received
  subvolume is checked over SSH the same way).
- Endpoint addressing mirrors btrbk's `ssh://user@host:port/path`
  (`volume_url`/`target` with an `ssh://` scheme). The driving side parses an
  endpoint into `{local | ssh{user,host,port,identity}}` and picks the adapter.

**Transfer.** `send_receive` becomes `btrfs send -p … | ssh host btrfs receive …`
(or the reverse for a remote *source*). The pipe is local→ssh; both ends are spawned
with argv (no shell), so the no-shell-interpolation rule (RULES §16, `02 §3`) is
preserved — `ssh host --` then the btrfs argv as separate arguments.

**Security.** Reuse btrbk's hardening posture: a dedicated key, and an authorized
`command="…"` / `ssh_filter_btrbk.sh`-style restricted wrapper on the remote so the
key can run only btrfs send/receive (see `../btrbk/btrbk/doc/` and
`contrib/`). Document, do not auto-configure.

**Status: IMPLEMENTED (backup to a remote target).** `crates/adapters/src/ssh.rs`
(`SshEndpoint`/`parse_endpoint` + `SshCommandRunner` + `SshMountTable`) and
`BtrfsCliAdapter::ssh_target`; the CLI accepts `mybtrfs run … ssh://[user@]host[:port]/path`
(`crates/cli/src/cli.rs`). 13 unit tests, and the pattern was **validated live**
against a real host: a btrfs stream piped into `ssh host -- sudo btrfs receive`
produced a readonly subvolume with a `received_uuid` (#1), and remote
`btrfs subvolume show` returns the verification fields.
A full `mybtrfs run … ssh://user@host/path` was **validated end-to-end on
2026-06-23** against a real host: a loopback btrfs source → local snapshot →
`btrfs send | ssh … sudo btrfs receive` produced `/mnt/btrfs-test/data.<ts>`,
verified readonly with a Received UUID. Reproducible via
`contrib/test/mybtrfs-ssh-smoke.sh`.

Remote **pruning** works too: a `RoutingDeletePort` (composition root) routes each
deletion by path — target backups (under the remote dir) delete over ssh, source
snapshots delete locally — so one `RetentionService` prunes both sides across the
two transports and `--target-preserve …` applies to an `ssh://` target.
**Still open:** *restore from* a remote source (the reverse pipe).

---

## §3 Raw & encrypted targets

**Goal.** Targets that are **not** a receiving btrfs filesystem but a *stream on
disk*: `btrfs send … | [compress] | [encrypt] > <name>.btrfs[.gz][.gpg]`, plus the
inverse to restore. Lets backups land on any filesystem / object store.

**Hexagonal placement.** A *new target kind* behind a small port addition, because
the post-conditions differ from a btrfs receive:

- A `RawTargetPort` (or a `TransferPort` variant) whose `send_receive` writes a
  stream file and records its metadata in a **sidecar** (`<name>.info`: parent
  uuid, received-from, timestamp, sizes) — there is no btrfs subvolume on the
  target to query, so the relationship graph for incremental `-p` and for retention
  is rebuilt from sidecars instead of `btrfs subvolume list`. This is the one place
  the stateless "re-derive from btrfs metadata" rule needs a documented exception
  (the stream files *are* the metadata).
- Compression: `gzip|zstd|xz` (btrbk `*_target_compress`). Encryption: `gpg`
  (recipient key) or `openssl enc` (btrbk `raw_target_encrypt`), with split support
  (`raw_target_split`) for size-capped media.
- **Restore** of a raw target reverses the pipe: `[decrypt] | [decompress] | btrfs
  receive` — this composes with Phase 4's transfer-back (it is just a different
  "source stream").

**Invariant note.** #1 (never trust a transfer by exit code) is *weaker* for raw
targets — there is no received subvolume to re-`show`. Mitigate by verifying the
written stream's size/checksum against the sidecar and, on restore, by checking the
received subvolume the usual way.

**Unit-testable now:** the pipeline argv + sidecar parse/format.
**Needs real infra to validate:** GPG keys / openssl, and a real `btrfs send`
stream; out of sandbox.

---

## §4 Optional config / backup-sets — *design tension, decide before building*

**The tension.** "CLI-first (btrbk is config-file-driven)" is an **intentional
divergence** (`CLAUDE.md`). A full btrbk-style config reverses that identity. But
backing up *many* subvolumes to *many* targets from cron is clumsy as raw CLI.

**Proposal (keep the divergence).** Do **not** build a btrbk config language.
Instead add a thin **backup-set file** that is *only* a list of CLI invocations'
arguments — parsed by a new **driving adapter** that loops the *existing* use cases
once per entry. The domain/application never learn about a config; it is sugar at
the composition root, exactly like `EnvironmentFile` is for systemd today.

- Format: minimal and obvious (TOML/`KEY=VALUE` blocks), one block per
  source→target with optional retention — the same fields the CLI already takes.
- It composes with §1: one systemd timer runs `mybtrfs run --set /etc/mybtrfs.sets`
  instead of N units.
- Parsing is **pure and fully unit-testable now** (no I/O) — the one Phase 5 item
  that fits the sandbox, if we accept the design direction.

**Alternative.** Stay strictly CLI-only and let `contrib/` examples show multiple
units. Lighter, fully consistent with the stated identity. **Recommendation:** the
backup-set file (sugar, not a config DSL) if multi-subvolume cron is a real need;
otherwise the alternative.

---

## §5 Validation matrix

| Feature | New code | Unit-testable in sandbox | Needs real infra |
|---------|----------|--------------------------|------------------|
| §1 Scheduling | none (contrib drop-ins) | n/a (config files) | run on a real host |
| §2 SSH | SSH command runner + adapter selection | argv/pipe construction | a 2nd btrfs host (VM/CI) |
| §3 Raw/encrypted | RawTargetPort + pipeline + sidecars | pipeline argv, sidecar parse | GPG/openssl + real send stream |
| §4 Backup-set file | driving-adapter parser | **yes, fully** | none |

**Order if pursued:** §4 parser (pure, testable) → §2 SSH (highest user value) →
§3 raw/encrypted. Each lands behind its port with the domain untouched, gated like
the existing loopback/oracle suites for the parts that need real infrastructure.
