#!/usr/bin/env bash
# Locates the pinned oracle sqlite3 (same search order and version pin as
# tools/gen_fixtures.sh) and exports the env vars that point rusqlite's
# build (libsqlite3-sys, no `bundled` feature) at that exact build instead
# of whatever stale/codec system sqlite3 happens to be on PATH.
#
# Usage: `source tools/bench_env.sh` before `cargo bench`/`cargo build` so
# rusqlite links against the pin (#111 fairness rule: oracle version pinned
# in the harness). Assumes a `<prefix>/bin/sqlite3` layout, true of both the
# Homebrew keg and a self-built oracle — not of the bare system binary,
# which is why `find_oracle` below skips it same as gen_fixtures.sh does.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ORACLE_VERSION="$(
  sed -n '/^\[package\.metadata\.oracle\]/,/^\[lints/p' "$REPO_ROOT/Cargo.toml" \
    | sed -n 's/^version *= *"\([^"]*\)".*/\1/p'
)"
[ -n "$ORACLE_VERSION" ] || {
  echo "error: could not read [package.metadata.oracle] version from Cargo.toml" >&2
  return 1 2>/dev/null || exit 1
}

find_oracle() {
  for candidate in "${ORACLE_SQLITE3:-}" /opt/homebrew/opt/sqlite/bin/sqlite3 /usr/local/opt/sqlite/bin/sqlite3; do
    [ -z "$candidate" ] && continue
    if command -v "$candidate" >/dev/null 2>&1; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

ORACLE="$(find_oracle)" || {
  echo "error: no pinned oracle sqlite3 found (checked ORACLE_SQLITE3, Homebrew keg paths)" >&2
  echo "  install it, e.g.: brew install sqlite" >&2
  return 1 2>/dev/null || exit 1
}

FOUND_VERSION="$("$ORACLE" -version | awk '{print $1}')"
if [ "$FOUND_VERSION" != "$ORACLE_VERSION" ]; then
  echo "error: pinned oracle is sqlite3 $ORACLE_VERSION, found $FOUND_VERSION at $ORACLE" >&2
  return 1 2>/dev/null || exit 1
fi

ORACLE_PREFIX="$(cd "$(dirname "$ORACLE")/.." && pwd)"
export ORACLE_SQLITE3="$ORACLE"
export SQLITE3_LIB_DIR="$ORACLE_PREFIX/lib"
export SQLITE3_INCLUDE_DIR="$ORACLE_PREFIX/include"
export DYLD_LIBRARY_PATH="$SQLITE3_LIB_DIR${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
export LD_LIBRARY_PATH="$SQLITE3_LIB_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

echo "bench oracle: $ORACLE (sqlite3 $FOUND_VERSION) — SQLITE3_LIB_DIR=$SQLITE3_LIB_DIR" >&2
