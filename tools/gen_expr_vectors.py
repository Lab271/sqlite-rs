#!/usr/bin/env python3
"""Oracle vector harvest for spec 008 (value semantics) — issue #77.

Runs the pinned oracle `sqlite3` over hand-authored expression/column sets
covering type affinity, cross-type comparison, collations, NULL semantics,
and numeric coercion (per datatype3.html and vdbemem.c/func.c behavior),
and emits one JSON-lines vector file per family under
`tests/corpus/expr_vectors/`. These vectors are the oracle-backed evidence
for spec 008's scenarios; spike 008 (#59) later ratchets (adds to) them
once a throwaway tree-walker exists to exercise them.

Each line is `{"expr": "<SQL expression or column decl>", "value": ...,
"type": "<sqlite typeof() result>"}` for expression families, or
`{"declared_type": "...", "affinity": "..."}` for the affinity family.

Usage: python3 tools/gen_expr_vectors.py [--oracle /path/to/sqlite3]
"""

import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
CARGO_TOML = REPO_ROOT / "Cargo.toml"
ORACLE_VERSION = tomllib.loads(CARGO_TOML.read_text())["package"]["metadata"]["oracle"]["version"]
ORACLE_CANDIDATES = [
    "/opt/homebrew/opt/sqlite/bin/sqlite3",
    "/usr/local/opt/sqlite/bin/sqlite3",
    "sqlite3",
]
OUT_DIR = REPO_ROOT / "tests/corpus/expr_vectors"


def find_oracle(explicit):
    if explicit:
        return explicit
    for candidate in ORACLE_CANDIDATES:
        try:
            out = subprocess.run(
                [candidate, "-version"], capture_output=True, text=True, timeout=5
            )
        except (FileNotFoundError, OSError):
            continue
        if out.returncode == 0 and out.stdout.startswith(ORACLE_VERSION):
            return candidate
    print(
        f"error: no sqlite3 matching pinned oracle version {ORACLE_VERSION} found "
        f"(tried {ORACLE_CANDIDATES}); set --oracle explicitly",
        file=sys.stderr,
    )
    sys.exit(1)


def run_sql(oracle, sql):
    out = subprocess.run(
        [oracle, "-batch", "-noheader", "-separator", "\t", ":memory:"],
        input=sql,
        capture_output=True,
        text=True,
        timeout=10,
    )
    if out.returncode != 0:
        raise RuntimeError(f"oracle error for SQL:\n{sql}\n{out.stderr}")
    return out.stdout


def eval_exprs(oracle, exprs):
    """Runs SELECT <expr>, typeof(<expr>) for each expr, one line each."""
    vectors = []
    for expr in exprs:
        sql = f"SELECT quote({expr}), typeof({expr});\n"
        line = run_sql(oracle, sql).strip("\n")
        value_q, sqltype = line.split("\t")
        vectors.append({"expr": expr, "value_quoted": value_q, "type": sqltype})
    return vectors


AFFINITY_DECLS = [
    "INTEGER", "INT", "TINYINT", "SMALLINT", "MEDIUMINT", "BIGINT",
    "UNSIGNED BIG INT", "INT2", "INT8",
    "TEXT", "CHARACTER(20)", "VARCHAR(255)", "VARYING CHARACTER(255)",
    "NCHAR(55)", "NATIVE CHARACTER(70)", "NVARCHAR(100)", "CLOB",
    "BLOB", "",
    "REAL", "DOUBLE", "DOUBLE PRECISION", "FLOAT",
    "NUMERIC", "DECIMAL(10,5)", "BOOLEAN", "DATE", "DATETIME",
    "POINT", "STRING",
]


def eval_affinity(oracle):
    vectors = []
    for decl in AFFINITY_DECLS:
        col_decl = decl if decl else ""
        sql = (
            f"CREATE TABLE t(x {col_decl});\n"
            "SELECT (SELECT type FROM pragma_table_info('t') WHERE name='x');\n"
        )
        declared = run_sql(oracle, sql).strip("\n")
        affinity_sql = (
            f"CREATE TABLE t(x {col_decl});\n"
            "INSERT INTO t VALUES('1.5');\n"
            "SELECT typeof(x) FROM t;\n"
        )
        stored_type = run_sql(oracle, affinity_sql).strip("\n")
        vectors.append(
            {"declared_type": decl, "affinity_probe_stored_type": stored_type}
        )
    return vectors


COMPARISON_EXPRS = [
    "NULL < 1", "NULL < 'a'", "NULL < x'00'", "NULL = NULL",
    "1 < 'a'", "1 < x'00'", "'a' < x'00'",
    "1 < 2.5", "2 = 2.0", "1 < 1.0000001",
    "'abc' < 'abd'", "'ABC' < 'abc'",
    "x'01' < x'0100'", "x'0100' < x'02'",
    "9223372036854775807 < 9223372036854775807.0",
]

COLLATION_EXPRS = [
    "'abc' = 'ABC' COLLATE BINARY",
    "'abc' = 'ABC' COLLATE NOCASE",
    "'abc ' = 'abc' COLLATE RTRIM",
    "'straße' = 'STRASSE' COLLATE NOCASE",
    "'é' = 'É' COLLATE NOCASE",
    "'abc' = 'abc  ' COLLATE RTRIM",
    "'I' = 'i' COLLATE NOCASE",
]

NULL_EXPRS = [
    "NULL + 1", "NULL || 'x'", "NULL = NULL", "NULL IS NULL",
    "NULL IS NOT NULL", "NULL AND 0", "NULL AND 1", "NULL OR 1",
    "NULL OR 0", "NOT NULL", "1 = NULL", "NULL IS 1",
    "NULL IN (1,2,3)", "NULL NOT IN (1,2,3)",
    "coalesce(NULL, NULL, 3)", "1 <> NULL",
]

FUNCTION_EXPRS = [
    "length('héllo')", "length(x'0011')", "length(12345)", "length(NULL)",
    "upper('café')", "lower('CAFÉ')",
    "substr('hello',-3)", "substr('hello',0)", "substr('hello',2,-1)",
    "substr('hello',-100,2)", "substr('hello',2,2)",
    "abs(-5)", "abs('abc')", "abs(NULL)",
    "coalesce(NULL,NULL,3)", "ifnull(NULL,5)", "nullif(1,1)", "nullif(1,2)",
    "typeof(NULL)", "typeof(1)", "typeof(1.5)", "typeof('a')", "typeof(x'00')",
    "hex('AB')", "hex(5)", "hex(1.5)", "unhex('4142')", "unhex('xyz')",
    "quote('it''s')", "quote(x'0011')", "quote(NULL)", "quote(1.5)",
    "min(3,1,2)", "max(3,1,2)", "min(1,NULL)", "max(1,NULL)",
    "round(2.5)", "round(-2.5)", "round(1.5,0)",
    "sign(-5)", "sign(0)", "sign(5)", "sign(NULL)",
    "instr('hello world','wor')", "instr('hello','x')", "instr(NULL,'x')",
    "trim('  hi  ')", "ltrim('xxhixx','x')", "rtrim('xxhixx','x')",
    "replace('abcabc','a','Z')",
    "typeof(zeroblob(3))", "length(zeroblob(3))",
    "iif(1,'a','b')", "iif(0,'a','b')", "iif(NULL,'a','b')",
    "iif('0.0','a','b')",
    "round(1.5,NULL)", "round(1.5,40)",
    "length(zeroblob(-1))",
    "like('a%b','axxb')", "like('a\\%b','a%b','\\')", "like('abc','ABC')",
    "like('x',NULL)", "glob('a[^b]c','abc')", "glob('a[^b]c','axc')",
    "glob('a?c','abc')", "glob('abc','ABC')",
]

COERCION_EXPRS = [
    "'123' + 1", "'123abc' + 1", "'abc' + 1", "'  123  ' + 1",
    "'1e3' + 1", "'0x10' + 1", "'' + 1", "'.5' + 1",
    "9223372036854775807 + 1", "9223372036854775807 + 1.0",
    "-9223372036854775808 - 1",
    "9223372036854775807 * 2",
    "cast('123' AS INTEGER)", "cast('123abc' AS INTEGER)",
    "cast('abc' AS REAL)", "cast(3.9 AS INTEGER)",
    "cast(-3.9 AS INTEGER)",
]


WALKER_EXPRS = [
    # CASE (searched and simple forms)
    "CASE WHEN 1=0 THEN 'a' WHEN 1=1 THEN 'b' ELSE 'c' END",
    "CASE WHEN NULL THEN 'a' ELSE 'b' END",
    "CASE WHEN 0 THEN 'a' END",
    "CASE 2 WHEN 1 THEN 'a' WHEN 2 THEN 'b' ELSE 'c' END",
    "CASE NULL WHEN NULL THEN 'a' ELSE 'b' END",
    "CASE 1 WHEN 1 THEN 'a' WHEN 1 THEN 'b' END",
    # CAST
    "CAST('123' AS INTEGER)",
    "CAST('123abc' AS INTEGER)",
    "CAST('abc' AS REAL)",
    "CAST(3.9 AS INTEGER)",
    "CAST(-3.9 AS INTEGER)",
    "CAST(123 AS TEXT)",
    "CAST(1.5 AS TEXT)",
    "CAST(NULL AS INTEGER)",
    "CAST('abc' AS BLOB)",
    "CAST(x'4142' AS TEXT)",
    "CAST('99999999999999999999' AS INTEGER)",
    # #142: NUMERIC target and BLOB-from-numeric, plus the NUMERIC
    # downgrade rule (a text/blob source with no fractional part
    # downgrades to INTEGER; an already-REAL source does not).
    "CAST(5 AS NUMERIC)",
    "CAST(5.0 AS NUMERIC)",
    "CAST('5.0' AS NUMERIC)",
    "CAST('abc' AS NUMERIC)",
    "CAST(x'4142' AS NUMERIC)",
    "CAST(5 AS BLOB)",
    "CAST(5.5 AS BLOB)",
    "CAST(x'4142' AS INTEGER)",
    "CAST(x'4142' AS REAL)",
    "CAST(1e300 AS INTEGER)",
    "CAST(-1e300 AS INTEGER)",
    "CAST(NULL AS TEXT)",
    "CAST(NULL AS BLOB)",
    "CAST('-99999999999999999999' AS INTEGER)",
    "CAST(9223372036854775807 AS REAL)",
    "CAST(x'4142' AS BLOB)",
    "CAST(x'34' AS NUMERIC)",
    "CAST(x'3435' AS INTEGER)",
    "CAST('123.5' AS NUMERIC)",
    # LIKE / GLOB
    "'abc' LIKE 'abc'",
    "'ABC' LIKE 'abc'",
    "'axxb' LIKE 'a%b'",
    "'ab' LIKE 'a%b'",
    "'axb' LIKE 'a_b'",
    "'a%b' LIKE 'a\\%b' ESCAPE '\\'",
    "'a_b' LIKE 'a\\_b' ESCAPE '\\'",
    "NULL LIKE 'x'",
    "'x' LIKE NULL",
    "'abc' GLOB 'abc'",
    "'ABC' GLOB 'abc'",
    "'abc' GLOB 'a[bc]c'",
    "'abc' GLOB 'a[^b]c'",
    "'axc' GLOB 'a[^b]c'",
    "'abc' GLOB 'a?c'",
    "'abc' GLOB 'a*'",
    "'abc' NOT LIKE 'xyz'",
    # BETWEEN
    "5 BETWEEN 1 AND 10",
    "5 NOT BETWEEN 1 AND 10",
    "5 BETWEEN 10 AND 1",
    "NULL BETWEEN 1 AND 10",
    "5 BETWEEN NULL AND 10",
    # IN-list
    "5 IN (1,2,3)",
    "5 IN (1,2,5)",
    "5 NOT IN (1,2,3)",
    "NULL IN (1,2,3)",
    "5 IN (1,NULL,5)",
    "5 IN (1,NULL,3)",
    "5 NOT IN (1,NULL,3)",
    # Short-circuit AND/OR (functional result only — evaluation-order
    # findings are behavioral, captured in findings.md, not here)
    "0 AND (1/0)",
    "1 OR (1/0)",
    "NULL AND 0",
    "NULL OR 1",
    # Arithmetic: division/modulo/bitwise/concat/unary
    "7/2", "-7/2", "7.0/2", "7/0", "7/2.0",
    "7%2", "-7%2", "7%2.5", "7%0",
    "5&3", "5|2", "1<<3", "256>>4", "5&3.5",
    "-5", "~5", "-9223372036854775808",
    "'a'||'b'", "1||2", "1.5||'x'", "NULL||'x'",
]


def write_jsonl(path, vectors):
    with path.open("w") as f:
        for v in vectors:
            f.write(json.dumps(v, sort_keys=True, ensure_ascii=False) + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--oracle", help="explicit sqlite3 binary path")
    args = parser.parse_args()

    oracle = find_oracle(args.oracle)
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    write_jsonl(OUT_DIR / "affinity.jsonl", eval_affinity(oracle))
    write_jsonl(OUT_DIR / "comparison.jsonl", eval_exprs(oracle, COMPARISON_EXPRS))
    write_jsonl(OUT_DIR / "collation.jsonl", eval_exprs(oracle, COLLATION_EXPRS))
    write_jsonl(OUT_DIR / "null.jsonl", eval_exprs(oracle, NULL_EXPRS))
    write_jsonl(OUT_DIR / "coercion.jsonl", eval_exprs(oracle, COERCION_EXPRS))
    write_jsonl(OUT_DIR / "functions.jsonl", eval_exprs(oracle, FUNCTION_EXPRS))
    write_jsonl(OUT_DIR / "walker.jsonl", eval_exprs(oracle, WALKER_EXPRS))

    print(f"wrote vectors to {OUT_DIR} using oracle {oracle} ({ORACLE_VERSION})")


if __name__ == "__main__":
    main()
