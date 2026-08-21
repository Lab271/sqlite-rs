#!/usr/bin/env python3
"""Version-pin gate — one sqlite3 version for the whole project.

`Cargo.toml`'s `[package.metadata.oracle] version` is the single source of
truth for the pinned sqlite3 version: the oracle binary fixtures are generated
with, the `parse.y` the EBNF grammar is re-derived from, and the TCL suite the
SQL corpus is extracted from.

Most consumers read it directly at run time (`tools/harvest_opcodes.py`,
`tools/grammar_drift.py`, `tools/extract_sql_corpus.py`,
`tools/gen_fixtures.sh`). Two cannot:

  - `tests/corpus/oracle.rs` — a Rust `const`, needed at compile time
  - `.github/workflows/ci.yml` — a workflow-level `env:` value

Those carry a literal, and this gate asserts the literal still agrees. Drift
here is not cosmetic: an oracle a version off silently diffs against different
behaviour, which is exactly the failure spec 004 Requirement 1 exists to
prevent.

Usage:
    python3 tools/version_pin.py            # report
    python3 tools/version_pin.py --strict   # CI gate: exit 1 on disagreement
"""

import argparse
import re
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
CARGO_TOML = REPO_ROOT / "Cargo.toml"

# (path, regex capturing the version in group 1, human description)
PINNED_LITERALS = [
    (
        "tests/corpus/oracle.rs",
        re.compile(r'pub const ORACLE_VERSION: &str = "([^"]+)"'),
        "ORACLE_VERSION const",
    ),
    (
        ".github/workflows/ci.yml",
        re.compile(r'SQLITE_ORACLE_VERSION: "([^"]+)"'),
        "CI SQLITE_ORACLE_VERSION",
    ),
    (
        "tests/corpus/support/fake_sqlite3_codec.sh",
        re.compile(r'echo "(\d+\.\d+\.\d+) '),
        "fake codec oracle version string",
    ),
    (
        "tests/performance/engine.rs",
        re.compile(r'pub const ORACLE_VERSION: &str = "([^"]+)"'),
        "tier-1 bench ORACLE_VERSION const",
    ),
    (
        "src/vdbe/functions.rs",
        re.compile(r'fn sqlite_version.*?Value::Text\("([^"]+)"', re.DOTALL),
        "sqlite_version() return literal",
    ),
]

# CI names the source tarball by SQLite's zero-padded encoding of the version
# (3.53.4 -> 3530400), which is easy to forget when bumping the pin — and gets
# you a cached oracle of the *previous* version that still passes its own
# version check, because the check compares against the same stale literal.
TARBALL_RE = re.compile(r"SQLITE_ORACLE_TARBALL: \"sqlite-autoconf-(\d+)\"")


def encoded_version(version):
    """3.53.4 -> 3530400, SQLite's tarball/SQLITE_VERSION_NUMBER encoding."""
    major, minor, patch = (int(part) for part in version.split("."))
    return f"{major}{minor:02d}{patch:02d}00"

# Consumers that read the pin at run time. Asserting they contain no hardcoded
# version keeps someone from "helpfully" reintroducing a literal.
RUNTIME_READERS = [
    "tools/harvest_opcodes.py",
    "tools/grammar_drift.py",
    "tools/extract_sql_corpus.py",
    "tools/gen_fixtures.sh",
    "tools/bench_env.sh",
]
VERSION_LITERAL_RE = re.compile(r'"3\.\d+\.\d+"')


def pinned_version():
    return tomllib.loads(CARGO_TOML.read_text())["package"]["metadata"]["oracle"]["version"]


def main():
    ap = argparse.ArgumentParser(description="sqlite3 version-pin consistency gate")
    ap.add_argument("--strict", action="store_true", help="exit 1 on disagreement")
    args = ap.parse_args()

    expected = pinned_version()
    problems = []

    print("=" * 60)
    print(f"Version pin — Cargo.toml [package.metadata.oracle] = {expected}")
    print("=" * 60)

    for rel, pattern, description in PINNED_LITERALS:
        path = REPO_ROOT / rel
        if not path.exists():
            problems.append(f"{rel}: missing (expected to carry the {description})")
            print(f"  MISSING  {rel}")
            continue
        m = pattern.search(path.read_text())
        if m is None:
            problems.append(f"{rel}: could not find the {description}")
            print(f"  UNREADABLE  {rel} — {description} not found")
        elif m.group(1) != expected:
            problems.append(f"{rel}: {description} is {m.group(1)}, expected {expected}")
            print(f"  DRIFT    {rel}: {m.group(1)} != {expected}")
        else:
            print(f"  ok       {rel}")

    ci_path = REPO_ROOT / ".github" / "workflows" / "ci.yml"
    if ci_path.exists():
        m = TARBALL_RE.search(ci_path.read_text())
        want = encoded_version(expected)
        if m is None:
            problems.append("ci.yml: could not find SQLITE_ORACLE_TARBALL")
            print("  UNREADABLE  .github/workflows/ci.yml — SQLITE_ORACLE_TARBALL not found")
        elif m.group(1) != want:
            problems.append(
                f"ci.yml: SQLITE_ORACLE_TARBALL encodes {m.group(1)}, expected {want} "
                f"for {expected}"
            )
            print(f"  DRIFT    .github/workflows/ci.yml tarball: {m.group(1)} != {want}")
        else:
            print(f"  ok       .github/workflows/ci.yml tarball ({want})")

    for rel in RUNTIME_READERS:
        path = REPO_ROOT / rel
        if not path.exists():
            problems.append(f"{rel}: missing (expected to read the pin at run time)")
            print(f"  MISSING  {rel}")
            continue
        stray = VERSION_LITERAL_RE.search(path.read_text())
        if stray:
            problems.append(
                f"{rel}: hardcodes {stray.group(0)} — it should read the pin from Cargo.toml"
            )
            print(f"  HARDCODED  {rel}: {stray.group(0)}")
        else:
            print(f"  ok       {rel} (reads the pin)")

    print("=" * 60)
    if problems:
        print(f"version-pin: {len(problems)} problem(s)")
        for p in problems:
            print(f"  - {p}")
        return 1 if args.strict else 0
    print(f"version-pin: all sites agree on {expected}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
