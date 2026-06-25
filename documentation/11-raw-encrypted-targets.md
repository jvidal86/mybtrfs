# 11 — Raw & Encrypted Stream Targets (Phase 5 §3)

**Status:** Ready for implementation (design reviewed, logic errors resolved)
**Branch:** `feature/raw-encrypted-targets`
**GPG test key:** `C8D7DA12` (`mybtrfs-test@localhost`, no passphrase — unattended-safe)

---

## §1 — Goal

mybtrfs currently backs up only to a **btrfs filesystem** (local or SSH remote) via
`btrfs send | btrfs receive`. This adds a second target kind: a **raw stream file**
on any filesystem, optionally compressed and encrypted:

```
btrfs send … | zstd | gpg --symmetric --batch --passphrase-file KEY > name.btrfs.zst.gpg
```

Lets backups land on non-btrfs storage — ext4 drives, NAS, object stores — without
requiring a btrfs filesystem at the target.

**Hexagonal placement:** no domain or application code changes. All new code is
adapters wired at the CLI composition root, exactly as SSH in §2.

---

## §2 — Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Encryption | GPG symmetric (`gpg --symmetric`) | Single secret to manage; no PKI; unattended with `--passphrase-file`; btrbk-compatible |
| Cipher selection | Delegated to `gpg.conf` | mybtrfs stays thin; user controls AES-256 / KDF iterations there |
| Compression | zstd (default), gzip, xz, none | zstd is already the btrfs-level default; best speed/ratio balance |
| Compression order | Before encryption | Encrypted data is incompressible; compress first for meaningful gains |
| Target layout | Flat directory | Simple to inspect; matches how btrfs backups land today |
| Metadata store | `.info` sidecar file per stream | Stateless: re-derived from filesystem on each run; no side database |
| Parent resolution | `received_uuid` in sidecar | Reuses the existing `is_correlated()` mechanism unchanged |

---

## §3 — Architecture

```
cli/src/cli.rs  (--raw, --compress, --passphrase-file on run/resume only)
    │
    └── RawStreamAdapter  (crates/adapters/src/raw_stream.rs)  ← NEW FILE
            implements TransferPort        → pipe3(btrfs-send, zstd, gpg) + sidecar
            implements DeletePort          → rm stream file + sidecar pair
            implements SubvolumeRepository → scan *.info sidecars → synthetic Subvolumes
```

`RawStreamAdapter` holds a `Box<dyn CommandRunner>` (same injection pattern as
`BtrfsCliAdapter` and `SshCommandRunner`). The `CommandRunner` trait gains a `pipe3()`
method so the three-stage pipeline is injectable and fully unit-testable with fake runners.

---

## §4 — Sidecar format

File: `<leaf>.info` alongside `<leaf>.btrfs[.zst][.gpg]`
where `<leaf>` is the btrbk name (`<basename>.<timestamp>[_N]`).

```toml
uuid               = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
received_from_uuid = "11111111-2222-3333-4444-555555555555"
leaf               = "home.20260625T020000+0200"
stream_file        = "home.20260625T020000+0200.btrfs.zst.gpg"
created_at         = 1750809600
compress           = "zstd"
encrypt            = "gpg-symmetric"
```

**Fields:**

| Field | Type | Purpose |
|---|---|---|
| `uuid` | UUID string | Unique per backup; source of stable `id` and `Subvolume.uuid` |
| `received_from_uuid` | UUID string | Source snapshot's uuid → `Subvolume.received_uuid` (correlation key) |
| `leaf` | string | btrbk leaf name → `Subvolume.path`; drives `parse_name()` for retention |
| `stream_file` | string | Full filename with extensions → used by `DeletePort` to locate the stream |
| `created_at` | integer | Unix timestamp → `Subvolume.cgen` / `generation`; orders within the raw target |
| `compress` | string | `"zstd"` \| `"gzip"` \| `"xz"` \| `"none"` |
| `encrypt` | string | `"gpg-symmetric"` \| `"none"` |

**No `parent_stream` field.** Parent resolution uses `received_uuid` / `parent_uuid` on
`Subvolume` structs. A name-based field is unused dead weight and creates Rule 16
parse obligations for zero benefit.

**Rule 16 applies throughout:** a present-but-wrong-type field is a parse error
(`PortError::Parse`), never a silent `None`. In particular, `uuid` and
`received_from_uuid` present but not valid UUIDs → `PortError::Parse`.

**Atomic write:** write to `<leaf>.info.tmp`, then `rename` to `<leaf>.info`.

---

## §5 — Target filesystem UUID

A stable `fs_uuid` is needed so `best_parent()` (which filters candidates by
`fs_uuid`) correctly scopes parent candidates to the raw target's own "filesystem."

Stored in `<target_dir>/.mybtrfs_raw_fs_uuid` (a plain UUID string). Lifecycle:

- **Created** during the first `send_receive()` call, not during `list()`.
- **`list()`** behaviour:
  - UUID file absent **and** no `.info` sidecars → return empty `Vec` (first run, no backups yet).
  - UUID file absent **but** sidecars present → `PortError::Verification` (corrupted state).
  - UUID file present → use it for all synthetic `Subvolume`s.

---

## §6 — Synthetic `Subvolume` construction

The retention scheduler, safety policy, and parent resolver all operate on
`&[Subvolume]`. Raw backups produce synthetic structs reconstructed from sidecars.

| `Subvolume` field | Value | Notes |
|---|---|---|
| `uuid` | parsed `sidecar.uuid` | Rule 16: malformed → `PortError::Parse` |
| `received_uuid` | parsed `sidecar.received_from_uuid` | Rule 16: malformed → `PortError::Parse` |
| `parent_uuid` | `None` | Full-send shape; Strict mode not supported for raw targets |
| `readonly` | `true` | Always — required for `is_correlated()` to match |
| `path` | `PathBuf::from(&sidecar.leaf)` | Leaf only (relative); `parse_name()` extracts timestamp |
| `mountpoint` | `target_dir` | Deletion path = `mountpoint.join(&path)` |
| `fs_uuid` | from `.mybtrfs_raw_fs_uuid` | Stable; scopes `best_parent()` to this target |
| `cgen` / `generation` | `sidecar.created_at` | Unix timestamps sort correctly within a raw target; cross-target comparison is impossible (`best_parent()` filters by `fs_uuid` first) |
| `id` | `u64::from_le_bytes(uuid_bytes[0..8])` | Content-addressed; stable under concurrent access (unlike list position) |

---

## §7 — `SubvolumeRepository` implementation

**`list(filesystem: &Path)`**

The `filesystem` argument is ignored semantically (no btrfs involved). Implementation
scans `target_dir/*.info` sidecars:

1. If `.mybtrfs_raw_fs_uuid` absent and no `.info` files → return `Ok(vec![])`.
2. If `.mybtrfs_raw_fs_uuid` absent but `.info` files present → return `PortError::Verification`.
3. Otherwise parse each `.info`, build synthetic `Subvolume`s, sort by `cgen`, return.

**`show(path: &Path)`**

`path` is `mountpoint/leaf` (no extension, as stored in `Subvolume.mountpoint.join(&path)`).
Derives sidecar path as `path.with_extension("info")`, reads and parses it,
returns the synthetic `Subvolume`. Returns `PortError::Io` if the sidecar is absent.

---

## §8 — `DeletePort` implementation

`delete(path: &Path, _commit: DeleteCommit)` receives `mountpoint/leaf` (no extension).
`DeleteCommit` is ignored — no btrfs transactions.

Steps:
1. Derive sidecar path: `path.with_extension("info")`
2. Read sidecar → get `stream_file` field (full filename with codec extensions)
3. `log::debug!` before removing stream file
4. Delete `target_dir.join(&stream_file)`
5. `log::debug!` before removing sidecar
6. Delete sidecar
7. If sidecar was absent at step 2: `log::warn!` and attempt stream deletion anyway
   (partial cleanup, not an error)

Any `PortError::Io` surfaced from removal must have a `log::error!` at the detection
site (Rule 24).

---

## §9 — Verification contract for raw targets

The existing `TransferPort` contract mandates: "a successful return means a trustworthy,
verified backup." For btrfs targets this is enforced via `btrfs subvolume show` after
receive (checking `received_uuid`, `parent_uuid`, `ro`). For raw targets there is no
received btrfs subvolume to inspect. Substitute verification in `send_receive()`:

1. `pipe3()` returned `Ok(())` — GPG exit code 0, all three stages exited cleanly.
2. Stream file exists and `metadata().len() > 0` — non-empty file on disk.
3. Sidecar round-trip — read back the just-written sidecar and confirm fields match.

If any check fails: delete the stream file and sidecar before returning
`PortError::Verification`. Document the relaxed contract in the adapter's `/// # Errors`
and `/// # Notes` doc comments.

---

## §10 — Three-stage pipeline: `pipe3()` on `CommandRunner`

The existing `CommandRunner::pipe()` handles exactly two processes. The
`btrfs send | zstd | gpg` chain requires three. Adding `pipe3()` to the trait makes
it injectable and unit-testable with fake runners — the same reason `pipe()` is on
the trait rather than inlined.

**Trait addition** (`crates/adapters/src/command.rs`):

```rust
fn pipe3(
    &self,
    producer: (&str, &[&OsStr]),
    middle:   (&str, &[&OsStr]),
    consumer: (&str, &[&OsStr]),
    on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
) -> Result<(), PortError>;
```

Default implementation panics with `unimplemented!()`. Existing implementors
(`SshCommandRunner`, `SshSourceRunner`) inherit the default and are unaffected until
they need the feature. `SystemCommandRunner` gets the real implementation.

**`SystemCommandRunner::pipe3()` — process lifecycle (deadlock-safe):**

```
[btrfs send] ─pipe1─► [zstd] ─pipe2─► [gpg -o outfile]
```

1. Create `pipe1` and `pipe2` (`std::io::pipe()`).
2. Spawn btrfs-send with `stdout = pipe1_write`.
3. Spawn zstd with `stdin = pipe1_read`, `stdout = pipe2_write`.
4. Spawn gpg with `stdin = pipe2_read`, `-o <outfile>` (no shell redirect).
5. **Close all pipe write-ends in the parent process immediately after spawning.**
   This is critical: each stage sees EOF only when its predecessor's write-end is
   closed. Failing to close in the parent causes deadlock.
6. Optionally insert a progress-counting bridge thread on `pipe1` (same pattern as
   `SystemCommandRunner::pipe()` lines 91-135 in `command.rs`).
7. Wait consumer-first: gpg → zstd → btrfs-send.
8. Drain btrfs-send stderr on a background thread (prevents deadlock if stderr fills).

`log::debug!` emitted before each of the three spawns (Rule 24).

**`RecordingRunner::pipe3()` for tests:** records `("PIPE3", [producer_args, "|", middle_args, "|", consumer_args])` into the call log. Allows unit tests of `send_receive()` argv construction without real processes.

When `compress = "none"` **and** `encrypt = "none"`, `send_receive()` delegates to
the existing `CommandRunner::pipe()` (two-stage).

---

## §11 — CLI flags

Added to **`Command::Run` and `Command::Resume` only.** Not `Command::Snapshot` —
snapshot creates a local btrfs subvolume with no transfer stage; a raw stream target
does not apply and `--raw` there would silently do nothing.

```rust
/// Write a raw btrfs stream file instead of btrfs-receiving into a filesystem.
/// TARGET is treated as a plain directory path (not an ssh:// endpoint).
#[arg(long)]
raw: bool,

/// Compression codec for raw stream files (default: zstd).
#[arg(long, value_name = "CODEC", default_value = "zstd")]
compress: Compress,   // enum Compress: Zstd | Gzip | Xz | None

/// Path to a file containing the GPG symmetric passphrase.
/// Requires --raw.
#[arg(long, value_name = "PATH")]
passphrase_file: Option<PathBuf>,
```

**Validation (enforced in dispatch, exit 2 on violation):**
- `--passphrase-file` without `--raw` → usage error.
- `--raw` with an `ssh://` TARGET → usage error (raw targets are local only for now).
- `--raw` with `--compress none` and no `--passphrase-file` → valid (unencrypted raw stream; useful for non-btrfs local targets).

`Command::Restore` receives `--passphrase-file` only. The restore path detects a raw
target from the presence of a `.info` sidecar alongside the provided path, then
reverses the pipeline: `gpg --decrypt | zstd -d | btrfs receive`.

---

## §12 — Files to create / modify

| File | Action |
|---|---|
| `crates/adapters/src/raw_stream.rs` | **NEW** — `RawStreamAdapter`, `SidecarInfo`, synthetic `Subvolume` builder, `stable_id()` |
| `crates/adapters/src/command.rs` | Add `pipe3()` to `CommandRunner` trait + `SystemCommandRunner` impl |
| `crates/adapters/src/lib.rs` | `pub(crate) mod raw_stream;` + `pub use raw_stream::RawStreamAdapter;` |
| `crates/cli/src/cli.rs` | `Compress` enum, `--raw`/`--compress`/`--passphrase-file` flags, dispatch |
| `man/mybtrfs-run.1` | Document `--raw`, `--compress`, `--passphrase-file` |
| `man/mybtrfs-restore.1` | Document `--passphrase-file` for raw target restore |

**No changes to `domain/`, `application/`, or `ports.rs`.**

---

## §13 — Implementation order (TDD)

Each step: write the failing test first (red), implement to green, refactor.
`cargo clippy --workspace --all-targets` and `cargo fmt --check` must be clean after
every step.

**Step 1 — Sidecar parse/write** (pure, zero I/O in tests)
- `SidecarInfo` struct + `to_toml() -> String` + `from_toml(s: &str) -> Result<SidecarInfo, PortError>`
- `toml` crate (already a workspace dep)
- Tests: round-trip, malformed UUID → `PortError::Parse`, wrong-type field → `PortError::Parse`, missing required field → `PortError::Parse`

**Step 2 — Synthetic `Subvolume` builder** (pure)
- `stable_id(uuid: &Uuid) -> u64` — `u64::from_le_bytes(uuid.as_bytes()[0..8])`
- `sidecar_to_subvolume(info: &SidecarInfo, target_dir: &Path, fs_uuid: Uuid) -> Subvolume`
- Tests: all field mappings, `path.file_name()` round-trips via `parse_name()`, `id` is deterministic

**Step 3 — `SubvolumeRepository` impl** (uses `tempdir`)
- `list()`: absent UUID + no sidecars → empty; absent UUID + sidecars → `PortError::Verification`
- `show()`: reads sidecar at `path.with_extension("info")`
- Tests with pre-written sidecar files in a `tempdir`

**Step 4 — `DeletePort` impl** (uses `tempdir`)
- Read sidecar → `stream_file` → delete pair; missing sidecar → warn, delete stream only
- Tests: normal delete removes both files; missing sidecar warns and removes stream

**Step 5 — `pipe3()` on `CommandRunner` trait**
- Trait method with `unimplemented!()` default; `SystemCommandRunner` real impl
- Unit tests with real processes (`echo` / `cat` / `head`): correct wait ordering, no deadlock
- `RecordingRunner::pipe3()` for `raw_stream.rs` tests

**Step 6 — `TransferPort` impl**
- `send_receive()`: `pipe3` → write sidecar atomically → verify (size > 0, round-trip)
- Argv unit tests via `RecordingRunner`: assert btrfs-send / zstd / gpg argv without spawning real btrfs
- Integration test (`#[ignore]`): requires `/mnt/mybtrfs-source` + `/mnt/mybtrfs-backup/raw` + key `C8D7DA12`

**Step 7 — CLI wiring**
- `Compress` enum + flags on `Command::Run` and `Command::Resume`
- `--passphrase-file` on `Command::Restore`
- Dispatch: `--raw` → `RawStreamAdapter`; mutual exclusion validation
- Tests: flag parsing, `--passphrase-file` requires `--raw`, `--raw` + `ssh://` → error

---

## §14 — Verification

```bash
# Unit tests (no btrfs, no GPG needed)
cargo test -p mybtrfs-adapters raw_stream
cargo test --workspace

# Lint gate
cargo clippy --workspace --all-targets
cargo fmt --check

# Integration smoke test (requires root + loopback env + GPG key C8D7DA12)
sudo contrib/setup-local-backup-env.sh setup
echo -n "" > /tmp/mybtrfs-test.passphrase   # empty passphrase (matches no-protection key)
sudo mkdir -p /mnt/mybtrfs-backup/raw

sudo mybtrfs run /mnt/mybtrfs-source/@data /mnt/mybtrfs-source/snapshots data \
  /mnt/mybtrfs-backup/raw \
  --raw --compress zstd --passphrase-file /tmp/mybtrfs-test.passphrase

# Verify output on disk
ls /mnt/mybtrfs-backup/raw/
# → data.20260625T…btrfs.zst.gpg  (stream)
# → data.20260625T….info           (sidecar)

# Verify decryption + decompression
gpg --batch --passphrase-file /tmp/mybtrfs-test.passphrase \
  --decrypt /mnt/mybtrfs-backup/raw/data.*.btrfs.zst.gpg \
  | zstd -d | wc -c   # must be non-zero

# Restore
sudo mybtrfs restore /mnt/mybtrfs-backup/raw/data.20260625T… /mnt/restore \
  --passphrase-file /tmp/mybtrfs-test.passphrase

btrfs property get /mnt/restore ro   # → ro=false (writable snapshot)
```
