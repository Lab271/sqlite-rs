#!/usr/bin/env bash
# Test double for oracle_test.rs: reports a non-pinned version with no
# codec, to exercise tools/gen_fixtures.sh's version-mismatch path in
# isolation from the codec-rejection path.
set -euo pipefail
if [ "${1:-}" = "-version" ]; then
  echo "9.99.99 2026-01-01 00:00:00 fake"
  exit 0
fi
echo "THREADSAFE=1"
