#!/usr/bin/env bash
#
# mybtrfs progress indicator demo — local loopback test.
#
# Tests the progress indicators (spinner + bytes + speed) by creating a 200M
# loopback btrfs, populating it with test files, and running a backup to a
# local target. The transfer takes long enough to see the progress spinner
# and throughput counter update in real time.
#
#   ./contrib/test/mybtrfs-progress-demo.sh
#
# Why sudo: btrfs operations (mount, subvolume create) require root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"

info() { printf '\033[1m>> %s\033[0m\n' "$*"; }
die()  { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run with sudo:  sudo $0"

# Locate the mybtrfs binary.
MYBTRFS="${MYBTRFS:-}"
if [ -z "$MYBTRFS" ]; then
  for c in "$REPO/target/release/mybtrfs" "$REPO/target/debug/mybtrfs" "$(command -v mybtrfs 2>/dev/null || true)"; do
    [ -n "$c" ] && [ -x "$c" ] && { MYBTRFS="$c"; break; }
  done
fi
[ -n "$MYBTRFS" ] && [ -x "$MYBTRFS" ] || die "mybtrfs not found — build it (cargo build) or set MYBTRFS=/path"
info "mybtrfs:  $MYBTRFS"

# Set up fresh loopback btrfs filesystems (source and target)
WORK=$(mktemp -d /tmp/mybtrfs-test.XXXXXX)
SRC_IMG="$WORK/src.img"
SRC_MNT="$WORK/mnt"
TGT_IMG="$WORK/tgt.img"
TGT_MNT="$WORK/target"

cleanup() {
  set +e
  umount "$SRC_MNT" 2>/dev/null
  umount "$TGT_MNT" 2>/dev/null
  losetup -j "$SRC_IMG" 2>/dev/null | cut -d: -f1 | xargs -r -n1 losetup -d 2>/dev/null
  losetup -j "$TGT_IMG" 2>/dev/null | cut -d: -f1 | xargs -r -n1 losetup -d 2>/dev/null
  rm -rf "$WORK"
  info "cleaned up source and target loopbacks"
}
trap cleanup EXIT

info "creating 200M source loopback btrfs at $SRC_MNT"
truncate -s 200M "$SRC_IMG"
mkfs.btrfs -q "$SRC_IMG"
mkdir -p "$SRC_MNT" "$SRC_MNT/.snap"
mount -o loop "$SRC_IMG" "$SRC_MNT"
btrfs subvolume create "$SRC_MNT/data" >/dev/null

info "creating 500M target loopback btrfs at $TGT_MNT"
truncate -s 500M "$TGT_IMG"
mkfs.btrfs -q "$TGT_IMG"
mkdir -p "$TGT_MNT"
mount -o loop "$TGT_IMG" "$TGT_MNT"

info "populating with 100 test files (to make transfer visible)"
for i in {1..100}; do
  dd if=/dev/urandom of="$SRC_MNT/data/file$i.bin" bs=1M count=1 2>/dev/null
  if [ $((i % 10)) -eq 0 ]; then
    printf '   %d/100 files created\n' "$i"
  fi
done

info "target directory:  $TGT_MNT"
echo ""
info "running backup — watch for the progress spinner + bytes + speed!"
info "spinner cycles: ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏ (updates every ~250ms)"
echo ""

# Run backup with progress indicators visible
"$MYBTRFS" run "$SRC_MNT/data" "$SRC_MNT/.snap" "data" "$TGT_MNT" --yes

echo ""
info "backup complete!"
btrfs subvolume list "$TGT_MNT" | awk '{print $NF}'
echo ""
printf '\033[32mPASS\033[0m — progress indicators working end-to-end.\n'
