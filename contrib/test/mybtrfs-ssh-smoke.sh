#!/usr/bin/env bash
#
# mybtrfs SSH remote-backup smoke test (Phase 5 §2).
#
# Makes a tiny THROWAWAY loopback btrfs source, backs it up to a remote btrfs
# target over ssh, verifies the received subvolume is a real backup (readonly +
# Received UUID), and cleans up the local source afterwards. Touches no real data.
#
#   sudo contrib/test/mybtrfs-ssh-smoke.sh
#
# Why sudo: the local `btrfs send` needs root, so mybtrfs runs as root — and the
# `ssh` it spawns then runs as root too. The script therefore installs your key +
# a host block into /root/.ssh so root can reach the target (idempotent; left in
# place for re-runs).
#
# Config via env (defaults target the apolo IsardVDI test host over its VPN):
#   MYBTRFS      path to the mybtrfs binary   (default: repo target/{release,debug}, then PATH)
#   REMOTE_HOST  ssh host/IP of the target    (default: 10.2.152.181)
#   REMOTE_USER  ssh user on the target       (default: isard)
#   REMOTE_PATH  btrfs dir on the target      (default: /mnt/btrfs-test)
#   SSH_KEY      private key for the target   (default: <your>/.ssh/mybtrfs_apollo)
#   BASENAME     snapshot/backup base name    (default: data)
#
# Prerequisites on the target (one-time): btrfs-progs, a mounted btrfs at
# REMOTE_PATH, your key in authorized_keys, and passwordless sudo for btrfs
# (`isard ALL=(root) NOPASSWD: /usr/bin/btrfs`). The VPN must be up locally.

set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-10.2.152.181}"
REMOTE_USER="${REMOTE_USER:-isard}"
REMOTE_PATH="${REMOTE_PATH:-/mnt/btrfs-test}"
BASENAME="${BASENAME:-data}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"

info() { printf '\033[1m>> %s\033[0m\n' "$*"; }
die()  { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run with sudo:  sudo $0"

# Locate the invoking user's key (sudo's HOME is /root).
USER_HOME="$(eval echo "~${SUDO_USER:-root}")"
SSH_KEY="${SSH_KEY:-$USER_HOME/.ssh/mybtrfs_apollo}"
[ -f "$SSH_KEY" ] || die "ssh key not found: $SSH_KEY  (set SSH_KEY=/path/to/key)"

# Locate the mybtrfs binary.
MYBTRFS="${MYBTRFS:-}"
if [ -z "$MYBTRFS" ]; then
  for c in "$REPO/target/release/mybtrfs" "$REPO/target/debug/mybtrfs" "$(command -v mybtrfs 2>/dev/null || true)"; do
    [ -n "$c" ] && [ -x "$c" ] && { MYBTRFS="$c"; break; }
  done
fi
[ -n "$MYBTRFS" ] && [ -x "$MYBTRFS" ] || die "mybtrfs not found — build it (cargo build --release) or set MYBTRFS=/path"
info "mybtrfs:  $MYBTRFS"

# Give root ssh access to the target (mybtrfs's ssh runs as root under sudo).
install -d -m700 /root/.ssh
install -m600 "$SSH_KEY" /root/.ssh/mybtrfs_smoke_key
if ! grep -qiE "^[[:space:]]*Host[[:space:]]+$REMOTE_HOST([[:space:]]|$)" /root/.ssh/config 2>/dev/null; then
  cat >> /root/.ssh/config <<EOF

Host $REMOTE_HOST
  User $REMOTE_USER
  IdentityFile /root/.ssh/mybtrfs_smoke_key
  IdentitiesOnly yes
  StrictHostKeyChecking accept-new
  BatchMode yes
EOF
  info "added a Host $REMOTE_HOST block to /root/.ssh/config"
fi

remote() { ssh -o BatchMode=yes "${REMOTE_USER}@${REMOTE_HOST}" "$@"; }

info "preflight: reach the target + remote sudo btrfs"
remote "sudo -n btrfs --version" >/dev/null 2>&1 \
  || die "cannot run 'sudo btrfs' on ${REMOTE_USER}@${REMOTE_HOST} — VPN up? key authorized? NOPASSWD btrfs set?"
remote "test -d '$REMOTE_PATH'" \
  || die "remote target dir not found: $REMOTE_PATH"

# A tiny throwaway loopback btrfs source.
WORK="$(mktemp -d /tmp/mybtrfs-smoke.XXXXXX)"
IMG="$WORK/src.img"; MNT="$WORK/mnt"; LOCK="$WORK/lock"
ENDPOINT="ssh://${REMOTE_USER}@${REMOTE_HOST}${REMOTE_PATH}"

cleanup() {
  set +e
  umount "$MNT" 2>/dev/null
  losetup -j "$IMG" 2>/dev/null | cut -d: -f1 | xargs -r -n1 losetup -d 2>/dev/null
  rm -rf "$WORK"
  info "cleaned up local source"
  echo "   (remote copies remain under $REMOTE_PATH — remove with:"
  echo "    ssh ${REMOTE_USER}@${REMOTE_HOST} 'sudo btrfs subvolume delete $REMOTE_PATH/$BASENAME.*' )"
}
trap cleanup EXIT

info "creating a 512M loopback btrfs source"
truncate -s 512M "$IMG"
mkfs.btrfs -q "$IMG"
mkdir -p "$MNT"
mount -o loop "$IMG" "$MNT"
btrfs subvolume create "$MNT/$BASENAME" >/dev/null
echo "hello mybtrfs over ssh @ $(date -Is)" > "$MNT/$BASENAME/hello.txt"
mkdir "$MNT/.snap"

info "mybtrfs run  $MNT/$BASENAME  ->  $ENDPOINT"
"$MYBTRFS" run "$MNT/$BASENAME" "$MNT/.snap" "$BASENAME" "$ENDPOINT" --yes --lock "$LOCK"

info "verifying the received subvolume on the target"
name="$(remote "sudo btrfs subvolume list -o '$REMOTE_PATH'" \
        | awk '{print $NF}' | sed 's#.*/##' | grep -E "^${BASENAME}\.[0-9]" | sort | tail -1)"
[ -n "$name" ] || die "no ${BASENAME}.<timestamp> subvolume found under $REMOTE_PATH"
show="$(remote "sudo btrfs subvolume show '$REMOTE_PATH/$name'")"
printf '%s\n' "$show" | grep -qi "readonly" \
  || die "received subvolume is not readonly:
$show"
printf '%s\n' "$show" | grep -iE "Received UUID:[[:space:]]*[0-9a-f]{8}-" >/dev/null \
  || die "received subvolume has no Received UUID (not a real backup):
$show"

echo
printf '\033[32mPASS\033[0m — backed up to %s  (readonly + Received UUID).\n' "$REMOTE_PATH/$name"
echo "Remote-target SSH backup works end-to-end."
