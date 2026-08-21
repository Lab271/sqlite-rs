#!/usr/bin/env python3
"""Assembles tools/bench-status.json from the raw tier-1 (criterion) and
tier-2 (hyperfine) outputs `make bench`/`make bench-cli` already produced.

Same treatment as tools/assurance.py's sqllogictest-status.json: a small,
machine-readable committed artifact (ratios, not raw samples) that a
dashboard line or future ratchet gate can read without re-running anything.

Usage:
    python3 tools/bench_status.py
"""

import json
import platform
import re
import subprocess
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent
CRITERION_DIR = REPO_ROOT / "target" / "criterion"
HYPERFINE_DIR = REPO_ROOT / "target" / "bench-fixtures" / "hyperfine"
OUT_PATH = REPO_ROOT / "tools" / "bench-status.json"

TIER1_SCENARIOS = [
    "full_scan",
    "point_lookup",
    "filter_scan",
    "order_by_limit",
    "expr_heavy",
    "prepare_only",
    # #301: V4 join/aggregate/subquery scenarios.
    "join",
    "group_by_agg",
    "subquery",
    # #303: correlated counterpart of "subquery" — bench_1mb.db only, see
    # tests/performance/engine.rs's own scenario comment.
    "correlated_subquery",
    # #322: uncorrelated aggregate subquery inside an aggregate outer query.
    "agg_subquery",
    # #323: IN (SELECT ...) subquery inside an aggregate outer query.
    "in_subquery_agg_outer",
    # tests/performance/crud.rs: 15-scenario full-CRUD tier-1 bench
    # (Create/Read/Update/Delete), bench_1mb.db only — see that file's
    # own module doc comment for why writes don't also run against
    # bench_50mb.db. Was entirely missing from this list (a pre-existing
    # gap noticed while triaging #336).
    "read_pk",
    "read_indexed_range",
    "read_full_scan",
    "read_join",
    "read_group_by_agg",
    "insert_single",
    "insert_batch_10",
    "insert_no_explicit_pk",
    "update_pk",
    "update_filtered_range",
    "update_indexed_column",
    "update_multi_column",
    "delete_pk",
    "delete_filtered_range",
    "delete_equality_bucket",
]
FIXTURES = ["bench_1mb.db", "bench_50mb.db"]


def pinned_oracle_version():
    cargo_toml = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text())
    return cargo_toml["package"]["metadata"]["oracle"]["version"]


def hardware_fingerprint():
    cpu = "unknown"
    if platform.system() == "Darwin":
        try:
            cpu = subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.strip()
        except (subprocess.CalledProcessError, FileNotFoundError):
            pass
    elif platform.system() == "Linux":
        info = Path("/proc/cpuinfo").read_text()
        m = re.search(r"model name\s*:\s*(.+)", info)
        if m:
            cpu = m.group(1).strip()
    return {
        "platform": platform.platform(),
        "cpu": cpu,
        "cpu_count": os_cpu_count(),
    }


def os_cpu_count():
    import os

    return os.cpu_count()


def criterion_median_ns(group_dir_name, engine):
    path = CRITERION_DIR / group_dir_name / engine / "new" / "estimates.json"
    if not path.exists():
        return None
    data = json.loads(path.read_text())
    return data["median"]["point_estimate"]


def tier1_results():
    results = []
    for scenario in TIER1_SCENARIOS:
        for fixture in FIXTURES:
            group_dir_name = f"{scenario}_{fixture}"
            ours_ns = criterion_median_ns(group_dir_name, "ours")
            oracle_ns = criterion_median_ns(group_dir_name, "oracle")
            if ours_ns is None or oracle_ns is None:
                continue
            results.append(
                {
                    "scenario": scenario,
                    "fixture": fixture,
                    "ours_ns": ours_ns,
                    "oracle_ns": oracle_ns,
                    "ratio_ours_over_oracle": ours_ns / oracle_ns,
                }
            )
    return results


def tier2_results():
    results = []
    if not HYPERFINE_DIR.exists():
        return results
    for path in sorted(HYPERFINE_DIR.glob("*.json")):
        data = json.loads(path.read_text())
        runs = data.get("results", [])
        if len(runs) != 2:
            continue
        ours, oracle = runs[0], runs[1]
        results.append(
            {
                "comparison": path.stem,
                "ours_command": ours["command"],
                "oracle_command": oracle["command"],
                "ours_mean_s": ours["mean"],
                "oracle_mean_s": oracle["mean"],
                "ratio_ours_over_oracle": ours["mean"] / oracle["mean"],
            }
        )
    return results


def main():
    status = {
        "oracle_version": pinned_oracle_version(),
        "hardware": hardware_fingerprint(),
        "tier1_engine": tier1_results(),
        "tier2_cli": tier2_results(),
    }
    OUT_PATH.write_text(json.dumps(status, indent=2) + "\n")
    print(f"wrote {OUT_PATH}")

    outliers = [
        r
        for r in status["tier1_engine"]
        if r["ratio_ours_over_oracle"] >= 10 or r["ratio_ours_over_oracle"] <= 0.1
    ] + [
        r
        for r in status["tier2_cli"]
        if r["ratio_ours_over_oracle"] >= 10 or r["ratio_ours_over_oracle"] <= 0.1
    ]
    if outliers:
        print(f"\n{len(outliers)} outlier(s) (ratio >= 10x or <= 0.1x):")
        for o in outliers:
            print(f"  {o}")


if __name__ == "__main__":
    main()
