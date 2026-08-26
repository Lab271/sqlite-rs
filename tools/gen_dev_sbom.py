#!/usr/bin/env python3
"""Generates sqlite-rs-dev.cdx.json — a CycloneDX SBOM covering the full
`Cargo.lock` closure (production + dev + build dependencies), unlike
`cargo-cyclonedx`'s default output (`sqlite-rs.cdx.json`, `make sbom`),
which structurally omits dev-dependencies entirely rather than including
and scope-tagging them.

Why this exists: build-time code execution is a real supply-chain attack
vector independent of what ships in the release artifact (the xz-utils
backdoor was smuggled in via build/test infrastructure, not a declared
runtime dependency) — DEPENDENCIES.md was retired, but "what do we use in
build and test" visibility from that file's old "Development-only
dependencies" table needed a machine-readable home. `make deny`/`make
audit`/`cargo vet` already scan this full closure; this SBOM makes that
closure legible as CycloneDX (per its own component `scope`: `required`
for the production graph reachable from the root by normal-only edges —
currently empty, since #563 — and `optional` for everything only reached
via a dev/build edge somewhere in the chain).

Run via `make sbom-dev`. Reads `cargo metadata --format-version 1`
directly rather than a JSON library dependency, matching this repo's
minimal-dependencies stance for its own tooling.
"""

import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
OUTPUT = REPO_ROOT / "sqlite-rs-dev.cdx.json"


def cargo_metadata():
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


def reachable_via_normal_only(meta):
    """Package ids reachable from the workspace root by a chain of only
    `kind: null` (normal) dependency edges — i.e. the actual production
    trust boundary. Currently empty (#563: zero production deps), but
    computed properly rather than hardcoded so this script keeps working
    correctly if that ever changes.
    """
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root_id = meta["resolve"]["root"]
    seen = set()
    frontier = [root_id]
    while frontier:
        pkg_id = frontier.pop()
        if pkg_id in seen:
            continue
        seen.add(pkg_id)
        for dep in nodes[pkg_id]["deps"]:
            is_normal = any(k["kind"] is None for k in dep["dep_kinds"])
            if is_normal and dep["pkg"] not in seen:
                frontier.append(dep["pkg"])
    seen.discard(root_id)
    return seen


def purl(name, version):
    return f"pkg:cargo/{name}@{version}"


def license_field(pkg):
    lic = pkg.get("license")
    if lic:
        return [{"expression": lic}]
    return []


def main():
    meta = cargo_metadata()
    packages = {p["id"]: p for p in meta["packages"]}
    root_id = meta["resolve"]["root"]
    root = packages[root_id]
    required = reachable_via_normal_only(meta)

    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()
    epoch = subprocess.run(
        ["git", "log", "-1", "--format=%ct"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()
    timestamp = subprocess.run(
        ["date", "-u", "-r", epoch, "+%Y-%m-%dT%H:%M:%S.000000000Z"]
        if sys.platform == "darwin"
        else ["date", "-u", "-d", f"@{epoch}", "+%Y-%m-%dT%H:%M:%S.000000000Z"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()

    components = []
    for pkg_id, pkg in sorted(packages.items(), key=lambda kv: kv[1]["name"]):
        if pkg_id == root_id:
            continue
        components.append(
            {
                "type": "library",
                "bom-ref": purl(pkg["name"], pkg["version"]),
                "name": pkg["name"],
                "version": pkg["version"],
                "scope": "required" if pkg_id in required else "optional",
                "licenses": license_field(pkg),
                "purl": purl(pkg["name"], pkg["version"]),
            }
        )

    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "tools": [{"vendor": "sqlite-rs", "name": "tools/gen_dev_sbom.py", "version": "1"}],
            "component": {
                "type": "application",
                "bom-ref": purl(root["name"], root["version"]),
                "name": root["name"],
                "version": root["version"],
                "licenses": license_field(root),
                "purl": purl(root["name"], root["version"]),
            },
            "properties": [
                {"name": "sqlite-rs:generated-at-commit", "value": commit},
            ],
        },
        "components": components,
    }

    OUTPUT.write_text(json.dumps(bom, indent=2) + "\n")
    required_count = sum(1 for c in components if c["scope"] == "required")
    print(
        f"wrote {OUTPUT.relative_to(REPO_ROOT)}: {len(components)} components "
        f"({required_count} required, {len(components) - required_count} optional/dev-and-build-only)"
    )


if __name__ == "__main__":
    main()
