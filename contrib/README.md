# contrib — testing, scheduling, and automation for mybtrfs

mybtrfs is CLI-first and stateless: there is no daemon and no config file. To run
backups on a schedule you simply invoke `mybtrfs run …` from your system's
scheduler. These drop-ins do exactly that, using the flags built for unattended
use — `--yes` (no prompts), `--lock` (refuse overlapping runs → exit 3), and
`--journal` (append an audit line per run). btrfs needs root, so run them as root
(otherwise mybtrfs exits 4 with "re-run with sudo").

## Local testing with persistent btrfs fixture

**`setup-local-backup-env.sh`** creates a two-filesystem loopback btrfs environment
(500 MB source + 2 GB backup) that survives reboots, allowing iterative manual
testing without CI or real external drives:

```bash
sudo contrib/setup-local-backup-env.sh setup      # create & mount loopback images
sudo ./target/debug/mybtrfs run /mnt/mybtrfs-source/@data \
    /mnt/mybtrfs-source/snapshots data /mnt/mybtrfs-backup/backups
sudo contrib/setup-local-backup-env.sh populate   # simulate daily file churn
sudo ./target/debug/mybtrfs run …                 # test incremental backups
sudo contrib/setup-local-backup-env.sh teardown   # clean unmount & delete images
```

**Subcommands:**
- `setup` — create images, format, mount, create `@data` subvolume, populate with test data
- `teardown` — unmount, detach loop devices, delete images (with robust fallback cleanup)
- `status` — show whether setup is active, list snapshot/backup content
- `populate` — add another batch of files to simulate daily changes

The setup survives reboots (images stored in `/var/tmp`, not `/tmp`), making it ideal
for development across sessions. No temporary test data is left behind on teardown.

## systemd (recommended)

```sh
sudo install -m0644 systemd/mybtrfs-backup.service /etc/systemd/system/
sudo install -m0644 systemd/mybtrfs-backup.timer   /etc/systemd/system/
sudo install -m0640 systemd/mybtrfs.env.example    /etc/default/mybtrfs
sudoedit /etc/default/mybtrfs            # set SOURCE / SNAPSHOT_DIR / BASENAME / TARGET
sudo systemctl daemon-reload
sudo systemctl enable --now mybtrfs-backup.timer

systemctl list-timers mybtrfs-backup.timer   # when it next fires
sudo systemctl start mybtrfs-backup.service   # run once now, to test
journalctl -u mybtrfs-backup.service          # see the result
```

Edit `mybtrfs-backup.timer` (`OnCalendar=`) for a different cadence, and
`mybtrfs-backup.service` (`ExecStart=`) to add retention flags or back up several
subvolumes (one service/timer pair per subvolume, or several `ExecStart=` lines).
`RequiresMountsFor=` is the clean way to skip a run when the backup drive is absent.

## cron

```sh
sudo install -m0644 cron/mybtrfs.crontab /etc/cron.d/mybtrfs
sudoedit /etc/cron.d/mybtrfs            # set the paths
```

## Remote (ssh) targets

`mybtrfs run … ssh://[user@]host[:port]/path` backs up to a btrfs filesystem on
another host (Phase 5 §2). `test/mybtrfs-ssh-smoke.sh` proves the path end-to-end
against a real target without touching any real data — it builds a tiny throwaway
loopback btrfs source, backs it up over ssh, verifies the received subvolume
(readonly + Received UUID), and cleans up:

```sh
sudo contrib/test/mybtrfs-ssh-smoke.sh
# override the target with env vars, e.g.:
#   sudo REMOTE_HOST=10.2.152.181 REMOTE_PATH=/mnt/btrfs-test contrib/test/mybtrfs-ssh-smoke.sh
```

Prereqs on the remote: btrfs-progs, a mounted btrfs at the target path, your key in
`authorized_keys`, and passwordless sudo for btrfs
(`isard ALL=(root) NOPASSWD: /usr/bin/btrfs`). Retention works against a remote
target too (`--target-preserve …` / `--target-preserve-min …`): target backups are
pruned over ssh while source snapshots prune locally.

## Notes

- **Target must be explicit.** The interactive drive picker is for terminals; a
  scheduled run takes an already-mounted `--target`/`MYBTRFS_TARGET` path (local or
  `ssh://…`).
- **Keep-all by default.** Without `--snapshot-preserve`/`--target-preserve`,
  nothing is pruned — add them once you trust the backups.
- **One lock for the host.** `--lock /run/mybtrfs.lock` serializes every scheduled
  run; a second invocation while one is in flight exits 3 and changes nothing.
