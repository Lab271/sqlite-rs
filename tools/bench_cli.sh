#!/usr/bin/env bash
# Tier 2 (CLI-to-CLI) bench, per #111/#112: `sqlite-rs dump`/`query` vs
# `sqlite3 .dump`/`sqlite3 "<sql>"`, end-to-end (process startup included —
# that's the point of this tier, unlike tier 1's engine-to-engine numbers).
#
# Usage: ./tools/bench_cli.sh
#   BENCH_FIXTURES_DIR — override fixture location (default target/bench-fixtures)
#   HYPERFINE_JSON_DIR  — where per-comparison JSON exports land (default
#                         target/bench-fixtures/hyperfine, gitignored)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

command -v hyperfine >/dev/null 2>&1 || {
  echo "error: hyperfine not found — install it (e.g. brew install hyperfine)" >&2
  exit 1
}

# shellcheck source=tools/bench_env.sh
source "$REPO_ROOT/tools/bench_env.sh"

BENCH_DIR="${BENCH_FIXTURES_DIR:-$REPO_ROOT/target/bench-fixtures}"
JSON_DIR="${HYPERFINE_JSON_DIR:-$BENCH_DIR/hyperfine}"
mkdir -p "$JSON_DIR"

if [ ! -f "$BENCH_DIR/bench_1mb.db" ] || [ ! -f "$BENCH_DIR/bench_50mb.db" ]; then
  echo "bench fixtures missing, generating them..." >&2
  "$REPO_ROOT/tools/gen_fixtures.sh" --bench
fi

echo "building sqlite-rs (release)..." >&2
cargo build --release --bin sqlite-rs --quiet

SQLITE_RS="$REPO_ROOT/target/release/sqlite-rs"
QUERY_SQL="SELECT id, n, x, f, s FROM bench_data WHERE x > 50000"

for fixture in bench_1mb.db bench_50mb.db; do
  db="$BENCH_DIR/$fixture"

  hyperfine \
    --warmup 3 \
    --export-json "$JSON_DIR/dump_${fixture%.db}.json" \
    --command-name "sqlite-rs dump ($fixture)" "$SQLITE_RS dump $db" \
    --command-name "sqlite3 .dump ($fixture)" "$ORACLE_SQLITE3 $db .dump"

  hyperfine \
    --warmup 3 \
    --export-json "$JSON_DIR/query_${fixture%.db}.json" \
    --command-name "sqlite-rs query ($fixture)" "$SQLITE_RS query $db \"$QUERY_SQL\"" \
    --command-name "sqlite3 query ($fixture)" "$ORACLE_SQLITE3 $db \"$QUERY_SQL\""
done

echo "wrote hyperfine JSON exports to $JSON_DIR"
