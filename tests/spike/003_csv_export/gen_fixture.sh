#!/usr/bin/env bash
# Regenerates fixture.db per issue #12: 3+ tables, one multi-page (1500
# rows, forces interior table b-tree nodes), one with all value types
# (mirrors spike 002's fixture), one with a large BLOB (forces an
# overflow chain). Also one sqlite_-internal-adjacent case is implicit:
# sqlite_master itself, always present.
set -euo pipefail
cd "$(dirname "$0")"
rm -f fixture.db

sqlite3 fixture.db <<'SQL'
CREATE TABLE bulk(id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE typed(a INTEGER, b TEXT, c REAL, d BLOB, e);
CREATE TABLE big(id INTEGER PRIMARY KEY, payload BLOB);

WITH RECURSIVE seq(x) AS (
  SELECT 1
  UNION ALL
  SELECT x + 1 FROM seq WHERE x < 1500
)
INSERT INTO bulk(id, val) SELECT x, 'row-' || x || '-' || hex(randomblob(8)) FROM seq;

INSERT INTO typed VALUES (42, 'hello', 3.14, X'DEADBEEF', NULL);
INSERT INTO typed VALUES (-1, 'unicode: héllo→', 2.5e300, X'', 7);

INSERT INTO big(id, payload) VALUES (1, randomblob(60000));
SQL

echo "wrote fixture.db"
