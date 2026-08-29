// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! CLI-surface oracle-parity test for `PRAGMA synchronous` (#645):
//! `PRAGMA synchronous = <level>; PRAGMA synchronous;` round-trips to
//! the numeric value just set, matching stock `sqlite3`. Driven through
//! `repl` (not `exec`, which deliberately never prints result rows —
//! see its module doc — and not `query`, which only accepts a single
//! `SELECT`/introspection-pragma statement) via the same piped-stdin
//! pattern `tests/unit/repl_dot_commands.rs` uses.

#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use oracle::{pinned_oracle, run_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-pragma-synchronous-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

/// A scratch db seeded with one bootstrap table — `PRAGMA synchronous`
/// itself needs no schema, but `repl`/`exec` (unlike stock `sqlite3`)
/// never create the file, so it must already exist.
fn seed_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle)
            .arg(&db)
            .arg("CREATE TABLE seed_bootstrap(x)")
            .status()
            .unwrap();
        assert!(status.success(), "seeding via oracle failed");
    } else {
        let out = Command::new(CLI)
            .arg("exec")
            .arg(&db)
            .arg("CREATE TABLE seed_bootstrap(x)")
            .output()
            .unwrap();
        assert!(out.status.success(), "seeding via our own exec failed");
    }
    db
}

fn run_repl_script(db: &Path, script: &str) -> Output {
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {CLI} repl {}: {e}", db.display()));
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(script.as_bytes())
        .expect("writing repl script");
    child.wait_with_output().expect("waiting for repl")
}

fn stdout_of(db: &Path, script: &str) -> String {
    let out = run_repl_script(db, script);
    assert!(
        out.status.success(),
        "repl script failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Strips the interactive `sqlite> ` prompts `repl` prints between
/// statements, so its output can be diffed against a non-interactive
/// oracle invocation (which never emits a prompt at all).
fn strip_prompts(s: &str) -> String {
    s.replace("sqlite> ", "").replace("sqlite>", "")
}

#[test]
fn synchronous_query_defaults_to_full() {
    let db = seed_db("default");
    let out = stdout_of(&db, "PRAGMA synchronous;\n.quit\n");
    assert!(out.contains('2'), "{out}");
}

#[test]
fn synchronous_off_then_query_round_trips() {
    let db = seed_db("off");
    let out = stdout_of(
        &db,
        "PRAGMA synchronous = OFF;\nPRAGMA synchronous;\n.quit\n",
    );
    assert!(out.contains('0'), "{out}");
}

#[test]
fn synchronous_normal_then_query_round_trips() {
    let db = seed_db("normal");
    let out = stdout_of(
        &db,
        "PRAGMA synchronous = NORMAL;\nPRAGMA synchronous;\n.quit\n",
    );
    assert!(out.contains('1'), "{out}");
}

#[test]
fn synchronous_full_then_query_round_trips() {
    let db = seed_db("full");
    let out = stdout_of(
        &db,
        "PRAGMA synchronous = FULL;\nPRAGMA synchronous;\n.quit\n",
    );
    assert!(out.contains('2'), "{out}");
}

/// Diffs the round-trip against the real `sqlite3` oracle: same
/// sequence of statements, same reported numeric level.
#[test]
fn synchronous_round_trip_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("synchronous_round_trip_matches_oracle");
        return;
    };
    let db = seed_db("oracle-diff");
    for (level, expected) in [("OFF", "0"), ("NORMAL", "1"), ("FULL", "2")] {
        let ours = stdout_of(
            &db,
            &format!("PRAGMA synchronous = {level};\nPRAGMA synchronous;\n.quit\n"),
        );
        let theirs = run_oracle(
            &oracle,
            &db,
            &[],
            &format!("PRAGMA synchronous = {level}; PRAGMA synchronous;"),
        );
        assert_eq!(
            strip_prompts(&ours).trim(),
            theirs.trim(),
            "level {level}: expected {expected}"
        );
    }
}
