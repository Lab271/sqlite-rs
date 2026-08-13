#!/usr/bin/env python3
"""sqlite-rs Assurance Dashboard — the case, assembled from three levels.

Per ADR-0061, "assurance" is the argument that the project is fit for
purpose; this script assembles it from three independently-measurable
levels:

    Traceability (S->P, E->P): do spec, program and evidence connect?
                                 measured here, scenario-weighted.
    Evidence:                   corpus files present, line coverage if cached.
                                 measured here (reads cached results, doesn't run anything).
    Verification:                does the program satisfy its spec?
                                 NOT measured here — see `make verify` / `make test`.

Traceability is scored per *scenario*, not per requirement: a single
**Tests:** link on a requirement with five `#### Scenario:` blocks only
covers 1/5 of that requirement's claims, not all of it.

Usage:
    python3 tools/assurance.py                    # full dashboard (traceability + evidence)
    python3 tools/assurance.py --verbose           # show each requirement
    python3 tools/assurance.py --traceability-only # fast path: no corpus/coverage I/O
    python3 tools/assurance.py --min 0.75          # CI gate: exit 1 if below 75%
"""

import argparse
import re
import sys
from pathlib import Path

SPEC_DIR = Path(__file__).parent.parent / ".openspec" / "specs"
SRC_DIR = Path(__file__).parent.parent / "src"
TESTS_DIR = Path(__file__).parent.parent / "tests"


def parse_specs():
    """Parse all spec files and extract requirements."""
    requirements = []
    for spec_dir in sorted(SPEC_DIR.iterdir()):
        spec_file = spec_dir / "spec.md" if spec_dir.is_dir() else None
        if not spec_file or not spec_file.exists():
            continue

        text = spec_file.read_text()
        spec_name = spec_dir.name

        # Find all requirements
        req_blocks = re.split(r"(?=^### Requirement \d+)", text, flags=re.MULTILINE)
        for block in req_blocks:
            m = re.match(r"### Requirement (\d+): (.+?) \[(\w+)\]", block)
            if not m:
                continue

            num, title, level = m.group(1), m.group(2), m.group(3)

            # Check for Implementation link
            impl_match = re.search(r"\*\*Implementation:\*\*\s*`(.+?)`(\s*\(planned[^)]*\))?", block)
            impl_path = impl_match.group(1) if impl_match else None
            planned = bool(impl_match and impl_match.group(2))
            impl_file = impl_path.split("::")[0].strip() if impl_path else None
            if impl_file and not planned:
                _resolved = (SRC_DIR.parent / impl_file).resolve()
                _repo_root = SRC_DIR.parent.resolve()
                impl_exists = _resolved.is_relative_to(_repo_root) and _resolved.exists()
            else:
                impl_exists = False

            # Check for Tests link. A requirement's **Tests:** line may list
            # several comma-separated test paths — that count is the number
            # of distinct claims it can plausibly back, used below to weight
            # scenario-level coverage rather than treat the link as binary.
            tests_match = re.search(r"\*\*Tests:\*\*\s*(.+)", block)
            tests_path = tests_match.group(1).strip() if tests_match else None
            # A listed test only counts if its file exists on disk — a link
            # to a not-yet-written test is a plan, not evidence (symmetric
            # with impl_exists above).
            tests_listed = 0
            tests_declared = 0
            if tests_path:
                for entry in tests_path.split(","):
                    entry = entry.strip().strip("`")
                    if not entry:
                        continue
                    tests_declared += 1
                    test_file = entry.split("::")[0].strip()
                    # strip inline qualifiers like "inline #[cfg(test)] in src/x.rs"
                    m2 = re.search(r"[\w/.-]+\.rs", test_file)
                    if m2:
                        test_file = m2.group(0)
                    resolved = (SRC_DIR.parent / test_file).resolve()
                    repo_root = SRC_DIR.parent.resolve()
                    if resolved.is_relative_to(repo_root) and resolved.exists():
                        tests_listed += 1

            # Check for Corpus link
            corpus_files = re.findall(r"\*\*Corpus:\*\*\s*`(.+?)`", block)
            corpus_present = all(
                (SRC_DIR.parent / f).exists() for f in corpus_files
            )

            # Count scenarios
            scenarios = len(re.findall(r"#### Scenario:", block))

            requirements.append(
                {
                    "spec": spec_name,
                    "num": int(num),
                    "title": title,
                    "level": level,
                    "impl_path": impl_path,
                    "impl_exists": impl_exists,
                    "planned": planned,
                    "tests_path": tests_path,
                    "tests_linked": tests_listed > 0,
                    "tests_declared": tests_declared,
                    "tests_listed": tests_listed,
                    "corpus_files": corpus_files,
                    "corpus_present": corpus_present,
                    "scenarios": scenarios,
                }
            )

    return requirements


def scenario_coverage(r):
    """Fraction of a requirement's falsifiable claims backed by a test link.

    A requirement with no `#### Scenario:` blocks (older specs predate the
    scenario format) has nothing to weight against, so it falls back to the
    previous binary reading: 1.0 if any test is linked, else 0.0.

    A requirement with N scenarios and a **Tests:** line listing only M < N
    tests is scored M/N, not 1.0 — one link cannot silently cover five
    claims (ADR-0061 SS3).
    """
    if r["scenarios"] == 0:
        return 1.0 if r["tests_linked"] else 0.0
    return min(r["tests_listed"], r["scenarios"]) / r["scenarios"]


def _get_test_coverage():
    """Try to read cached line coverage from cargo-tarpaulin or cargo-llvm-cov.

    Returns a string like '87.3%' or None if no coverage tool is available.
    Doesn't run coverage itself — reads cached results if present (`make coverage`
    populates the cache; see the Evidence level in ADR-0061).
    """
    # Try llvm-cov cache (macOS + Linux)
    llvm_cov_out = Path(__file__).parent.parent / "target" / "llvm-cov.json"
    if llvm_cov_out.exists():
        try:
            import json
            data = json.loads(llvm_cov_out.read_text())
            lines = data["data"][0]["totals"]["lines"]
            return f"{lines['percent']:.1f}% ({lines['covered']}/{lines['count']} lines)"
        except (json.JSONDecodeError, KeyError, IndexError):
            pass

    # Try tarpaulin cache (Linux only)
    tarpaulin_out = Path(__file__).parent.parent / "target" / "tarpaulin" / "coverage.json"
    if tarpaulin_out.exists():
        try:
            import json
            data = json.loads(tarpaulin_out.read_text())
            if "coverage" in data:
                return f"{data['coverage']:.1f}%"
        except (json.JSONDecodeError, KeyError):
            pass

    return None


def report(requirements, verbose=False, traceability_only=False):
    """Print the assurance dashboard: Traceability, then Evidence.

    Planned requirements (marked `(planned)` after the Implementation backtick)
    are excluded from totals — they describe aspirational architecture, not
    current behaviour, and double-counting them as missing distorts the metric.

    Returns (completeness, coverage) — the two independent traceability
    ratios. There is no longer a combined "assurance" ratio: it was the
    conjunction of these two and could not fall below either (ADR-0061 SS2).
    """
    planned_count = sum(1 for r in requirements if r["planned"])
    active = [r for r in requirements if not r["planned"]]
    total = len(active)
    if total == 0:
        print("No requirements found in .openspec/specs/")
        return 0.0, 0.0

    impl_linked = sum(1 for r in active if r["impl_path"])
    impl_exists = sum(1 for r in active if r["impl_exists"])
    total_scenarios = sum(r["scenarios"] for r in active)

    completeness = impl_exists / total if total else 0

    # Coverage is scenario-weighted: sum each requirement's scenario_coverage()
    # and average over the requirement count, so a requirement with partial
    # scenario coverage contributes partially rather than as a flat 0/1.
    coverage = sum(scenario_coverage(r) for r in active) / total if total else 0
    tests_linked = sum(1 for r in active if r["tests_linked"])

    print("=" * 60)
    print("sqlite-rs Assurance Case")
    print("=" * 60)
    print(f"Requirements:     {total}" + (f" ({planned_count} planned excluded)" if planned_count else ""))
    print(f"Scenarios:        {total_scenarios}")
    print()
    print("-- Traceability " + "-" * 44)
    print(f"Completeness (S->P):  {impl_exists}/{total} spec -> implementation  ({completeness:.0%})")
    print(f"  - Linked:           {impl_linked}/{total}")
    print(f"  - File exists:      {impl_exists}/{total}")
    print()
    print(f"Coverage (E->P):      scenario-weighted  ({coverage:.0%})")
    declared = sum(1 for r in active if r.get("tests_declared", 0) > 0)
    print(f"  - Tests declared:   {declared}/{total} requirements (links in spec)")
    print(f"  - Tests existing:   {tests_linked}/{total} requirements (files on disk)")
    if total_scenarios:
        weighted = sum(min(r["tests_listed"], r["scenarios"]) for r in active if r["scenarios"])
        print(f"  - Scenarios backed: {weighted}/{total_scenarios} (requirements with #### Scenario: blocks only)")

    if not traceability_only:
        corpus_total = sum(1 for r in active if r["corpus_files"])
        corpus_present = sum(1 for r in active if r["corpus_files"] and r["corpus_present"])
        test_coverage = _get_test_coverage()

        print()
        print("-- Evidence " + "-" * 48)
        if corpus_total:
            print(f"Corpus files present: {corpus_present}/{corpus_total}")
        else:
            print("Corpus files present: n/a (no **Corpus:** links)")
        print(f"Line coverage:        {test_coverage if test_coverage is not None else 'not cached — run `make evidence`'}")

    print()
    print("-- Verification " + "-" * 44)
    print("Not measured here — run `make verification` (alias for `make test`)")
    print("=" * 60)

    if verbose:
        print()
        print("  Legend: [impl][tests][corpus]")
        print("    impl:   ✓=exists  ○=linked/missing  P=planned  ✗=not linked")
        print("    tests:  T=fully linked  t=partially linked  -=none")
        print("    corpus: C=present c=linked/missing  -=none")
        print()
        for r in requirements:
            if r["planned"]:
                status = "P"
            else:
                status = "✓" if r["impl_exists"] else "○" if r["impl_path"] else "✗"
            cov = scenario_coverage(r)
            test_status = "T" if cov >= 1.0 else "t" if cov > 0.0 else "-"
            corpus_status = (
                "C"
                if r["corpus_files"] and r["corpus_present"]
                else "c"
                if r["corpus_files"]
                else "-"
            )
            print(
                f"  [{status}][{test_status}][{corpus_status}] "
                f"{r['spec']}/Req {r['num']}: {r['title']} "
                f"({r['scenarios']} scenarios, {cov:.0%} backed)"
            )

    return completeness, coverage


def main():
    parser = argparse.ArgumentParser(description="sqlite-rs Assurance Dashboard")
    parser.add_argument("-v", "--verbose", action="store_true", help="Show each requirement")
    parser.add_argument(
        "--traceability-only",
        action="store_true",
        help="Skip Evidence section (corpus/coverage I/O) — fast path for `make traceability`",
    )
    parser.add_argument("--min", type=float, default=0.0, help="Minimum score (0.0-1.0) for CI gate, applied to both completeness and coverage")
    args = parser.parse_args()

    requirements = parse_specs()
    completeness, coverage = report(requirements, verbose=args.verbose, traceability_only=args.traceability_only)

    if args.min > 0:
        worst = min(completeness, coverage)
        if worst < args.min:
            print(f"\nFAIL: below threshold {args.min:.0%}")
            print(f"  completeness: {completeness:.0%}")
            print(f"  coverage:     {coverage:.0%}")
            sys.exit(1)
        else:
            print(f"\nPASS: completeness {completeness:.0%}, coverage {coverage:.0%} — both above {args.min:.0%}")


if __name__ == "__main__":
    main()
