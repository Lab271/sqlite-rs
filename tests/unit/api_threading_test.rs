// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! The `Send + Sync` handle and its worker thread (spec 013
//! Requirement 4).
//!
//! The engine is `Rc`/`RefCell` by decision (ADR-0013, ADR-0017) and `Rc`
//! is not `Send`, so a handle that a pool or an async task can hold cannot
//! own the engine directly. No wrapper fixes that either: `Mutex<T>` is
//! `Send` only when `T: Send`. The connection therefore owns a thread and
//! is spoken to over a channel (ADR-0041).
//!
//! What these tests pin is the contract that makes the design usable rather
//! than merely type-correct: the handle really is `Send + Sync`, several
//! threads really can share one, and the thread really goes away on drop.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlite_rs::api::Connection;
use sqlite_rs::record::Value;

/// Compile-time proof, not a runtime check. If `Connection` ever stops
/// being `Send + Sync` this fails to build, which is the point —
/// Requirement 4 is a type-level claim and deserves a type-level test.
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<Connection>();
const _: () = assert_send_sync::<sqlite_rs::api::Error>();

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-api-thread-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn clean(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn handle_is_send_sync() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(a INTEGER, b TEXT);
         INSERT INTO t VALUES (1, 'x');
         INSERT INTO t VALUES (2, 'y');",
    )
    .unwrap();

    // Cloned into several threads, each running statements concurrently.
    // The worker serializes them; the test is that every one succeeds and
    // none deadlocks.
    let mut handles = Vec::new();
    for worker in 0..8 {
        let conn = conn.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..25 {
                conn.execute("SELECT a FROM t").unwrap();
                conn.execute_with(
                    "INSERT INTO t VALUES (?1, ?2)",
                    vec![Value::from(1000 + worker * 100 + i), Value::from("z")],
                )
                .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().expect("a worker thread panicked");
    }

    // 2 seeded + 8 threads x 25 inserts.
    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 202);
}

/// A `&Connection` shared through an `Arc` rather than cloned — the shape a
/// trait object or a pool hands out, and the one that needs `Sync` rather
/// than only `Send`.
#[test]
fn a_shared_reference_works_across_threads() {
    let conn = Arc::new(Connection::open_in_memory().unwrap());
    conn.execute("CREATE TABLE t(a INTEGER)").unwrap();

    let mut handles = Vec::new();
    for i in 0..4 {
        let conn = Arc::clone(&conn);
        handles.push(std::thread::spawn(move || {
            conn.execute_with("INSERT INTO t VALUES (?1)", vec![Value::from(i)])
                .unwrap();
        }));
    }
    for handle in handles {
        handle.join().expect("a worker thread panicked");
    }
    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 4);
}

/// The thread is released on drop, and this test would *hang* rather than
/// fail if it were not.
///
/// That is deliberate and worth stating: `Drop for Shared` closes the
/// request channel and then joins. If the worker did not terminate, the
/// join would block forever and this test would time out — so completing at
/// all is the assertion. (An earlier draft of the facade had exactly that
/// bug: it joined while still holding the only sender, so the worker waited
/// on a channel that could never close while the drop waited on the
/// worker.)
///
/// The observable consequence is checked too. A `Pager` releases its file
/// locks when the worker's stack unwinds, so if drop returned before the
/// join the next connection could meet its predecessor's SHARED lock. Two
/// hundred sequential open/write/drop cycles on one path would surface that.
#[test]
fn worker_thread_joins_on_drop() {
    let path = scratch("join");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t(a INTEGER)").unwrap();
    }

    for i in 0..200 {
        let conn = Connection::open(&path).unwrap();
        conn.execute_with("INSERT INTO t VALUES (?1)", vec![Value::from(i)])
            .unwrap();
        // Dropped here, at the end of each iteration.
    }

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.execute("DELETE FROM t").unwrap(),
        200,
        "every cycle's write should have committed and been visible to the next"
    );

    clean(&path);
}

/// Dropping one clone must not close the connection for the others.
#[test]
fn dropping_one_clone_leaves_the_rest_working() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER)").unwrap();

    let clone_a = conn.clone();
    let clone_b = conn.clone();
    drop(conn);
    drop(clone_a);

    // The last clone still owns a live worker.
    assert_eq!(clone_b.execute("INSERT INTO t VALUES (1)").unwrap(), 1);
    assert_eq!(clone_b.changes().unwrap(), 1);
}

/// The counters are connection state, not thread-local state — a value set
/// on one thread is readable from another through the same handle.
#[test]
fn counters_are_connection_scoped_not_thread_scoped() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();

    let writer = conn.clone();
    std::thread::spawn(move || {
        writer
            .execute("INSERT INTO u(id, v) VALUES (7, 'a')")
            .unwrap();
    })
    .join()
    .expect("the writing thread panicked");

    assert_eq!(conn.changes().unwrap(), 1);
    assert_eq!(conn.last_insert_rowid().unwrap(), 7);
}
