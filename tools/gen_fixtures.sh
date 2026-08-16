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

# Resolved before the `cd` below, so a relative $0 still finds Cargo.toml.
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Read from Cargo.toml's [package.metadata.oracle] — the one place the sqlite3
# pin is declared. Kept as plain sed so this script needs no python3.
ORACLE_VERSION="$(
  sed -n '/^\[package\.metadata\.oracle\]/,/^\[lints/p' "$REPO_ROOT/Cargo.toml" \
    | sed -n 's/^version *= *"\([^"]*\)".*/\1/p'
)"
[ -n "$ORACLE_VERSION" ] || {
  echo "error: could not read [package.metadata.oracle] version from Cargo.toml" >&2
  exit 1
}

FIXTURES_DIR="${FIXTURES_DIR:-$REPO_ROOT/tests/corpus/fixtures}"
mkdir -p "$FIXTURES_DIR"
cd "$FIXTURES_DIR"
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
#
# Captured into a variable rather than piped straight to `grep -q`: with
# `pipefail` set, grep exiting the instant it finds a match can SIGPIPE a
# still-writing producer, and pipefail then reports that producer's
# non-zero (SIGPIPE) exit over grep's successful match — silently
# swallowing the codec detection. Timing-dependent; only surfaced on CI's
# GNU grep, never locally on macOS's BSD grep.
compile_options="$("$ORACLE" :memory: "PRAGMA compile_options;")"
if grep -qi codec <<<"$compile_options"; then
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

# --- bench fixtures: opt-in, not part of the default corpus regen ---
#
# Generated into target/bench-fixtures/ (under the gitignored target/ tree),
# not tests/corpus/fixtures/: these are sized for wall-clock benching
# (~1MB, ~50MB per #111/#112), not committed as oracle-diff corpus. A
# recursive CTE with an arithmetic PRNG (no random()/randomblob()) keeps
# row content bit-for-bit reproducible without a seeded RNG.
if [ "${1:-}" = "--bench" ]; then
  BENCH_DIR="${BENCH_FIXTURES_DIR:-$REPO_ROOT/target/bench-fixtures}"
  mkdir -p "$BENCH_DIR"

  gen_bench_fixture() {
    fixture_out="$1"
    fixture_rows="$2"
    rm -f "$fixture_out"
    "$ORACLE" "$fixture_out" <<SQL
CREATE TABLE bench_data(
  id INTEGER PRIMARY KEY,
  n INTEGER,
  x INTEGER,
  f REAL,
  s TEXT
);
WITH RECURSIVE seq(i) AS (
  SELECT 1
  UNION ALL
  SELECT i + 1 FROM seq WHERE i < $fixture_rows
)
INSERT INTO bench_data(id, n, x, f, s)
SELECT
  i,
  (i * 2654435761) % 1000000,
  (i * 40503) % 100000,
  CAST((i * 40503) % 100000 AS REAL) / 1000.0,
  CASE WHEN i % 10 < 3 THEN NULL
       ELSE substr(
         'the quick brown fox jumps over the lazy dog while sqlite reads pages from disk',
         1 + (i % 40),
         10 + (i % 40)
       ) || '-' || i
  END
FROM seq;
CREATE INDEX bench_data_x ON bench_data(x);
SQL
    echo "wrote $fixture_out ($fixture_rows rows, $(du -h "$fixture_out" | cut -f1))"
  }

  # Row counts tuned empirically against this schema's average row width to
  # land near the target file sizes; re-tune if the schema changes.
  gen_bench_fixture "$BENCH_DIR/bench_1mb.db" 16700
  gen_bench_fixture "$BENCH_DIR/bench_50mb.db" 830000
  exit 0
fi

rm -rf -- serialtypes encodings pagesizes btrees features invalid journalstates
mkdir -p serialtypes encodings pagesizes btrees features invalid journalstates

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

# --- journalstates/: mid-life files a plain CLI invocation can't produce ---
#
# Both families below rely on the same trick (spike #7 / issue #21, findings
# 1 and 4, tests/spike/004_wal_reading/gen_fixture.sh): sqlite3 auto-recovers
# or auto-checkpoints these transient states the instant a connection opens
# or closes cleanly, so a plain `sqlite3 db "INSERT ..."` invocation can
# never leave one on disk. A backgrounded connection blocked on a fifo holds
# the state open long enough to `cp` both files; forcing a cache spill
# (`cache_spill=1` + a small negative-KB `cache_size` — a small positive
# page count does not reliably spill, see spike #7 finding 4) makes the
# in-flight write visible in the main/WAL file before commit, rather than
# only in the page cache.
cd journalstates

# hot_journal.db / hot_journal.db-journal — simulates a writer that died
# mid-transaction in rollback-journal mode: the journal (with a valid
# header) is still present, and the main db file already has the
# uncommitted, spilled pages written into it. A reader that ignores the
# hot journal and serves the main file's current bytes as committed data
# would show ~1999 rows; the true committed state (which a real sqlite3
# reports once it rolls the hot journal back) is 1 row.
"$ORACLE" work.db "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES(1,'committed-before');"
{
  echo "PRAGMA cache_spill=1;"
  echo "PRAGMA cache_size=-1;"
  echo "BEGIN;"
  python3 -c "
for i in range(2, 2000):
    print(f\"INSERT INTO t VALUES({i},'row-{i}-padding-xxxxxxxxxxxxxxxxxxxx');\")
"
} >sql_hot_journal.txt
mkfifo writer.fifo
(cat sql_hot_journal.txt writer.fifo | "$ORACLE" work.db) &
WRITER_PID=$!
sleep 1
cp work.db hot_journal.db
cp work.db-journal hot_journal.db-journal
echo "ROLLBACK;" >writer.fifo
wait "$WRITER_PID" 2>/dev/null || true
rm -f writer.fifo sql_hot_journal.txt work.db work.db-journal

# wal_pending.db(-wal) — primary case: three separate commits to the same
# page, none checkpointed into the main file (all invisible reading
# wal_pending.db alone; all visible once the WAL is merged in).
"$ORACLE" work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"
mkfifo reader.fifo
{
  echo "BEGIN;"
  echo "SELECT count(*) FROM t;"
  cat reader.fifo
} | "$ORACLE" work.db &
READER_PID=$!
sleep 0.3
"$ORACLE" work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(1,'one');"
"$ORACLE" work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(2,'two');"
"$ORACLE" work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(3,'three');"
cp work.db wal_pending.db
cp work.db-wal wal_pending.db-wal
echo "ROLLBACK;" >reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm

# wal_pending_trailing.db(-wal) — a big transaction spills dirty pages into
# the WAL as non-commit frames, then rolls back: the WAL ends with
# committed-looking-but-not frames trailing after the last real commit. A
# reader must show only the previously committed row.
"$ORACLE" work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES(1,'committed-before');"
"$ORACLE" work.db "PRAGMA wal_checkpoint(FULL);" >/dev/null
{
  echo "PRAGMA cache_spill=1;"
  echo "PRAGMA cache_size=-1;"
  echo "BEGIN;"
  python3 -c "
for i in range(2, 2000):
    print(f\"INSERT INTO t VALUES({i},'row-{i}-padding-to-make-it-bigger-xxxxxxxxxxxxxxxxxxxx');\")
"
} >sql_trailing.txt
mkfifo writer.fifo
(cat sql_trailing.txt writer.fifo | "$ORACLE" work.db) &
WRITER_PID=$!
sleep 1
cp work.db wal_pending_trailing.db
cp work.db-wal wal_pending_trailing.db-wal
echo "ROLLBACK;" >writer.fifo
wait "$WRITER_PID" 2>/dev/null || true
rm -f writer.fifo sql_trailing.txt work.db work.db-wal work.db-shm

# wal_pending_stale.db(-wal) — a committed frame lifted from an unrelated
# WAL generation (different salts) is appended after this fixture's own
# last commit. A reader must reject it on salt mismatch.
"$ORACLE" work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"
mkfifo reader.fifo
{
  echo "BEGIN;"
  echo "SELECT count(*) FROM t;"
  cat reader.fifo
} | "$ORACLE" work.db &
READER_PID=$!
sleep 0.3
"$ORACLE" work.db "INSERT INTO t VALUES(999,'STALE-FRAME-MUST-NOT-APPEAR');"
cp work.db-wal poison.wal
echo "ROLLBACK;" >reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm

"$ORACLE" work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"
mkfifo reader.fifo
{
  echo "BEGIN;"
  echo "SELECT count(*) FROM t;"
  cat reader.fifo
} | "$ORACLE" work.db &
READER_PID=$!
sleep 0.3
"$ORACLE" work.db "INSERT INTO t VALUES(10,'ten');"
"$ORACLE" work.db "INSERT INTO t VALUES(11,'eleven');"
cp work.db wal_pending_stale.db
cp work.db-wal wal_pending_stale.db-wal
# Skip poison.wal's own 32-byte WAL header, keep only its one 24-byte frame
# header + page content (both fixtures use the default 4096-byte page size,
# so frame sizes match).
tail -c +33 poison.wal >>wal_pending_stale.db-wal
echo "ROLLBACK;" >reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm poison.wal

# wal_pending_bigendian.db(-wal) — the other checksum-endianness path (magic
# 0x377f0683). This host's sqlite3 only ever writes native-endian checksums
# (0x377f0682, little-endian here — see spike #7 finding 2: native is the
# *default* encoding, not big-endian, despite what the magic's name
# suggests). Synthesized from wal_pending's content: magic flipped, every
# checksum (header + each frame, in order) recomputed independently in
# Python using explicit big-endian arithmetic.
cp wal_pending.db wal_pending_bigendian.db
python3 - <<'PYEOF'
import struct

with open("wal_pending.db-wal", "rb") as f:
    wal = bytearray(f.read())

page_size = struct.unpack(">I", wal[8:12])[0]
frame_size = 24 + page_size


def cksum_be(data, s1, s2):
    for i in range(0, len(data), 8):
        w0 = struct.unpack_from(">I", data, i)[0]
        w1 = struct.unpack_from(">I", data, i + 4)[0]
        s1 = (s1 + w0 + s2) & 0xFFFFFFFF
        s2 = (s2 + w1 + s1) & 0xFFFFFFFF
    return s1, s2


wal[0:4] = bytes.fromhex("377f0683")
s1, s2 = cksum_be(wal[0:24], 0, 0)
wal[24:28] = struct.pack(">I", s1)
wal[28:32] = struct.pack(">I", s2)

offset = 32
while offset + frame_size <= len(wal):
    frame_header = wal[offset : offset + 8]
    page_content = wal[offset + 24 : offset + 24 + page_size]
    s1, s2 = cksum_be(frame_header, s1, s2)
    s1, s2 = cksum_be(page_content, s1, s2)
    wal[offset + 16 : offset + 20] = struct.pack(">I", s1)
    wal[offset + 20 : offset + 24] = struct.pack(">I", s2)
    offset += frame_size

with open("wal_pending_bigendian.db-wal", "wb") as f:
    f.write(wal)
PYEOF

cat >manifest.txt <<'EOF'
hot_journal.db, hot_journal.db-journal — a rollback-journal writer that
never committed: the journal (valid header) is present, and the main file
already carries the uncommitted, spilled pages (~1999 rows). The true
committed state, which a hot-journal-aware reader must report instead of
those spilled rows, is 1 row.
wal_pending.db(-wal) — three separate commits to the same page, none
checkpointed into the main file: 3 rows (1,2,3), invisible reading
wal_pending.db alone, visible once the WAL is merged in.
wal_pending_trailing.db(-wal) — a big transaction spills dirty pages into
the WAL as non-commit frames, then rolls back. Correct row count: 1
(only the pre-existing committed row).
wal_pending_stale.db(-wal) — a committed frame from an unrelated WAL
generation (different salts) appended after this fixture's own last
commit. Correct rows: 10, 11 — the foreign frame's poisoned row must never
surface.
wal_pending_bigendian.db(-wal) — wal_pending.db's content with the WAL
magic flipped to 0x377f0683 and every checksum independently recomputed in
big-endian arithmetic, exercising the less common checksum-endianness path.
Correct rows: 1, 2, 3 (identical to wal_pending.db).
EOF

cd ..

echo "wrote $(find . -name '*.db' | wc -l | tr -d ' ') fixtures across $(find . -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') families to $(pwd)"
