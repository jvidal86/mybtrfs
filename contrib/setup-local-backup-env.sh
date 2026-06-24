#!/usr/bin/env bash
#
# Local btrfs backup environment setup/teardown — persistent test fixture
#
# Creates a two-filesystem loopback btrfs setup for manual testing of mybtrfs:
#   - Source filesystem (500 MB) with @data subvolume
#   - Backup filesystem (2 GB) with backups directory
#
# Subcommands:
#   setup     — create images, mount, populate test data
#   teardown  — unmount, detach loops, delete images
#   status    — show active images/mounts/content
#   populate  — add another batch of test data (simulate daily changes)
#
# Requires: root (btrfs mount/subvolume ops need sudo)
#
#   sudo contrib/setup-local-backup-env.sh setup
#   sudo contrib/setup-local-backup-env.sh teardown
#

set -euo pipefail

# Paths and configuration
SOURCE_IMG=/var/tmp/mybtrfs-source.img
BACKUP_IMG=/var/tmp/mybtrfs-backup.img
SOURCE_MNT=/mnt/mybtrfs-source
BACKUP_MNT=/mnt/mybtrfs-backup
STATE_FILE=/var/tmp/mybtrfs-demo-state

SOURCE_SIZE=500M
BACKUP_SIZE=2G

# Color output helpers
info()  { printf '\033[1m>> %s\033[0m\n' "$*"; }
warn()  { printf '\033[33m⚠️  %s\033[0m\n' "$*" >&2; }
error() { printf '\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
ok()    { printf '\033[32m✓ %s\033[0m\n' "$*"; }

# Ensure running as root
[ "$(id -u)" -eq 0 ] || error "must run with sudo"

# Save loop device paths to state file (for cleanup)
save_state() {
  local src_loop="$1"
  local bak_loop="$2"
  {
    echo "SOURCE_LOOP=$src_loop"
    echo "BACKUP_LOOP=$bak_loop"
  } > "$STATE_FILE"
  ok "state saved to $STATE_FILE"
}

# Restore loop device paths from state file
load_state() {
  if [ ! -f "$STATE_FILE" ]; then
    return 1
  fi
  # shellcheck source=/dev/null
  source "$STATE_FILE"
  [ -n "${SOURCE_LOOP:-}" ] && [ -n "${BACKUP_LOOP:-}" ]
}

# Populate filesystem with varied test files (~50 MB first time, ~15 MB on subsequent runs)
populate_data() {
  local target_dir="$1"
  local batch_name="$2"

  mkdir -p "$target_dir/docs" "$target_dir/code" "$target_dir/media"

  # Docs: text files 1-100 KB
  info "  Creating doc files..."
  for i in {1..20}; do
    local size=$((RANDOM % 100 + 1))
    dd if=/dev/urandom bs=1K count="$size" of="$target_dir/docs/${batch_name}_doc_$i.md" 2>/dev/null
  done

  # Code: source files 1-500 KB
  info "  Creating source files..."
  for ext in rs py sh json; do
    for i in {1..5}; do
      local size=$((RANDOM % 500 + 1))
      dd if=/dev/urandom bs=1K count="$size" of="$target_dir/code/${batch_name}_file_$i.$ext" 2>/dev/null
    done
  done

  # Media: binary blobs 1-10 MB
  info "  Creating media files..."
  for i in {1..3}; do
    local size=$((RANDOM % 10 + 1))
    dd if=/dev/urandom bs=1M count="$size" of="$target_dir/media/${batch_name}_media_$i.bin" 2>/dev/null
  done
}

# ============================================================================
# SUBCOMMAND: setup
# ============================================================================
cmd_setup() {
  # Guard: fail if already set up
  if mountpoint -q "$SOURCE_MNT" 2>/dev/null; then
    error "already set up — source is mounted at $SOURCE_MNT. Run 'teardown' first."
  fi

  info "Creating loopback images..."
  truncate -s "$SOURCE_SIZE" "$SOURCE_IMG"
  truncate -s "$BACKUP_SIZE" "$BACKUP_IMG"
  ok "images created"

  info "Formatting btrfs filesystems..."
  mkfs.btrfs -q -L mybtrfs-source "$SOURCE_IMG"
  mkfs.btrfs -q -L mybtrfs-backup "$BACKUP_IMG"
  ok "filesystems formatted"

  info "Attaching loop devices..."
  local src_loop bak_loop
  src_loop=$(losetup --find --show "$SOURCE_IMG")
  bak_loop=$(losetup --find --show "$BACKUP_IMG")
  ok "source loop: $src_loop"
  ok "backup loop: $bak_loop"

  info "Creating mount points..."
  mkdir -p "$SOURCE_MNT" "$BACKUP_MNT"
  ok "mount points created"

  info "Mounting filesystems..."
  mount "$src_loop" "$SOURCE_MNT"
  mount "$bak_loop" "$BACKUP_MNT"
  ok "filesystems mounted"

  info "Creating subvolumes and directories..."
  btrfs subvolume create "$SOURCE_MNT/@data" > /dev/null
  mkdir -p "$SOURCE_MNT/snapshots" "$BACKUP_MNT/backups"
  ok "subvolumes and directories created"

  info "Populating with test data..."
  local batch_name
  batch_name=$(date +%Y%m%d_%H%M%S)
  populate_data "$SOURCE_MNT/@data" "$batch_name"
  ok "test data populated (batch: $batch_name)"

  # Save state for teardown
  save_state "$src_loop" "$bak_loop"

  # Print usage guide
  cat << 'GUIDE'

✓ Setup complete. Local btrfs backup environment ready.

Source subvolume : /mnt/mybtrfs-source/@data
Snapshot dir     : /mnt/mybtrfs-source/snapshots
Backup dir       : /mnt/mybtrfs-backup/backups

=== Full Backup ===
sudo ./target/debug/mybtrfs run \
  /mnt/mybtrfs-source/@data \
  /mnt/mybtrfs-source/snapshots \
  data \
  /mnt/mybtrfs-backup/backups

=== Add Changes & Incremental Backup ===
sudo contrib/setup-local-backup-env.sh populate
sudo ./target/debug/mybtrfs run \
  /mnt/mybtrfs-source/@data \
  /mnt/mybtrfs-source/snapshots \
  data \
  /mnt/mybtrfs-backup/backups

=== Inspect Results ===
# List all snapshots and backups
sudo ./target/debug/mybtrfs list \
  /mnt/mybtrfs-source/snapshots \
  /mnt/mybtrfs-backup/backups

# Show health/sync status
sudo ./target/debug/mybtrfs status \
  /mnt/mybtrfs-source/snapshots \
  /mnt/mybtrfs-backup/backups

# Retention preview (dry-run)
sudo ./target/debug/mybtrfs prune --dry-run \
  /mnt/mybtrfs-source/snapshots \
  /mnt/mybtrfs-backup/backups

=== Teardown ===
sudo contrib/setup-local-backup-env.sh teardown

GUIDE
}

# ============================================================================
# SUBCOMMAND: teardown
# ============================================================================
cmd_teardown() {
  info "Tearing down local btrfs environment..."

  # Load state from file if present
  if load_state; then
    SOURCE_LOOP="${SOURCE_LOOP:-}"
    BACKUP_LOOP="${BACKUP_LOOP:-}"
    if [ -n "$SOURCE_LOOP" ]; then
      info "Using saved loop device: $SOURCE_LOOP"
    fi
    if [ -n "$BACKUP_LOOP" ]; then
      info "Using saved loop device: $BACKUP_LOOP"
    fi
  else
    warn "state file not found; will use losetup -j fallback"
    SOURCE_LOOP=""
    BACKUP_LOOP=""
  fi

  # Best-effort unmount
  if mountpoint -q "$SOURCE_MNT" 2>/dev/null; then
    info "Unmounting $SOURCE_MNT..."
    umount "$SOURCE_MNT" 2>/dev/null || warn "umount $SOURCE_MNT failed (will retry after loop detach)"
  fi
  if mountpoint -q "$BACKUP_MNT" 2>/dev/null; then
    info "Unmounting $BACKUP_MNT..."
    umount "$BACKUP_MNT" 2>/dev/null || warn "umount $BACKUP_MNT failed (will retry after loop detach)"
  fi

  # Detach loop devices (try saved paths first, then fallback to losetup -j)
  if [ -n "$SOURCE_LOOP" ]; then
    if losetup -d "$SOURCE_LOOP" 2>/dev/null; then
      ok "detached $SOURCE_LOOP"
    else
      warn "failed to detach $SOURCE_LOOP; trying fallback..."
    fi
  fi
  if [ -n "$BACKUP_LOOP" ]; then
    if losetup -d "$BACKUP_LOOP" 2>/dev/null; then
      ok "detached $BACKUP_LOOP"
    else
      warn "failed to detach $BACKUP_LOOP; trying fallback..."
    fi
  fi

  # Fallback: use losetup -j to find by image if direct detach failed
  if [ -f "$SOURCE_IMG" ]; then
    info "Detaching loops by image (fallback)..."
    losetup -j "$SOURCE_IMG" 2>/dev/null | cut -d: -f1 | xargs -r -n1 losetup -d 2>/dev/null || true
    losetup -j "$BACKUP_IMG" 2>/dev/null | cut -d: -f1 | xargs -r -n1 losetup -d 2>/dev/null || true
  fi

  # Try unmount again (loop device detach may have freed them)
  if mountpoint -q "$SOURCE_MNT" 2>/dev/null; then
    info "Final unmount attempt: $SOURCE_MNT"
    umount "$SOURCE_MNT" 2>/dev/null || warn "still mounted; may need manual intervention"
  fi
  if mountpoint -q "$BACKUP_MNT" 2>/dev/null; then
    info "Final unmount attempt: $BACKUP_MNT"
    umount "$BACKUP_MNT" 2>/dev/null || warn "still mounted; may need manual intervention"
  fi

  # Delete images and state
  info "Cleaning up images and state..."
  rm -f "$SOURCE_IMG" "$BACKUP_IMG" "$STATE_FILE"
  ok "images and state deleted"

  # Remove empty mount directories (best-effort)
  rmdir "$SOURCE_MNT" 2>/dev/null || warn "$SOURCE_MNT still contains data (may be mounted elsewhere)"
  rmdir "$BACKUP_MNT" 2>/dev/null || warn "$BACKUP_MNT still contains data (may be mounted elsewhere)"

  ok "teardown complete"
}

# ============================================================================
# SUBCOMMAND: status
# ============================================================================
cmd_status() {
  local src_mounted=0
  local bak_mounted=0

  # Check images
  echo ""
  echo "Images:"
  if [ -f "$SOURCE_IMG" ]; then
    local src_size
    src_size=$(du -h "$SOURCE_IMG" | cut -f1)
    echo "  ✓ $SOURCE_IMG ($src_size)"
  else
    echo "  ✗ $SOURCE_IMG (not found)"
  fi

  if [ -f "$BACKUP_IMG" ]; then
    local bak_size
    bak_size=$(du -h "$BACKUP_IMG" | cut -f1)
    echo "  ✓ $BACKUP_IMG ($bak_size)"
  else
    echo "  ✗ $BACKUP_IMG (not found)"
  fi

  # Check loop devices
  echo ""
  echo "Loop Devices:"
  local found_src=0
  local found_bak=0
  if [ -f "$SOURCE_IMG" ]; then
    local src_loops
    src_loops=$(losetup -j "$SOURCE_IMG" 2>/dev/null || true)
    if [ -n "$src_loops" ]; then
      echo "  $src_loops" | sed 's/^/    /'
      found_src=1
    fi
  fi
  if [ -f "$BACKUP_IMG" ]; then
    local bak_loops
    bak_loops=$(losetup -j "$BACKUP_IMG" 2>/dev/null || true)
    if [ -n "$bak_loops" ]; then
      echo "  $bak_loops" | sed 's/^/    /'
      found_bak=1
    fi
  fi
  if [ "$found_src" -eq 0 ] && [ "$found_bak" -eq 0 ]; then
    echo "  (none attached)"
  fi

  # Check mounts
  echo ""
  echo "Mounts:"
  if mountpoint -q "$SOURCE_MNT" 2>/dev/null; then
    echo "  ✓ $SOURCE_MNT (mounted)"
    src_mounted=1
  else
    echo "  ✗ $SOURCE_MNT (not mounted)"
  fi

  if mountpoint -q "$BACKUP_MNT" 2>/dev/null; then
    echo "  ✓ $BACKUP_MNT (mounted)"
    bak_mounted=1
  else
    echo "  ✗ $BACKUP_MNT (not mounted)"
  fi

  # Check content (only if mounted)
  if [ "$src_mounted" -eq 1 ]; then
    echo ""
    echo "Content:"
    if [ -d "$SOURCE_MNT/@data" ]; then
      local data_size
      data_size=$(du -sh "$SOURCE_MNT/@data" 2>/dev/null | cut -f1)
      echo "  @data size: $data_size"
    fi

    if [ -d "$SOURCE_MNT/snapshots" ]; then
      local snap_count
      snap_count=$(find "$SOURCE_MNT/snapshots" -maxdepth 1 -type d -not -name 'snapshots' 2>/dev/null | wc -l)
      echo "  Snapshots: $snap_count"
      find "$SOURCE_MNT/snapshots" -maxdepth 1 -type d -not -name 'snapshots' -printf '%f\n' 2>/dev/null | sort | sed 's/^/    /'
    fi
  fi

  if [ "$bak_mounted" -eq 1 ]; then
    if [ -d "$BACKUP_MNT/backups" ]; then
      local bak_count
      bak_count=$(find "$BACKUP_MNT/backups" -maxdepth 1 -type d -not -name 'backups' 2>/dev/null | wc -l)
      echo "  Backups: $bak_count"
      find "$BACKUP_MNT/backups" -maxdepth 1 -type d -not -name 'backups' -printf '%f\n' 2>/dev/null | sort | sed 's/^/    /'
    fi
  fi

  echo ""
}

# ============================================================================
# SUBCOMMAND: populate
# ============================================================================
cmd_populate() {
  if ! mountpoint -q "$SOURCE_MNT/@data" 2>/dev/null; then
    error "@data is not mounted. Run 'setup' first."
  fi

  info "Adding new batch of test data to @data..."
  local batch_name
  batch_name=$(date +%Y%m%d_%H%M%S)
  populate_data "$SOURCE_MNT/@data" "$batch_name"
  ok "batch added: $batch_name"
}

# ============================================================================
# Main dispatcher
# ============================================================================
main() {
  local cmd="${1:-}"

  case "$cmd" in
    setup)
      cmd_setup
      ;;
    teardown)
      cmd_teardown
      ;;
    status)
      cmd_status
      ;;
    populate)
      cmd_populate
      ;;
    "")
      cat << 'USAGE'
Local btrfs backup environment — setup/teardown/status/populate

Usage:
  sudo contrib/setup-local-backup-env.sh setup     # create images, mount, populate
  sudo contrib/setup-local-backup-env.sh teardown  # unmount, detach, delete
  sudo contrib/setup-local-backup-env.sh status    # show active setup
  sudo contrib/setup-local-backup-env.sh populate  # add more test data

Paths:
  Source image  : /var/tmp/mybtrfs-source.img (500 MB)
  Backup image  : /var/tmp/mybtrfs-backup.img (2 GB)
  Source mount  : /mnt/mybtrfs-source (@data subvolume + snapshots/)
  Backup mount  : /mnt/mybtrfs-backup (backups/)

For details, see: contrib/README.md
USAGE
      ;;
    *)
      error "unknown subcommand: $cmd"
      ;;
  esac
}

main "$@"
