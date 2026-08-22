#!/usr/bin/env python3
"""MC/DC harvest dashboard — summarizes `cargo-mvl-mcdc harvest`'s raw output
(a JSON array of DischargeRecord followed by a summary line and a plain-text
`undischarged: ...` listing) into a short dashboard by default, or a full
per-obligation breakdown with `--verbose`.

`vectors_required` doubles as a leaf-count signal: 0 means compiler-void
(free discharge, exhaustive `match`), 2 means a single-leaf `if`/`while`
(plain branch coverage, not a real MC/DC candidate), and 3+ means a genuine
multi-leaf `&&`/`||` decision — the obligations this project's tagged-test
convention (#52) actually targets.

Usage:
    cargo-mvl-mcdc harvest --obligations=FILE --run-dir=. | python3 tools/mcdc_report.py
    cargo-mvl-mcdc harvest --obligations=FILE --run-dir=. | python3 tools/mcdc_report.py --verbose
"""

import argparse
import json
import sys


def parse_records(raw: str) -> list[dict]:
    start = raw.index("[")
    depth = 0
    for i in range(start, len(raw)):
        if raw[i] == "[":
            depth += 1
        elif raw[i] == "]":
            depth -= 1
            if depth == 0:
                return json.loads(raw[start : i + 1])
    raise ValueError("unterminated JSON array in harvest output")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verbose", action="store_true", help="list every obligation, not just the dashboard")
    args = parser.parse_args()

    raw = sys.stdin.read()
    records = parse_records(raw)

    void = [r for r in records if r["compiler_void"]]
    single_leaf = [r for r in records if not r["compiler_void"] and r["vectors_required"] == 2]
    multi_leaf = [r for r in records if not r["compiler_void"] and r["vectors_required"] >= 3]
    total_discharged = sum(1 for r in records if r["discharged"])

    print("MC/DC dashboard (src/btree.rs + src/btree/ — #52, more modules to follow)")
    print(f"  total obligations:        {len(records)}")
    print(f"  compiler-void (free):     {len(void)}")
    print(f"  single-leaf branches:     {len(single_leaf)} (plain branch coverage, not tagged by convention)")
    print(f"  multi-leaf (real MC/DC):  {len(multi_leaf)} total, "
          f"{sum(1 for r in multi_leaf if r['discharged'])} discharged")
    print(f"  overall discharged:       {total_discharged}/{len(records)} "
          f"({100 * total_discharged / len(records):.1f}%)")

    if not args.verbose:
        print()
        print("Run with VERBOSE=1 for the per-obligation action list (multi-leaf only).")
        return 0

    # Single-leaf branches and compiler-void obligations are out of scope by
    # convention (see dashboard counts above) — verbose mode only breaks
    # down the multi-leaf obligations, since those are the only ones this
    # ticket's tagged-test convention actually targets.
    print()
    for r in multi_leaf:
        passing_vectors = {t["vector"] for t in r["tagged_tests"] if t["passed"]}
        failing_tests = [t for t in r["tagged_tests"] if not t["passed"]]

        if r["discharged"]:
            print(f"DISCHARGED  {r['id']} ({r['file']}:{r['line']})")
            continue

        missing_vectors = sorted(set(range(1, r["vectors_required"] + 1)) - passing_vectors)
        for v in missing_vectors:
            print(f"ADD TEST    {r['id']} ({r['file']}:{r['line']}) -- "
                  f"tag a test mcdc__{r['id']}__v{v}_<description>")
        for t in failing_tests:
            print(f"FIX TEST    {r['id']} ({r['file']}:{r['line']}) -- {t['name']} is failing")

    return 0


if __name__ == "__main__":
    sys.exit(main())
