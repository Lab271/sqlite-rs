// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! The durability and retryable-busy contract (spec 013 Requirement 5),
//! for the parts observable without a second process.
//!
//! The busy variant itself is exercised in
//! `tests/corpus/api_durability_oracle_test.rs`, against a real second
//! process. It has to be, and the reason is a finding rather than a
//! convenience:
//!
//! # Two connections in one process do not lock against each other
//!
//! POSIX `fcntl` locks are scoped to `(process, inode)`, not to the open
//! file description — which this crate's own `src/vfs/lock.rs:96` documents
//! and `FileLockState::check_reserved_lock` states outright ("whether some
//! *other* process currently holds a write lock"). So a second `Connection`
//! on the same path in the same process escalates to EXCLUSIVE without
//! conflict, even while the first holds RESERVED.
//!
//! Measured on this tree: connection A takes `BEGIN IMMEDIATE` and inserts
//! row 2; connection B's insert of row 3 returns `Ok(1)`; A commits; the
//! file then contains rows `[1, 2]`. Row 3 is silently gone, and
//! `PRAGMA integrity_check` reports `ok`, so nothing flags it.
//!
//! Stock SQLite prevents exactly this with `unixInodeInfo` in `os_unix.c` —
//! a process-global registry keyed by `(device, inode)` carrying its own
//! mutex and lock counts, so two connections in one process serialize the
//! same way two processes do. This crate has no equivalent.
//!
//! That is a pre-existing engine gap, not something the facade introduced,
//! but the facade makes it far easier to reach: Requirement 4 exists so a
//! *pool* can hold a handle, and opening the same path twice is the
//! obvious thing to do. `in_process_connections_lock_against_each_other`
//! below is the ratchet, `#[ignore]`d until the registry exists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlite_rs::api::{Connection, TransactionBehavior};

fn scratch(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-api-dur-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn clean(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::remove_dir_all(dir).ok();
    }
}

fn prepared(label: &str) -> PathBuf {
    let path = scratch(label);
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x');")
        .unwrap();
    path
}

fn count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM t")
        .unwrap()
        .expect("count returns a row")
        .get(0)
        .unwrap()
}

#[test]
fn the_busy_timeout_is_settable_and_defaults_to_zero() {
    let conn = Connection::open_in_memory().unwrap();
    // Accepted at any value, and setting it does not disturb the
    // connection. The default is zero, matching stock SQLite, where
    // `sqlite3_busy_timeout` is unset until asked for — there is no getter
    // here because SQLite has none either.
    conn.set_busy_timeout(Duration::from_millis(250)).unwrap();
    conn.set_busy_timeout(Duration::ZERO).unwrap();
    conn.set_busy_timeout(Duration::from_secs(30)).unwrap();

    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'x')").unwrap();
    assert_eq!(count(&conn), 1);
}

/// `PRAGMA synchronous` is the durability knob Requirement 5 names, and all
/// three levels are implemented (#645, ADR-0036). This pins that the facade
/// can reach them and that writes still land afterwards.
#[test]
fn the_durability_knob_is_reachable_and_writes_survive_each_level() {
    for level in ["FULL", "NORMAL", "OFF"] {
        let path = prepared("sync");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma("synchronous", level)
                .unwrap_or_else(|e| panic!("PRAGMA synchronous = {level} failed: {e}"));

            let tx = conn.transaction().unwrap();
            tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap();
            tx.commit().unwrap();
        }
        // Reopened: the commit reached the file, whichever level was set.
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            count(&conn),
            2,
            "a commit under synchronous={level} was lost"
        );
        clean(&path);
    }
}

/// **Ratchet, currently failing by construction.**
///
/// Two connections in one process must exclude each other's writes the way
/// two processes do. They do not: see this module's documentation for the
/// measurement and for `unixInodeInfo`, the mechanism stock SQLite uses.
///
/// Un-`#[ignore]` this when the process-global inode registry lands. It is
/// written to assert the *correct* behaviour, so it will pass at that point
/// without being rewritten — the ratchet convention this repo uses for
/// spike findings (ADR-0008).
#[test]
#[ignore = "in-process connections do not lock against each other (POSIX fcntl is per-process); needs a unixInodeInfo-equivalent registry — see this module's docs"]
fn in_process_connections_lock_against_each_other() {
    let path = prepared("in-process");

    let a = Connection::open(&path).unwrap();
    let b = Connection::open(&path).unwrap();

    let tx = a.transaction_with(TransactionBehavior::Immediate).unwrap();
    tx.execute("INSERT INTO t VALUES (2, 'a')").unwrap();

    // Either of these outcomes is correct; silently succeeding and then
    // losing the row is not.
    match b.execute("INSERT INTO t VALUES (3, 'b')") {
        Err(e) => assert!(
            e.is_retryable(),
            "a contended write should be retryable, got {e:?}"
        ),
        Ok(_) => {
            // If it is allowed, the row must actually survive.
            tx.commit().unwrap();
            let survivors: i64 = b
                .query_row("SELECT count(*) FROM t WHERE a = 3")
                .unwrap()
                .unwrap()
                .get(0)
                .unwrap();
            assert_eq!(
                survivors, 1,
                "a write that reported success was silently discarded"
            );
        }
    }

    clean(&path);
}
