#!/usr/bin/env python3
"""Grammar drift check — compare .openspec/grammar/sqlite.ebnf against SQLite's parse.y.

SQLite publishes no EBNF; the authority is src/parse.y (Lemon grammar).
Our EBNF is a structural re-derivation whose rules carry annotations of
the form `[parse.y:LINE rulename]` or `[parse.y rulename ...]`. This tool
keeps the two honest against each other:

Features:

1.  Pinned download: fetches parse.y at the pinned SQLite version tag
    (PARSE_Y_URL) into a local cache (target/parse.y-<version>, not
    committed — parse.y is public domain but the EBNF is a re-derivation
    by policy). --refresh re-downloads; an existing cache is never
    re-fetched otherwise, so runs are deterministic and offline-friendly.

2.  Rule inventory: extracts every Lemon nonterminal defined in parse.y
    (lines matching `name(ALIAS) ::=` / `name ::=`), deduplicated — the
    denominator of grammar coverage.

3.  Annotation validation: every `[parse.y ...]` annotation in the EBNF
    must reference a rule name that exists in parse.y. Unknown names are
    reported as DRIFT (typo, renamed upstream rule, or version bump).

4.  Line verification: annotations of the form `[parse.y:LINE rulename]`
    are checked — the named rule must be defined within a tolerance
    window of that line (default ±5). Mismatches are reported; they are
    the early-warning signal when the parse.y pin is bumped.

5.  Coverage report: how many distinct parse.y rules the EBNF references,
    per V-block tag (`(* V2 *)`, `(* V3 *)`, ...), plus the stub list of
    future-block rules — same defined-vs-delivered philosophy as
    tools/assurance.py (strict: an annotation is only credit if it
    resolves; anything else is drift, not partial credit).

6.  CI gate: --strict exits 1 on any unknown annotation or line mismatch.

Usage:
    python3 tools/grammar_drift.py             # report
    python3 tools/grammar_drift.py --strict    # CI gate
    python3 tools/grammar_drift.py --refresh   # re-download parse.y
    python3 tools/grammar_drift.py --tolerance 10
"""

import argparse
import re
import sys
import tomllib
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
EBNF_PATH = REPO_ROOT / ".openspec" / "grammar" / "sqlite.ebnf"

# Pinned version comes from Cargo.toml's [package.metadata.oracle] — the one
# place the sqlite3 pin is declared (see the comment there before bumping it).
CARGO_TOML = REPO_ROOT / "Cargo.toml"
SQLITE_VERSION = tomllib.loads(CARGO_TOML.read_text())["package"]["metadata"]["oracle"]["version"]
PARSE_Y_URL = (
    f"https://raw.githubusercontent.com/sqlite/sqlite/version-{SQLITE_VERSION}/src/parse.y"
)
CACHE_PATH = REPO_ROOT / "target" / f"parse.y-{SQLITE_VERSION}"


def fetch_parse_y(refresh=False):
    """Download parse.y at the pinned version into the cache (feature 1)."""
    if CACHE_PATH.exists() and not refresh:
        return CACHE_PATH.read_text()
    CACHE_PATH.parent.mkdir(parents=True, exist_ok=True)
    print(f"fetching {PARSE_Y_URL}")
    with urllib.request.urlopen(PARSE_Y_URL) as resp:
        text = resp.read().decode("utf-8")
    CACHE_PATH.write_text(text)
    return text


# Lemon rule definition: `name(A) ::= ...` or `name ::= ...` at line start.
LEMON_RULE_RE = re.compile(r"^([a-z_][a-z0-9_]*)\s*(?:\([A-Za-z0-9_]+\))?\s*::=", re.MULTILINE)


def parse_y_rules(text):
    """Extract {rulename: [line numbers]} for every Lemon nonterminal (feature 2)."""
    rules = {}
    for i, line in enumerate(text.splitlines(), start=1):
        m = LEMON_RULE_RE.match(line)
        if m:
            rules.setdefault(m.group(1), []).append(i)
    return rules


# EBNF annotations: `[parse.y:520 cmd]`, `[parse.y:295-309]`,
# `[parse.y expr ::= ...]`, `[parse.y:207-247 create_table, create_table_args]`
ANNOTATION_RE = re.compile(r"\[parse\.y(?::(\d+)(?:-\d+)?)?\s*([^\]]*)\]")
# Rule-name tokens inside an annotation body (lowercase lemon identifiers)
NAME_RE = re.compile(r"\b([a-z_][a-z0-9_]{2,})\b")
# Words that appear in annotation prose but are not rule names
STOPWORDS = {
    "expr", "cmd",  # real rules — keep (handled normally); listed here only if needed
}
PROSE_WORDS = {
    "subset", "only", "single", "table", "see", "full", "line", "and", "the",
    "keywords", "fall", "back", "keyword", "fallback", "write", "semantics",
    "with", "aggregates", "planner", "partial", "index", "need", "functions",
    "rulename",  # the header's format example, not a rule
}
VBLOCK_RE = re.compile(r"\(\*\s*(V\d+)[^)]*\*\)")


def ebnf_annotations(text):
    """Extract (line, cited_parse_y_line_or_None, [rule names]) from the EBNF (feature 3)."""
    found = []
    for i, line in enumerate(text.splitlines(), start=1):
        for m in ANNOTATION_RE.finditer(line):
            cited_line = int(m.group(1)) if m.group(1) else None
            body = m.group(2)
            # Strip lemon RHS notation from name harvesting: take tokens
            # before '::=' if present (the rule name), else all candidates.
            if "::=" in body:
                body = body.split("::=")[0]
            # Multi-segment annotations: `[parse.y:1082 cmd, :1108 insert_cmd,
            # :1122 idlist_opt]` — each `:LINE name` segment carries its own
            # citation; segments without a line inherit the leading one.
            segments = body.split(",")
            for seg in segments:
                seg_line = cited_line
                m2 = re.match(r"\s*:(\d+)(?:-\d+)?\s*(.*)", seg)
                if m2:
                    seg_line = int(m2.group(1))
                    seg = m2.group(2)
                names = [n for n in NAME_RE.findall(seg) if n not in PROSE_WORDS]
                if names:
                    found.append((i, seg_line, names))
    return found


def ebnf_vblock_counts(text):
    """Count EBNF rules per V-block tag (feature 5)."""
    counts = {}
    for m in VBLOCK_RE.finditer(text):
        counts[m.group(1)] = counts.get(m.group(1), 0) + 1
    return counts


def main():
    ap = argparse.ArgumentParser(description="EBNF vs parse.y drift check")
    ap.add_argument("--strict", action="store_true", help="exit 1 on drift (CI gate)")
    ap.add_argument("--refresh", action="store_true", help="re-download parse.y")
    ap.add_argument("--tolerance", type=int, default=5,
                    help="line-number tolerance for [parse.y:LINE name] checks")
    args = ap.parse_args()

    parse_y = fetch_parse_y(refresh=args.refresh)
    rules = parse_y_rules(parse_y)
    ebnf = EBNF_PATH.read_text()
    annotations = ebnf_annotations(ebnf)

    unknown = []       # (ebnf_line, name)
    line_mismatch = [] # (ebnf_line, name, cited, actual_lines)
    referenced = set()

    for ebnf_line, cited, names in annotations:
        for name in names:
            if name not in rules:
                unknown.append((ebnf_line, name))
                continue
            referenced.add(name)
            if cited is not None:
                actual = rules[name]
                if not any(abs(a - cited) <= args.tolerance for a in actual):
                    line_mismatch.append((ebnf_line, name, cited, actual))

    vblocks = ebnf_vblock_counts(ebnf)

    print("=" * 60)
    print(f"Grammar drift check — sqlite.ebnf vs parse.y {SQLITE_VERSION}")
    print("=" * 60)
    print(f"parse.y nonterminals:    {len(rules)}")
    print(f"referenced by EBNF:      {len(referenced)}  ({len(referenced)/len(rules):.0%} of parse.y)")
    print(f"annotations checked:     {sum(len(n) for _, _, n in annotations)}")
    print(f"EBNF rules per V-block:  " + ", ".join(f"{k}: {v}" for k, v in sorted(vblocks.items())))
    print()
    if unknown:
        print(f"DRIFT — unknown rule names ({len(unknown)}):")
        for line, name in unknown:
            print(f"  sqlite.ebnf:{line}  '{name}' not defined in parse.y")
    if line_mismatch:
        print(f"DRIFT — line citations off by more than ±{args.tolerance} ({len(line_mismatch)}):")
        for line, name, cited, actual in line_mismatch:
            print(f"  sqlite.ebnf:{line}  [{name}] cited :{cited}, defined at {actual}")
    if not unknown and not line_mismatch:
        print("No drift: every annotation resolves to a parse.y rule at its cited location.")
    print("=" * 60)

    if args.strict and (unknown or line_mismatch):
        sys.exit(1)


if __name__ == "__main__":
    main()
