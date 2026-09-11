// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #710: the file change counter (header offset 24) and version-valid-for
//! (offset 92) must be bumped on every committed write transaction, the
//! same way stock `sqlite3` does — otherwise a long-lived reader that has
//! already cached page 1 has no signal to invalidate that cache and keeps
//! serving stale rows after our write.
//!
//! `run_oracle` (`oracle.rs`) re-invokes `sqlite3` fresh for every call,
//! so it re-reads the header every time and can never observe this bug —
//! that's exactly why this file uses `oracle::OracleSession` instead: one
//! `sqlite3` process, held open across our write, asked to read the same
//! table before and after.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle, OracleSession};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

/// Header byte offset of the file change counter (#710).
const CHANGE_COUNTER_OFFSET: usize = 24;
/// Header byte offset of version-valid-for, kept equal to the change
/// counter on every bump.
const VERSION_VALID_FOR_OFFSET: usize = 92;

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-change-counter-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn run_exec(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()))
}

fn header_field(db: &Path, offset: usize) -> [u8; 4] {
    let bytes = std::fs::read(db).unwrap_or_else(|e| panic!("reading {}: {e}", db.display()));
    bytes[offset..offset + 4]
        .try_into()
        .unwrap_or_else(|_| panic!("{} is shorter than {offset} + 4 bytes", db.display()))
}

/// The headline acceptance criterion: a long-lived `sqlite3` session that
/// has already read (and cached) the table must see a row we wrote after
/// the fact, not the stale pre-write count — the change-counter bump is
/// exactly the signal that tells it to discard its page-1 cache and
/// re-read. Without #710's fix, this reproduces the bug: the session
/// keeps answering `1` forever.
#[test]
fn long_lived_oracle_session_sees_our_write_after_it_already_cached_the_table() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle(
            "long_lived_oracle_session_sees_our_write_after_it_already_cached_the_table",
        );
        return;
    };

    let db = scratch_db("long-lived-reader");
    let status = run_exec(&db, "create table t(a integer); insert into t values (1);").status;
    assert!(status.success());

    let mut session = OracleSession::spawn(&oracle, &db);
    // Populates the session's page cache with the pre-write row count.
    let before = session.exec("select count(*) from t;");
    assert_eq!(before.trim(), "1");

    let status = run_exec(&db, "insert into t values (2);").status;
    assert!(status.success());

    let after = session.exec("select count(*) from t;");
    assert_eq!(
        after.trim(),
        "2",
        "a long-lived reader that already cached the table must see our write \
         after it happens, not keep serving its stale cached count"
    );
}

/// Offset 24 (change counter) and offset 92 (version-valid-for) must be
/// byte-identical between a database stock `sqlite3` writes and one our
/// engine writes, given the same write sequence starting from the same
/// state — both bump by exactly one per committed transaction.
#[test]
fn change_counter_and_version_valid_for_match_oracle_byte_for_byte() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("change_counter_and_version_valid_for_match_oracle_byte_for_byte");
        return;
    };

    let ours = scratch_db("ours");
    let theirs = scratch_db("theirs");

    let sql = "create table t(a integer); insert into t values (1); insert into t values (2);";
    assert!(run_exec(&ours, sql).status.success());
    assert!(Command::new(&oracle)
        .arg(&theirs)
        .arg(sql)
        .status()
        .unwrap()
        .success());

    assert_eq!(
        header_field(&ours, CHANGE_COUNTER_OFFSET),
        header_field(&theirs, CHANGE_COUNTER_OFFSET),
        "change counter must match the oracle's after the same write sequence"
    );
    assert_eq!(
        header_field(&ours, VERSION_VALID_FOR_OFFSET),
        header_field(&theirs, VERSION_VALID_FOR_OFFSET),
        "version-valid-for must match the oracle's after the same write sequence"
    );

    assert_integrity_check_ok(&oracle, &ours);
}

/// A file we create from scratch (one `CREATE TABLE` transaction, no
/// prior writes) must carry the same initial change-counter/
/// version-valid-for values stock `sqlite3` gives a fresh database.
#[test]
fn fresh_database_has_the_same_initial_change_counter_as_the_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("fresh_database_has_the_same_initial_change_counter_as_the_oracle");
        return;
    };

    let ours = scratch_db("ours-fresh");
    let theirs = scratch_db("theirs-fresh");

    let sql = "create table t(a integer);";
    assert!(run_exec(&ours, sql).status.success());
    assert!(Command::new(&oracle)
        .arg(&theirs)
        .arg(sql)
        .status()
        .unwrap()
        .success());

    assert_eq!(
        header_field(&ours, CHANGE_COUNTER_OFFSET),
        header_field(&theirs, CHANGE_COUNTER_OFFSET),
        "a freshly created database's change counter must match the oracle's"
    );
    assert_eq!(
        header_field(&ours, VERSION_VALID_FOR_OFFSET),
        header_field(&theirs, VERSION_VALID_FOR_OFFSET),
        "a freshly created database's version-valid-for must match the oracle's"
    );
}
