#!/usr/bin/env bash
# Regenerates the three fixture pairs for issue #7:
#
#   fixture.db / fixture.db-wal
#     Primary case: a WAL-mode db with committed frames that have
#     deliberately NOT been checkpointed into the main db file (three
#     separate auto-commit INSERTs, so the same page gets three superseding
#     committed frames — "latest committed frame per page wins").
#
#   fixture_trailing.db / fixture_trailing.db-wal
#     Edge case: a big transaction forces the pager to spill dirty pages
#     into the WAL as non-commit frames (db-size-if-commit == 0) *before*
#     the transaction ever commits — then it's rolled back. The WAL ends
#     with those spilled frames trailing after the last real commit. A
#     reader must show only the pre-existing committed rows, none of the
#     rolled-back insert.
#
#   fixture_stale.db / fixture_stale.db-wal
#     Edge case: a frame lifted from a completely unrelated WAL generation
#     (different random salts) is appended after fixture_stale's own last
#     commit. A reader must reject it on salt mismatch and never surface
#     its poisoned content.
#
# All three snapshots rely on the same trick: sqlite3's default behavior is
# to fully checkpoint (truncate) the WAL when the last connection closes —
# confirmed empirically, and true even with `PRAGMA wal_autocheckpoint=0`
# (that pragma only disables the size-triggered auto-checkpoint, not the
# close-time one). A second connection that opens a read transaction
# BEFORE the writes happen and holds it open blocks that close-time
# checkpoint, so the WAL file keeps its committed-but-unapplied frames
# until we've copied it — then we release the reader (whose own close
# checkpoints the working files away, which is fine — the copies are what
# we keep).
set -euo pipefail
cd "$(dirname "$0")"

rm -f work.db work.db-wal work.db-shm reader.fifo writer.fifo
rm -f fixture.db fixture.db-wal fixture.db-shm
rm -f fixture_bigendian.db fixture_bigendian.db-wal fixture_bigendian.db-shm
rm -f fixture_trailing.db fixture_trailing.db-wal fixture_trailing.db-shm
rm -f fixture_stale.db fixture_stale.db-wal fixture_stale.db-shm poison.wal

# === fixture.db / fixture.db-wal — primary case ===

sqlite3 work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"

mkfifo reader.fifo
{ echo "BEGIN;"; echo "SELECT count(*) FROM t;"; cat reader.fifo; } | sqlite3 work.db &
READER_PID=$!
sleep 0.3

sqlite3 work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(1,'one');"
sqlite3 work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(2,'two');"
sqlite3 work.db "PRAGMA wal_autocheckpoint=0; INSERT INTO t VALUES(3,'three');"

cp work.db fixture.db
cp work.db-wal fixture.db-wal
echo "wrote fixture.db + fixture.db-wal"
ls -la fixture.db fixture.db-wal

echo "ROLLBACK;" > reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm

# === fixture_trailing.db / fixture_trailing.db-wal — spilled, uncommitted ===

sqlite3 work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES(1,'committed-before');"
sqlite3 work.db "PRAGMA wal_checkpoint(FULL);" >/dev/null

{
  echo "PRAGMA cache_spill=1;"
  echo "PRAGMA cache_size=-1;" # 1 KB cache — forces the pager to spill dirty
                                # pages into the WAL well before commit.
  echo "BEGIN;"
  python3 -c "
for i in range(2, 2000):
    print(f\"INSERT INTO t VALUES({i},'row-{i}-padding-to-make-it-bigger-xxxxxxxxxxxxxxxxxxxx');\")
"
} > sql_trailing.txt
mkfifo writer.fifo
( cat sql_trailing.txt writer.fifo | sqlite3 work.db ) &
WRITER_PID=$!
sleep 1

cp work.db fixture_trailing.db
cp work.db-wal fixture_trailing.db-wal
echo "wrote fixture_trailing.db + fixture_trailing.db-wal (uncommitted spill size: $(stat -f%z fixture_trailing.db-wal) bytes)"

echo "ROLLBACK;" > writer.fifo
wait "$WRITER_PID" 2>/dev/null || true
rm -f writer.fifo sql_trailing.txt work.db work.db-wal work.db-shm

# === fixture_stale.db / fixture_stale.db-wal — foreign-salt frame appended ===

# Unrelated WAL generation whose one committed frame we'll lift as "poison".
sqlite3 work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"
mkfifo reader.fifo
{ echo "BEGIN;"; echo "SELECT count(*) FROM t;"; cat reader.fifo; } | sqlite3 work.db &
READER_PID=$!
sleep 0.3
sqlite3 work.db "INSERT INTO t VALUES(999,'STALE-FRAME-MUST-NOT-APPEAR');"
cp work.db-wal poison.wal
echo "ROLLBACK;" > reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm

# The real fixture: its own independent WAL generation (different salts).
sqlite3 work.db "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT);"
mkfifo reader.fifo
{ echo "BEGIN;"; echo "SELECT count(*) FROM t;"; cat reader.fifo; } | sqlite3 work.db &
READER_PID=$!
sleep 0.3
sqlite3 work.db "INSERT INTO t VALUES(10,'ten');"
sqlite3 work.db "INSERT INTO t VALUES(11,'eleven');"

cp work.db fixture_stale.db
cp work.db-wal fixture_stale.db-wal
# Append poison.wal's one frame (skip its 32-byte WAL header, keep only the
# 24-byte frame header + page content) after fixture_stale's own last
# commit. Both used the default 4096-byte page size, so frame sizes match.
tail -c +33 poison.wal >> fixture_stale.db-wal
echo "wrote fixture_stale.db + fixture_stale.db-wal (with one foreign-salt frame appended)"

echo "ROLLBACK;" > reader.fifo
wait "$READER_PID" 2>/dev/null || true
rm -f reader.fifo work.db work.db-wal work.db-shm poison.wal

# === fixture_bigendian.db / fixture_bigendian.db-wal — the other checksum path ===
#
# sqlite3 on this host only ever writes magic 0x377f0682 (native-endian
# checksums, since this host is little-endian) — there's no way to make it
# produce the 0x377f0683 (always-big-endian) variant without a special
# compile flag. To exercise that code path too, synthesize it: same main
# db, same frame content, magic flipped to 0x83, and every checksum
# (header + each frame, in order) recomputed from scratch using explicit
# big-endian arithmetic — independent of the Rust reader under test, so
# this isn't circular verification.
cp fixture.db fixture_bigendian.db
python3 - <<'PYEOF'
import struct

with open("fixture.db-wal", "rb") as f:
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
    frame_header = wal[offset : offset + 8]  # page number + db-size-if-commit
    page_content = wal[offset + 24 : offset + 24 + page_size]
    s1, s2 = cksum_be(frame_header, s1, s2)
    s1, s2 = cksum_be(page_content, s1, s2)
    wal[offset + 16 : offset + 20] = struct.pack(">I", s1)
    wal[offset + 20 : offset + 24] = struct.pack(">I", s2)
    offset += frame_size

with open("fixture_bigendian.db-wal", "wb") as f:
    f.write(wal)
PYEOF
echo "wrote fixture_bigendian.db + fixture_bigendian.db-wal (magic 0x377f0683, big-endian checksums)"

echo "done."
