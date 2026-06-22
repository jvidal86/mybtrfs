#!/usr/bin/env bash
#
# Run the root-gated loopback-btrfs end-to-end test for `mybtrfs run`.
#
# The test builds two loopback btrfs images, runs the real `mybtrfs run`, and
# checks that a read-only backup with a received_uuid lands on the target
# (see crates/cli/tests/e2e.rs / documentation/05-e2e-test-spec.md).
#
# It needs root (losetup/mkfs.btrfs/mount). To avoid root-owned build artifacts
# and PATH surprises under sudo, this script:
#   1. builds the test + binary AS YOU (normal user),
#   2. then runs ONLY the compiled test binary under sudo.
# You'll be prompted for your sudo password once.
#
# Usage:   ./scripts/run-e2e.sh
#
set -euo pipefail

# --- locate the repo root (this script lives in <repo>/scripts) ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# --- prerequisites ---
need() { command -v "$1" >/dev/null 2>&1 || { echo "error: required tool '$1' not found in PATH" >&2; exit 1; }; }
need cargo
need sudo
need btrfs
need mkfs.btrfs
need losetup
need mount

# --- best-effort cleanup of leftovers from a hard-crashed previous run ---
# (the test cleans up after itself on success and on a normal failure; this only
#  matters if a prior run was killed.) Needs sudo, so it primes the credential.
echo "==> Clearing any leftover loopback state from a previous run…"
for tag in pool drive; do
  sudo umount "/tmp/mybtrfs-e2e-${tag}-mnt" 2>/dev/null || true
  loopdev="$(losetup -j "/tmp/mybtrfs-e2e-${tag}.img" 2>/dev/null | cut -d: -f1 || true)"
  [ -n "${loopdev}" ] && sudo losetup -d "${loopdev}" 2>/dev/null || true
  sudo rm -rf "/tmp/mybtrfs-e2e-${tag}.img" "/tmp/mybtrfs-e2e-${tag}-mnt" 2>/dev/null || true
done

# --- 1) build the e2e test + the mybtrfs binary as the normal user ---
echo "==> Building the e2e test and the mybtrfs binary (as $(whoami))…"
cargo test --test e2e --no-run

# --- find the freshly built e2e test binary ---
TEST_BIN="$(find target/debug/deps -maxdepth 1 -type f -name 'e2e-*' ! -name '*.d' \
            -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
if [ -z "${TEST_BIN:-}" ] || [ ! -x "${TEST_BIN}" ]; then
  echo "error: could not find the compiled e2e test binary under target/debug/deps" >&2
  exit 1
fi
echo "==> Test binary: ${TEST_BIN}"

# --- 2) run ONLY the test binary under sudo (loopback btrfs needs root) ---
echo "==> Running the e2e under sudo…"
echo
if sudo env "PATH=/usr/sbin:/sbin:/usr/bin:/bin:${PATH}" \
        "${TEST_BIN}" --ignored --nocapture; then
  echo
  echo "==> SUCCESS — 'mybtrfs run' is proven against real btrfs."
else
  status=$?
  echo
  echo "==> FAILED (exit ${status}). Paste the output above back and I'll fix it." >&2
  exit "${status}"
fi
