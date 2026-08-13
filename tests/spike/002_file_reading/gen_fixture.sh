#!/usr/bin/env bash
# Regenerates fixture.db exactly as specified in issue #4.
set -euo pipefail
cd "$(dirname "$0")"
rm -f fixture.db
sqlite3 fixture.db "CREATE TABLE t(a INTEGER, b TEXT, c REAL, d BLOB, e); \
  INSERT INTO t VALUES(42,'hello',3.14,X'DEADBEEF',NULL); \
  INSERT INTO t VALUES(-1,'unicode: héllo→',2.5e300,X'',7);"
echo "wrote fixture.db"
