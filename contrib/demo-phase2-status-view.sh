#!/bin/bash
#
# Demo: Phase 2 — Status View
#
# This script demonstrates the status view feature: `mybtrfs status <source> <target>`
# shows backup health without a side database — purely metadata-derived.
#
# REQUIREMENTS:
# - btrfs filesystem(s) with snapshots and backups
# - mybtrfs binary built and in PATH (or use ./target/debug/mybtrfs)
#
# EXPECTED OUTPUT:
# A human-readable status report showing:
# - Source and target directories
# - Snapshot and backup counts
# - Latest snapshot/backup names and ages (infrastructure ready)
# - Health checks (e.g., "latest backup matches latest snapshot")
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
║ Phase 2 Demo: Status View                                              ║
║                                                                         ║
║ Feature: `status <source> <target>` shows backup health at a glance    ║
║ Location: crates/application/src/status.rs                             ║
║ Tests: 7 unit tests (counts, health checks, edge cases)                ║
║ CLI: crates/cli/src/cli.rs — integrated status command                 ║
╚════════════════════════════════════════════════════════════════════════╝

EOF

echo "📝 About this demo:"
echo "   • Shows backup health without a side database"
echo "   • All data derived from live btrfs metadata"
echo "   • Read-only: no mutations, safe to run anytime"
echo "   • Stateless: re-derives truth each run"
echo ""

echo "🔍 Running: $MYBTRFS status $SOURCE_SNAP $TARGET_BACKUP"
echo ""

# Run the status command
if $MYBTRFS status "$SOURCE_SNAP" "$TARGET_BACKUP" 2>/dev/null; then

    echo ""
    echo "✅ Success! Status shown above."
    echo ""
    echo "📊 What you're seeing:"
    echo "   • Status Report header"
    echo "   • Source: path to snapshot directory"
    echo "   • Target: path to backup directory"
    echo "   • Snapshot count: number of snapshots found"
    echo "   • Backup count: number of backups found"
    echo ""
    echo "🎯 Future enhancements (v1.1+ roadmap):"
    echo "   • Latest snapshot name with age (\"just now\", \"7 days ago\")"
    echo "   • Latest backup name with age"
    echo "   • Health checks:"
    echo "     - ✅ Backup matches latest snapshot (incremental parent OK)"
    echo "     - ⚠️  Backup lags behind latest snapshot (not yet backed up)"
    echo "     - ⚠️  Orphaned snapshots (no corresponding backup)"
    echo ""
    echo "🔧 Implementation status:"
    echo "   ✅ StatusService orchestrates repo queries"
    echo "   ✅ StatusReport structure defined and tested"
    echo "   ✅ CLI integration complete (status command)"
    echo "   ⏳ Age calculations deferred (infrastructure ready in compute_age())"
    echo "   ⏳ Health check details deferred (can compare latest names)"
else
    echo "⚠️  Command failed (expected if no btrfs filesystem)"
    echo ""
    echo "ℹ️  To test this feature on your system:"
    echo "   1. Create or find a btrfs filesystem"
    echo "   2. Create some snapshots: mybtrfs snapshot <source> <snap_dir> mydata"
    echo "   3. Create backups: mybtrfs run <source> <snap_dir> <target_dir>"
    echo "   4. Then run: mybtrfs status <snap_dir> <target_dir>"
fi

echo ""
echo "📚 Code references:"
echo "   • StatusService — orchestrates SubvolumeRepository queries"
echo "   • StatusReport — holds snapshot/backup lists and metadata"
echo "   • Command::Status in cli.rs — CLI dispatch"
echo "   • print_status() in cli.rs — output formatting"
echo ""
echo "🧪 Unit tests:"
echo "   • status_counts_snapshots_and_backups() — verify counts"
echo "   • status_identifies_latest_snapshot_and_backup() — latest detection"
echo "   • status_health_check_* — health criterion tests"
echo "   • status_handles_empty_* — edge cases"
echo ""
