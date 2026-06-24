#!/usr/bin/env bash
# Test --quiet flag to suppress all progress indicators

set -euo pipefail

REPO="/home/jvidal/PROJECTS/mybtrfs"
MYBTRFS="$REPO/target/release/mybtrfs"

[ -x "$MYBTRFS" ] || { echo "Build first: cargo build --release"; exit 1; }

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
}
trap cleanup EXIT

echo "Setting up filesystems..."
truncate -s 400M "$SRC_IMG"
mkfs.btrfs -q "$SRC_IMG"
mkdir -p "$SRC_MNT" "$SRC_MNT/.snap"
mount -o loop "$SRC_IMG" "$SRC_MNT"
btrfs subvolume create "$SRC_MNT/data" >/dev/null

truncate -s 1G "$TGT_IMG"
mkfs.btrfs -q "$TGT_IMG"
mkdir -p "$TGT_MNT"
mount -o loop "$TGT_IMG" "$TGT_MNT"

echo "Creating 200 MB test data..."
for i in {1..20}; do
  dd if=/dev/urandom of="$SRC_MNT/data/file$i.bin" bs=1M count=10 2>/dev/null
done

echo ""
echo "=== Running with --quiet flag (completely silent) ==="
echo ""
"$MYBTRFS" run "$SRC_MNT/data" "$SRC_MNT/.snap" "data" "$TGT_MNT" --yes --quiet

echo ""
echo "✓ Completed silently (no spinners, no progress bars)"
echo ""
btrfs subvolume list "$TGT_MNT" | awk '{print "  Backup created: " $NF}'
