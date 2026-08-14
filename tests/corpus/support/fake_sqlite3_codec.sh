#!/usr/bin/env bash
# Test double for oracle_test.rs: mimics just enough of `sqlite3 -version`
# and `sqlite3 :memory: "PRAGMA compile_options;"` to exercise
# tools/gen_fixtures.sh's codec-rejection path in isolation from the
# version-mismatch path (real codec binaries we have access to are also
# the wrong version, which would leave that path untested on its own).
set -euo pipefail
if [ "${1:-}" = "-version" ]; then
  echo "3.53.3 2026-06-26 20:14:12 fake"
  exit 0
fi
echo "CODEC=fake"
echo "THREADSAFE=1"
