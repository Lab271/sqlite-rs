#!/usr/bin/env python3
"""Opcode harvest for the V2 (single-table SELECT) query class — spike 007 (#58).

Runs the pinned oracle sqlite3's `EXPLAIN` over a representative set of
single-table SELECT queries (WHERE, ORDER BY, LIMIT/OFFSET, expressions,
comparisons, arithmetic, common scalar functions — the V2 grammar slice per
epic #56) and harvests every `(opcode, p4-variant)` pair that appears.

Emits `tools/opcodes-v2.json`: the opcode set, per-opcode frequency, an
example query per opcode, and a classification into cursor / control /
compare / arithmetic / function / sorter / result / other. This becomes
phase 3's VDBE scope definition and completeness checklist.

Query set caveat: the #2 parser-corpus slice and a vendored sqllogictest
subset don't exist in this repo yet (#2 is deferred pending spike #57), so
the queries below are hand-authored against an ad-hoc single-table schema
rather than drawn from a corpus. Re-run this tool once #2 lands to widen
the input set — this run's output should be treated as a first cut.

Usage: python3 tools/harvest_opcodes.py [--oracle /path/to/sqlite3]
"""

import argparse
import json
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

CARGO_TOML = Path(__file__).parent.parent / "Cargo.toml"
ORACLE_VERSION = tomllib.loads(CARGO_TOML.read_text())["package"]["metadata"]["oracle"]["version"]
ORACLE_CANDIDATES = [
    "/opt/homebrew/opt/sqlite/bin/sqlite3",
    "/usr/local/opt/sqlite/bin/sqlite3",
    "sqlite3",
]

SCHEMA_SQL = """
CREATE TABLE products(id INTEGER PRIMARY KEY, name TEXT, price REAL, qty INTEGER, note TEXT);
INSERT INTO products VALUES(1, 'widget', 9.99, 100, 'blue');
INSERT INTO products VALUES(2, 'gadget', 19.99, 5, NULL);
INSERT INTO products VALUES(3, 'gizmo', 29.99, 0, 'red');
"""

# The V2 grammar slice: single-FROM SELECT, WHERE, ORDER BY, LIMIT/OFFSET,
# comparisons, arithmetic, common scalar functions. No JOINs, subqueries,
# GROUP BY, or aggregates — those land in later V-blocks.
QUERIES = [
    "SELECT * FROM products",
    "SELECT name, price FROM products",
    "SELECT DISTINCT note FROM products",
    "SELECT * FROM products WHERE price > 10",
    "SELECT * FROM products WHERE price >= 10 AND qty < 50",
    "SELECT * FROM products WHERE id = 2",
    "SELECT * FROM products WHERE id <> 2",
    "SELECT * FROM products WHERE note IS NULL",
    "SELECT * FROM products WHERE note IS NOT NULL",
    "SELECT * FROM products WHERE name LIKE 'g%'",
    "SELECT * FROM products WHERE id IN (1, 2, 3)",
    "SELECT * FROM products WHERE price BETWEEN 10 AND 30",
    "SELECT * FROM products ORDER BY price",
    "SELECT * FROM products ORDER BY price DESC, name ASC",
    "SELECT * FROM products LIMIT 2",
    "SELECT * FROM products LIMIT 2 OFFSET 1",
    "SELECT * FROM products ORDER BY price LIMIT 1",
    "SELECT price * qty AS total FROM products",
    "SELECT price + 1, price - 1, price / 2, qty % 2 FROM products",
    "SELECT -price FROM products",
    "SELECT length(name), upper(name), lower(name) FROM products",
    "SELECT abs(price), round(price, 1) FROM products",
    "SELECT coalesce(note, 'none') FROM products",
    "SELECT ifnull(note, 'none') FROM products",
    "SELECT CASE WHEN price > 10 THEN 'expensive' ELSE 'cheap' END FROM products",
    "SELECT rowid, * FROM products WHERE rowid = 2",
]

CLASSIFICATION = {
    # control
    "Init": "control", "Goto": "control", "Halt": "control",
    "If": "control", "IfNot": "control", "IfNullRow": "control",
    "Once": "control", "Gosub": "control", "Return": "control",
    "NotNull": "control", "IsNull": "control", "MustBeInt": "control",
    "NoConflict": "control", "NotExists": "control", "HaltIfNull": "control",
    "Noop": "control", "Explain": "control", "ParseSchema": "control",
    "Transaction": "control", "ReadCookie": "control", "SetCookie": "control",
    "TableLock": "control", "VerifyCookie": "control", "Expire": "control",
    # cursor
    "OpenRead": "cursor", "OpenWrite": "cursor", "OpenEphemeral": "cursor",
    "OpenAutoindex": "cursor", "OpenPseudo": "cursor", "Close": "cursor",
    "Rewind": "cursor", "Next": "cursor", "Prev": "cursor", "Last": "cursor",
    "SeekRowid": "cursor", "NotFound": "cursor", "Found": "cursor",
    "SeekGE": "cursor", "SeekGT": "cursor", "SeekLE": "cursor", "SeekLT": "cursor",
    "IdxGE": "cursor", "IdxGT": "cursor", "IdxLE": "cursor", "IdxLT": "cursor",
    "Rowid": "cursor", "Column": "cursor", "IdxRowid": "cursor",
    "NewRowid": "cursor", "DeferredSeek": "cursor",
    # compare
    "Eq": "compare", "Ne": "compare", "Lt": "compare", "Le": "compare",
    "Gt": "compare", "Ge": "compare", "Compare": "compare", "Permutation": "compare",
    "ElseNotEq": "compare", "IsNullOrType": "compare",
    # arithmetic
    "Add": "arithmetic", "Subtract": "arithmetic", "Multiply": "arithmetic",
    "Divide": "arithmetic", "Remainder": "arithmetic", "BitAnd": "arithmetic",
    "BitOr": "arithmetic", "ShiftLeft": "arithmetic", "ShiftRight": "arithmetic",
    "BitNot": "arithmetic", "Negative": "arithmetic", "Concat": "arithmetic",
    "Cast": "arithmetic", "AddImm": "arithmetic",
    # function
    "Function": "function", "PureFunc": "function",
    "AggStep": "function", "AggFinal": "function",
    # limit/offset counters and subroutine control (not in the issue's
    # taxonomy verbatim — grouped under control as the closest fit)
    "IfNotZero": "control", "IfPos": "control", "DecrJumpZero": "control",
    "OffsetLimit": "control", "BeginSubrtn": "control",
    # comparison-adjacent coercion/setup
    "RealAffinity": "compare", "CollSeq": "compare",
    # ephemeral-table bookkeeping (DISTINCT, scalar-subquery materialization)
    "Sequence": "cursor", "IdxInsert": "cursor", "Delete": "cursor", "NullRow": "cursor",
    # sorter
    "SorterOpen": "sorter", "SorterInsert": "sorter", "SorterSort": "sorter",
    "SorterNext": "sorter", "SorterData": "sorter", "SorterCompare": "sorter",
    "Sort": "sorter",
    # result
    "ResultRow": "result", "MakeRecord": "result", "Copy": "result",
    "SCopy": "result", "Move": "result", "Integer": "result", "Real": "result",
    "String8": "result", "Blob": "result", "Null": "result", "ZeroOrNull": "result",
    "Variable": "result", "IntCopy": "result",
}


def find_oracle(explicit):
    candidates = [explicit] if explicit else []
    candidates += ORACLE_CANDIDATES
    for candidate in candidates:
        if not candidate:
            continue
        try:
            out = subprocess.run(
                [candidate, "-version"], capture_output=True, text=True, check=True
            ).stdout
        except (FileNotFoundError, subprocess.CalledProcessError):
            continue
        version = out.split()[0]
        if version != ORACLE_VERSION:
            continue
        compile_options = subprocess.run(
            [candidate, ":memory:", "PRAGMA compile_options;"],
            capture_output=True, text=True, check=True,
        ).stdout
        if "codec" in compile_options.lower():
            continue
        return candidate
    return None


def explain(oracle, db_path, query):
    # EXPLAIN gets a hardcoded fixed-column display in the CLI regardless of
    # `.mode`/`.separator` when they're passed as positional SQL arguments
    # (only the first positional argument after FILENAME is treated as SQL,
    # the rest are additional statements, not shell dot-commands) — `.mode`
    # and `.explain off` must go through `-cmd` instead to actually take
    # effect before EXPLAIN runs.
    result = subprocess.run(
        [oracle, "-cmd", ".mode list", "-cmd", ".separator |", "-cmd", ".explain off",
         db_path, f"EXPLAIN {query};"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        return None, result.stderr.strip()
    return result.stdout, None


def harvest(oracle, db_path):
    opcodes = {}
    skipped = []
    for query in QUERIES:
        output, err = explain(oracle, db_path, query)
        if err is not None:
            skipped.append({"query": query, "error": err})
            continue
        for line in output.splitlines():
            fields = line.split("|")
            if len(fields) < 8:
                continue
            # fields: addr|opcode|p1|p2|p3|p4|p5|comment
            opcode, p4 = fields[1], fields[5]
            entry = opcodes.setdefault(
                opcode,
                {"count": 0, "p4_variants": set(), "example_query": query,
                 "category": CLASSIFICATION.get(opcode, "other")},
            )
            entry["count"] += 1
            if p4:
                entry["p4_variants"].add(p4)
    return opcodes, skipped


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", help="path to pinned sqlite3 binary (overrides auto-detect)")
    parser.add_argument(
        "--out", default=str(Path(__file__).parent / "opcodes-v2.json"),
        help="output JSON path (default: tools/opcodes-v2.json)",
    )
    args = parser.parse_args()

    oracle = find_oracle(args.oracle)
    if oracle is None:
        print(
            f"error: no pinned non-codec sqlite3 {ORACLE_VERSION} found "
            "(set --oracle or ORACLE_SQLITE3-equivalent path)",
            file=sys.stderr,
        )
        return 1

    with tempfile.TemporaryDirectory() as tmp:
        db_path = str(Path(tmp) / "harvest.db")
        subprocess.run([oracle, db_path, SCHEMA_SQL], capture_output=True, text=True, check=True)
        opcodes, skipped = harvest(oracle, db_path)

    output = {
        "oracle_version": ORACLE_VERSION,
        "query_count": len(QUERIES),
        "opcode_count": len(opcodes),
        "opcodes": {
            name: {
                "count": entry["count"],
                "p4_variants": sorted(entry["p4_variants"]),
                "example_query": entry["example_query"],
                "category": entry["category"],
            }
            for name, entry in sorted(opcodes.items())
        },
        "skipped_queries": skipped,
    }

    Path(args.out).write_text(json.dumps(output, indent=2) + "\n")
    print(f"oracle: {oracle} (sqlite3 {ORACLE_VERSION})")
    print(f"harvested {len(opcodes)} opcodes from {len(QUERIES)} queries ({len(skipped)} skipped)")
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
