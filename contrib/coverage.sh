#!/usr/bin/env bash
# Run test coverage with cargo-llvm-cov.
#
# Prerequisites (one-time setup):
#   cargo install cargo-llvm-cov --locked
#   rustup component add llvm-tools-preview
#
# Usage:
#   contrib/coverage.sh          # text summary to stdout (default)
#   contrib/coverage.sh --html   # HTML report in coverage/
#   contrib/coverage.sh --lcov   # LCOV data in coverage/lcov.info
#
# E2E tests (#[ignore]d, require root+btrfs) are excluded automatically.
# To include them: run with sudo and pass --include-ignored after --.

set -euo pipefail

FORMAT=${1:-text}

# Exclude test helper files from coverage metrics.
IGNORE='(tests?/|test_support)'

case "$FORMAT" in
  --html|html)
    cargo llvm-cov \
      --workspace \
      --ignore-filename-regex "$IGNORE" \
      --html \
      --output-dir coverage/
    echo "Coverage report written to: coverage/index.html"
    ;;
  --lcov|lcov)
    mkdir -p coverage
    cargo llvm-cov \
      --workspace \
      --ignore-filename-regex "$IGNORE" \
      --lcov \
      --output-path coverage/lcov.info
    echo "LCOV data written to: coverage/lcov.info"
    echo "(View with: genhtml coverage/lcov.info -o coverage/ && xdg-open coverage/index.html)"
    ;;
  text|--text)
    cargo llvm-cov \
      --workspace \
      --ignore-filename-regex "$IGNORE" \
      --summary-only
    ;;
  *)
    echo "Usage: $0 [--html | --lcov | text]" >&2
    exit 2
    ;;
esac
