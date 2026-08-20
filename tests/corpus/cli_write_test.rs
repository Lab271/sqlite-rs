//! End-to-end tests of the `sqlite-rs exec` CLI subcommand (#215, Phase 4
//! of the V3 epic #161): INSERT/UPDATE/DELETE/CREATE TABLE/DROP TABLE/
//! CREATE INDEX/DROP INDEX, each written via the CLI binary against a
//! scratch copy, then verified by reading back — via the CLI's own
//! `query` subcommand (round trip) and, when the pinned oracle `sqlite3`
//! is available, via `PRAGMA integrity_check` and a `SELECT` (write via
//! CLI -> read via stock `sqlite3` produces identical results, per the
//! issue's acceptance criteria).
//!
//! Every test starts from a fresh scratch file rather than a committed
//! fixture — `export`'s convention of never mutating the fixture tree
//! applies doubly here, since these tests write to the database itself.

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-cli-write-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
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

fn run_query(db: &Path, sql: &str) -> String {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()));
    assert!(
        output.status.success(),
        "query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A scratch db seeded via the pinned oracle if available, else via our
/// own CLI's CREATE TABLE — either way, gives every test a real on-disk
/// database with a valid header before it starts exercising `exec`.
fn seed_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle)
            .arg(&db)
            .arg("CREATE TABLE t(a INTEGER, b TEXT)")
            .status()
            .unwrap();
        assert!(status.success());
    } else {
        let output = run_exec(&db, "CREATE TABLE seed_bootstrap(x)");
        assert!(output.status.success());
        let output = run_exec(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
        assert!(output.status.success());
    }
    db
}

fn oracle_select(oracle: &Path, db: &Path, sql: &str) -> String {
    let output = Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "oracle query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn insert_round_trips_through_cli_query() {
    let db = seed_db("insert");
    let output = run_exec(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y')");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = run_query(&db, "SELECT * FROM t");
    assert_eq!(rows, "1|x\n2|y\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        assert_eq!(oracle_select(&oracle, &db, "SELECT * FROM t"), "1|x\n2|y\n");
    } else {
        skip_no_oracle("insert_round_trips_through_cli_query (oracle cross-check)");
    }
}

#[test]
fn update_and_delete_round_trip_through_cli_query() {
    let db = seed_db("update_delete");
    assert!(
        run_exec(&db, "INSERT INTO t VALUES (1,'x'),(2,'y'),(3,'z')")
            .status
            .success()
    );
    assert!(run_exec(&db, "UPDATE t SET b = 'zz' WHERE a = 3")
        .status
        .success());
    assert!(run_exec(&db, "DELETE FROM t WHERE a = 1").status.success());

    let rows = run_query(&db, "SELECT * FROM t");
    assert_eq!(rows, "2|y\n3|zz\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        assert_eq!(
            oracle_select(&oracle, &db, "SELECT * FROM t"),
            "2|y\n3|zz\n"
        );
    } else {
        skip_no_oracle("update_and_delete_round_trip_through_cli_query (oracle cross-check)");
    }
}

/// A fresh scratch db seeded with every `ddl` statement given — unlike
/// `seed_db`'s single hardcoded `t(a, b)`, for tests (like `INSERT ...
/// SELECT`) that need more than one table. Same oracle-if-available,
/// else-bootstrap-via-`exec` shape as `seed_db`.
fn multi_table_db(label: &str, ddls: &[&str]) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        for ddl in ddls {
            let status = Command::new(&oracle).arg(&db).arg(ddl).status().unwrap();
            assert!(status.success());
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for ddl in ddls {
            assert!(run_exec(&db, ddl).status.success());
        }
    }
    db
}

/// #208: `INSERT INTO t SELECT ...` drives the same scan/filter/project
/// machinery as a plain `SELECT`, feeding each projected row into the
/// target table's per-row constraint-check/write path instead of
/// `ResultRow`.
#[test]
fn insert_select_copies_filtered_rows_into_target_table() {
    let db = multi_table_db(
        "insert_select_basic",
        &[
            "CREATE TABLE src(a INTEGER, b TEXT)",
            "CREATE TABLE dst(a INTEGER, b TEXT)",
        ],
    );
    assert!(
        run_exec(&db, "INSERT INTO src VALUES (1,'x'),(2,'y'),(3,'z')")
            .status
            .success()
    );

    let output = run_exec(&db, "INSERT INTO dst SELECT a, b FROM src WHERE a > 1");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(run_query(&db, "SELECT * FROM dst"), "2|y\n3|z\n");
    // The source table is read-only for this statement — untouched.
    assert_eq!(run_query(&db, "SELECT * FROM src"), "1|x\n2|y\n3|z\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    } else {
        skip_no_oracle("insert_select_copies_filtered_rows_into_target_table (oracle cross-check)");
    }
}

/// #208: an explicit target column list re-orders which SELECT column
/// lands in which target column, exactly like a literal-VALUES INSERT's
/// column list already does.
#[test]
fn insert_select_honors_explicit_target_column_list() {
    let db = multi_table_db(
        "insert_select_columns",
        &[
            "CREATE TABLE src(a INTEGER, b TEXT)",
            "CREATE TABLE dst(a INTEGER, b TEXT)",
        ],
    );
    assert!(run_exec(&db, "INSERT INTO src VALUES (1,'x')")
        .status
        .success());

    // Swap: src's b -> dst's a, src's a -> dst's b.
    let output = run_exec(&db, "INSERT INTO dst (b, a) SELECT a, b FROM src");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(run_query(&db, "SELECT * FROM dst"), "x|1\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    } else {
        skip_no_oracle("insert_select_honors_explicit_target_column_list (oracle cross-check)");
    }
}

/// #208: `ORDER BY`/`LIMIT` on the `SELECT` side (the sorted-scan path,
/// `compile_sorted_scan`) drives the insert exactly like the direct-scan
/// path — full parity, not just plain scan+WHERE.
#[test]
fn insert_select_with_order_by_and_limit_uses_sorted_scan() {
    let db = multi_table_db(
        "insert_select_order_limit",
        &[
            "CREATE TABLE src(a INTEGER, b TEXT)",
            "CREATE TABLE dst(a INTEGER, b TEXT)",
        ],
    );
    assert!(
        run_exec(&db, "INSERT INTO src VALUES (3,'c'),(1,'a'),(2,'b')")
            .status
            .success()
    );

    let output = run_exec(
        &db,
        "INSERT INTO dst SELECT a, b FROM src ORDER BY a DESC LIMIT 2",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Insertion order matters here only insofar as it must be the
    // sorted-then-limited order, not source-table scan order.
    assert_eq!(run_query(&db, "SELECT * FROM dst ORDER BY a"), "2|b\n3|c\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    } else {
        skip_no_oracle(
            "insert_select_with_order_by_and_limit_uses_sorted_scan (oracle cross-check)",
        );
    }
}

/// #208: a `SELECT`-sourced row that violates a target-table constraint
/// (NOT NULL here) fails the same way a literal-`VALUES` row would —
/// the scan/write machinery is shared, so constraint enforcement is too.
#[test]
fn insert_select_row_violating_not_null_fails_cleanly() {
    let db = multi_table_db(
        "insert_select_not_null",
        &[
            "CREATE TABLE src(a INTEGER, b TEXT)",
            "CREATE TABLE dst(a INTEGER, b TEXT NOT NULL)",
        ],
    );
    assert!(run_exec(&db, "INSERT INTO src VALUES (1, NULL)")
        .status
        .success());

    let output = run_exec(&db, "INSERT INTO dst SELECT a, b FROM src");
    assert!(
        !output.status.success(),
        "a NULL projected into a NOT NULL column must fail"
    );
    assert_eq!(run_query(&db, "SELECT * FROM dst"), "");
}

#[test]
fn create_table_is_visible_to_cli_query_and_tables() {
    let db = seed_db("create_table");
    let output = run_exec(&db, "CREATE TABLE u(c INTEGER)");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(run_exec(&db, "INSERT INTO u VALUES (42)").status.success());
    assert_eq!(run_query(&db, "SELECT * FROM u"), "42\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        assert_eq!(oracle_select(&oracle, &db, "SELECT * FROM u"), "42\n");
    } else {
        skip_no_oracle("create_table_is_visible_to_cli_query_and_tables (oracle cross-check)");
    }
}

#[test]
fn create_index_populates_existing_rows_and_survives_reopen() {
    let db = seed_db("create_index");
    assert!(run_exec(&db, "INSERT INTO t VALUES (1,'x'),(2,'y')")
        .status
        .success());
    let output = run_exec(&db, "CREATE INDEX idx_t_b ON t(b)");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Re-opening and querying again proves the index's own root page and
    // sqlite_master row persisted correctly, not just that the in-process
    // write succeeded.
    assert_eq!(run_query(&db, "SELECT * FROM t"), "1|x\n2|y\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        assert_eq!(
            oracle_select(&oracle, &db, "SELECT * FROM t ORDER BY b"),
            "1|x\n2|y\n"
        );
    } else {
        skip_no_oracle(
            "create_index_populates_existing_rows_and_survives_reopen (oracle cross-check)",
        );
    }
}

#[test]
fn drop_index_then_drop_table_removes_them_from_the_schema() {
    let db = seed_db("drop");
    assert!(run_exec(&db, "INSERT INTO t VALUES (1,'x')")
        .status
        .success());
    assert!(run_exec(&db, "CREATE INDEX idx_t_b ON t(b)")
        .status
        .success());

    let output = run_exec(&db, "DROP INDEX idx_t_b");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_exec(&db, "DROP TABLE t");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        let remaining = oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('t','idx_t_b')",
        );
        assert_eq!(remaining.trim(), "0");
    } else {
        skip_no_oracle(
            "drop_index_then_drop_table_removes_them_from_the_schema (oracle cross-check)",
        );
    }
}

/// A path that doesn't exist must fail cleanly (exit 1, diagnostic on
/// stderr, no panic) rather than propagating an I/O panic — `exec`'s
/// `dump::open` failure path.
#[test]
fn exec_on_a_nonexistent_database_fails_cleanly() {
    let dir = scratch_db("missing").parent().unwrap().to_path_buf();
    let missing = dir.join("does_not_exist.db");

    let output = run_exec(&missing, "SELECT 1");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!stderr.is_empty(), "expected a diagnostic; got nothing");
    assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
}

/// A NOT NULL violation is a runtime constraint check inside
/// `execute_with_writable_db`, not a compile-time error — `exec` must
/// surface it as a clean failure (exit 1) rather than a panic, and must
/// leave the table state unaffected (verified via a follow-up SELECT).
#[test]
fn exec_not_null_violation_fails_cleanly_at_runtime() {
    let db = seed_db("not-null");
    assert!(run_exec(&db, "CREATE TABLE u(id INTEGER, v TEXT NOT NULL)")
        .status
        .success());

    let output = run_exec(&db, "INSERT INTO u(id, v) VALUES (1, NULL)");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.contains("NOT NULL"),
        "expected a NOT NULL constraint diagnostic; got: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");

    assert_eq!(run_query(&db, "SELECT * FROM u"), "");
}

/// Each statement-specific parser in `compile_statement` can return a
/// non-`Accepted` outcome (`Unsupported`/`Invalid`) for malformed input;
/// `exec` must report it as a clean failure (exit 1) rather than panic,
/// for every branch of the dispatch — INSERT/UPDATE/DELETE/CREATE TABLE/
/// CREATE INDEX/DROP TABLE/DROP INDEX — plus the final unrecognized-
/// statement fallback.
#[test]
fn exec_malformed_or_unrecognized_statements_fail_cleanly() {
    let db = seed_db("malformed");
    assert!(run_exec(&db, "CREATE INDEX idx_t_b ON t(b)")
        .status
        .success());

    for sql in [
        "INSERT INTO",
        "UPDATE",
        "DELETE FROM",
        "CREATE TABLE",
        "CREATE INDEX",
        "DROP TABLE",
        "DROP INDEX",
        "SELECT 1",
        "PRAGMA foo",
    ] {
        let output = run_exec(&db, sql);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(1),
            "expected exit 1 for exec {sql:?}; stderr: {stderr}"
        );
        assert!(output.stdout.is_empty(), "exec {sql:?} wrote to stdout");
        assert!(!stderr.is_empty(), "exec {sql:?} gave no diagnostic");
        assert!(
            !stderr.contains("panicked at"),
            "exec {sql:?} must not panic: {stderr}"
        );
    }
}

/// Full lifecycle in one test, driven entirely through the CLI: create
/// schema, insert, update, delete, select, export — mirroring what a
/// real user session against `sqlite-rs` looks like end to end, rather
/// than each verb tested in isolation.
#[test]
fn full_lifecycle_schema_to_export_round_trips() {
    let db = seed_db("lifecycle");

    // 1. Create schema — a second table, distinct from seed_db's own
    // bootstrap table, so this test's assertions aren't coupled to it.
    assert!(
        run_exec(
            &db,
            "CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"
        )
        .status
        .success(),
        "CREATE TABLE failed"
    );

    // 2. Create records.
    assert!(
        run_exec(
            &db,
            "INSERT INTO people(id, name, age) VALUES \
             (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Carol', 40)"
        )
        .status
        .success(),
        "INSERT failed"
    );
    assert_eq!(
        run_query(&db, "SELECT * FROM people"),
        "1|Alice|30\n2|Bob|25\n3|Carol|40\n"
    );

    // 3. Update records.
    assert!(
        run_exec(&db, "UPDATE people SET age = 31 WHERE name = 'Alice'")
            .status
            .success(),
        "UPDATE failed"
    );
    assert_eq!(
        run_query(&db, "SELECT age FROM people WHERE name = 'Alice'"),
        "31\n"
    );

    // 4. Delete records.
    assert!(
        run_exec(&db, "DELETE FROM people WHERE name = 'Bob'")
            .status
            .success(),
        "DELETE failed"
    );
    assert_eq!(
        run_query(&db, "SELECT name FROM people ORDER BY id"),
        "Alice\nCarol\n"
    );

    // 5. Select records — final state, ordered for a stable assertion.
    assert_eq!(
        run_query(&db, "SELECT id, name, age FROM people ORDER BY id"),
        "1|Alice|31\n3|Carol|40\n"
    );

    // 6. Export records — `export`'s CSV output, one file per table,
    // written as a sibling of the database file.
    let output = Command::new(CLI)
        .arg("export")
        .arg(&db)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} export {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let csv_path = db.with_file_name(format!(
        "people_{}.csv",
        db.file_stem().unwrap().to_string_lossy()
    ));
    let csv = std::fs::read_to_string(&csv_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", csv_path.display()));
    assert_eq!(
        csv, "id,name,age\r\n1,Alice,31\r\n3,Carol,40\r\n",
        "exported CSV mismatch"
    );

    // Cross-check the final state against stock sqlite3, when available.
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
        assert_eq!(
            oracle_select(&oracle, &db, "SELECT id, name, age FROM people ORDER BY id"),
            "1|Alice|31\n3|Carol|40\n"
        );
    } else {
        skip_no_oracle("full_lifecycle_schema_to_export_round_trips (oracle cross-check)");
    }
}
