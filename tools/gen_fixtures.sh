#!/usr/bin/env bash
# Regenerates the fixture corpus in tests/corpus/fixtures/<family>/ from
# scratch. See .openspec/specs/004-corpus/spec.md for the requirements this
# implements and tests/corpus/README.md for a quick orientation.
#
# Pinned oracle (spike 002 finding, #4 / #22): macOS's system sqlite3 is
# compiled with a codec (CODEC=see-cccrypt) and reserves 12 bytes/page even
# unencrypted, which would silently poison every other fixture's
# reserved_space. All fixtures except the dedicated 12-reserved-byte one are
# generated with a pinned non-codec build; that one fixture deliberately
# uses the codec-enabled system binary, since it's the only reliable way to
# produce a structurally valid reserved_space=12 database.
set -euo pipefail
FIXTURES_DIR="${FIXTURES_DIR:-$(dirname "$0")/../tests/corpus/fixtures}"
mkdir -p "$FIXTURES_DIR"
cd "$FIXTURES_DIR"

ORACLE_VERSION="3.53.3"
CODEC_SQLITE3="${CODEC_SQLITE3:-/usr/bin/sqlite3}"

find_oracle() {
  for candidate in "${ORACLE_SQLITE3:-}" /opt/homebrew/opt/sqlite/bin/sqlite3 /usr/local/opt/sqlite/bin/sqlite3 sqlite3; do
    [ -z "$candidate" ] && continue
    if command -v "$candidate" >/dev/null 2>&1; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

ORACLE="$(find_oracle)" || {
  echo "error: no sqlite3 binary found (set ORACLE_SQLITE3 to a non-codec build)" >&2
  exit 1
}

# Codec check runs before the version check so a codec build is always
# reported as "codec", even if it also happens to be the wrong version
# (true of the macOS system binary, which is both codec and a version
# behind the pin) — see spec 004 Requirement 1's two independent scenarios.
if "$ORACLE" :memory: "PRAGMA compile_options;" | grep -qi codec; then
  echo "error: oracle at $ORACLE is codec-enabled — pin a non-codec build instead" >&2
  exit 1
fi

FOUND_VERSION="$("$ORACLE" -version | awk '{print $1}')"
if [ "$FOUND_VERSION" != "$ORACLE_VERSION" ]; then
  echo "error: pinned oracle is sqlite3 $ORACLE_VERSION, found $FOUND_VERSION at $ORACLE" >&2
  echo "  set ORACLE_SQLITE3 to a $ORACLE_VERSION build, or update ORACLE_VERSION in this script" >&2
  exit 1
fi

echo "oracle: $ORACLE (sqlite3 $FOUND_VERSION, non-codec)"

rm -rf -- serialtypes encodings pagesizes btrees features invalid
mkdir -p serialtypes encodings pagesizes btrees features invalid

# --- serialtypes/: every serial type at its edge values, NULL, empty/large blobs ---
"$ORACLE" serialtypes/values.db <<'SQL'
CREATE TABLE t(i INTEGER, r REAL, txt TEXT, blb BLOB);
INSERT INTO t VALUES(NULL, NULL, NULL, NULL);
INSERT INTO t VALUES(0, 0.0, '', X'');
INSERT INTO t VALUES(1, -0.0, 'hello', X'deadbeef');
INSERT INTO t VALUES(-1, 3.14, 'unicode: héllo→', X'00');
INSERT INTO t VALUES(127, 1e308, NULL, NULL);
INSERT INTO t VALUES(-128, -1e308, NULL, NULL);
INSERT INTO t VALUES(32767, NULL, NULL, NULL);
INSERT INTO t VALUES(-32768, NULL, NULL, NULL);
INSERT INTO t VALUES(8388607, NULL, NULL, NULL);
INSERT INTO t VALUES(-8388608, NULL, NULL, NULL);
INSERT INTO t VALUES(2147483647, NULL, NULL, NULL);
INSERT INTO t VALUES(-2147483648, NULL, NULL, NULL);
INSERT INTO t VALUES(140737488355327, NULL, NULL, NULL);
INSERT INTO t VALUES(-140737488355328, NULL, NULL, NULL);
INSERT INTO t VALUES(9223372036854775807, NULL, NULL, NULL);
INSERT INTO t VALUES(-9223372036854775808, NULL, NULL, NULL);
INSERT INTO t VALUES(NULL, NULL, NULL, zeroblob(64));
SQL
cat > serialtypes/manifest.txt <<'EOF'
values.db — every serial type: i8/i16/i24/i32/i48/i64 min/max, 0, -0.0,
huge floats (1e308), NULL, empty and 64-byte blobs, non-ASCII text.
EOF

# --- encodings/: same content, three database-level text encodings ---
for enc_pragma_name in utf8 utf16le utf16be; do
  case "$enc_pragma_name" in
    utf8) pragma_value="UTF-8" ;;
    utf16le) pragma_value="UTF-16le" ;;
    utf16be) pragma_value="UTF-16be" ;;
  esac
  "$ORACLE" "encodings/${enc_pragma_name}.db" <<SQL
PRAGMA encoding = "${pragma_value}";
CREATE TABLE t(txt TEXT);
INSERT INTO t VALUES('unicode: héllo→ 日本語');
SQL
done
cat > encodings/manifest.txt <<'EOF'
utf8.db, utf16le.db, utf16be.db — identical non-ASCII text content, each
under a different database-level text encoding (header byte 56).
EOF

# --- pagesizes/: page size and reserved-bytes boundaries ---
for page_size in 512 65536; do
  "$ORACLE" "pagesizes/page_size_${page_size}.db" <<SQL
PRAGMA page_size=${page_size};
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES(1, 'row one');
INSERT INTO t VALUES(2, 'row two');
SQL
done

"$ORACLE" pagesizes/reserved_bytes_0.db <<'SQL'
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES(1, 'reserved bytes: 0');
SQL

"$CODEC_SQLITE3" pagesizes/reserved_bytes_12.db <<'SQL'
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES(1, 'reserved bytes: 12');
SQL
cat > pagesizes/manifest.txt <<'EOF'
page_size_512.db, page_size_65536.db — page size boundaries, including the
`1` = 65536 header encoding (4096 is covered implicitly by every other
family's fixtures).
reserved_bytes_0.db, reserved_bytes_12.db — both reserved-bytes-per-page
cases (usable_size = page_size - reserved). The 12 case uses the
codec-enabled system sqlite3 deliberately — see the script's oracle notes.
EOF

# --- btrees/: shapes the table/index b-tree cursor layer must handle ---
"$ORACLE" btrees/table_single_page.db <<'SQL'
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES(1, 'a single leaf page');
SQL

"$ORACLE" btrees/table_multipage.db <<'SQL'
CREATE TABLE t(a INTEGER, b TEXT);
WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<3000)
INSERT INTO t SELECT x, 'row number ' || x FROM seq;
SQL

"$ORACLE" btrees/index.db <<'SQL'
CREATE TABLE t(a INTEGER, b TEXT);
CREATE INDEX idx_b ON t(b);
WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<3000)
INSERT INTO t SELECT x, 'row number ' || x FROM seq;
SQL

"$ORACLE" btrees/without_rowid.db <<'SQL'
CREATE TABLE t(k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;
WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<500)
INSERT INTO t SELECT 'key' || x, 'value number ' || x FROM seq;
SQL

"$ORACLE" btrees/overflow_single_page.db <<'SQL'
CREATE TABLE t(a INTEGER, blb BLOB);
INSERT INTO t VALUES(1, zeroblob(6000));
SQL

"$ORACLE" btrees/overflow_multi_page.db <<'SQL'
CREATE TABLE t(a INTEGER, blb BLOB);
INSERT INTO t VALUES(1, zeroblob(60000));
SQL
cat > btrees/manifest.txt <<'EOF'
table_single_page.db — one row, a single leaf page.
table_multipage.db — 3000 rows, forces interior table b-tree nodes.
index.db — an indexed column over 3000 rows, multi-page index b-tree.
without_rowid.db — WITHOUT ROWID table, 500 rows.
overflow_single_page.db — a 6000-byte blob, forces one overflow page.
overflow_multi_page.db — a 60000-byte blob, forces a 14-page overflow chain.
EOF

# --- features/: extensions and modes Tier 0 must read as raw rows ---
"$ORACLE" features/autovacuum.db <<'SQL'
PRAGMA auto_vacuum=FULL;
CREATE TABLE t(a INTEGER, b TEXT);
INSERT INTO t VALUES(1, 'auto-vacuum full');
SQL

"$ORACLE" features/fts5.db <<'SQL'
CREATE VIRTUAL TABLE t USING fts5(txt);
INSERT INTO t VALUES('the quick brown fox');
SQL

"$ORACLE" features/rtree.db <<'SQL'
CREATE VIRTUAL TABLE t USING rtree(id, minX, maxX, minY, maxY);
INSERT INTO t VALUES(1, 0.0, 10.0, 0.0, 10.0);
SQL

"$ORACLE" features/strict_generated.db <<'SQL'
CREATE TABLE t(a INTEGER, b INTEGER GENERATED ALWAYS AS (a*2) STORED) STRICT;
INSERT INTO t(a) VALUES(21);
SQL
cat > features/manifest.txt <<'EOF'
autovacuum.db — PRAGMA auto_vacuum=FULL; page 2 is a pointer-map page
(verified by raw byte inspection — dbstat doesn't surface ptrmap pages
since they aren't part of any b-tree).
fts5.db, rtree.db — virtual tables via FTS5 and R-Tree modules.
strict_generated.db — a STRICT table with a GENERATED ALWAYS ... STORED
column. All raw-row readable per Tier 0 (spec 001 Requirement 4) without
needing query-level support for the extension.
EOF

# --- invalid/: malformed inputs #11's VFS/header layer must reject cleanly ---
: > invalid/empty.db

head -c 50 serialtypes/values.db > invalid/truncated.db

python3 - <<'PY'
data = bytearray(open("serialtypes/values.db", "rb").read())
data[0:16] = b"not a sqlite db!"
open("invalid/magic.db", "wb").write(data)
PY
cat > invalid/manifest.txt <<'EOF'
empty.db — zero-byte file.
truncated.db — a valid database chopped off mid-header (first 50 bytes).
magic.db — otherwise-valid database with its 16-byte magic string overwritten.
EOF

echo "wrote $(find . -name '*.db' | wc -l | tr -d ' ') fixtures across $(find . -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') families to $(pwd)"
