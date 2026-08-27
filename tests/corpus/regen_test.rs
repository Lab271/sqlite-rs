// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Requirement 2, "Regeneration round-trip" scenario. Runs entirely in
//! scratch directories via `FIXTURES_DIR` — never touches the committed
//! corpus. Skips (prints and returns) when the pinned oracle isn't
//! available in this environment, rather than failing CI machines that
//! don't have it installed.

use crate::oracle::{gen_fixtures_script, ORACLE_VERSION};
use std::path::Path;
use std::process::Command;

fn oracle_available() -> bool {
    for candidate in [
        "/opt/homebrew/opt/sqlite/bin/sqlite3",
        "/usr/local/opt/sqlite/bin/sqlite3",
        "sqlite3",
    ] {
        if let Ok(output) = Command::new(candidate).arg("-version").output() {
            if String::from_utf8_lossy(&output.stdout).starts_with(ORACLE_VERSION) {
                return true;
            }
        }
    }
    false
}

#[test]
fn regeneration_is_reproducible() {
    if !oracle_available() {
        println!("skip: pinned oracle {ORACLE_VERSION} not available in this environment");
        return;
    }

    let run1 = std::env::temp_dir().join("sqlite_rs_corpus_regen_run1");
    let run2 = std::env::temp_dir().join("sqlite_rs_corpus_regen_run2");
    for dir in [&run1, &run2] {
        std::fs::remove_dir_all(dir).ok();
        let status = Command::new(gen_fixtures_script())
            .env("FIXTURES_DIR", dir)
            .status()
            .expect("running gen_fixtures.sh");
        assert!(status.success());
    }

    let mut mismatches = Vec::new();
    compare_dirs(&run1, &run2, &mut mismatches);
    assert!(
        mismatches.is_empty(),
        "regeneration is not reproducible: {mismatches:?}"
    );

    std::fs::remove_dir_all(&run1).ok();
    std::fs::remove_dir_all(&run2).ok();
}

/// Requirement 2 explicitly allows this: "byte-identity not required where
/// sqlite3 embeds nondeterminism." The `journalstates/` family (#21, #35,
/// #36) is the corpus's first and only fixture family built from WAL/
/// rollback-journal files, both of which embed SQLite-generated random
/// salts/nonces by design (WAL generation salts, the journal header's
/// random nonce) — every other byte is deterministic given the same
/// script, but these fields necessarily differ run to run. Checked for
/// "functionally identical" (same size — content differs only in the
/// fixed-width random fields and the checksums that cover them) rather
/// than exact bytes.
fn is_nondeterministic(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "journalstates")
}

fn compare_dirs(a: &Path, b: &Path, mismatches: &mut Vec<String>) {
    let mut a_entries: Vec<_> = std::fs::read_dir(a).unwrap().map(|e| e.unwrap()).collect();
    a_entries.sort_by_key(|e| e.file_name());
    for entry in a_entries {
        let name = entry.file_name();
        let a_path = entry.path();
        let b_path = b.join(&name);
        if !b_path.exists() {
            mismatches.push(format!("{} missing in second run", b_path.display()));
            continue;
        }
        if a_path.is_dir() {
            compare_dirs(&a_path, &b_path, mismatches);
        } else if is_nondeterministic(&a_path) {
            let a_len = std::fs::metadata(&a_path).unwrap().len();
            let b_len = std::fs::metadata(&b_path).unwrap().len();
            if a_len != b_len {
                mismatches.push(format!(
                    "{} differs in size between runs ({a_len} vs {b_len})",
                    a_path.display()
                ));
            }
        } else {
            let a_bytes = std::fs::read(&a_path).unwrap();
            let b_bytes = std::fs::read(&b_path).unwrap();
            if a_bytes != b_bytes {
                mismatches.push(format!("{} differs between runs", a_path.display()));
            }
        }
    }
}
