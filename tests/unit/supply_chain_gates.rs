// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Enforces spec 005-assurance Requirement 3 (Supply-Chain Gates), both
//! scenarios:
//! - "Locked, advisory-clean build": CI runs `cargo test --locked` (or
//!   equivalent `--locked` invocations) and a `cargo deny check` gate
//!   backed by a committed `deny.toml`.
//! - "Actions pinned by SHA": every non-container `uses:` step in the
//!   workflow references a 40-hex-character commit SHA, not a mutable
//!   tag like `@v4`.
//!
//! Without this, both properties hold only by convention — a future edit
//! could drop `--locked` or repin an action to a tag without any gate
//! noticing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::fs;

fn workflow() -> String {
    fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/ci.yml"
    ))
    .expect("ci.yml must exist")
}

#[test]
fn ci_enforces_locked_resolution_and_deny_check() {
    let workflow = workflow();
    assert!(
        workflow.contains("make deny") || workflow.contains("cargo deny check"),
        "CI must run a cargo-deny gate (advisories/licenses/bans/sources)"
    );
    assert!(
        fs::metadata(concat!(env!("CARGO_MANIFEST_DIR"), "/deny.toml")).is_ok(),
        "deny.toml must exist for cargo deny to have a policy to check against"
    );

    let makefile = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Makefile"))
        .expect("Makefile must exist");
    let locked_test_targets = makefile
        .lines()
        .filter(|l| {
            l.contains("cargo test") || l.contains("cargo build") || l.contains("cargo clippy")
        })
        .filter(|l| l.contains("--locked"))
        .count();
    assert!(
        locked_test_targets > 0,
        "Makefile's build/test/lint targets must invoke cargo with --locked, \
         so a stale Cargo.lock fails the build instead of silently re-resolving"
    );
}

#[test]
fn every_non_container_action_is_pinned_to_a_commit_sha() {
    let workflow = workflow();
    let mut unpinned = Vec::new();

    for (lineno, line) in workflow.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("uses:") else {
            continue;
        };
        let action_ref = rest.trim();

        // Docker container actions (`docker://...`) are the documented
        // exception (Lab271 SOP) — they're pinned by image digest, not a
        // git SHA, and don't take the `owner/repo@ref` shape at all.
        if action_ref.starts_with("docker://") {
            continue;
        }

        let Some((_, at_ref)) = action_ref.split_once('@') else {
            unpinned.push(format!("{}: {action_ref} (no @ref at all)", lineno + 1));
            continue;
        };
        // A comment like `# v7.0.1` may trail the ref; strip it before
        // measuring the ref's own length.
        let sha_candidate = at_ref.split_whitespace().next().unwrap_or(at_ref);

        let is_full_sha =
            sha_candidate.len() == 40 && sha_candidate.chars().all(|c| c.is_ascii_hexdigit());
        if !is_full_sha {
            unpinned.push(format!("{}: {action_ref}", lineno + 1));
        }
    }

    assert!(
        unpinned.is_empty(),
        "every `uses:` step must be pinned to a 40-character commit SHA, not a mutable tag \
         (a trailing `# vX.Y.Z` comment is fine) — found unpinned actions:\n{}",
        unpinned.join("\n")
    );
}
