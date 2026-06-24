#!/bin/bash
#
# Backup Workflow Simulation
#
# This simulates a realistic backup scenario:
# 1. Create snapshots over 10 days
# 2. Create backups for the snapshots
# 3. Run retention preview (see what would be deleted)
# 4. Run status view (see backup health)
#

set -e

TEMP_DIR="/tmp/mybtrfs-demo-$$"
SNAP_DIR="$TEMP_DIR/snapshots"
BACKUP_DIR="$TEMP_DIR/backups"

echo "╔════════════════════════════════════════════════════════════════════════╗"
echo "║        BACKUP WORKFLOW SIMULATION: Retention Preview + Status View      ║"
echo "║                                                                         ║"
echo "║  Scenario: 10 days of snapshots, 7-day retention policy                ║"
echo "╚════════════════════════════════════════════════════════════════════════╝"
echo ""

# Cleanup function
cleanup() {
    echo "Cleaning up: $TEMP_DIR"
    rm -rf "$TEMP_DIR"
}
trap cleanup EXIT

echo "📁 Setting up directories..."
mkdir -p "$SNAP_DIR" "$BACKUP_DIR"

echo "   Created: $SNAP_DIR"
echo "   Created: $BACKUP_DIR"
echo ""

# Create snapshot files for the past 10 days
# Following mybtrfs naming convention: data.YYYYMMDDTHHMM
echo "📸 Creating 10 days of snapshots..."
echo ""

SNAPSHOTS=(
    "data.20260624T1432"  # Day 1 (today)
    "data.20260623T1432"  # Day 2
    "data.20260622T1432"  # Day 3
    "data.20260621T1432"  # Day 4
    "data.20260620T1432"  # Day 5
    "data.20260619T1432"  # Day 6
    "data.20260618T1432"  # Day 7
    "data.20260617T1432"  # Day 8 (outside retention window)
    "data.20260616T1432"  # Day 9 (outside retention window)
    "data.20260615T1432"  # Day 10 (outside retention window)
)

for snap in "${SNAPSHOTS[@]}"; do
    touch "$SNAP_DIR/$snap"
    echo "   ✓ Created snapshot: $snap"
done

echo ""
echo "📊 Snapshot timeline:"
echo "   ──────────────────────────────────────────────────"
echo "   Today        : ${SNAPSHOTS[0]}"
echo "   Yesterday    : ${SNAPSHOTS[1]}"
echo "   7 days ago   : ${SNAPSHOTS[6]} ← Retention boundary"
echo "   8+ days ago  : ${SNAPSHOTS[7]}, ${SNAPSHOTS[8]}, ${SNAPSHOTS[9]} ← Will be deleted"
echo "   ──────────────────────────────────────────────────"
echo ""

# Create backups (simulating incremental backups)
echo "💾 Creating backups for snapshots..."
echo ""

# Create backups for the first 7 snapshots
for i in {0..6}; do
    touch "$BACKUP_DIR/${SNAPSHOTS[$i]}"
    echo "   ✓ Backed up: ${SNAPSHOTS[$i]}"
done

# Note: older backups also exist (showing weekly backups beyond daily retention)
touch "$BACKUP_DIR/data.20260610T1432"  # Weekly backup 2 weeks ago
echo "   ✓ Weekly backup: data.20260610T1432"

echo ""
echo "📊 Backup timeline:"
echo "   7 recent daily backups  : ${SNAPSHOTS[0]} through ${SNAPSHOTS[6]}"
echo "   1 weekly backup         : data.20260610T1432 (14 days ago)"
echo ""

# Show what we have
echo "═════════════════════════════════════════════════════════════════════════"
echo "CURRENT STATE"
echo "═════════════════════════════════════════════════════════════════════════"
echo ""

echo "Snapshots:"
ls -1 "$SNAP_DIR" | sed 's/^/   /'
echo ""
echo "Count: $(ls -1 "$SNAP_DIR" | wc -l) snapshots"
echo ""

echo "Backups:"
ls -1 "$BACKUP_DIR" | sed 's/^/   /'
echo ""
echo "Count: $(ls -1 "$BACKUP_DIR" | wc -l) backups"
echo ""

# Now simulate the retention preview
echo "═════════════════════════════════════════════════════════════════════════"
echo "PHASE 1: RETENTION PREVIEW"
echo "═════════════════════════════════════════════════════════════════════════"
echo ""

echo "$ mybtrfs prune --dry-run $SNAP_DIR $BACKUP_DIR --snapshot-preserve='7d'"
echo ""

echo "=== Snapshots (tab-separated) ==="
echo "action	name	age"
for i in {0..6}; do
    age_days=$((6-$i))
    if [ $age_days -eq 0 ]; then
        age_text="0d"
    else
        age_text="${age_days}d"
    fi
    echo "preserve	${SNAPSHOTS[$i]}	$age_text"
done

for i in {7..9}; do
    age_days=$(($(echo ${SNAPSHOTS[$i]} | cut -c10-11) - 24))
    age_days=$((6 - $i + 7))
    echo "delete	${SNAPSHOTS[$i]}	${age_days}d"
done

echo ""
echo "=== Backups (tab-separated) ==="
echo "action	name	age"
for i in {0..6}; do
    age_days=$((6-$i))
    if [ $age_days -eq 0 ]; then
        age_text="0d"
    else
        age_text="${age_days}d"
    fi
    echo "preserve	${SNAPSHOTS[$i]}	$age_text"
done
echo "preserve	data.20260610T1432	14d"

echo ""
echo "Parse with awk:"
echo "  $ mybtrfs prune --dry-run ... | awk -F'\\t' '\$1 == \"delete\" {print}'"
echo "  delete	data.20260617T1432	7d"
echo "  delete	data.20260616T1432	8d"
echo "  delete	data.20260615T1432	9d"
echo ""
echo "Count deletions:"
echo "  $ mybtrfs prune --dry-run ... | awk -F'\\t' '\$1 == \"delete\" {count++} END {print count}'"
echo "  3"
echo ""

# Now show status view
echo ""
echo "═════════════════════════════════════════════════════════════════════════"
echo "PHASE 2: STATUS VIEW"
echo "═════════════════════════════════════════════════════════════════════════"
echo ""

echo "$ mybtrfs status $SNAP_DIR $BACKUP_DIR"
echo ""

echo "metric	value	detail"
echo "snapshots	10	${SNAPSHOTS[0]}"
echo "backups	8	${SNAPSHOTS[0]}"
echo "latest_backed	yes	${SNAPSHOTS[0]}"
echo ""

echo "Parse with awk:"
echo "  $ mybtrfs status ... | awk -F'\\t' '{print \$1, \$2}'"
echo "  metric value"
echo "  snapshots 10"
echo "  backups 8"
echo "  latest_backed yes"
echo ""

echo "Get snapshot count:"
echo "  $ mybtrfs status ... | awk -F'\\t' '\$1 == \"snapshots\" {print \$2}'"
echo "  10"
echo ""
echo "Get backup count:"
echo "  $ mybtrfs status ... | awk -F'\\t' '\$1 == \"backups\" {print \$2}'"
echo "  8"
echo ""

# Show what happens after prune
echo "═════════════════════════════════════════════════════════════════════════"
echo "AFTER RUNNING: mybtrfs prune --yes --snapshot-preserve='7d' ..."
echo "═════════════════════════════════════════════════════════════════════════"
echo ""

echo "$ mybtrfs status $SNAP_DIR $BACKUP_DIR"
echo ""
echo "metric	value	detail"
echo "snapshots	7	${SNAPSHOTS[0]}"
echo "backups	8	${SNAPSHOTS[0]}"
echo "latest_backed	yes	${SNAPSHOTS[0]}"
echo ""

echo "═════════════════════════════════════════════════════════════════════════"
echo "SUMMARY"
echo "═════════════════════════════════════════════════════════════════════════"
echo ""
echo "Phase 1: Retention Preview"
echo "  • Tab-separated output (action, name, age)"
echo "  • Parse: awk -F'\\t' '\$1 == \"delete\" {print \$2}'"
echo "  • Easy scripting: count, filter, validate deletions"
echo ""
echo "Phase 2: Status View"
echo "  • Tab-separated metrics (metric, value, detail)"
echo "  • Parse: awk -F'\\t' '\$1 == \"metric\" {print \$2}'"
echo "  • Monitor: track snapshot/backup counts over time"
echo ""
echo "Both features use traditional terminal output (no emoji)."
echo "All outputs are designed for grep/awk/cut/sort pipelines."
echo ""
echo "Cleanup: $TEMP_DIR will be removed when script exits"
echo ""
