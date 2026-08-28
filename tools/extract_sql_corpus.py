#!/usr/bin/env python3
"""SQL corpus extraction — pull real SQL statements from the external test suites.

Issue #70, follow-up to #2. #2 hand-authored a small three-way labeled corpus
scoped to the V2 single-table SELECT slice; this tool replaces hand-authoring
with extraction from the two corpora SQLite itself is validated against:

1.  **sqllogictest** — the 7.2M-query suite from https://www.sqlite.org/sqllogictest/.
    Line-oriented format: `statement ok|error` followed by SQL until a blank
    line, or `query <TYPES> <sort> [label]` followed by SQL until a `----`
    separator. `skipif <db>` / `onlyif <db>` lines gate the directive that
    follows them.

2.  **SQLite's own TCL suite** — `test/*.test` in the SQLite source tree.
    SQL lives inside `do_execsql_test NAME { SQL } { expected }` and
    `do_catchsql_test NAME { SQL } { 1 {error} }` brace blocks.

Provenance and offline reproducibility
--------------------------------------

Neither corpus is committed whole (110 MB and 13 MB respectively). A curated
subset of source `.test` files is vendored verbatim under
`tests/corpus/sql/vendor/`, and the committed extraction under
`tests/corpus/sql/<category>/` is generated from that vendored subset — so a
clean checkout regenerates byte-identical output with no network access:

    python3 tools/extract_sql_corpus.py            # regenerate from vendor/

`--fetch` instead pulls the full upstream corpora into a `target/` cache (the
same pin-and-cache pattern `tools/grammar_drift.py` uses for parse.y) and
extracts from those, for growing the vendored subset:

    python3 tools/extract_sql_corpus.py --fetch --source sqllogictest

Note on sources: sqlite.org's Fossil tarball endpoint returns an HTML
anti-robot page with a 200 status and cannot be fetched by tooling, so the
sqllogictest pin is the `gregrahn/sqllogictest` GitHub mirror. The TCL pin
tracks `[package.metadata.oracle]` in Cargo.toml so extracted SQL matches the
sqlite3 the corpus tests are labeled against.

Representativeness
------------------

The generated `random/` and `index/` sqllogictest files are enormously
repetitive — millions of queries differing only in literal values. Taking the
first N would yield N near-identical statements. Instead every statement is
reduced to a *shape key* (literals and whitespace normalized away) and at most
`--per-shape` statements are kept per distinct shape, so a cap of N buys N
structurally different statements. Everything dropped is counted and reported;
nothing is silently truncated.

Usage:
    python3 tools/extract_sql_corpus.py                     # regenerate committed corpus
    python3 tools/extract_sql_corpus.py --fetch             # refresh vendored subset from upstream
    python3 tools/extract_sql_corpus.py --limit 2000        # raise the per-category cap
"""

import argparse
import re
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
CORPUS_ROOT = REPO_ROOT / "tests" / "corpus" / "sql"
VENDOR_ROOT = CORPUS_ROOT / "vendor"

# sqlite.org's Fossil tarball endpoint is not scriptable (see module docstring),
# so the sqllogictest pin is the GitHub mirror, pinned by commit SHA.
SQLLOGICTEST_SHA = "c67f97bf3ca7e590d12e073408bcacaf2ff0f3a0"
SQLLOGICTEST_URL = (
    f"https://codeload.github.com/gregrahn/sqllogictest/tar.gz/{SQLLOGICTEST_SHA}"
)

# Read from Cargo.toml's [package.metadata.oracle] — the one place the sqlite3
# pin is declared — so extracted SQL lines up with the sqlite3 the corpus
# labels are validated against.
CARGO_TOML = REPO_ROOT / "Cargo.toml"
SQLITE_VERSION = tomllib.loads(CARGO_TOML.read_text())["package"]["metadata"]["oracle"]["version"]
SQLITE_URL = (
    f"https://codeload.github.com/sqlite/sqlite/tar.gz/refs/tags/version-{SQLITE_VERSION}"
)

CACHE_DIR = REPO_ROOT / "target"

# Curated vendored subsets. sqllogictest's evidence/ files are hand-written and
# statement-type diverse; select1-5 are the hand-written query files that stay
# a sane size (every random/ + index/ file is multi-megabyte generated output
# and stays out of the repo — see vendor/README.md). select3/4/5 push past the
# V2 single-table slice into joins/subqueries/aggregates (V4, .openspec/plan.md).
VENDOR_SQLLOGICTEST = [
    "test/evidence/in1.test",
    "test/evidence/in2.test",
    "test/evidence/slt_lang_aggfunc.test",
    "test/evidence/slt_lang_createtrigger.test",
    "test/evidence/slt_lang_createview.test",
    "test/evidence/slt_lang_dropindex.test",
    "test/evidence/slt_lang_droptable.test",
    "test/evidence/slt_lang_droptrigger.test",
    "test/evidence/slt_lang_dropview.test",
    "test/evidence/slt_lang_reindex.test",
    "test/evidence/slt_lang_replace.test",
    "test/evidence/slt_lang_update.test",
    "test/select1.test",
    "test/select2.test",
    "test/select3.test",
    "test/select4.test",
    "test/select5.test",
]

# TCL files chosen to cover every statement category, widened past the V4
# single-table gate up through V7 per .openspec/plan.md's own corpus
# citations (V4: `join*.test`, `select2-8.test`, `subquery*.test`,
# `with*.test` non-recursive, `aggnested.test`; V6/V7: recursive CTEs
# deferred to V7 (`with3-6.test`); V7: `pragma*.test`, `savepoint*.test`,
# `analyze.test`). This suite — not sqllogictest, which is query-focused and
# whose DML is incidental setup — is where DML and DDL diversity actually
# lives, so the DML/DDL selection here is deliberately broader than the
# SELECT selection.
VENDOR_TCL = [
    # SELECT and expression surface (V1-V4)
    "select1.test", "select2.test", "select3.test", "select4.test", "select5.test",
    "select6.test", "select7.test", "select8.test",
    "expr.test", "func.test", "func2.test", "cast.test", "between.test", "in.test",
    "join.test", "join2.test", "join3.test", "subquery.test", "subquery2.test",
    "aggnested.test", "distinct.test", "orderby1.test",
    # CTEs: non-recursive (V6) through recursive (deferred to V7)
    "with1.test", "with2.test", "with3.test", "with4.test", "with5.test", "with6.test",
    # INSERT / UPDATE / DELETE
    "insert.test", "insert2.test", "insert3.test", "insert4.test", "insert5.test",
    "update.test", "update2.test", "delete.test", "delete2.test", "delete4.test",
    "upsert1.test", "upsert2.test", "conflict.test", "returning1.test",
    # DDL
    "table.test", "createtab.test", "alter.test", "altertab.test", "altertab2.test",
    "altertab3.test", "view.test", "index.test", "index3.test", "index4.test",
    "index6.test", "index7.test", "trigger2.test",
    # V7: transactions/pragmas/introspection
    "savepoint.test", "savepoint2.test", "pragma.test", "pragma2.test", "analyze.test",
]

CATEGORIES = ["select", "insert", "update", "delete", "ddl"]

# Leading keyword -> category. Anything unmatched is counted as "other" and
# dropped (reported, not silent).
CATEGORY_KEYWORDS = {
    "select": "select",
    "with": "select",
    "values": "select",
    "insert": "insert",
    "replace": "insert",
    "update": "update",
    "delete": "delete",
    "create": "ddl",
    "drop": "ddl",
    "alter": "ddl",
    "reindex": "ddl",
}


def fetch(url, cache_name):
    """Download a tarball into the target/ cache; never re-fetch if present."""
    path = CACHE_DIR / cache_name
    if path.exists():
        return path
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    print(f"fetching {url}", file=sys.stderr)
    with urllib.request.urlopen(url) as resp:
        data = resp.read()
    path.write_bytes(data)
    return path


def read_members(tar_path, wanted_suffixes):
    """Yield (relative_path, text) for tar members whose path ends in a wanted suffix."""
    with tarfile.open(tar_path, "r:gz") as tf:
        for member in tf:
            if not member.isfile():
                continue
            # Strip the tarball's top-level directory.
            rel = member.name.split("/", 1)[1] if "/" in member.name else member.name
            if rel in wanted_suffixes:
                fh = tf.extractfile(member)
                if fh is not None:
                    yield rel, fh.read().decode("utf-8", errors="replace")


# --- sqllogictest parsing -------------------------------------------------

# `statement ok` / `statement error`; `query <TYPES> <sort> [label]`.
STATEMENT_RE = re.compile(r"^statement\s+(ok|error)\s*$")
QUERY_RE = re.compile(r"^query\s+\S+")
DIRECTIVE_RE = re.compile(r"^(skipif|onlyif|halt|hash-threshold|control)\b")


def parse_sqllogictest(text):
    """Yield (sql, expect_ok) for every statement/query block in a .test file."""
    lines = text.splitlines()
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i].strip()
        i += 1
        if not line or line.startswith("#") or DIRECTIVE_RE.match(line):
            continue

        m = STATEMENT_RE.match(line)
        if m:
            expect_ok = m.group(1) == "ok"
            body, i = _take_until_blank(lines, i)
            if body:
                yield body, expect_ok
            continue

        if QUERY_RE.match(line):
            # SQL runs until the `----` expected-results separator (or a blank
            # line, for query blocks with no recorded results).
            body_lines = []
            while i < n and lines[i].strip() and lines[i].strip() != "----":
                body_lines.append(lines[i])
                i += 1
            # Skip past the expected-results block.
            if i < n and lines[i].strip() == "----":
                i += 1
                while i < n and lines[i].strip():
                    i += 1
            if body_lines:
                yield "\n".join(body_lines).strip(), True


def _take_until_blank(lines, i):
    """Collect lines from i until a blank line; return (text, new_index)."""
    body = []
    while i < len(lines) and lines[i].strip():
        body.append(lines[i])
        i += 1
    return "\n".join(body).strip(), i


# --- TCL parsing ----------------------------------------------------------

# Two anchor kinds. `do_execsql_test NAME {SQL} {expected}` puts a test-name
# token between the construct and the SQL; bare `execsql {SQL}` does not.
# Ordering matters: the alternation must try the long names first. `\b` before
# `execsql` cannot match inside `do_execsql_test` (the preceding `_` is a word
# character), so the bare forms do not double-match the wrapped ones.
TCL_TEST_RE = re.compile(r"\b(do_execsql_test|do_catchsql_test|execsql|catchsql)\b")
TCL_NAMED_ANCHORS = {"do_execsql_test", "do_catchsql_test"}
# TCL string interpolation (`$var`), command substitution (`[cmd]`) and
# `format` templates (`%s`, `%d`) — none resolvable without a TCL interpreter.
# Blocks containing these are skipped, and counted.
TCL_DYNAMIC_RE = re.compile(r"[$\[]|%[sdq]\b")


def _match_braces(text, start):
    """Given text[start] == '{', return (inner, index_after_closing_brace)."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if c == "\\":
            i += 2
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1 : i], i + 1
        i += 1
    return None, len(text)


def parse_tcl(text):
    """Yield (sql, expect_ok, dynamic) for every TCL block carrying literal SQL.

    `dynamic` is True when the block's SQL relies on TCL interpolation (`$var`,
    `[cmd]`) and so cannot be resolved without a TCL interpreter; the caller
    counts these for the skip report rather than dropping them silently.
    """
    for m in TCL_TEST_RE.finditer(text):
        anchor = m.group(1)
        expect_ok = anchor in ("do_execsql_test", "execsql")
        i = m.end()

        if anchor in TCL_NAMED_ANCHORS:
            # Skip the test-name token, which may itself be braced, to land on
            # the SQL brace group.
            brace = _skip_test_name(text, i)
        else:
            brace = text.find("{", i)
            # A bare execsql's brace must follow closely; anything further off
            # is an unrelated block (e.g. `execsql $sql`).
            if brace == -1 or not text[i:brace].strip() == "":
                continue
            # The legacy idiom `set v [catch {execsql {BAD SQL}} msg]` is an
            # error test, but the `catch` sits outside the anchor. Look back a
            # short way for it, else deliberately-invalid SQL lands in the
            # valid corpus.
            if "catch" in text[max(0, m.start() - 40) : m.start()]:
                expect_ok = False

        if brace == -1:
            continue
        sql, _ = _match_braces(text, brace)
        if sql is None:
            continue
        sql = sql.strip()
        if not sql:
            continue
        yield sql, expect_ok, bool(TCL_DYNAMIC_RE.search(sql))


def _skip_test_name(text, i):
    """From just after a do_*_test construct, return the index of the SQL brace.

    The test name is either a bare word (`do_execsql_test select1-1.4 {SQL}`)
    or a braced short word (`do_execsql_test {name} {SQL}`). Returns -1 if no
    plausible SQL brace group follows.
    """
    brace = text.find("{", i)
    if brace == -1:
        return -1
    between = text[i:brace]
    # A newline between construct and first brace means the name was bare and
    # the brace group we found is already the SQL.
    inner, after = _match_braces(text, brace)
    if inner is None:
        return -1
    # A braced test name is short, single-line, and followed by another brace
    # group on the same line — that second group is the SQL.
    if "\n" not in inner and len(inner) < 60 and "\n" not in between:
        nxt = text.find("{", after)
        if nxt != -1 and "\n" not in text[after:nxt]:
            return nxt
    return brace


# --- normalization, categorization, selection -----------------------------

STRING_LITERAL_RE = re.compile(r"'(?:[^']|'')*'")
NUMBER_RE = re.compile(r"\b\d+(?:\.\d+)?(?:[eE][-+]?\d+)?\b")
WHITESPACE_RE = re.compile(r"\s+")
LINE_COMMENT_RE = re.compile(r"--[^\n]*")


def split_statements(block):
    """Split a block into individual statements on top-level semicolons.

    Naive but adequate: semicolons inside string literals are masked first.
    Blocks containing BEGIN...END (triggers) are kept whole, since their inner
    semicolons are structural.
    """
    if re.search(r"\bBEGIN\b", block, re.IGNORECASE) and re.search(r"\bEND\b", block, re.IGNORECASE):
        return [block.strip()]
    masked = STRING_LITERAL_RE.sub(lambda m: "'" + "\x00" * (len(m.group()) - 2) + "'", block)
    parts = []
    start = 0
    for idx, ch in enumerate(masked):
        if ch == ";":
            parts.append(block[start:idx])
            start = idx + 1
    parts.append(block[start:])
    return [p.strip() for p in parts if p.strip()]


def normalize(sql):
    """Collapse a statement to single-line form for storage."""
    sql = LINE_COMMENT_RE.sub(" ", sql)
    return WHITESPACE_RE.sub(" ", sql).strip().rstrip(";")


def shape_key(sql):
    """Reduce a statement to its structural shape, ignoring literal values."""
    s = STRING_LITERAL_RE.sub("'s'", sql)
    s = NUMBER_RE.sub("0", s)
    return WHITESPACE_RE.sub(" ", s).strip().lower()


def categorize(sql):
    m = re.match(r"\s*([a-zA-Z]+)", sql)
    if not m:
        return None
    return CATEGORY_KEYWORDS.get(m.group(1).lower())


class Collector:
    """Accumulates statements per category with shape-diversity capping."""

    def __init__(self, limit, per_shape):
        self.limit = limit
        self.per_shape = per_shape
        self.kept = {c: [] for c in CATEGORIES}
        self.seen_exact = set()
        self.shape_counts = {}
        self.stats = {
            "total": 0, "duplicate": 0, "shape_capped": 0,
            "limit_capped": 0, "uncategorized": 0, "tcl_dynamic": 0,
            "expected_error": 0,
        }

    def add(self, sql):
        self.stats["total"] += 1
        sql = normalize(sql)
        if not sql:
            return
        category = categorize(sql)
        if category is None:
            self.stats["uncategorized"] += 1
            return
        if sql in self.seen_exact:
            self.stats["duplicate"] += 1
            return
        key = shape_key(sql)
        if self.shape_counts.get(key, 0) >= self.per_shape:
            self.stats["shape_capped"] += 1
            return
        if len(self.kept[category]) >= self.limit:
            self.stats["limit_capped"] += 1
            return
        self.seen_exact.add(sql)
        self.shape_counts[key] = self.shape_counts.get(key, 0) + 1
        self.kept[category].append(sql)


def write_corpus(source_name, collector, header_note):
    """Write one .sql file per category; return {category: count}."""
    counts = {}
    for category in CATEGORIES:
        statements = collector.kept[category]
        counts[category] = len(statements)
        out_dir = CORPUS_ROOT / category
        out_dir.mkdir(parents=True, exist_ok=True)
        path = out_dir / f"{source_name}.sql"
        if not statements:
            if path.exists():
                path.unlink()
            continue
        body = "\n".join(statements)
        path.write_text(f"{header_note}\n{body}\n")
    return counts


def vendor_files(tar_path, wanted, dest_root, strip_prefix=""):
    """Copy the curated source .test subset out of a tarball into the repo."""
    dest_root.mkdir(parents=True, exist_ok=True)
    written = 0
    for rel, text in read_members(tar_path, set(wanted)):
        out = dest_root / (rel[len(strip_prefix):] if strip_prefix else rel)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(text)
        written += 1
    return written


def vendored_texts(root):
    """Yield (relative_path, text) for every vendored .test file under root."""
    if not root.exists():
        return
    for path in sorted(root.rglob("*.test")):
        yield path.relative_to(root).as_posix(), path.read_text()


def run(args):
    if args.fetch:
        if args.source in ("sqllogictest", "all"):
            tar = fetch(SQLLOGICTEST_URL, f"sqllogictest-{SQLLOGICTEST_SHA[:12]}.tar.gz")
            vendor_files(tar, VENDOR_SQLLOGICTEST, VENDOR_ROOT / "sqllogictest")
        if args.source in ("tcl", "all"):
            tar = fetch(SQLITE_URL, f"sqlite-{SQLITE_VERSION}.tar.gz")
            wanted = {f"test/{name}" for name in VENDOR_TCL}
            vendor_files(tar, wanted, VENDOR_ROOT / "tcl", strip_prefix="test/")

    summary = {}

    if args.source in ("sqllogictest", "all"):
        collector = Collector(args.limit, args.per_shape)
        files = 0
        for _rel, text in vendored_texts(VENDOR_ROOT / "sqllogictest"):
            files += 1
            for block, expect_ok in parse_sqllogictest(text):
                # `statement error` blocks hold deliberately-invalid SQL; they
                # are negative-test material, not corpus material.
                if not expect_ok:
                    collector.stats["expected_error"] += 1
                    continue
                for stmt in split_statements(block):
                    collector.add(stmt)
        note = (
            "-- Extracted by tools/extract_sql_corpus.py from the vendored\n"
            "-- sqllogictest subset under tests/corpus/sql/vendor/sqllogictest/.\n"
            "-- Do not edit by hand; run `make extract-sql-corpus` to regenerate (#70)."
        )
        summary["sqllogictest"] = (files, collector, write_corpus("sqllogictest", collector, note))

    if args.source in ("tcl", "all"):
        collector = Collector(args.limit, args.per_shape)
        files = 0
        for _rel, text in vendored_texts(VENDOR_ROOT / "tcl"):
            files += 1
            for sql, expect_ok, dynamic in parse_tcl(text):
                if dynamic:
                    collector.stats["tcl_dynamic"] += 1
                    continue
                # do_catchsql_test / catch-wrapped execsql expect failure.
                if not expect_ok:
                    collector.stats["expected_error"] += 1
                    continue
                for stmt in split_statements(sql):
                    collector.add(stmt)
        note = (
            "-- Extracted by tools/extract_sql_corpus.py from the vendored SQLite\n"
            f"-- TCL suite subset (version {SQLITE_VERSION}) under tests/corpus/sql/vendor/tcl/.\n"
            "-- Do not edit by hand; run `make extract-sql-corpus` to regenerate (#70)."
        )
        summary["tcl"] = (files, collector, write_corpus("tcl", collector, note))

    report(summary)
    return 0


def report(summary):
    print("SQL corpus extraction (#70)")
    print("=" * 64)
    for source, (files, collector, counts) in summary.items():
        total_kept = sum(counts.values())
        print(f"\n{source}: {files} vendored .test files -> {total_kept} statements")
        for category in CATEGORIES:
            print(f"  {category:<8} {counts[category]:>6}")
        s = collector.stats
        print(f"  -- seen {s['total']}, dropped: "
              f"{s['duplicate']} exact-dup, {s['shape_capped']} shape-capped, "
              f"{s['limit_capped']} over-limit, {s['uncategorized']} uncategorized, "
              f"{s['expected_error']} expected-error"
              + (f", {s['tcl_dynamic']} TCL-dynamic" if s["tcl_dynamic"] else ""))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--source", choices=["sqllogictest", "tcl", "all"], default="all")
    ap.add_argument("--fetch", action="store_true",
                    help="download upstream corpora and refresh the vendored subset")
    ap.add_argument("--limit", type=int, default=1000,
                    help="max statements kept per category per source (default 1000)")
    ap.add_argument("--per-shape", type=int, default=2,
                    help="max statements kept per distinct structural shape (default 2)")
    args = ap.parse_args()
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
