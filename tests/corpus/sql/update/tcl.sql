-- Extracted by tools/extract_sql_corpus.py from the vendored SQLite
-- TCL suite subset (version 3.53.4) under tests/corpus/sql/vendor/tcl/.
-- Do not edit by hand; run `make sql-corpus` to regenerate (#70).
UPDATE tbl3 SET a = 'G' where a = 'g'
UPDATE mytable SET geom = X'1234'
UPDATE y SET f1 = 'x' WHERE f1 = 1
UPDATE sqlite_schema SET sql='CREATE TABLE t1(a INT, b TEXT)' WHERE name LIKE 't1'
UPDATE OR IGNORE t1 SET a=1000
UPDATE OR REPLACE t1 SET a=1001
UPDATE abc SET a=a+1
UPDATE OR IGNORE t5 SET a=a+1 WHERE a=1
UPDATE OR REPLACE t5 SET a=a+1 WHERE a=1
UPDATE sqlite_master SET rootpage = ( SELECT rootpage FROM sqlite_master WHERE name = 't5' ) WHERE name = 't4'
UPDATE t1 SET b=b*2 WHERE b IN (SELECT b FROM t1 WHERE a>8)
UPDATE sqlite_master SET sql='nonsense' WHERE name='t1d'
UPDATE t1 SET a=b
UPDATE t1 SET a=NULL WHERE b%3!=0
UPDATE t1 SET b=b+100
UPDATE t1 SET a=CASE WHEN b%3!=0 THEN b END
UPDATE t1 SET b=b-100
UPDATE t2 SET a=NULL WHERE b%2==0
UPDATE t2 SET a=b, b=b+10000
UPDATE t3 SET a=999 WHERE b%5!=0
UPDATE OR REPLACE t6 SET b=789
UPDATE t9 SET b=c WHERE a in (10,12,20)
UPDATE t2 SET a=NULL WHERE b%5==0
UPDATE OR REPLACE t1 SET a=2 WHERE b=4
UPDATE OR REPLACE t2 SET a=1, b=3 WHERE a=1
UPDATE t4 SET y='lots of data for the row where x=' || x || ' and y=' || y || ' - even more data to fill space'
UPDATE t1 SET a = randstr(10,10) WHERE (rowid%4)==0
UPDATE t6 SET a='xyz'
UPDATE t6 SET a=1
UPDATE t1 SET c='bellum' WHERE c='pax' RETURNING rowid, b, '|'
UPDATE t2 SET b='123' WHERE b='abc' RETURNING (SELECT b FROM t1)
UPDATE t2 SET b='123' WHERE b='abc' RETURNING b
UPDATE t1 SET b=9 WHERE a=1 RETURNING a, b, 'x'
UPDATE t3 SET f=e+100 RETURNING 'U', e, f
UPDATE t1 SET x=x+1 RETURNING x, affinity(x)
UPDATE bug SET x=NULL WHERE id = 20 RETURNING quote(x), x IS NULL
UPDATE savepoint SET release = 5
UPDATE tbl SET a = a * 10, b = b * 10
UPDATE tbl SET b = 1, c = 10
UPDATE tbl SET b = 10
UPDATE tbl SET d = 4 WHERE a = 0
UPDATE tbl SET a = 4, b = 10
UPDATE log SET a = 0
UPDATE tbl SET a = 1 WHERE a = 4
UPDATE OR REPLACE tbl SET a = 1 WHERE a = 4
UPDATE abcd SET a = 100, b = 5*5 WHERE a = 1
UPDATE test1 SET f2=f2*3
UPDATE test1 SET f2=f2/3 WHERE f1<=5
UPDATE test1 SET f2=f2/3 WHERE f1>5
UPDATE test1 SET F2=f1, F1=f2
UPDATE test1 SET f2=f2+1 WHERE f1==8
UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2>800
UPDATE test1 SET f2=f2-1 WHERE f1==8 and f2<800
UPDATE test1 SET f1=f1+1 WHERE f2==128
UPDATE test1 SET f1=f1-1 WHERE f1>100 and f2==128
UPDATE test1 SET f1=f1-1 WHERE f1<=100 and f2==128
UPDATE t1 SET e=e+1 WHERE b IN (SELECT b FROM t1)
UPDATE t1 SET e=e+1 WHERE a IN (SELECT a FROM t1)
UPDATE t1 AS xyz SET e=e+1 WHERE xyz.a IN (SELECT a FROM t1)
UPDATE t1 AS xyz SET e=e+1 WHERE EXISTS(SELECT 1 FROM t1 WHERE t1.a<xyz.a)
UPDATE t2 SET rowid=rowid-1
UPDATE t2 SET rowid=rowid+10000
UPDATE t2 SET rowid=rowid-9999
UPDATE t16 SET a=a
UPDATE t1 SET x=2, y=3 WHERE 3
UPDATE t0 SET c1=345
UPDATE t1 SET a = quote(b) WHERE b>=2
UPDATE t1 SET vkey = 100 WHERE c5 is null
UPDATE t1 SET vkey = 100 WHERE NOT (-10*(select min(vkey) from t1) >= c5)
UPDATE t1 SET vkey = 100 WHERE c5 is null OR NOT (-10*(select min(vkey) from t1) >= c5)
UPDATE t1 SET x=x+100, y=x<=(SELECT min(x) FROM t1) WHERE x<3 OR (1 BETWEEN 0 AND x<=(SELECT min(x)+2 FROM t1))
UPDATE t1 SET b = repeat(b, 100)
UPDATE t4 SET c=c+2 WHERE c>2
UPDATE OR REPLACE b1 SET c=c+10 WHERE a BETWEEN 4 AND 7
UPDATE OR REPLACE c1 SET c=c+10 WHERE d BETWEEN 4 AND 7
UPDATE x1 SET c=c+1 WHERE b='a'
UPDATE d1 SET a = a+2 WHERE a>0 OR b>0
UPDATE OR REPLACE t1 SET x=1
