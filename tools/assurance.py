#!/usr/bin/env python3
"""sqlite-rs Assurance Dashboard — the case, assembled from three levels.

"Assurance" is the argument that the project is fit for purpose; this
script assembles it from three independently-measurable levels:

    Traceability (S->P, E->P): do spec, program and evidence connect?
                                 measured here, scenario-weighted.
    Evidence:                   corpus files present, line coverage if cached.
                                 measured here (reads cached results, doesn't run anything).
    Verification:                does the program satisfy its spec?
                                 NOT measured here — see `make verification` / `make test`.

Features (keep this list current — spec 005's maintenance rule applies
to this header too):

1.  Spec parsing: walks `.openspec/specs/*/spec.md`, extracting every
    `### Requirement N: Title [LEVEL]` block and its `#### Scenario:` blocks.

2.  Planned exclusion: an Implementation link suffixed `(planned)` marks the
    requirement as future work — excluded from all scores, shown as [P] in
    verbose mode. Flipping planned -> active is how the dashboard tracks
    V-block progress.

3.  Completeness (S->P): fraction of active requirements whose
    `**Implementation:**` file exists on disk (path resolved inside the
    repo; `::qualifiers` stripped).

4.  Coverage (E->P), scenario-weighted: a requirement with 5 scenarios and
    1 valid test link scores 1/5, not 100%. Per requirement:
      covered = scenarios with their own valid link
              + min(requirement-level valid links, scenarios still uncovered)
    A requirement with no scenarios falls back to binary (any valid link).

5.  Per-scenario Tests links (preferred convention): a `**Tests:**` line
    INSIDE a `#### Scenario:` block backs exactly that scenario.
    Requirement-level `**Tests:**` lines (before the first scenario) remain
    supported as a pool counted against remaining scenarios.

6.  Existence validation: a listed test link only counts if its file exists
    on disk. A link to a not-yet-written test is a plan, not evidence
    (symmetric with Completeness).

7.  Symbol validation: a link of the form `path/file.rs::symbol` (or
    `::Class::method`) only counts if the trailing symbol name also occurs
    in the file. File-exists-but-symbol-missing is a dead link.

8.  Dead-link reporting: every declared link that fails validation (missing
    file or missing symbol) is counted, summarized in the dashboard, and
    listed per-requirement in --verbose.

9.  Corpus links: `**Corpus:**` fixture paths are checked for existence and
    reported at the Evidence level.

10. Line coverage: reads cached cargo-llvm-cov (target/llvm-cov.json) or
    tarpaulin output if present; never runs coverage itself.

11. CI gate: --min X exits 1 if completeness OR coverage is below X.

Usage:
    python3 tools/assurance.py                    # full dashboard (traceability + evidence)
    python3 tools/assurance.py --verbose           # per-requirement detail + dead links
    python3 tools/assurance.py --traceability-only # fast path: no corpus/coverage I/O
    python3 tools/assurance.py --min 0.75          # CI gate: exit 1 if below 75%

Link syntax accepted on **Tests:** / **Implementation:** lines:
    `tests/record_test.rs`                          file only
    `tests/record_test.rs::test_varint_lengths`     file + symbol
    `src/x.rs::Struct::method`                      trailing symbol is checked
    inline #[cfg(test)] in src/record/varint.rs     prose containing a path
    comma-separated lists of the above
"""

import argparse
import re
import sys
from pathlib import Path

SPEC_DIR = Path(__file__).parent.parent / ".openspec" / "specs"
REPO_ROOT = Path(__file__).parent.parent.resolve()


def _validate_link(entry):
    """Validate one link entry. Returns (entry, error) — error is None if valid.

    Feature 6 (file must exist) and feature 7 (trailing ::symbol must occur
    in the file). Prose entries ("inline #[cfg(test)] in src/x.rs") are
    reduced to their path token first.
    """
    entry = re.sub(r"\(planned[^)]*\)", "", entry).replace("`", "").strip()
    if not entry:
        return None
    parts = entry.split("::")
    file_part = parts[0].strip()
    m = re.search(r"[\w/.-]+\.(?:rs|py|sh|toml)", file_part)
    if m:
        file_part = m.group(0)
    resolved = (REPO_ROOT / file_part).resolve()
    if not (resolved.is_relative_to(REPO_ROOT) and resolved.exists() and resolved.is_file()):
        return (entry, "file missing")
    if len(parts) > 1:
        symbol = re.sub(r"\(.*\)$", "", parts[-1].strip())
        if symbol and symbol not in resolved.read_text():
            return (entry, f"symbol '{symbol}' not in file")
    return (entry, None)


def _parse_tests_line(text):
    """Extract comma-separated link entries from the first **Tests:** line in text."""
    m = re.search(r"\*\*Tests:\*\*\s*(.+)", text)
    if not m:
        return []
    return [e for e in (x.strip() for x in m.group(1).split(",")) if e]


def parse_specs():
    """Parse all spec files and extract requirements (features 1-2, 5-9)."""
    requirements = []
    for spec_dir in sorted(SPEC_DIR.iterdir()):
        spec_file = spec_dir / "spec.md" if spec_dir.is_dir() else None
        if not spec_file or not spec_file.exists():
            continue

        text = spec_file.read_text()
        spec_name = spec_dir.name

        req_blocks = re.split(r"(?=^### Requirement \d+)", text, flags=re.MULTILINE)
        for block in req_blocks:
            m = re.match(r"### Requirement (\d+): (.+?) \[(\w+)\]", block)
            if not m:
                continue

            num, title, level = m.group(1), m.group(2), m.group(3)

            # Split into requirement preamble and per-scenario chunks (feature 5)
            chunks = re.split(r"(?=^#### Scenario:)", block, flags=re.MULTILINE)
            preamble, scenario_blocks = chunks[0], chunks[1:]
            scenarios = len(scenario_blocks)

            # Implementation link (feature 3) — from preamble only
            impl_match = re.search(
                r"\*\*Implementation:\*\*\s*`(.+?)`(\s*\(planned[^)]*\))?", preamble
            )
            impl_path = impl_match.group(1) if impl_match else None
            planned = bool(impl_match and impl_match.group(2))
            impl_exists = False
            if impl_path and not planned:
                impl_file = impl_path.split("::")[0].strip()
                resolved = (REPO_ROOT / impl_file).resolve()
                impl_exists = resolved.is_relative_to(REPO_ROOT) and resolved.exists()

            dead_links = []

            # Requirement-level Tests pool (from preamble, feature 4/6/7)
            tests_declared = 0
            req_level_valid = 0
            for entry in _parse_tests_line(preamble):
                v = _validate_link(entry)
                if v is None:
                    continue
                tests_declared += 1
                if v[1] is None:
                    req_level_valid += 1
                else:
                    dead_links.append(v)

            # Per-scenario Tests links (feature 5)
            scenarios_backed = 0
            for sb in scenario_blocks:
                entries = _parse_tests_line(sb)
                backed = False
                for entry in entries:
                    v = _validate_link(entry)
                    if v is None:
                        continue
                    tests_declared += 1
                    if v[1] is None:
                        backed = True
                    else:
                        dead_links.append(v)
                if backed:
                    scenarios_backed += 1

            # Corpus links (feature 9) — anywhere in the block
            corpus_files = re.findall(r"\*\*Corpus:\*\*\s*`(.+?)`", block)
            corpus_present = all((REPO_ROOT / f).exists() for f in corpus_files)

            requirements.append(
                {
                    "spec": spec_name,
                    "num": int(num),
                    "title": title,
                    "level": level,
                    "impl_path": impl_path,
                    "impl_exists": impl_exists,
                    "planned": planned,
                    "tests_declared": tests_declared,
                    "req_level_valid": req_level_valid,
                    "scenarios_backed": scenarios_backed,
                    "dead_links": dead_links,
                    "corpus_files": corpus_files,
                    "corpus_present": corpus_present,
                    "scenarios": scenarios,
                }
            )

    return requirements


def covered_scenarios(r):
    """Number of a requirement's scenarios backed by a valid test link (feature 4).

    Scenario-level links back their own scenario; requirement-level links are
    a pool counted against scenarios not already backed directly.
    """
    if r["scenarios"] == 0:
        return 0
    remaining = r["scenarios"] - r["scenarios_backed"]
    return r["scenarios_backed"] + min(r["req_level_valid"], remaining)


def scenario_coverage(r):
    """Fraction of a requirement's falsifiable claims backed by a valid test link."""
    if r["scenarios"] == 0:
        return 1.0 if (r["req_level_valid"] + r["scenarios_backed"]) > 0 else 0.0
    return covered_scenarios(r) / r["scenarios"]


def _get_test_coverage():
    """Read cached line coverage (feature 10). Never runs coverage itself."""
    llvm_cov_out = REPO_ROOT / "target" / "llvm-cov.json"
    if llvm_cov_out.exists():
        try:
            import json
            data = json.loads(llvm_cov_out.read_text())
            lines = data["data"][0]["totals"]["lines"]
            return f"{lines['percent']:.1f}% ({lines['covered']}/{lines['count']} lines)"
        except (json.JSONDecodeError, KeyError, IndexError):
            pass

    tarpaulin_out = REPO_ROOT / "target" / "tarpaulin" / "coverage.json"
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

    Planned requirements are excluded from totals (feature 2). Returns
    (completeness, coverage) — two independent traceability ratios.
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
    coverage = sum(scenario_coverage(r) for r in active) / total if total else 0

    declared = sum(1 for r in active if r["tests_declared"] > 0)
    existing = sum(1 for r in active if (r["req_level_valid"] + r["scenarios_backed"]) > 0)
    total_dead = sum(len(r["dead_links"]) for r in active)
    backed = sum(covered_scenarios(r) for r in active)
    direct = sum(r["scenarios_backed"] for r in active)

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
    print(f"  - Tests declared:   {declared}/{total} requirements (links in spec)")
    print(f"  - Tests valid:      {existing}/{total} requirements (file + symbol exist)")
    if total_scenarios:
        print(f"  - Scenarios backed: {backed}/{total_scenarios} ({direct} by per-scenario links)")
    if total_dead:
        print(f"  - DEAD LINKS:       {total_dead} (declared but file/symbol missing — see --verbose)")

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
        print(f"Line coverage:        {test_coverage if test_coverage is not None else 'not cached — run `make coverage`'}")

    print()
    print("-- Verification " + "-" * 44)
    print("Not measured here — run `make verification` (alias for `make test`)")
    print("=" * 60)

    if verbose:
        print()
        print("  Legend: [impl][tests][corpus]")
        print("    impl:   ✓=exists  ○=linked/missing  P=planned  ✗=not linked")
        print("    tests:  T=all scenarios backed  t=partially  -=none")
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
                "C" if r["corpus_files"] and r["corpus_present"]
                else "c" if r["corpus_files"]
                else "-"
            )
            print(
                f"  [{status}][{test_status}][{corpus_status}] "
                f"{r['spec']}/Req {r['num']}: {r['title']} "
                f"({covered_scenarios(r)}/{r['scenarios']} scenarios backed)"
            )
            for entry, err in r["dead_links"]:
                print(f"        DEAD: {entry} — {err}")

    return completeness, coverage


def main():
    parser = argparse.ArgumentParser(description="sqlite-rs Assurance Dashboard")
    parser.add_argument("-v", "--verbose", action="store_true", help="Show each requirement + dead links")
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
