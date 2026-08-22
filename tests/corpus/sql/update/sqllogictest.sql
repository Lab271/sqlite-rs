-- Extracted by tools/extract_sql_corpus.py from the vendored
-- sqllogictest subset under tests/corpus/sql/vendor/sqllogictest/.
-- Do not edit by hand; run `make extract-sql-corpus` to regenerate (#70).
UPDATE view1 SET x=2
UPDATE t1 SET x=1 WHERE x>0
UPDATE t1 SET x=2 WHERE x>0
UPDATE t1 SET y='true' WHERE x>0
UPDATE t1 SET y='unknown' WHERE x>0
UPDATE t1 SET x=3
UPDATE t1 SET x=1 WHERE y='unknown'
UPDATE t1 SET x=1 WHERE y='foo'
UPDATE t1 SET x=3+1
UPDATE t1 SET x=3, x=4, x=5
UPDATE t1 SET x=2
UPDATE t1 SET x=x+2
