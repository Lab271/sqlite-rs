-- Extracted by tools/extract_sql_corpus.py from the vendored SQLite
-- TCL suite subset (version 3.53.4) under tests/corpus/sql/vendor/tcl/.
-- Do not edit by hand; run `make extract-sql-corpus` to regenerate (#70).
SELECT (SELECT sum(x+(SELECT y)) FROM bb) FROM aa
SELECT (SELECT sum(x+y) FROM bb) FROM aa
SELECT min((SELECT count(y) FROM ty)) FROM tx
SELECT max((SELECT a FROM (SELECT count(*) AS a FROM ty) AS s)) FROM tx
SELECT ( SELECT total( (SELECT b FROM x1) ) ) FROM x2
SELECT ( SELECT total( (SELECT 2 FROM x1) ) ) FROM x2
SELECT( SELECT max(b) LIMIT ( SELECT total( (SELECT a FROM t1) ) ) ) FROM t2
WITH c AS(SELECT a) SELECT(SELECT(SELECT string_agg(b, b) LIMIT(SELECT 0.100000 * AVG(DISTINCT(SELECT 0 FROM a ORDER BY b, b, b)))) FROM a GROUP BY b, b, b) FROM a EXCEPT SELECT b FROM a ORDER BY b, b, b
SELECT ( SELECT t2.b FROM (SELECT t2.b AS c FROM t1) GROUP BY 1 HAVING t2.b ) FROM t2 GROUP BY 'constant_string'
SELECT ( SELECT c FROM (SELECT t2.b AS c FROM t1) GROUP BY c HAVING t2.b ) FROM t2 GROUP BY 'constant_string'
SELECT sum(amount), name from invoice group by name having (select v > 6 from (select sum(amount) v) t)
SELECT (select 1 from (select sum(amount))) FROM invoice
SELECT (SELECT y FROM (SELECT sum(x) AS y) AS t2 ) FROM t1
SELECT ( SELECT y FROM ( SELECT z AS y FROM (SELECT sum(x) AS z) AS t2 ) ) FROM t1
SELECT ( SELECT a FROM ( SELECT y AS a FROM ( SELECT z AS y FROM (SELECT sum(x) AS z) AS t2 ) ) ) FROM t1
WITH out(i, j, k) AS ( VALUES(1234, 5678, 9012) ) SELECT ( SELECT ( SELECT min(abc) = ( SELECT ( SELECT 1234 fROM (SELECT abc) ) ) FROM ( SELECT sum( out.i ) + ( SELECT sum( out.i ) ) AS abc FROM (SELECT out.j) ) ) ) FROM out
SELECT ( SELECT min(y) + (SELECT x) FROM ( SELECT sum(a) AS x, b AS y FROM t2 ) ) FROM t1
SELECT ( SELECT min(y) + (SELECT (SELECT x)) FROM ( SELECT sum(a) AS x, b AS y FROM t2 ) ) FROM t1
SELECT ( SELECT (SELECT x) FROM ( SELECT sum(a) AS x, b AS y FROM t2 ) GROUP BY y ) FROM t1
SELECT ( SELECT (SELECT (SELECT x)) FROM ( SELECT sum(a) AS x, b AS y FROM t2 ) GROUP BY y ) FROM t1
SELECT * FROM t0 WHERE EXISTS (SELECT 1 FROM t1 GROUP BY c3 HAVING ( SELECT count(*) FROM (SELECT 1 UNION ALL SELECT sum(DISTINCT c1) ) ) ) BETWEEN 1 AND 1
SELECT type, name, tbl_name FROM objlist ORDER BY tbl_name, type desc, name
SELECT * FROM t4 WHERE a = 'main'
SELECT * FROM t4 WHERE a = 'aux'
SELECT * FROM t5
SELECT * FROM t5 WHERE b = 'main'
SELECT * FROM aux.t5 WHERE b = 'aux'
SELECT * FROM tbl1
SELECT * FROM tbl2
SELECT * FROM temp.sqlite_master WHERE type = 'trigger'
SELECT a FROM tbl1
SELECT a FROM tbl2
SELECT name FROM sqlite_master WHERE type='table' AND name NOT GLOB 'sqlite*'
SELECT max(oid) FROM sqlite_master
SELECT typeof(a), a, typeof(b), b FROM t1
SELECT sum(b) FROM t2
SELECT a, sum(b) FROM t2 GROUP BY a
SELECT SQLITE_RENAME_COLUMN(0,0,0,0,0,0,0,0,0)
SELECT name FROM sqlite_master WHERE name GLOB 'xyz*'
SELECT name FROM sqlite_master WHERE name GLOB 'sqlite_autoindex*'
SELECT * FROM v1
SELECT name FROM sqlite_master WHERE name GLOB 't3102*' ORDER BY 1
SELECT * FROM t16a ORDER BY a
SELECT * FROM t16a_rn ORDER BY a
SELECT name FROM sqlite_schema WHERE sql LIKE '%t2%'
SELECT name FROM sqlite_schema WHERE sql LIKE '%t3%' ORDER BY name
SELECT name, type FROM sqlite_schema ORDER BY name
SELECT sql FROM sqlite_master
SELECT * FROM eee
SELECT * FROM fff
SELECT * FROM vvv
SELECT sql FROM sqlite_master WHERE name='vvv'
SELECT * FROM uuu
SELECT sql FROM sqlite_master WHERE name='uuu'
SELECT * FROM ttt
SELECT sql FROM sqlite_temp_master WHERE name='ttt'
SELECT squish(sql) FROM sqlite_master WHERE name = 'tr1'
SELECT * FROM v
SELECT * FROM vv
SELECT sql FROM sqlite_master WHERE name = 'x2'
SELECT sqlite_rename_table(db, 0, 0, sql, zOld, zNew, bTemp) FROM ddd
SELECT sql FROM aux.sqlite_master WHERE name = 'c1'
SELECT sql FROM sqlite_temp_master
SELECT sql FROM sqlite_master WHERE type='trigger'
SELECT * FROM ggg
SELECT name, tbl_name FROM sqlite_temp_master
SELECT * FROM t1
SELECT * FROM x
SELECT sql FROM sqlite_master WHERE name = 'y'
SELECT * FROM z1_segments
SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL
SELECT * FROM t2
SELECT * FROM v0
SELECT sql FROM sqlite_schema WHERE name='v0'
SELECT * FROM v2
SELECT sql FROM sqlite_schema WHERE name='v2'
SELECT * FROM v3
SELECT sql FROM sqlite_schema ORDER BY rowid
SELECT quote(a) FROM t1 ORDER BY +a
SELECT * FROM ffff
SELECT sql FROM sqlite_master WHERE name LIKE 'c%'
SELECT sql FROM sqlite_master WHERE type = 'trigger'
SELECT a,b,c FROM t1 UNION SELECT d,e,f FROM t1 ORDER BY b,c
SELECT type, name, tbl_name, sql FROM sqlite_master WHERE type='table' AND name!='t1'
SELECT sql FROM sqlite_master WHERE tbl_name = 't2'
SELECT sql FROM sqlite_schema WHERE name = 't1'
SELECT sql FROM sqlite_schema WHERE type='trigger'
SELECT sql FROM sqlite_schema WHERE type='view'
WITH s(i) AS ( SELECT 1 UNION ALL SELECT i+1 FROM s WHERE i<50000 ) INSERT INTO t1 SELECT NULL, i, 5.0 FROM s
SELECT rr FROM t1 LIMIT 1
SELECT sql FROM sqlite_schema
SELECT count(*) FROM sqlite_master WHERE name='sqlite_stat1'
SELECT * FROM sqlite_stat1 WHERE idx NOT NULL
SELECT * FROM sqlite_stat1
SELECT * FROM sqlite_stat1 ORDER BY idx
SELECT idx, stat FROM sqlite_stat1 ORDER BY idx
SELECT * FROM t4 WHERE x=1234
SELECT DISTINCT idx FROM sqlite_stat1 ORDER BY 1
SELECT DISTINCT tbl FROM sqlite_stat1 ORDER BY 1
SELECT DISTINCT idx FROM sqlite_stat4 ORDER BY 1
SELECT DISTINCT tbl FROM sqlite_stat4 ORDER BY 1
SELECT tbl FROM sqlite_stat1 WHERE idx IS NULL ORDER BY tbl
SELECT * FROM t1 WHERE b>7223372036854775
SELECT * FROM t1 LEFT JOIN t2 ON (x BETWEEN 1 AND 3)
SELECT * FROM t1 LEFT JOIN t2 ON (x BETWEEN 5 AND 7)
SELECT x'616263'
SELECT typeof(x'616263')
SELECT CAST(x'616263' AS text)
SELECT typeof(CAST(x'616263' AS text))
SELECT CAST(x'616263' AS numeric)
SELECT typeof(CAST(x'616263' AS numeric))
SELECT CAST(x'616263' AS blob)
SELECT typeof(CAST(x'616263' AS blob))
SELECT CAST(x'616263' AS integer)
SELECT typeof(CAST(x'616263' AS integer))
SELECT null
SELECT typeof(NULL)
SELECT CAST(NULL AS text)
SELECT typeof(CAST(NULL AS text))
SELECT CAST(NULL AS numeric)
SELECT typeof(CAST(NULL AS numeric))
SELECT CAST(NULL AS blob)
SELECT typeof(CAST(NULL AS blob))
SELECT CAST(NULL AS integer)
SELECT typeof(CAST(NULL AS integer))
SELECT 123
SELECT typeof(123)
SELECT CAST(123 AS text)
SELECT typeof(CAST(123 AS text))
SELECT CAST(123 AS numeric)
SELECT typeof(CAST(123 AS numeric))
SELECT CAST(123 AS blob)
SELECT typeof(CAST(123 AS blob))
SELECT CAST(123 AS integer)
SELECT typeof(CAST(123 AS integer))
SELECT 123.456
SELECT typeof(123.456)
SELECT CAST(123.456 AS text)
SELECT typeof(CAST(123.456 AS text))
SELECT CAST(123.456 AS numeric)
SELECT typeof(CAST(123.456 AS numeric))
SELECT CAST(123.456 AS blob)
SELECT typeof(CAST(123.456 AS blob))
SELECT CAST(123.456 AS integer)
SELECT typeof(CAST(123.456 AS integer))
SELECT '123abc'
SELECT typeof('123abc')
SELECT CAST('123abc' AS text)
SELECT typeof(CAST('123abc' AS text))
SELECT CAST('123abc' AS numeric)
SELECT typeof(CAST('123abc' AS numeric))
SELECT CAST('123abc' AS blob)
SELECT typeof(CAST('123abc' AS blob))
SELECT CAST('123abc' AS integer)
SELECT typeof(CAST('123abc' AS integer))
SELECT CAST('123.5abc' AS numeric)
SELECT CAST('123.5abc' AS integer)
SELECT CAST(null AS REAL)
SELECT typeof(CAST(null AS REAL))
SELECT CAST(1 AS REAL)
SELECT typeof(CAST(1 AS REAL))
SELECT CAST('1' AS REAL)
SELECT typeof(CAST('1' AS REAL))
SELECT CAST('abc' AS REAL)
SELECT typeof(CAST('abc' AS REAL))
SELECT CAST(x'31' AS REAL)
SELECT typeof(CAST(x'31' AS REAL))
SELECT CAST(9223372036854774800 AS real)
SELECT CAST(CAST(9223372036854774800 AS real) AS integer)
SELECT CAST(-9223372036854774800 AS integer)
SELECT CAST(-9223372036854774800 AS numeric)
SELECT CAST(-9223372036854774800 AS real)
SELECT CAST(CAST(-9223372036854774800 AS real) AS integer)
SELECT CAST(CAST('9223372036854774800' AS real) AS integer)
SELECT CAST(CAST('-9223372036854774800' AS real) AS integer)
SELECT CAST(x'39323233333732303336383534373734383030' AS integer)
SELECT CAST(x'39323233333732303336383534373734383030' AS numeric)
SELECT CAST(x'39323233333732303336383534373734383030' AS real)
SELECT CAST(CAST(x'39323233333732303336383534373734383030' AS real) AS integer)
SELECT '' - 2851427734582196970
SELECT 0 - 2851427734582196970
SELECT '' - 1
SELECT CAST(c0 AS NUMERIC) FROM t0
SELECT -'.'
SELECT '.'+0
SELECT -CAST('.' AS numeric)
SELECT quote(X'310032003300')==quote(substr(X'310032003300', 1))
SELECT CAST(X'310032003300' AS TEXT) ==CAST(substr(X'310032003300', 1) AS TEXT)
SELECT v1.c0 FROM v1, t0 WHERE v1.c0=0
SELECT x, typeof(x) FROM (SELECT CAST(4 AS NUMERIC) AS x) JOIN dual
SELECT x, typeof(x) FROM dual CROSS JOIN (SELECT CAST(4 AS NUMERIC) AS x)
SELECT x, typeof(x) FROM (SELECT CAST(4.0 AS NUMERIC) AS x) JOIN dual
SELECT x, typeof(x) FROM dual CROSS JOIN (SELECT CAST(4.0 AS NUMERIC) AS x)
VALUES(CAST(44 AS REAL)),(55)
SELECT CAST(44 AS REAL) AS 'm' UNION ALL SELECT 55
SELECT * FROM (VALUES(CAST(44 AS REAL)),(55))
SELECT * FROM (SELECT CAST(44 AS REAL) AS 'm' UNION ALL SELECT 55)
SELECT * FROM dual CROSS JOIN (VALUES(CAST(44 AS REAL)),(55))
SELECT * FROM dual CROSS JOIN (SELECT CAST(44 AS REAL) AS 'm' UNION ALL SELECT 55)
SELECT name, type FROM pragma_table_info('v1')
SELECT type FROM pragma_table_info('v2')
SELECT c FROM t1 ORDER BY c
SELECT c FROM t1
SELECT x FROM t2
SELECT c FROM t2 ORDER BY b
SELECT a FROM t1 ORDER BY b
SELECT x FROM t3
SELECT count(*), min(a), max(b) FROM t1
SELECT b FROM t1 WHERE a=1000
SELECT count(*) FROM t1
SELECT b FROM t1 WHERE a=1001
SELECT * FROM t3
SELECT * FROM t4
SELECT * FROM abc
SELECT * FROM t13
SELECT a FROM t1 ORDER BY a
SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1
SELECT * FROM table1 ORDER BY f1
SELECT count(*) FROM table1
SELECT f1 FROM table1 ORDER BY f1
SELECT count(*) FROM table2
SELECT f1 FROM table1 WHERE f1<10 ORDER BY f1
SELECT f1 FROM table2 WHERE f1<10 ORDER BY f1
SELECT f1 FROM table2 ORDER BY f1
SELECT f1 FROM table1
SELECT f1 FROM table2
SELECT * FROM cnt
SELECT * FROM t1 WHERE a='1' AND b='2'
WITH RECURSIVE cnt(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM cnt WHERE x<20) INSERT INTO t11(a,b) SELECT x, (x*17)%100 FROM cnt
SELECT * FROM t11
SELECT * FROM t0
SELECT * FROM q WHERE id='id.1'
SELECT * FROM q
SELECT x FROM t1
SELECT i FROM t1 ORDER BY i
SELECT a FROM t1 WHERE b=2
SELECT (SELECT DISTINCT o.a FROM t1 AS i) FROM t1 AS o ORDER BY rowid
SELECT quote(x) FROM t2 ORDER BY 1
SELECT DISTINCT x FROM t1 ORDER BY x ASC
SELECT DISTINCT x FROM t1 ORDER BY x DESC
SELECT DISTINCT x FROM t1 ORDER BY x
SELECT (SELECT 'mmm' UNION SELECT DISTINCT max(name) ORDER BY 1) FROM sqlite_master
WITH t2(b) AS ( SELECT DISTINCT y FROM t5 ORDER BY y ) SELECT * FROM t4 CROSS JOIN t3 CROSS JOIN t1 WHERE (t1.a=t3.a) AND (SELECT count(*) FROM t2 AS y WHERE t4.x!='abc')=t1.a
SELECT DISTINCT pid FROM person where pid = 10
SELECT DISTINCT a, b FROM t1 ORDER BY a, b
SELECT DISTINCT a COLLATE nocase, b COLLATE nocase FROM t1 ORDER BY a COLLATE nocase, b COLLATE nocase
SELECT DISTINCT 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1 ORDER BY 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x','x','x','x','x','x','x', 'x','x','x','x'
SELECT * FROM test1 ORDER BY a
SELECT CURRENT_TIME
SELECT CURRENT_DATE
SELECT CURRENT_TIMESTAMP
SELECT CURRENT_TIME==time('now')
SELECT CURRENT_DATE==date('now')
SELECT CURRENT_TIMESTAMP==datetime('now')
SELECT round(-('-'||'123'))
SELECT typeof(+9223372036854775807)
SELECT typeof(+000000009223372036854775807)
SELECT typeof(-9223372036854775808)
SELECT typeof(-00000009223372036854775808)
SELECT 0+'9223372036854775807'
SELECT '9223372036854775807'+0
SELECT 0+'9223372036854775808'
SELECT "" <= ''
SELECT '' <= ""
SELECT count(*) FROM t1 WHERE (x OR (8==9)) != (CASE WHEN x THEN 1 ELSE 0 END)
SELECT count(*) FROM t1 WHERE (x OR (8==9)) != (NOT NOT x)
SELECT sum(NOT x) FROM t1 WHERE x
SELECT sum(CASE WHEN x THEN 0 ELSE 1 END) FROM t1 WHERE x
SELECT implies_nonnull_row( (b=1 AND 0)>(b=3 AND 0),a) FROM dual LEFT JOIN t1
SELECT implies_nonnull_row( (b=1 AND 0)>(b=3 AND a=4),a) FROM dual LEFT JOIN t1
SELECT implies_nonnull_row( (b=1 AND a=2)>(b=3 AND a=4),a) FROM dual LEFT JOIN t1
SELECT t1 FROM tbl1 ORDER BY t1
SELECT length(t1) FROM tbl1 ORDER BY t1
SELECT octet_length(t1) FROM tbl1 ORDER BY t1
SELECT length(t1), count(*) FROM tbl1 GROUP BY length(t1) ORDER BY length(t1)
SELECT coalesce(length(a),-1) FROM t2
SELECT octet_length(12345)
SELECT octet_length(NULL)
SELECT octet_length(7.5)
SELECT octet_length(x'30313233')
WITH c(x) AS (VALUES(char(350,351,352,353,354))) SELECT length(x), octet_length(x) FROM c
SELECT substr(t1,1,2) FROM tbl1 ORDER BY t1
SELECT substr(t1,2,1) FROM tbl1 ORDER BY t1
SELECT substr(t1,-1,1) FROM tbl1 ORDER BY t1
SELECT substr(t1,-1,2) FROM tbl1 ORDER BY t1
SELECT t1 FROM tbl1 ORDER BY substr(t1,2,20)
SELECT substr(a,1,1) FROM t2
SELECT substr(a,2,2) FROM t2
SELECT substr('abcdefg',0x100000001,2)
SELECT substr('abcdefg',1,0x100000002)
SELECT quote(substr(x'313233343536373839',0x7ffffffffffffffe,5))
SELECT t1 FROM tbl1
SELECT abs(a) FROM t2
SELECT abs(t1) FROM tbl1
SELECT coalesce(round(a,2),'nil') FROM t2
SELECT round(t1,2) FROM tbl1
SELECT typeof(round(5.1,1))
SELECT typeof(round(5.1))
SELECT round(40223.4999999999)
SELECT round(40224.4999999999)
SELECT round(1234567890123.35,1)
SELECT round(1234567890123.445,2)
SELECT round(123.456 , 4294967297)
SELECT upper(t1) FROM tbl1
SELECT lower(upper(t1)) FROM tbl1
SELECT upper(a), lower(a) FROM t2
SELECT coalesce(a,'xyz') FROM t2
SELECT coalesce(upper(a),'nil') FROM t2
SELECT coalesce(nullif(1,1),'nil')
SELECT coalesce(nullif(1,2),'nil')
SELECT coalesce(nullif(1,NULL),'nil')
SELECT last_insert_rowid()
SELECT sum(a), count(a), round(avg(a),2), min(a), max(a), count(*) FROM t2
SELECT sum(a), count(a), avg(a), min(a), max(a), count(*) FROM t2
SELECT max('z+'||a||'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP') FROM t2
SELECT min('z+'||a||'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP') FROM t3
SELECT max('z+'||a||'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP') FROM t3
SELECT sum(x) FROM (SELECT '9223372036' || '854775807' AS x UNION ALL SELECT -9223372036854775807)
SELECT typeof(sum(x)) FROM (SELECT '9223372036' || '854775807' AS x UNION ALL SELECT -9223372036854775807)
SELECT typeof(sum(x)) FROM (SELECT '9223372036' || '854775808' AS x UNION ALL SELECT -9223372036854775807)
SELECT sum(x)>0.0 FROM (SELECT '9223372036' || '854775808' AS x UNION ALL SELECT -9223372036850000000)
SELECT sum(x)>0 FROM (SELECT '9223372036' || '854775808' AS x UNION ALL SELECT -9223372036850000000)
SELECT random() is not null
SELECT typeof(random())
SELECT randomblob(32) is not null
SELECT typeof(randomblob(32))
SELECT length(randomblob(32)), length(randomblob(-5)), length(randomblob(2000))
SELECT hex(x'00112233445566778899aAbBcCdDeEfF')
SELECT hex(replace('abcdefg','ef','12'))
SELECT hex(replace('abcdefg','','12'))
WITH RECURSIVE c(x) AS ( VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<1040 ) SELECT count(*), sum(length(replace(printf('abc%.*cxyz',x,'m'),'m','nnnn'))-(6+x*4)) FROM c
SELECT testfunc( 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'int', 1234 )
SELECT testfunc( 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'string', NULL )
SELECT testfunc( 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'double', 1.234 )
SELECT testfunc( 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'int', 1234, 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'string', NULL, 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'double', 1.234, 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'int', 1234, 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'string', NULL, 'string', 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ', 'double', 1.234 )
SELECT sqlite_version(*)
SELECT test_destructor('hello world'), test_destructor_count()
SELECT test_destructor16('hello world'), test_destructor_count()
SELECT test_destructor_count()
SELECT test_destructor('hello')||' world'
SELECT min(test_destructor(x)), max(test_destructor(x)) FROM t4
SELECT test_auxdata('hello world')
SELECT test_auxdata('hello world') FROM t4
SELECT test_auxdata('hello world', 123) FROM t4
SELECT test_auxdata('hello world', a) FROM t4
SELECT test_auxdata('hello'||'world', a) FROM t4
SELECT test_auxdata('constant') FROM t4
SELECT quote(a), quote(b) FROM tbl2
SELECT quote(4.2e+859), quote(-7.8e+904)
SELECT sum(x) FROM t5
SELECT sum(x), total(x) FROM t5
SELECT sum(x) - ((1<<62)+1) from t6
SELECT typeof(sum(x)) FROM t6
SELECT sum(-9223372036854775805)
SELECT match(a,b) FROM t1 WHERE 0
SELECT typeof(replace('This is the main test string', NULL, 'ALT'))
SELECT typeof(replace(NULL, 'main', 'ALT'))
SELECT typeof(replace('This is the main test string', 'main', NULL))
SELECT replace('This is the main test string', 'main', 'ALT')
SELECT replace('This is the main test string', 'main', 'larger-main')
SELECT typeof(replace(1,'',0))
SELECT trim(' hi ')
SELECT ltrim(' hi ')
SELECT rtrim(' hi ')
SELECT trim(' hi ','xyz')
SELECT ltrim(' hi ','xyz')
SELECT rtrim(' hi ','xyz')
SELECT trim('xyxzy hi zzzy','xyz')
SELECT ltrim('xyxzy hi zzzy','xyz')
SELECT rtrim('xyxzy hi zzzy','xyz')
SELECT hex(trim(x'c280e1bfbff48fbfbf6869',x'6162e1bfbfc280'))
SELECT hex(trim(x'6869c280e1bfbff48fbfbf61', x'6162e1bfbfc280f48fbfbf'))
SELECT hex(trim(x'ceb1ceb2ceb3',x'ceb1'))
SELECT typeof(trim(NULL))
SELECT typeof(trim(NULL,'xyz'))
SELECT typeof(trim('hello',NULL))
SELECT trim('xyzzy',x'c0808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080')
SELECT legacy_count() FROM t6
SELECT group_concat(t1), string_agg(t1,',') FROM tbl1
SELECT group_concat(t1,' '), string_agg(t1,' ') FROM tbl1
SELECT group_concat(t1,' ' || rowid || ' ') FROM tbl1
SELECT group_concat(NULL,t1) FROM tbl1
SELECT group_concat(t1,NULL), string_agg(t1,NULL) FROM tbl1
SELECT 'BEGIN-'||group_concat(t1) FROM tbl1
SELECT group_concat(CASE t1 WHEN 'this' THEN '' ELSE t1 END) FROM tbl1
SELECT group_concat(CASE WHEN t1!='software' THEN '' ELSE t1 END) FROM tbl1
SELECT group_concat(CASE t1 WHEN 'this' THEN null ELSE t1 END) FROM tbl1
SELECT group_concat(CASE WHEN t1!='software' THEN null ELSE t1 END) FROM tbl1
SELECT group_concat(CASE t1 WHEN 'this' THEN '' WHEN 'program' THEN null ELSE t1 END) FROM tbl1
SELECT typeof(group_concat(x)) FROM (SELECT '' AS x)
SELECT typeof(group_concat(x,'')) FROM (SELECT '' AS x UNION ALL SELECT '')
SELECT test_isolation(t1,t1) FROM tbl1
SELECT typeof(c), typeof(d), typeof(e), typeof(f), typeof(g), typeof(h), typeof(i) FROM t29b
SELECT length(f), length(g), length(h), length(i) FROM t29b
SELECT quote(f), quote(g), quote(h), quote(i) FROM t29b
SELECT unicode('\u00A2')
SELECT unicode('\u20AC')
SELECT char(), length(char()), typeof(char())
SELECT test_frombind(1,2,3,4)
SELECT test_frombind(1,2,?,4)
SELECT test_frombind(1,(?),4,?+7)
SELECT * FROM (SELECT testdirectonly(15)) AS v33
WITH c(x) AS (SELECT testdirectonly(15)) SELECT * FROM c
SELECT coalesce(x, abs(-9223372036854775808)) FROM t1
SELECT coalesce(x, 'xyz' LIKE printf('%.1000000c','y')) FROM t1
SELECT 123 -> 456
SELECT 123 ->> 456
WITH t1(x) AS (VALUES(9e+999)) SELECT sum(x), avg(x), total(x) FROM t1
WITH t1(x) AS (VALUES(-9e+999)) SELECT sum(x), avg(x), total(x) FROM t1
WITH RECURSIVE c(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM c WHERE n<1) SELECT sum(1.7976931348623157e308), avg(1.7976931348623157e308), total(1.7976931348623157e308) FROM c
SELECT 'Supercalifragilisticexpialidocious'
SELECT SUBSTR('Supercalifragilisticexpialidocious', 0)
SELECT SUBSTR('Supercalifragilisticexpialidocious', 1)
SELECT SUBSTR('Supercalifragilisticexpialidocious', -0)
SELECT SUBSTR('Supercalifragilisticexpialidocious', -1)
SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, 1)
SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, 2)
SELECT SUBSTR('Supercalifragilisticexpialidocious', -0, 1)
SELECT SUBSTR('Supercalifragilisticexpialidocious', -1, 0)
SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, -1)
SELECT SUBSTR('Supercalifragilisticexpialidocious', 0, -2)
SELECT '' IN (zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB( zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(zerobloB(1) )))))))))))))))))))))))))))))))))))))))))))))))))))))))))))))
SELECT a FROM t1 WHERE b BETWEEN 10 AND 50 ORDER BY a
SELECT a FROM t1 WHERE b NOT BETWEEN 10 AND 50 ORDER BY a
SELECT a FROM t1 WHERE b BETWEEN a AND a*5 ORDER BY a
SELECT a FROM t1 WHERE b NOT BETWEEN a AND a*5 ORDER BY a
SELECT a FROM t1 WHERE b BETWEEN a AND a*5 OR b=512 ORDER BY a
SELECT a+ 100*(a BETWEEN 1 and 3) FROM t1 ORDER BY b
SELECT a FROM t1 WHERE b IN (8,12,16,24,32) ORDER BY a
SELECT a FROM t1 WHERE b NOT IN (8,12,16,24,32) ORDER BY a
SELECT a FROM t1 WHERE b IN (8,12,16,24,32) OR b=512 ORDER BY a
SELECT a FROM t1 WHERE b NOT IN (8,12,16,24,32) OR b=512 ORDER BY a
SELECT a+100*(b IN (8,16,24)) FROM t1 ORDER BY b
SELECT a FROM t1 WHERE b IN (b+8,64)
SELECT a FROM t1 WHERE b IN (max(5,10,b),20)
SELECT a FROM t1 WHERE b IN (8*2,64/2) ORDER BY b
SELECT a FROM t1 WHERE b IN (max(5,10),20)
SELECT a FROM t1 WHERE min(0,b IN (a,30))
SELECT a FROM t1 WHERE b IN (SELECT b FROM t1 WHERE a<5) ORDER BY a
SELECT a FROM t1 WHERE b IN (SELECT b FROM t1 WHERE a<5) OR b==512 ORDER BY a
SELECT a + 100*(b IN (SELECT b FROM t1 WHERE a<5)) FROM t1 ORDER BY b
SELECT b FROM t1 ORDER BY b
SELECT * FROM t1 WHERE a IN ( 'Do','an','IN','with','a','constant','RHS','but','where','the', 'has','many','elements','We','need','to','test','that', 'collisions','hash','table','are','resolved','properly', 'This','in-set','contains','thirty','one','entries','hello')
SELECT * FROM ta WHERE a<10
SELECT * FROM tb WHERE a<10
SELECT a FROM ta WHERE b IN (SELECT a FROM tb)
SELECT a FROM ta WHERE b NOT IN (SELECT a FROM tb)
SELECT a FROM ta WHERE b IN (SELECT b FROM tb)
SELECT a FROM ta WHERE b NOT IN (SELECT b FROM tb)
SELECT a FROM ta WHERE a IN (SELECT a FROM tb)
SELECT a FROM ta WHERE a NOT IN (SELECT a FROM tb)
SELECT a FROM ta WHERE a IN (SELECT b FROM tb)
SELECT a FROM ta WHERE a NOT IN (SELECT b FROM tb)
SELECT a FROM t1 WHERE a IN ()
SELECT a FROM t1 WHERE a IN (5)
SELECT a FROM t1 WHERE a NOT IN () ORDER BY a
SELECT a FROM t1 WHERE a IN (5) AND b IN ()
SELECT a FROM t1 WHERE a IN (5) AND b NOT IN ()
SELECT a FROM ta WHERE a IN ()
SELECT a FROM ta WHERE a NOT IN ()
SELECT * FROM ta LEFT JOIN tb ON (ta.b=tb.b) WHERE ta.a IN ()
SELECT b FROM t1 WHERE a IN ('hello','there')
SELECT b FROM t1 WHERE a IN ("hello",'there')
SELECT b FROM t1 WHERE a IN t4
SELECT b FROM t1 WHERE a NOT IN t4
SELECT * FROM t6 WHERE b IN (2)
SELECT * FROM t6 WHERE b IN ('2')
SELECT * FROM t6 WHERE +b IN ('2')
SELECT * FROM t6 WHERE a IN ('2')
SELECT * FROM t6 WHERE a IN (2)
SELECT * FROM t6 WHERE +a IN ('2')
SELECT 1 IN (NULL, 1, 2), 3 IN (NULL, 1, 2), 1 NOT IN (NULL, 1, 2), 3 NOT IN (NULL, 1, 2)
SELECT 2 IN (SELECT a FROM t7)
SELECT 6 IN (SELECT a FROM t7)
SELECT 2 IN (SELECT b FROM t7)
SELECT 6 IN (SELECT b FROM t7)
SELECT 2 IN (SELECT c FROM t7)
SELECT 6 IN (SELECT c FROM t7)
SELECT 2 NOT IN (SELECT a FROM t7), 6 NOT IN (SELECT a FROM t7), 2 NOT IN (SELECT b FROM t7), 6 NOT IN (SELECT b FROM t7), 2 NOT IN (SELECT c FROM t7), 6 NOT IN (SELECT c FROM t7)
SELECT b IN ( SELECT inside.a FROM t7 AS inside WHERE inside.b BETWEEN outside.b+1 AND outside.b+2 ) FROM t7 AS outside ORDER BY b
SELECT b NOT IN ( SELECT inside.a FROM t7 AS inside WHERE inside.b BETWEEN outside.b+1 AND outside.b+2 ) FROM t7 AS outside ORDER BY b
SELECT 2 IN (SELECT a FROM t7), 6 IN (SELECT a FROM t7), 2 IN (SELECT b FROM t7), 6 IN (SELECT b FROM t7), 2 IN (SELECT c FROM t7), 6 IN (SELECT c FROM t7)
SELECT * FROM a WHERE id NOT IN (SELECT id FROM b)
SELECT * FROM c1 WHERE a IN (SELECT a FROM c1) ORDER BY 1
SELECT a.id FROM t1 AS a JOIN t1 AS b ON a.id=b.id WHERE a.id IN (1,2,3)
SELECT b, a IN (3,4,5) FROM t2 ORDER BY b
SELECT CASE WHEN x NOT IN (5,6,7) THEN 'yes' ELSE 'no' END FROM t3
SELECT CASE WHEN x NOT IN (NULL,6,7) THEN 'yes' ELSE 'no' END FROM t3
SELECT CASE WHEN x NOT IN (5,6,7) OR x=0 THEN 'yes' ELSE 'no' END FROM t3
SELECT CASE WHEN x NOT IN (NULL,6,7) OR x=0 THEN 'yes' ELSE 'no' END FROM t3
WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<20) INSERT INTO t4(a,b) SELECT x, x+100 FROM c
SELECT b FROM t4 WHERE a IN (3,null,8) ORDER BY +b
SELECT b FROM t4 WHERE a NOT IN (3,null,8)
SELECT a.* FROM t5 AS 'a' JOIN t5 AS 'b' ON b.id=a.id WHERE b.id IN ( SELECT t6.t5_id FROM t6 WHERE name='Bob' AND t6.t5_id IS NOT NULL AND t6.id IN ( SELECT id FROM (SELECT t6.id, count(*) AS x FROM t6 WHERE name='Bob' ) AS 't' WHERE x=1 ) AND t6.id IN (1,id) )
SELECT * FROM x1 WHERE a IN (SELECT a FROM x1 WHERE (a%2)==0) ORDER BY a DESC, b
SELECT * FROM x1 WHERE a IN (SELECT a FROM x1 WHERE (a%7)==0) ORDER BY a DESC, b
SELECT 1 IN ('1')
SELECT 1 IN ('1' COLLATE nocase)
SELECT 1 IN (CAST('1' AS text))
SELECT 1 IN (CAST('1' AS text) COLLATE nocase)
SELECT * FROM t0 WHERE '1' IN (t0.c0)
SELECT 1 FROM t0 WHERE c0 IN ('2.0625')
SELECT c0 IN ('2.0625') FROM t0
SELECT c0 = ('2.0625') FROM t0
SELECT c0 = ('0.20625e+01') FROM t0
SELECT c0 IN ('2.0625',2,3) FROM t0
SELECT (1 IN (2 IS TRUE))
SELECT COUNT(*) FROM t0 ORDER BY (t0.c0 IN ())
WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<8) INSERT INTO t1(x,y) SELECT x, x*100 FROM c
SELECT * FROM t1 WHERE x IN (SELECT a FROM t2)
SELECT * FROM t1 WHERE x IN ((SELECT a FROM t2))
SELECT * FROM t1 WHERE x IN (((SELECT a FROM t2)))
SELECT * FROM t1 WHERE x IN ((((((SELECT a FROM t2))))))
SELECT a0.a, group_concat(a1.a) AS b FROM t4 AS a0 JOIN t4 AS a1 GROUP BY a0.a HAVING (SELECT sum( (a1.a == +a0.a COLLATE NOCASE) IN (SELECT b FROM t4)))
SELECT a0.a, group_concat(a1.a) AS b FROM t4 AS a0 JOIN t4 AS a1 GROUP BY a0.a HAVING (SELECT sum( (a1.a GLOB +a0.a COLLATE NOCASE) IN (SELECT b FROM t4)))
SELECT name FROM sqlite_master WHERE type!='meta' ORDER BY name
SELECT name, sql, tbl_name, type FROM sqlite_master WHERE name='index1'
SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='test1' ORDER BY name
SELECT cnt FROM test1 WHERE power=4
SELECT cnt FROM test1 WHERE power=1024
SELECT power FROM test1 WHERE cnt=6
SELECT name FROM sqlite_master WHERE type!='meta'
SELECT count(*) FROM test1
SELECT f1 FROM test1 WHERE f2=65536
SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='test1'
SELECT name FROM sqlite_master WHERE tbl_name='tab1'
SELECT name FROM sqlite_master WHERE tbl_name='tab1' ORDER BY name
SELECT b FROM t1 WHERE a=1 ORDER BY b
SELECT b FROM t1 WHERE a=2 ORDER BY b
SELECT c FROM t3 WHERE b==10
SELECT a FROM t4 ORDER BY b
SELECT a FROM t4 WHERE a==0 ORDER BY b
SELECT a FROM t4 WHERE a<0.5 ORDER BY b
SELECT a FROM t4 WHERE a>-0.5 ORDER BY b
SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='t5'
SELECT c FROM t6 ORDER BY a,b
SELECT c FROM t6 WHERE a=''
SELECT c FROM t6 WHERE b=''
SELECT c FROM t6 WHERE a>''
SELECT c FROM t6 WHERE a>=''
SELECT c FROM t6 WHERE a>123
SELECT c FROM t6 WHERE a>=123
SELECT c FROM t6 WHERE a<'abc'
SELECT c FROM t6 WHERE a<='abc'
SELECT c FROM t6 WHERE a<=''
SELECT c FROM t6 WHERE a<''
SELECT b FROM t1 ORDER BY a, b
SELECT b FROM t1 WHERE typeof(a) IN ('integer','real') ORDER BY b
SELECT count(*) FROM sqlite_master WHERE tbl_name = 't7' AND type = 'index'
SELECT name FROM sqlite_master WHERE tbl_name = 't7' AND type = 'index'
SELECT a, b, '|' FROM t1
WITH RECURSIVE c(x) AS (VALUES(1) UNION SELECT x+1 FROM c WHERE x<30) INSERT INTO t1(a,b,c,d,e) SELECT x, printf('ab%03xxy',x), x, x, x FROM c
SELECT a FROM t1 WHERE b='ab005xy' COLLATE nocase
SELECT name FROM sqlite_master WHERE tbl_name='t1' ORDER BY name
SELECT name FROM sqlite_master WHERE tbl_name LIKE 't2_' ORDER BY name
SELECT count(a), count(b) FROM t1
SELECT count(*) FROM t2 WHERE a IS NOT NULL
SELECT b FROM t2 WHERE a=15
SELECT b FROM t2 WHERE a=15 AND a<100
SELECT b FROM t2 WHERE a=515 AND a>200
SELECT count(*) FROM t3 WHERE a=999
SELECT count(*) FROM t3 WHERE t3.b BETWEEN 5 AND 10
SELECT stat+0 FROM sqlite_stat1 WHERE idx='t3b'
SELECT * FROM t6
SELECT * FROM t7a LEFT JOIN t7b ON (x=99) ORDER BY x
SELECT * FROM t7a JOIN t7b ON (x=99) ORDER BY x
SELECT * FROM t8a LEFT JOIN t8b ON (x = 'value' AND y = a)
SELECT a,b,c,'|' FROM t9 ORDER BY a
SELECT e FROM t10 WHERE a=1 AND b=2 AND c=3 ORDER BY d
SELECT e FROM t10 WHERE c=3 AND 2=b AND a=1 ORDER BY d DESC
SELECT e FROM t10 WHERE a=1 AND b=2 ORDER BY d DESC
SELECT 'one', * FROM t2 WHERE x NOT IN (SELECT a FROM t1)
SELECT 'two', * FROM t2 WHERE x NOT IN (SELECT a FROM t1)
SELECT x FROM t2 WHERE x IN (SELECT a FROM t1) ORDER BY +x
SELECT * FROM t0 WHERE c0 OR 1
SELECT * FROM t0 WHERE t0.c0 IS NOT 1
SELECT * FROM t0 WHERE CASE c0 WHEN 0 THEN 0 ELSE 1 END
SELECT 1 FROM t0 WHERE (t0.c0 IS FALSE) IS FALSE
SELECT 1 FROM t0 WHERE (t0.c0 IS FALSE) BETWEEN FALSE AND TRUE
SELECT 1 FROM t0 WHERE TRUE BETWEEN (t0.c0 IS FALSE) AND TRUE
SELECT 1 FROM t0 WHERE FALSE BETWEEN FALSE AND (t0.c0 IS FALSE)
SELECT 1 FROM t0 WHERE (c0 IS FALSE) IN (FALSE)
SELECT c1 <= c0, c0 >= c1 FROM t0
SELECT 2 FROM t0 WHERE c0 >= c1
SELECT 3 FROM t0 WHERE c1 <= c0
SELECT COUNT(*) FROM t0 WHERE t0.c0 GLOB t0.c0
SELECT * FROM t1 WHERE a IS NOT NULL
SELECT * FROM t2 RIGHT JOIN t3 ON d<>0 LEFT JOIN t1 ON c=3 WHERE t1.a<>0
SELECT c FROM t1 WHERE a NOT LIKE 'abc%' AND a=7 ORDER BY +b
SELECT * FROM (SELECT * FROM t5 WHERE a=1 AND b='xyz'), t4 WHERE c='abc'
SELECT * FROM v4 WHERE d='xyz' AND c='def'
SELECT * FROM t6 WHERE y IS TRUE ORDER BY x
SELECT * FROM test1
SELECT * FROM test1 ORDER BY one
SELECT * from test2
SELECT * FROM test2
SELECT * FROM test2 WHERE f1==-111
SELECT * FROM test2 WHERE f1==77
SELECT * FROM test2 ORDER BY f1
SELECT * FROM test2 WHERE f1='111' AND f2=-3.33
SELECT * FROM test2 WHERE f1=22 AND f2=-4.44
SELECT max(a) FROM t3
SELECT * FROM t3 ORDER BY a
SELECT b FROM t3 WHERE a = 0
SELECT b,c FROM t3 WHERE a IS NULL
SELECT * FROM t3 WHERE c=99
SELECT rootpage FROM sqlite_master WHERE name='test1'
SELECT rootpage FROM sqlite_temp_master WHERE name='t4'
SELECT b FROM t1 WHERE b=2
SELECT * FROM t1 WHERE b=4
SELECT * FROM t1 WHERE b=3
SELECT a FROM t1
SELECT rowid, x FROM t5
SELECT x, y FROM t6
SELECT * FROM t10
SELECT quote(a), quote(b), quote(c) FROM t11b
SELECT rowid, x FROM t12b
SELECT * FROM tab1
SELECT * FROM t12c
SELECT * FROM t13 ORDER BY +b
SELECT x FROM t14
SELECT a, length(b) FROM t1
SELECT x FROM fire ORDER BY x
SELECT *, 'x' FROM t2 ORDER BY a
SELECT *, 'x' FROM t3 ORDER BY a
SELECT * FROM d1 ORDER BY n
SELECT * FROM t1 ORDER BY log
SELECT cnt FROM t1 WHERE log=3
SELECT log FROM t1 WHERE cnt=4 ORDER BY log
SELECT * from t4
SELECT count(*) FROM t4
SELECT max(x) FROM t4
SELECT count(*) from t4
SELECT * FROM t5 ORDER BY rowid
SELECT * FROM log ORDER BY x
SELECT * FROM log2 ORDER BY x
SELECT 'a:', x, y FROM log UNION ALL SELECT 'b:', x, y FROM log2 ORDER BY x
SELECT 'a:', x, y FROM log UNION ALL SELECT 'b:', x, y FROM log2 ORDER BY x, y
SELECT * FROM t2dup
SELECT * FROM dest
SELECT * FROM t6a
SELECT * FROM t7b
SELECT * FROM B
SELECT (SELECT c FROM t2 ORDER BY coalesce(d,a) LIMIT 1) FROM t1
SELECT t1.rowid, t2.rowid, '|' FROM t1, t2 ON t1.a=t2.b
SELECT b FROM t1 NATURAL JOIN t2
SELECT b FROM t1 JOIN t2 USING(b)
SELECT * FROM t1 NATURAL CROSS JOIN t2
SELECT * FROM t1 CROSS JOIN t2 USING(b,c)
SELECT * FROM t1 NATURAL INNER JOIN t2
SELECT * FROM t1 INNER JOIN t2 USING(b,c)
SELECT * FROM t1 natural inner join t2
SELECT * FROM t1 natural join t2 natural join t3
SELECT * FROM t1 natural join t2 natural join t4
SELECT * FROM t1 natural join t2 natural join t3 WHERE t1.a=1
SELECT * FROM t1 NATURAL LEFT JOIN t2
SELECT * FROM t1 OUTER LEFT NATURAL JOIN t2
SELECT * FROM t1 NATURAL LEFT OUTER JOIN t2
SELECT * FROM t2 NATURAL LEFT OUTER JOIN t1
SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.d
SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.d WHERE t1.a>1
SELECT * FROM t1 LEFT JOIN t2 ON t1.a=t2.d WHERE t2.b IS NULL OR t2.b>1
SELECT * FROM t6 NATURAL JOIN t5
SELECT * FROM t6, t5 WHERE t6.a<t5.a
SELECT * FROM t6, t5 WHERE t6.a>t5.a
SELECT coalesce(t8.a,999) from t7 LEFT JOIN t8 on y=a
SELECT * FROM t9 LEFT JOIN v10_11 ON( a=x )
SELECT * FROM t9 LEFT JOIN (SELECT x, q FROM t10, t11 WHERE t10.y=t11.p) ON( a=x)
SELECT * FROM v10_11 LEFT JOIN t9 ON( a=x )
SELECT * FROM t9 LEFT JOIN (SELECT 44, p, q FROM t11) AS sub1 ON p=a
SELECT * FROM t12 NATURAL LEFT JOIN t13 EXCEPT SELECT * FROM t12 NATURAL LEFT JOIN (SELECT * FROM t13 WHERE b>0)
SELECT * FROM t12 NATURAL LEFT JOIN t13 EXCEPT SELECT * FROM t12 NATURAL LEFT JOIN v13
SELECT a FROM t21 LEFT JOIN t22 ON b=p WHERE q= (SELECT max(m.q) FROM t22 m JOIN t21 n ON n.b=m.p WHERE n.c=1)
SELECT * FROM t23 LEFT JOIN t24
SELECT * FROM t23 LEFT JOIN (SELECT * FROM t24)
SELECT * FROM t1 NATURAL JOIN t2
SELECT a FROM t1 JOIN t1 USING (a)
SELECT a FROM t1 JOIN t1 AS t2 USING (a)
SELECT * FROM t1 NATURAL JOIN t1 AS t2
SELECT * FROM t1 NATURAL JOIN t1
SELECT * FROM t2 NATURAL JOIN t1
SELECT * FROM aa LEFT JOIN bb, cc WHERE cc.c=aa.a
SELECT * FROM (SELECT 1 a) AS x LEFT JOIN (SELECT 1, * FROM (SELECT * FROM (SELECT 1)))
SELECT * FROM (SELECT 1 a) AS x LEFT JOIN (SELECT 1, * FROM (SELECT * FROM (SELECT * FROM (SELECT 1)))) AS y JOIN (SELECT * FROM (SELECT 9)) AS z
SELECT * FROM (SELECT 111) LEFT JOIN (SELECT cc+222, * FROM (SELECT * FROM (SELECT 333 cc)))
SELECT * FROM (SELECT 111) LEFT JOIN (SELECT c+222 FROM t1) GROUP BY 1
SELECT * FROM (SELECT 111) LEFT JOIN (SELECT c+222 FROM t1)
SELECT * FROM (SELECT 111 AS x UNION ALL SELECT 222) LEFT JOIN (SELECT c+333 AS y FROM t1) ON x=y GROUP BY 1
SELECT count(*) FROM (SELECT 111 AS x UNION ALL SELECT 222) LEFT JOIN (SELECT c+333 AS y FROM t1) ON x=y
SELECT count(*) FROM (SELECT c+333 AS y FROM t1) RIGHT JOIN (SELECT 111 AS x UNION ALL SELECT 222) ON x=y
SELECT * FROM (SELECT 111 AS x UNION ALL SELECT 111) LEFT JOIN (SELECT c+333 AS y FROM t1) ON x=y GROUP BY 1
SELECT * FROM (SELECT 111 AS x UNION ALL SELECT 111 UNION ALL SELECT 222) LEFT JOIN (SELECT c+333 AS y FROM t1) ON x=y GROUP BY 1
SELECT *, '|' FROM t3 LEFT JOIN v2 ON a=x WHERE b=1
SELECT *, '|' FROM t3 LEFT JOIN v2 ON a=x WHERE b+1=x
SELECT *, '|' FROM t3 LEFT JOIN v2 ON a=x ORDER BY b
SELECT t1.id, x2.id, x3.id FROM t1 LEFT JOIN (SELECT * FROM t2) AS x2 ON t1.id=x2.c2 LEFT JOIN t3 AS x3 ON x2.id=x3.c3
SELECT *, 'x' FROM t1 LEFT JOIN t2 WHERE CASE WHEN FALSE THEN a=x ELSE 1 END
SELECT *, 'x' FROM t1 LEFT JOIN t2 WHERE a IN (1,3,x,y)
SELECT *, 'x' FROM t1 LEFT JOIN t2 WHERE NOT ( 'x'='y' AND t2.y=1 )
SELECT *, 'x' FROM t1 LEFT JOIN t2 WHERE ~ ( 'x'='y' AND t2.y=1 )
SELECT *, 'x' FROM t1 LEFT JOIN t2 WHERE t2.y IS NOT 'abc'
SELECT a1, a2, a3, a4, a5 FROM (SELECT a AS a1 FROM t1 WHERE b=0) JOIN (SELECT x AS x1 FROM t2) LEFT JOIN (SELECT a AS a2, b AS b2 FROM t1) ON x1 IS TRUE AND b2=a1 JOIN (SELECT x AS x2 FROM t2) ON x2<=CASE WHEN x1 THEN CASE WHEN a2 THEN 1 ELSE -1 END ELSE 0 END LEFT JOIN (SELECT a AS a3, b AS b3 FROM t1) ON x2 IS TRUE AND b3=a2 JOIN (SELECT x AS x3 FROM t2) ON x3<=CASE WHEN x2 THEN CASE WHEN a3 THEN 1 ELSE -1 END ELSE 0 END LEFT JOIN (SELECT a AS a4, b AS b4 FROM t1) ON x3 IS TRUE AND b4=a3 JOIN (SELECT x AS x4 FROM t2) ON x4<=CASE WHEN x3 THEN CASE WHEN a4 THEN 1 ELSE -1 END ELSE 0 END LEFT JOIN (SELECT a AS a5, b AS b5 FROM t1) ON x4 IS TRUE AND b5=a4 ORDER BY a1, a2, a3, a4, a5
SELECT a, b FROM t1 LEFT JOIN t2 ON 0 WHERE (b IS NOT NULL)=0
SELECT * FROM t1 LEFT JOIN (SELECT abs(1) AS y FROM t1) ON x WHERE NOT(y='a')
SELECT * FROM t1 LEFT JOIN (SELECT abs(1)+2 AS y FROM t1) ON x WHERE NOT(y='a')
SELECT * FROM v0 WHERE NOT(v0.a IS FALSE)
SELECT * FROM t1 LEFT JOIN t0 WHERE NOT(a IS FALSE)
SELECT NOT(v0.a IS FALSE) FROM v0
SELECT * FROM v0 WHERE v0.c NOTNULL NOTNULL
SELECT * FROM t1 LEFT JOIN t2
SELECT * FROM t1 LEFT JOIN t2 WHERE (b IS NOT NULL) IS NOT NULL
SELECT (b IS NOT NULL) IS NOT NULL FROM t1 LEFT JOIN t2
SELECT * FROM t1 LEFT JOIN t2 WHERE (b IS NOT NULL AND b IS NOT NULL) IS NOT NULL
SELECT * FROM t0 LEFT JOIN t1 WHERE NULL IN (c1)
SELECT quote(z) FROM t1 RIGHT JOIN t2 ON y LEFT JOIN t3 ON y
SELECT 11, * FROM t1 LEFT JOIN t0 WHERE aa ISNULL
SELECT 12, * FROM t1 LEFT JOIN t0 WHERE +aa ISNULL
SELECT 13, * FROM t1 LEFT JOIN t0 ON aa ISNULL
SELECT 14, * FROM t1 LEFT JOIN t0 ON +aa ISNULL
SELECT 21, * FROM t1 LEFT JOIN t0 WHERE aa ISNULL
SELECT 22, * FROM t1 LEFT JOIN t0 WHERE +aa ISNULL
SELECT 23, * FROM t1 LEFT JOIN t0 ON aa ISNULL
SELECT 24, * FROM t1 LEFT JOIN t0 ON +aa ISNULL
SELECT DISTINCT c FROM t0 LEFT JOIN (SELECT a+1 AS c FROM t0) ORDER BY c
SELECT t0.c0, v0.c0, vt0.name FROM v0, t0 LEFT JOIN pragma_table_info('t0') AS vt0 ON vt0.name LIKE 'c0' WHERE v0.c0 == 0
SELECT a.value, b.value FROM b LEFT JOIN a ON a.value = b.value
SELECT * FROM t2 JOIN t1 WHERE a='abc' AND x='def'
SELECT * FROM t2 JOIN t1 WHERE a='abc' AND x='abc'
SELECT * FROM t2 LEFT JOIN t1 ON a=0 WHERE (x='x' OR x IS NULL)
SELECT count(*) FROM v0 LEFT JOIN t0 ON v0.c0
WITH t99(b) AS MATERIALIZED ( SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3) ) SELECT 5 FROM t2 JOIN t99 ON b IN (1,2,3)
WITH t99(b) AS NOT MATERIALIZED ( SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3) ) SELECT 5 FROM t2 JOIN t99 ON b IN (1,2,3)
WITH t99(b) AS (SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3)) SELECT 5 FROM t2 JOIN t99 ON b IN (1,2,3)
SELECT 5 FROM t2 JOIN ( SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3) ) AS t99 ON b IN (1,2,3)
WITH t99(b) AS ( SELECT coalesce(b,3) FROM t2 AS x LEFT JOIN t1 ON c IN (SELECT x FROM t3) ) SELECT d, e, b FROM t2 JOIN t99 ON b IN (1,2,3) ORDER BY +d
SELECT d, e, b2 FROM t2 JOIN (SELECT coalesce(b,3) AS b2 FROM t2 AS x LEFT JOIN t1 ON c IN (SELECT x FROM t3)) AS t99 ON b2 IN (1,2,3) ORDER BY +d
SELECT * FROM t2 JOIN (SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3)) AS t99 ON b IN (1,2,3)
SELECT * FROM t2 JOIN (SELECT b FROM t2 LEFT JOIN t1 ON c IN (SELECT x FROM t3)) AS t99 ON b IS NULL
WITH t99(b) AS ( SELECT b FROM t2 AS x LEFT JOIN t1 ON c IN (SELECT x FROM t3) ) SELECT d, e, b FROM t2 JOIN t99 ON b IS NULL
SELECT a, b, y FROM t4 JOIN t3 ON a=x
SELECT * FROM t1 JOIN v2 ON 0 FULL OUTER JOIN t0 ON true
SELECT * FROM t1 JOIN v2 ON 1=0 FULL OUTER JOIN t0 ON true
SELECT * FROM t1 JOIN v2 ON false FULL OUTER JOIN t0 ON true
SELECT DISTINCT a, b FROM t1 RIGHT JOIN t2 ON a=b LEFT JOIN v5 ON false WHERE x <= y
SELECT DISTINCT a, b FROM t0 JOIN t1 ON z=a RIGHT JOIN t2 ON a=b LEFT JOIN v5 ON false WHERE x <= y
SELECT * FROM t2 RIGHT JOIN t3 ON true LEFT JOIN t1 USING(c0)
SELECT * FROM t2 RIGHT JOIN t3 ON true NATURAL LEFT JOIN t1
SELECT * FROM t2n RIGHT JOIN t3 ON true LEFT JOIN t1 USING(c0)
SELECT * FROM t5 LEFT JOIN t1 USING(c0)
SELECT * FROM t3 LEFT JOIN t2 ON true LEFT JOIN t1 USING(c0)
SELECT * FROM t3 LEFT JOIN t2 ON true NATURAL LEFT JOIN t1
SELECT * FROM t3 LEFT JOIN t2 ON true JOIN t4 ON true NATURAL LEFT JOIN t1
SELECT * FROM t0 JOIN v0 ON w=z RIGHT JOIN t1 ON true INNER JOIN t2 ON y IS z
SELECT * FROM t0 JOIN v0 ON w=z RIGHT JOIN t1 ON true INNER JOIN t2 ON +y IS z
SELECT a1 FROM vchain ORDER BY a1
SELECT a1, b2 FROM vchain ORDER BY a1
SELECT * FROM t1 NATURAL JOIN t2 NATURAL JOIN t3
SELECT * FROM t1 NATURAL JOIN t2 NATURAL LEFT OUTER JOIN t3
SELECT * FROM t1 NATURAL LEFT OUTER JOIN t2 NATURAL JOIN t3
SELECT * FROM t2 NATURAL RIGHT OUTER JOIN t1 NATURAL JOIN t3
SELECT * FROM t1 NATURAL LEFT OUTER JOIN (t2 NATURAL JOIN t3)
SELECT a, b, c, d FROM t2 NATURAL JOIN t3 NATURAL RIGHT JOIN t1
SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v2)
SELECT v1, v3 FROM c1 LEFT JOIN c2 ON (c2.k=v1) LEFT JOIN c3 ON (c3.k=v1+1)
SELECT DISTINCT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1)
SELECT v1, v3 FROM c1 LEFT JOIN c2 LEFT JOIN c3 ON (c3.k=v1+1)
SELECT a.x FROM t1 AS a LEFT JOIN t1 AS b ON (a.x=b.x) LEFT JOIN t2 AS c ON (a.x=c.x)
WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<10) INSERT INTO t1(x) SELECT x FROM c
SELECT a.x, c.x FROM t1 AS a LEFT JOIN t1 AS b ON (a.x=b.x) LEFT JOIN t2 AS c ON (a.x=c.x)
SELECT * FROM test
SELECT * FROM t0 LEFT JOIN t1 WHERE (t1.c0 BETWEEN 0 AND 0) > ('' AND t0.c0)
SELECT typeof(c0), c0 FROM v0 WHERE c0>='0'
SELECT * FROM t0, v0 WHERE v0.c0 >= '0'
SELECT * FROM t0 LEFT JOIN v0 WHERE v0.c0 >= '0'
SELECT * FROM t0 LEFT JOIN v0 ON v0.c0 >= '0'
SELECT * FROM t0 LEFT JOIN v0 ON v0.c0 >= '0' WHERE TRUE UNION SELECT 0,0 WHERE 0
SELECT ccc, ccc IS NULL AS ddd FROM t1 LEFT JOIN v2
SELECT ( SELECT 1 FROM t2 LEFT JOIN (SELECT x AS v FROM t3) ON 500=v WHERE (v OR FALSE) ) FROM t1
SELECT ( SELECT 1 FROM t2 LEFT JOIN (SELECT x AS v FROM t3) ON 500=v WHERE (v) ) FROM t1
SELECT * FROM t1 LEFT JOIN t3 ON y=z
WITH RECURSIVE c(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM c WHERE n<100) INSERT INTO t1(a) SELECT n FROM c
SELECT t1.a1, t2.d2 FROM (t1 LEFT JOIN t3 ON t3.e3=t1.b1) JOIN t2 ON t2.c2=t1.a1 WHERE t1.a1=33 ORDER BY t2.d2 DESC
SELECT 5 UNION ALL SELECT 3 ORDER BY 1
SELECT 986 AS x GROUP BY X ORDER BY X
SELECT ( SELECT 'hardware' FROM ( SELECT 'software' ORDER BY 'firmware' ASC, 'sportswear' DESC ) GROUP BY 1 HAVING length(b) ) FROM abc
WITH cnt(i) AS ( SELECT 1 UNION ALL SELECT i+1 FROM cnt WHERE i<10000 ) INSERT INTO t1 SELECT i%2, randomblob(500) FROM cnt
SELECT (SELECT x||y FROM t2, t1 ORDER BY x, y)
SELECT b, rowid, '^' FROM t10 ORDER BY b, a LIMIT 4
SELECT rowid, * from t2
SELECT integrity_check AS x FROM pragma_integrity_check ORDER BY 1
SELECT * FROM sqlite_temp_master
SELECT * FROM aux.t1
SELECT * FROM temp_store_directory_test
SELECT * FROM temp_table
SELECT * FROM sqlite_master
select * from sqlite_master
SELECT *, '|' FROM t1
SELECT * FROM log
SELECT count(*) FROM blobs
SELECT x FROM blobs WHERE rowid = 2
SELECT name FROM sqlite_master
SELECT name FROM aux1.sqlite_master
SELECT name FROM aux2.sqlite_master
SELECT 'a', * FROM t1
SELECT 'b', * FROM t3
SELECT count(*) FROM t3
SELECT f1 FROM test1
SELECT f2 FROM test1
SELECT f2, f1 FROM test1
SELECT f1, f2 FROM test1
SELECT *, * FROM test1
SELECT *, min(f1,f2), max(f1,f2) FROM test1
SELECT 'one', *, 'two', * FROM test1
SELECT * FROM test1, test2
SELECT *, 'hi' FROM test1, test2
SELECT 'one', *, 'two', * FROM test1, test2
SELECT test1.f1, test2.r1 FROM test1, test2
SELECT test1.f1, test2.r1 FROM test2, test1
SELECT * FROM test2, test1
SELECT * FROM test1 AS a, test1 AS b
SELECT max(test1.f1,test2.r1), min(test1.f2,test2.r2) FROM test2, test1
SELECT min(test1.f1,test2.r1), max(test1.f2,test2.r2) FROM test1, test2
SELECT count(*),count(a),count(b) FROM t3
SELECT count(*),count(a),count(b) FROM t4
SELECT count(*),count(a),count(b) FROM t4 WHERE b=5
SELECT coalesce(min(a),'xyzzy') FROM t3
SELECT min(coalesce(a,'xyzzy')) FROM t3
SELECT min(b), min(b) FROM t4
SELECT coalesce(max(a),'xyzzy') FROM t3
SELECT max(coalesce(a,'xyzzy')) FROM t3
SELECT sum(a) FROM t3
SELECT f1 FROM test1 ORDER BY 8.4
SELECT f1 FROM test1 ORDER BY '8.4'
SELECT * FROM t5 ORDER BY 1
SELECT * FROM t5 ORDER BY 2
SELECT * FROM t5 ORDER BY +2
SELECT * FROM t5 ORDER BY 2, 1 DESC
SELECT * FROM t5 ORDER BY 1 DESC, b
SELECT * FROM t5 ORDER BY b DESC, 1
SELECT a FROM t6 WHERE b IN (SELECT b FROM t6 WHERE a<='b' UNION SELECT '3' AS x ORDER BY 1 LIMIT 1)
SELECT a FROM t6 WHERE b IN (SELECT b FROM t6 WHERE a<='b' UNION SELECT '3' AS x ORDER BY 1 DESC LIMIT 1)
SELECT a FROM t6 WHERE b IN (SELECT b FROM t6 WHERE a<='b' UNION SELECT '3' AS x ORDER BY b LIMIT 2) ORDER BY a
SELECT a FROM t6 WHERE b IN (SELECT b FROM t6 WHERE a<='b' UNION SELECT '3' AS x ORDER BY x DESC LIMIT 2) ORDER BY a
SELECT f1 FROM test1 WHERE 4.3+2.4 OR 1 ORDER BY f1
SELECT f1 FROM test1 WHERE ('x' || f1) BETWEEN 'x10' AND 'x20' ORDER BY f1
SELECT f1 FROM test1 WHERE 5-3==2 ORDER BY f1
SELECT coalesce(f1/(f1-11),'x'), coalesce(min(f1/(f1-11),5),'y'), coalesce(max(f1/(f1-33),6),'z') FROM test1 ORDER BY f1
SELECT min(1,2,3), -max(1,2,3) FROM test1 ORDER BY f1
SELECT f1 AS x FROM test1 ORDER BY x
SELECT f1 AS x FROM test1 ORDER BY -x
SELECT f1-23 AS x FROM test1 ORDER BY abs(x)
SELECT f1-23 AS x FROM test1 ORDER BY -abs(x)
SELECT f1-22 AS x, f2-22 as y FROM test1
SELECT f1-22 AS x, f2-22 as y FROM test1 WHERE x>0 AND y<50
SELECT f1 COLLATE nocase AS x FROM test1 ORDER BY x
SELECT * FROM t3, t4
SELECT t3.*, t4.b FROM t3, t4
SELECT "t3".*, t4.b FROM t3, t4
SELECT t3.b, t4.* FROM t3, t4
SELECT * FROM t3 UNION SELECT 3 AS 'a', 4 ORDER BY a
SELECT 3, 4 UNION SELECT * FROM t3
SELECT * FROM t3 WHERE a=(SELECT 1)
SELECT * FROM t3 WHERE a=(SELECT 2)
SELECT count( (SELECT a FROM abc WHERE a = NULL AND b >= upper.c) ) FROM abc AS upper
SELECT * FROM sqlite_master WHERE rowid>10
SELECT * FROM sqlite_master WHERE rowid=10
SELECT * FROM sqlite_master WHERE rowid<10
SELECT * FROM sqlite_master WHERE rowid<=10
SELECT * FROM sqlite_master WHERE rowid>=10
SELECT 10 IN (SELECT rowid FROM sqlite_master)
SELECT 2 IN (SELECT a FROM t1)
SELECT * FROM t1,(SELECT * FROM t2 WHERE y=2 ORDER BY y,z)
SELECT * FROM t1,(SELECT * FROM t2 WHERE y=2 ORDER BY y,z LIMIT 4)
SELECT * FROM t1,(SELECT * FROM t2 WHERE y=2 UNION ALL SELECT * FROM t2 WHERE y=3 ORDER BY y,z LIMIT 4)
SELECT x FROM t2, t1 WHERE x BETWEEN c AND null OR x AND x IN ((SELECT x FROM (SELECT x FROM t2, t1 WHERE x BETWEEN (SELECT x FROM (SELECT x COLLATE rtrim FROM t2, t1 WHERE x BETWEEN c AND null OR x AND x IN (c)), t1 WHERE x BETWEEN c AND null OR x AND x IN (c)) AND null OR NOT EXISTS(SELECT -4.81 FROM t1, t2 WHERE x BETWEEN c AND null OR x AND x IN ((SELECT x FROM (SELECT x FROM t2, t1 WHERE x BETWEEN (SELECT x FROM (SELECT x BETWEEN c AND null OR x AND x IN (c)), t1 WHERE x BETWEEN c AND null OR x AND x IN (c)) AND null OR x AND x IN (c)), t1 WHERE x BETWEEN c AND null OR x AND x IN (c)))) AND x IN (c) ), t1 WHERE x BETWEEN c AND null OR x AND x IN (c)))
SELECT x FROM t2, t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN ((SELECT x FROM (SELECT x FROM t2, t1 WHERE x BETWEEN (SELECT x FROM (SELECT x COLLATE rtrim FROM t2, t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN (c)), t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN (c)) AND (c+1) OR NOT EXISTS(SELECT -4.81 FROM t1, t2 WHERE x BETWEEN c AND (c+1) OR x AND x IN ((SELECT x FROM (SELECT x FROM t2, t1 WHERE x BETWEEN (SELECT x FROM (SELECT x BETWEEN c AND (c+1) OR x AND x IN (c)), t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN (c)) AND (c+1) OR x AND x IN (c)), t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN (c)))) AND x IN (c) ), t1 WHERE x BETWEEN c AND (c+1) OR x AND x IN (c)))
SELECT 1 FROM t1 WHERE ( SELECT 2 FROM t2 WHERE ( SELECT 3 FROM ( SELECT x FROM t2 WHERE x=c OR x=(SELECT x FROM (VALUES(0))) ) WHERE x>c OR x=c ) )
SELECT 1 FROM t1, t2 WHERE ( SELECT 3 FROM ( SELECT x FROM t2 WHERE x=c OR x=(SELECT x FROM (VALUES(0))) ) WHERE x>c OR x=c )
SELECT * FROM t1 JOIN t1 USING(a,b) WHERE ((SELECT t1.a FROM t1 AS x GROUP BY b) AND b=0) OR a = 10
SELECT ifnull(a, max((SELECT 123))), count(a) FROM t1
SELECT a,(+a)b,(+a)b,(+a)b,NOT EXISTS(SELECT null FROM t2),CASE z WHEN 487 THEN 992 WHEN 391 THEN 203 WHEN 10 THEN '?k<D Q' END,'' FROM t1 LEFT JOIN v1a ON z=b
SELECT count(*) FROM tbl2
SELECT count(*) FROM tbl2 WHERE f2>1000
SELECT f1 FROM tbl2 WHERE 1000=f2
SELECT f1 FROM tbl2 WHERE f2=1000
SELECT * FROM tbl2 WHERE 1000=f2
SELECT * FROM tbl2 WHERE f2=1000
SELECT f1 FROM tbl2 WHERE f2==2000
SELECT * FROM aa, bb WHERE max(a,b)>2
SELECT * FROM aa CROSS JOIN bb WHERE b
SELECT * FROM aa CROSS JOIN bb WHERE NOT b
SELECT * FROM aa, bb WHERE min(a,b)
SELECT * FROM aa, bb WHERE NOT min(a,b)
SELECT * FROM aa, bb WHERE CASE WHEN a=b-1 THEN 1 END
SELECT * FROM aa, bb WHERE CASE WHEN a=b-1 THEN 0 ELSE 1 END
SELECT DISTINCT log FROM t1 ORDER BY log
SELECT min(n),min(log),max(n),max(log),sum(n),sum(log),avg(n),avg(log) FROM t1
SELECT max(n)/avg(n), max(log)/avg(log) FROM t1
SELECT log, count(*) FROM t1 GROUP BY log ORDER BY log
SELECT log, min(n) FROM t1 GROUP BY log ORDER BY log
SELECT log, avg(n) FROM t1 GROUP BY log ORDER BY log
SELECT log, avg(n)+1 FROM t1 GROUP BY log ORDER BY log
SELECT log, avg(n)-min(n) FROM t1 GROUP BY log ORDER BY log
SELECT log*2+1, avg(n)-min(n) FROM t1 GROUP BY log ORDER BY log
SELECT log*2+1 as x, count(*) FROM t1 GROUP BY x ORDER BY x
SELECT log*2+1 AS x, count(*) AS y FROM t1 GROUP BY x ORDER BY y, x
SELECT log*2+1 AS x, count(*) AS y FROM t1 GROUP BY x ORDER BY 10-(x+y)
SELECT log, count(*) FROM t1 HAVING log>=4
SELECT count(*) FROM t1 HAVING log>=4
SELECT count(*) FROM t1 HAVING log!=400
SELECT log, count(*) FROM t1 GROUP BY log HAVING log>=4 ORDER BY log
SELECT log, count(*) FROM t1 GROUP BY log HAVING count(*)>=4 ORDER BY log
SELECT log, count(*) FROM t1 GROUP BY log HAVING count(*)>=4 ORDER BY max(n)+0
SELECT log AS x, count(*) AS y FROM t1 GROUP BY x HAVING y>=4 ORDER BY max(n)+0
SELECT log AS x FROM t1 GROUP BY x HAVING count(*)>=4 ORDER BY max(n)+0
SELECT log, count(*), avg(n), max(n+log*2) FROM t1 GROUP BY log ORDER BY max(n+log*2)+0, avg(n)+0
SELECT log, count(*), avg(n), max(n+log*2) FROM t1 GROUP BY log ORDER BY max(n+log*2)+0, min(log,avg(n))+0
SELECT log, min(n) FROM t1 GROUP BY log ORDER BY log DESC
SELECT log, min(n) FROM t1 GROUP BY log ORDER BY 1
SELECT log, min(n) FROM t1 GROUP BY log ORDER BY 1 DESC
SELECT a, sum(b) FROM t2 WHERE b=5 GROUP BY a
SELECT a, sum(b) FROM t2 WHERE b=5
SELECT typeof(sum(a3)) FROM a
SELECT typeof(sum(a3)) FROM a GROUP BY a1
SELECT * FROM t0 GROUP BY c0
SELECT max(t1.a), (SELECT 'xyz' FROM (SELECT * FROM t2 WHERE 0) WHERE t1.b=1) FROM t1
SELECT max(a), val FROM t1 LEFT JOIN ( SELECT 'constant' AS val FROM t2 WHERE x=1234 )
SELECT count(x), m FROM t1 LEFT JOIN (SELECT x, 59 AS m FROM t2) GROUP BY a
SELECT group_concat(x), m FROM t1 LEFT JOIN (SELECT x, 59 AS m FROM t2) GROUP BY a
SELECT group_concat(x), m, n FROM t1 LEFT JOIN (SELECT x, 59 AS m, 60 AS n FROM t2) GROUP BY a
SELECT DISTINCT log FROM t1
SELECT n FROM t1 WHERE log=3
SELECT DISTINCT log FROM t1 UNION ALL SELECT n FROM t1 WHERE log=3 ORDER BY log
SELECT DISTINCT log FROM t1 UNION ALL SELECT n FROM t1 WHERE log=2
SELECT log FROM t1 WHERE n IN (SELECT DISTINCT log FROM t1 UNION ALL SELECT n FROM t1 WHERE log=3) ORDER BY log
SELECT DISTINCT log FROM t1 UNION SELECT n FROM t1 WHERE log=3 ORDER BY log
SELECT log FROM t1 WHERE n IN (SELECT DISTINCT log FROM t1 UNION SELECT n FROM t1 WHERE log=3) ORDER BY log
SELECT 123 AS x ORDER BY (SELECT x ORDER BY 1)
SELECT DISTINCT log FROM t1 EXCEPT SELECT n FROM t1 WHERE log=3 ORDER BY log
SELECT log FROM t1 WHERE n IN (SELECT DISTINCT log FROM t1 EXCEPT SELECT n FROM t1 WHERE log=3) ORDER BY log
SELECT DISTINCT log FROM t1 INTERSECT SELECT n FROM t1 WHERE log=3 ORDER BY log
SELECT DISTINCT log FROM t1 UNION ALL SELECT 6 INTERSECT SELECT n FROM t1 WHERE log=3 ORDER BY t1.log
SELECT log FROM t1 WHERE n IN (SELECT DISTINCT log FROM t1 INTERSECT SELECT n FROM t1 WHERE log=3) ORDER BY log
SELECT log, count(*) as cnt FROM t1 GROUP BY log UNION SELECT log, n FROM t1 WHERE n=7 ORDER BY cnt, log
SELECT log, count(*) FROM t1 GROUP BY log UNION SELECT log, n FROM t1 WHERE n=7 ORDER BY count(*), log
SELECT NULL UNION SELECT NULL UNION SELECT 1 UNION SELECT 2 AS 'x' ORDER BY x
SELECT NULL UNION ALL SELECT NULL UNION ALL SELECT 1 UNION ALL SELECT 2 AS 'x' ORDER BY x
SELECT * FROM ( SELECT NULL, 1 UNION ALL SELECT NULL, 1 )
SELECT DISTINCT * FROM ( SELECT NULL, 1 UNION ALL SELECT NULL, 1 )
SELECT DISTINCT * FROM ( SELECT 1,2 UNION ALL SELECT 1,2 )
SELECT NULL EXCEPT SELECT NULL
SELECT * FROM t2 ORDER BY x
SELECT DISTINCT b FROM t3 ORDER BY c
SELECT DISTINCT c FROM t3 ORDER BY c
SELECT 0 AS x, 1 AS y UNION SELECT 2 AS y, -3 AS x ORDER BY x LIMIT 1
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT 4
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT 0
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT -1
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT -1 OFFSET 2
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT 3 OFFSET 2
SELECT DISTINCT log FROM t1 ORDER BY +log LIMIT 3 OFFSET 20
SELECT DISTINCT log FROM t1 ORDER BY log LIMIT 0 OFFSET 3
SELECT DISTINCT max(n), log FROM t1 ORDER BY +log
SELECT * FROM t14 INTERSECT VALUES(3,2,1),(2,3,1),(1,2,3),(2,1,3)
SELECT * FROM t14 INTERSECT VALUES(1,2,3)
SELECT * FROM t14 UNION VALUES(3,2,1),(2,3,1),(1,2,3),(7,8,9),(4,5,6) UNION SELECT * FROM t14 ORDER BY 1, 2, 3
SELECT * FROM t14 UNION VALUES(3,2,1) UNION SELECT * FROM t14 ORDER BY 1, 2, 3
SELECT * FROM t14 EXCEPT VALUES(3,2,1),(2,3,1),(1,2,3),(2,1,3)
SELECT * FROM t14 EXCEPT VALUES(1,2,3)
SELECT * FROM t14 EXCEPT VALUES(1,2,3) EXCEPT VALUES(4,5,6)
SELECT * FROM t14 EXCEPT VALUES('a','b','c') EXCEPT VALUES(4,5,6)
SELECT * FROM t14 UNION ALL VALUES(3,2,1),(2,3,1),(1,2,3),(2,1,3)
SELECT (VALUES(1),(2),(3),(4))
SELECT (SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4)
VALUES(1) UNION VALUES(2)
VALUES(1),(2),(3) EXCEPT VALUES(2)
VALUES(1),(2),(3) EXCEPT VALUES(1),(3)
SELECT * FROM (SELECT 123), (SELECT 456) ON likely(0 OR 1) OR 0
VALUES(1),(2),(3),(4) UNION ALL SELECT 5 LIMIT 99
VALUES(1),(2),(3),(4) UNION ALL SELECT 5 LIMIT 3
SELECT DISTINCT t0.id, t0.a, t0.b FROM tx AS t0, tx AS t1 WHERE t0.a=t1.a AND t1.a=33 AND t0.b=456 UNION SELECT DISTINCT t0.id, t0.a, t0.b FROM tx AS t0, tx AS t1 WHERE t0.a=t1.a AND t1.a=33 AND t0.b=789 ORDER BY 1
WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<100) INSERT INTO t1(a,b,c,d) SELECT x%10, x/10, x, printf('xyz%dabc',x) FROM c
SELECT t3.c FROM (SELECT a,max(b) AS m FROM t1 WHERE a>=5 GROUP BY a) AS t2 JOIN t1 AS t3 WHERE t2.a=t3.a AND t2.m=t3.b ORDER BY t3.a
SELECT t3.c FROM (SELECT a,max(b) AS m FROM t1 WHERE a>=5 GROUP BY a) AS t2 CROSS JOIN t1 AS t3 WHERE t2.a=t3.a AND t2.m=t3.b ORDER BY t3.a
SELECT t3.c FROM (SELECT a,max(b) AS m FROM t1 WHERE a>=5 GROUP BY a) AS t2 LEFT JOIN t1 AS t3 WHERE t2.a=t3.a AND t2.m=t3.b ORDER BY t3.a
SELECT x, y FROM ( SELECT 98 AS x, 99 AS y UNION SELECT a AS x, sum(b) AS y FROM t1 GROUP BY a ) AS w WHERE y>=20 ORDER BY +x
SELECT x, y FROM ( SELECT a AS x, sum(b) AS y FROM t1 GROUP BY a UNION SELECT 98 AS x, 99 AS y ) AS w WHERE y>=20 ORDER BY +x
SELECT *FROM v0 v1 JOIN v0 USING(v0) WHERE datetime(v0) = (v0.v0)AND v0 = 10
SELECT * FROM t1 AS z1 JOIN t1 AS z2 USING(aa) WHERE abs(z1.aa)=z2.aa AND z1.aa=123
SELECT sum((SELECT 1 FROM (SELECT 2 WHERE x IS NULL) WHERE 0)) FROM t1
SELECT DISTINCT y FROM t1 ORDER BY y
SELECT y, count(*) FROM t1 GROUP BY y ORDER BY y
SELECT y, count(*) FROM t1 GROUP BY y ORDER BY count(*), y
SELECT count(*), y FROM t1 GROUP BY y ORDER BY count(*), y
SELECT x, count(*), avg(y) FROM t1 GROUP BY x HAVING x<4 ORDER BY x
SELECT avg(x) FROM t1 WHERE x>100
SELECT count(x) FROM t1 WHERE x>100
SELECT min(x) FROM t1 WHERE x>100
SELECT max(x) FROM t1 WHERE x>100
SELECT sum(x) FROM t1 WHERE x>100
