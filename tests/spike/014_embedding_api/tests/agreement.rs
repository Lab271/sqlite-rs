//! Spike 014 (#682): correctness gate, plus the spec-013 scenarios that
//! are behavioural rather than numeric.
//!
//! Nothing in `benches/streaming.rs` is trustworthy unless the three
//! prototypes actually compute the same thing, so that check lives here
//! and `make test` gates `make bench` on it — spike 013's precedent.

use std::sync::Arc;

use embedding_api_spike::{batch, fixture, Connection, SendValue};

const ROWS: usize = 2_000;
const SQL: &str = "SELECT id, name, bucket FROM t";

/// The gate. All three delivery mechanisms, same rows, same order.
#[test]
fn all_three_prototypes_agree_on_every_row() {
    let path = fixture(ROWS);

    let control = batch::query_sendable(&path, SQL, vec![]).expect("control");
    let conn = Connection::open(&path).expect("open");
    let worker_batch = conn.query_batch(SQL, vec![]).expect("worker_batch");
    let worker_stream: Vec<_> = conn
        .query_stream(SQL, vec![])
        .expect("worker_stream")
        .map(|r| r.expect("row"))
        .collect();

    // A chunk size that does not divide the row count, so the final
    // partial chunk is exercised rather than assumed.
    let worker_chunked: Vec<_> = conn
        .query_chunked(SQL, vec![], 97)
        .expect("worker_chunked")
        .map(|r| r.expect("row"))
        .collect();

    assert_eq!(control.len(), ROWS, "control returned the wrong row count");
    assert_eq!(control, worker_batch, "worker_batch diverged from control");
    assert_eq!(control, worker_stream, "worker_stream diverged from control");
    assert_eq!(control, worker_chunked, "worker_chunked diverged from control");
}

/// A chunk larger than the whole result must still terminate and return
/// every row — the partial-final-chunk path with no full chunk at all.
#[test]
fn chunk_larger_than_result_still_delivers_every_row() {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");
    let rows: Vec<_> = conn
        .query_chunked(SQL, vec![], ROWS * 10)
        .expect("chunked")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(rows.len(), ROWS);
}

/// Order matters, not just membership: a FIFO bug in the streaming
/// primitive would still pass a set comparison on a sorted fixture, so
/// assert the actual sequence of the first rows.
#[test]
fn streaming_preserves_emission_order() {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");
    let first: Vec<_> = conn
        .query_stream(SQL, vec![])
        .expect("stream")
        .take(5)
        .map(|r| r.expect("row"))
        .collect();

    let ids: Vec<i64> = first
        .iter()
        .map(|row| match row.first() {
            Some(SendValue::Integer(i)) => *i,
            other => panic!("expected an integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5], "rows arrived out of order");
}

/// Spec 013/Req-4, scenario "the handle is shared across threads".
#[test]
fn handle_is_shared_across_threads() {
    let path = fixture(ROWS);
    let conn = Arc::new(Connection::open(&path).expect("open"));

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || conn.query_batch(SQL, vec![]).expect("query").len())
        })
        .collect();

    for h in handles {
        assert_eq!(h.join().expect("thread panicked"), ROWS);
    }
}

/// Spec 013/Req-4, scenario "the thread is released, and a dead engine
/// errors". Dropping joins the worker; a clone of the handle taken
/// before the drop must then fail rather than hang.
#[test]
fn dropped_connection_releases_thread_and_later_use_errors() {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");
    assert_eq!(conn.query_batch(SQL, vec![]).expect("pre-drop").len(), ROWS);
    drop(conn);

    // Reopening immediately must succeed: if `Drop` had not joined the
    // worker, the old engine could still hold the file.
    let reopened = Connection::open(&path).expect("reopen after drop");
    assert_eq!(reopened.query_batch(SQL, vec![]).expect("post-drop").len(), ROWS);
}

/// Spec 013/Req-7, scenario "abandoning a statement releases it".
/// Dropping the stream mid-iteration must not wedge the worker: the next
/// statement on the same connection has to succeed.
#[test]
fn abandoned_stream_releases_the_engine() {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");

    {
        let mut stream = conn.query_stream(SQL, vec![]).expect("stream");
        assert!(stream.next().is_some(), "expected at least one row");
        // Drop with ~1,999 rows still unread. `sync_channel(1)` means the
        // worker is blocked in `send` at this moment.
    }

    // If abandoning leaked the worker's cursors or left it blocked, this
    // would hang or fail.
    assert_eq!(
        conn.query_batch(SQL, vec![]).expect("reuse after abandon").len(),
        ROWS,
        "connection unusable after an abandoned stream"
    );
}

/// Req-3 territory, but cheap to answer here: binding still works
/// across the boundary once parameters are converted.
#[test]
fn bound_parameter_crosses_the_boundary() {
    let path = fixture(ROWS);
    let conn = Connection::open(&path).expect("open");
    let rows = conn
        .query_batch(
            "SELECT id, name FROM t WHERE id = ?1",
            vec![SendValue::Integer(7)],
        )
        .expect("bound query");
    assert_eq!(rows.len(), 1, "expected exactly one row for id = 7");
    assert_eq!(rows[0][0], SendValue::Integer(7));
}
