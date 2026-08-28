#!/usr/bin/env python3
"""License-header gate (`make supply-chain`): every tracked `.rs` file must
open with

    // Copyright 2026 Schuberg Philis
    // SPDX-License-Identifier: Apache-2.0

Vendored third-party source (`tests/spike/**/third_party/`, not ours to
relicense — see CLAUDE.md's module-layout convention for the same carve-out
applied to the no-`mod.rs` rule) is exempt.
"""

import subprocess
import sys

HEADER = (
    "// Copyright 2026 Schuberg Philis",
    "// SPDX-License-Identifier: Apache-2.0",
)


def tracked_rs_files():
    out = subprocess.run(
        ["git", "ls-files", "*.rs"], capture_output=True, text=True, check=True
    ).stdout
    return [
        line for line in out.splitlines() if line and "third_party/" not in line
    ]


def has_header(path):
    with open(path, encoding="utf-8") as f:
        lines = [f.readline().rstrip("\n") for _ in range(2)]
    return tuple(lines) == HEADER


def main():
    missing = [p for p in tracked_rs_files() if not has_header(p)]
    if missing:
        print(f"license-headers: {len(missing)} file(s) missing the required header:")
        for p in missing:
            print(f"  {p}")
        print()
        print("Required header (first two lines, verbatim):")
        for line in HEADER:
            print(f"  {line}")
        return 1
    print(f"license-headers: ok ({len(tracked_rs_files())} files checked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
