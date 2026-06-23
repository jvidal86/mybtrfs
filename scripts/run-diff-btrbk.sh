#!/usr/bin/env bash
#
# Runner scaffold for the 06 differential-oracle test (T1 — scheduler diff):
# compare mybtrfs's retention survivor set against the real btrbk reference
# oracle (see documentation/06-differential-oracle-test-spec.md).
#
# STATUS: scaffold. The Rust harness (crates/cli/tests/diff_btrbk.rs) is NOT yet
# implemented — writing the fake-`btrfs` shim blind risks silently mismatching
# btrbk's expected `subvolume list` format, so it must be developed against a
# live btrbk. This script (a) checks the environment, (b) captures btrbk's real
# `--format raw` schedule output so the parser can be pinned to ground truth,
# and (c) runs the gated test once it exists.
#
# Why it can't "just run": `btrbk -n -S` still shells out to `btrfs subvolume
# list/show` (they are non-destructive, so -n does not skip them) and btrbk has
# no --now, so a deterministic run needs BOTH a real-or-faked btrfs AND faketime.
#
# Usage:   MYBTRFS_TEST_SANDBOX=1 ./scripts/run-diff-btrbk.sh
#          BTRBK=/path/to/btrbk MYBTRFS_TEST_SANDBOX=1 ./scripts/run-diff-btrbk.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# --- Ring-1 sandbox gate (06 §"Test isolation"): refuse to run on a bare box ---
if [ "${MYBTRFS_TEST_SANDBOX:-}" != "1" ]; then
  echo "refusing to run: set MYBTRFS_TEST_SANDBOX=1 (only the container/VM should)." >&2
  echo "see documentation/06-differential-oracle-test-spec.md (three rings of defense)." >&2
  exit 1
fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: required tool '$1' not found in PATH" >&2; exit 1; }; }
need cargo

# --- locate the btrbk reference oracle (override with $BTRBK) ---
BTRBK="${BTRBK:-$REPO_ROOT/../btrbk/btrbk/btrbk}"
if [ ! -x "$BTRBK" ] && ! command -v "$BTRBK" >/dev/null 2>&1; then
  echo "error: btrbk oracle not found at '$BTRBK' (set \$BTRBK to the checkout)." >&2
  exit 1
fi

# --- faketime is required for a deterministic oracle (btrbk has no --now) ---
if ! command -v faketime >/dev/null 2>&1; then
  echo "error: 'faketime' (libfaketime) not found — the oracle is non-deterministic without it." >&2
  echo "install it (e.g. apt-get install libfaketime) and re-run." >&2
  exit 1
fi

DIFF_TEST="crates/cli/tests/diff_btrbk.rs"
if [ -f "$DIFF_TEST" ]; then
  echo "==> Building + running the gated T1 differential test…"
  cargo test --test diff_btrbk --no-run
  TEST_BIN="$(find target/debug/deps -maxdepth 1 -type f -name 'diff_btrbk-*' ! -name '*.d' \
              -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
  BTRBK="$BTRBK" MYBTRFS_TEST_SANDBOX=1 "${TEST_BIN}" --ignored --nocapture
  exit $?
fi

# --- harness not yet written: capture ground-truth oracle output for the parser ---
cat >&2 <<EOF
==> The Rust harness ($DIFF_TEST) is not implemented yet.
    Next concrete step (per documentation/06 "Implementation notes"): build a
    fake-\`btrfs\` PATH shim returning canned \`subvolume list -a -c -u -q -R\`
    output, then capture btrbk's real raw schedule rows to pin the parser:

      TZ=UTC faketime '2026-06-22 15:31:00' \\
        "$BTRBK" -c <test.conf> -S --format raw run    # NOTE: -S without -q

    Expected row shape (confirmed from btrbk source):
      format="schedule" topic='..' action='..' url='..' host='..' port='..' \\
        path='..' hod='..' dow='..' min='..' h='..' d='..' w='..' m='..' y='..'
EOF
exit 2
