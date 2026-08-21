//! Accept/reject-unsupported/reject-invalid parity between the V2
//! SELECT-core parser and a live `sqlite3` oracle (issue #61's "Oracle
//! parity" acceptance bar, spike 006's three-way outcome).
//!
//! Every case in `CASES` runs through `sqlite_rs::parser::parse_select`
//! and, when a pinned oracle is available, through `sqlite3 -readonly`
//! against a scratch table `t(a, b, c)`:
//!
//! - `Accept`: both must accept.
//! - `Unsupported`: sqlite-rs rejects as unsupported, but the oracle
//!   MUST accept — this is what proves it is "valid SQL we don't
//!   implement yet" rather than a bug.
//! - `Invalid`: both must reject (the oracle's stderr is not required to
//!   say "syntax error" verbatim, but its exit code must be non-zero).

use crate::oracle::{pinned_oracle, skip_no_oracle};
use sqlite_rs::parser::{parse_select, parse_update, ParseOutcome};
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Copy)]
enum Outcome {
    Accept,
    Unsupported,
    Invalid,
}

const CASES: &[(&str, Outcome)] = &[
    ("SELECT * FROM t", Outcome::Accept),
    ("SELECT a, b AS x FROM t WHERE a > 1", Outcome::Accept),
    (
        "SELECT a FROM t ORDER BY a DESC LIMIT 10 OFFSET 5",
        Outcome::Accept,
    ),
    ("SELECT (a + b) * c FROM t", Outcome::Accept),
    ("SELECT a FROM t WHERE a BETWEEN 1 AND 10", Outcome::Accept),
    ("SELECT a FROM t WHERE a IN (1, 2, 3)", Outcome::Accept),
    (
        "SELECT CASE a WHEN 1 THEN 'x' ELSE 'y' END FROM t",
        Outcome::Accept,
    ),
    ("SELECT CAST(a AS INTEGER) FROM t", Outcome::Accept),
    ("SELECT count(*) FROM t", Outcome::Accept),
    (
        "SELECT * FROM t JOIN t AS t2 ON t.a = t2.a",
        Outcome::Accept,
    ),
    ("SELECT * FROM t, t AS t2", Outcome::Accept),
    ("SELECT a FROM t GROUP BY a", Outcome::Accept),
    (
        "SELECT a, count(*) FROM t GROUP BY a HAVING count(*) > 1",
        Outcome::Accept,
    ),
    (
        "SELECT a FROM t UNION SELECT b FROM t",
        Outcome::Unsupported,
    ),
    ("SELECT (SELECT a FROM t) FROM t", Outcome::Accept),
    (
        "WITH cte AS (SELECT a FROM t) SELECT * FROM cte",
        Outcome::Unsupported,
    ),
    ("SELECT * FROM t WHERE a IN u", Outcome::Unsupported),
    ("SELECT * FROM (SELECT 1 AS a) AS x", Outcome::Accept),
    ("VALUES(2)", Outcome::Unsupported),
    (
        "SELECT count(*) FROM t HAVING count(*) >= 4",
        Outcome::Unsupported,
    ),
    ("SELECT * FROM t NOT INDEXED", Outcome::Unsupported),
    ("SELECT * FROM main.t", Outcome::Unsupported),
    ("SELECT 123 -> 456", Outcome::Unsupported),
    (
        "SELECT * FROM t OUTER LEFT NATURAL JOIN t AS t2",
        Outcome::Unsupported,
    ),
    ("SELECT FROM t", Outcome::Invalid),
    ("SELECT a FROM", Outcome::Invalid),
    ("SELECT (a + b FROM t", Outcome::Invalid),
    ("SELECT CASE a END FROM t", Outcome::Invalid),
];

/// Issue #190 UPDATE cases (oracle validated against scratch table `t`).
const UPDATE_CASES: &[(&str, Outcome)] = &[
    ("UPDATE t SET a=1", Outcome::Accept),
    ("UPDATE t SET a=1 WHERE b>0", Outcome::Accept),
    ("UPDATE t SET a=1, b=2, c=3", Outcome::Accept),
    ("UPDATE OR IGNORE t SET a=1000", Outcome::Accept),
    ("UPDATE OR REPLACE t SET a=1001", Outcome::Accept),
    ("UPDATE OR ROLLBACK t SET a=1", Outcome::Accept),
    ("UPDATE OR ABORT t SET a=1", Outcome::Accept),
    ("UPDATE OR FAIL t SET a=1", Outcome::Accept),
    ("UPDATE t SET (a, b) = (1, 2)", Outcome::Accept),
    ("UPDATE t SET a=(SELECT x FROM u)", Outcome::Accept),
    (
        "UPDATE t SET a=1 WHERE b IN (SELECT x FROM u)",
        Outcome::Accept,
    ),
    (
        "UPDATE t SET (a, b) = (SELECT x, x FROM u)",
        Outcome::Unsupported,
    ),
    ("UPDATE t SET a=", Outcome::Invalid),
    ("UPDATE t a=1", Outcome::Invalid),
    ("UPDATE t SET (a, b) = (1, 2, 3)", Outcome::Invalid),
];

fn scratch_db(suffix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_parser_oracle_test_{}_{suffix}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let status = Command::new("sqlite3")
        .arg(&path)
        .arg("CREATE TABLE t(a INTEGER, b INTEGER, c INTEGER); CREATE TABLE u(x INTEGER);")
        .status()
        .expect("creating scratch oracle db");
    assert!(status.success(), "failed to create scratch oracle db");
    path
}

fn oracle_accepts(oracle: &std::path::Path, db: &std::path::Path, sql: &str) -> bool {
    Command::new(oracle)
        .arg("-readonly")
        .arg(db)
        .arg(sql)
        .output()
        .expect("invoking sqlite3 oracle")
        .status
        .success()
}

/// Like `oracle_accepts`, but without `-readonly` — UPDATE needs to
/// actually write, unlike the read-only SELECT parity check above.
fn oracle_accepts_write(oracle: &std::path::Path, db: &std::path::Path, sql: &str) -> bool {
    Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .expect("invoking sqlite3 oracle")
        .status
        .success()
}

#[test]
fn parser_matches_oracle_three_way_outcome() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("parser_matches_oracle_three_way_outcome");
        return;
    };
    let db = scratch_db("select");

    for (sql, expected) in CASES {
        let ours = parse_select(sql);
        let oracle_ok = oracle_accepts(&oracle, &db, sql);

        match (expected, &ours) {
            (Outcome::Accept, ParseOutcome::Accepted(_)) => {
                assert!(oracle_ok, "oracle rejected an accept-case: {sql:?}");
            }
            (Outcome::Unsupported, ParseOutcome::Unsupported { .. }) => {
                assert!(
                    oracle_ok,
                    "oracle rejected an unsupported-but-should-be-valid case: {sql:?}"
                );
            }
            (Outcome::Invalid, ParseOutcome::Invalid { .. }) => {
                assert!(!oracle_ok, "oracle accepted an invalid case: {sql:?}");
            }
            (_, outcome) => {
                panic!("outcome mismatch for {sql:?}: got {outcome:?}");
            }
        }
    }

    let _ = std::fs::remove_file(&db);
}

/// Issue #190: UPDATE three-way parity against the same oracle/scratch-db
/// convention as `parser_matches_oracle_three_way_outcome`.
#[test]
fn parser_matches_oracle_three_way_outcome_update() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("parser_matches_oracle_three_way_outcome_update");
        return;
    };
    let db = scratch_db("update");

    for (sql, expected) in UPDATE_CASES {
        let ours = parse_update(sql);
        let oracle_ok = oracle_accepts_write(&oracle, &db, sql);

        match (expected, &ours) {
            (Outcome::Accept, ParseOutcome::Accepted(_)) => {
                assert!(oracle_ok, "oracle rejected an accept-case: {sql:?}");
            }
            (Outcome::Unsupported, ParseOutcome::Unsupported { .. }) => {
                assert!(
                    oracle_ok,
                    "oracle rejected an unsupported-but-should-be-valid case: {sql:?}"
                );
            }
            (Outcome::Invalid, ParseOutcome::Invalid { .. }) => {
                assert!(!oracle_ok, "oracle accepted an invalid case: {sql:?}");
            }
            (_, outcome) => {
                panic!("outcome mismatch for {sql:?}: got {outcome:?}");
            }
        }
    }

    let _ = std::fs::remove_file(&db);
}
