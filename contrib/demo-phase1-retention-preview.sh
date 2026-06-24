#!/bin/bash
#
# Demo: Phase 1 — Retention Preview
#
# This script demonstrates the retention preview feature: `prune --dry-run`
# shows what would be deleted before actually deleting anything.
#
# REQUIREMENTS:
# - btrfs filesystem(s) with snapshots and backups
# - mybtrfs binary built and in PATH (or use ./target/debug/mybtrfs)
#
# EXPECTED OUTPUT:
# A human-readable preview showing PRESERVE and DELETE sections with:
# - Snapshot/backup names with check marks (✅) or warnings (⚠️)
# - Computed ages like "just now", "7 days ago"
# - Counts like "(3 snapshots)" and "(2 snapshots) — run with --yes to confirm"
#

set -e

MYBTRFS="${MYBTRFS:-./target/debug/mybtrfs}"
SOURCE_SNAP="${SOURCE_SNAP:-./.snapshots}"
TARGET_BACKUP="${TARGET_BACKUP:-./.backups}"

if [ ! -x "$MYBTRFS" ]; then
    echo "Error: mybtrfs binary not found at $MYBTRFS"
    echo "Build with: cargo build -p mybtrfs"
    exit 1
fi

cat <<'EOF'
╔════════════════════════════════════════════════════════════════════════╗
║ Phase 1 Demo: Retention Preview                                        ║
║                                                                         ║
║ Feature: `prune --dry-run` shows what would be deleted                 ║
║ Location: crates/application/src/retention_preview.rs                  ║
║ Tests: 8 unit tests (parsing, age computation, edge cases)             ║
╚════════════════════════════════════════════════════════════════════════╝

EOF

echo "📝 About this demo:"
echo "   • Shows retention policy decisions before executing them"
echo "   • No data is deleted (--dry-run is safe to run)"
echo "   • Output includes snapshot names, ages, and counts"
echo "   • Users must confirm with --yes to actually delete"
echo ""

echo "🔍 Running: $MYBTRFS prune --dry-run \\"
echo "            --snapshot-preserve='7d' \\"
echo "            --target-preserve='4d 4w' \\"
echo "            $SOURCE_SNAP $TARGET_BACKUP"
echo ""

# Run the prune command with --dry-run
# This shows the preview without actually deleting anything
if $MYBTRFS prune --dry-run \
    --snapshot-preserve='7d' \
    --target-preserve='4d 4w' \
    "$SOURCE_SNAP" "$TARGET_BACKUP" 2>/dev/null; then

    echo ""
    echo "✅ Success! Preview shown above."
    echo ""
    echo "📊 What you're seeing:"
    echo "   • PRESERVE section:"
    echo "     - Snapshots/backups that WILL be kept"
    echo "     - ✅ checkmark + name + age (e.g., '7 days ago')"
    echo "     - Count: '(3 snapshots)'"
    echo ""
    echo "   • DELETE section:"
    echo "     - Snapshots/backups that WILL be removed"
    echo "     - ⚠️  warning icon + name + age"
    echo "     - Count: '(2 snapshots) — run with --yes to confirm'"
    echo ""
    echo "🎯 Next step: Run without --dry-run to actually delete:"
    echo "   $MYBTRFS prune --yes \\"
    echo "            --snapshot-preserve='7d' \\"
    echo "            --target-preserve='4d 4w' \\"
    echo "            $SOURCE_SNAP $TARGET_BACKUP"
else
    echo "⚠️  Command failed (expected if no btrfs filesystem)"
    echo ""
    echo "ℹ️  To test this feature on your system:"
    echo "   1. Create or find a btrfs filesystem"
    echo "   2. Create some snapshots: mybtrfs snapshot <source> <snap_dir> mydata"
    echo "   3. Create backups: mybtrfs run <source> <snap_dir> <target_dir>"
    echo "   4. Then run: mybtrfs prune --dry-run <snap_dir> <target_dir>"
fi

echo ""
echo "📚 Code references:"
echo "   • retention_preview::format_schedule() — formats the output"
echo "   • retention_preview::compute_age() — parses snapshot names and computes ages"
echo "   • print_prune_report() in cli.rs — integrates with prune command"
echo ""
