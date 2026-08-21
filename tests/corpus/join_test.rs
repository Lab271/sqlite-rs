//! End-to-end oracle-diff tests for INNER/LEFT [OUTER]/CROSS JOIN
//! (#237), via the `sqlite-rs` CLI's `exec`/`query` subcommands —
//! mirroring `cli_write_test.rs`'s scratch-db-per-test shape.

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-join-{label}-{}-{n}", std::process::id()));
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

/// A fresh scratch db seeded with every `ddl` statement given, then the
/// join fixture's rows. Same oracle-if-available, else-bootstrap-via-
/// `exec` shape as `cli_write_test.rs`'s `multi_table_db`.
fn join_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT)",
        "CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT)",
        "CREATE TABLE c(id INTEGER PRIMARY KEY, b_id INTEGER, note TEXT)",
    ];
    let rows = [
        "INSERT INTO a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        // a_id=1 matches a.id=1 twice; a_id=99 matches no row in `a`.
        "INSERT INTO b VALUES (10, 1, 'x'), (11, 1, 'y'), (12, 99, 'z')",
        "INSERT INTO c VALUES (100, 10, 'p'), (101, 11, 'q')",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls.iter().chain(rows.iter()) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls.iter().chain(rows.iter()) {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    db
}

/// A second fixture, purpose-built for USING/NATURAL join tests: `p`
/// and `q` share a column name (`id`) whose *values* actually overlap
/// (unlike `join_fixture_db`'s `a`/`b`/`c`, where every table's PK is
/// named `id` but the tables are related via differently-named FK
/// columns instead — great for `ON`, useless for USING/NATURAL's
/// same-name-same-value join semantics).
fn using_natural_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE p(id INTEGER PRIMARY KEY, name TEXT)",
        "CREATE TABLE q(id INTEGER, extra TEXT)",
    ];
    let rows = [
        "INSERT INTO p VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        // id=2 matches p.id=2; id=99 matches no row in `p`.
        "INSERT INTO q VALUES (1, 'x'), (2, 'y'), (99, 'z')",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls.iter().chain(rows.iter()) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls.iter().chain(rows.iter()) {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    db
}

/// Runs `sql` through the CLI and, when the pinned oracle is available,
/// asserts the two outputs are byte-identical; otherwise skips the
/// cross-check but still exercises the CLI path so a panic/crash is
/// still caught.
fn assert_matches_oracle(db: &Path, sql: &str, test_name: &str) {
    let ours = run_query(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

#[test]
fn inner_join_matches_oracle() {
    let db = join_fixture_db("inner");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id",
        "inner_join_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// Bare `JOIN` and `INNER JOIN` must compile identically.
#[test]
fn inner_and_bare_join_are_equivalent() {
    let db = join_fixture_db("inner_bare");
    let bare = run_query(&db, "SELECT * FROM a JOIN b ON a.id = b.a_id");
    let inner = run_query(&db, "SELECT * FROM a INNER JOIN b ON a.id = b.a_id");
    assert_eq!(bare, inner);
    assert_matches_oracle(
        &db,
        "SELECT * FROM a INNER JOIN b ON a.id = b.a_id",
        "inner_and_bare_join_are_equivalent",
    );
}

/// LEFT JOIN: `a`'s row `id=3` (carol) has no matching `b` row, so it
/// must appear exactly once with `b`'s columns NULL.
#[test]
fn left_join_null_extends_unmatched_rows() {
    let db = join_fixture_db("left");
    let output = run_query(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a LEFT JOIN b ON a.id = b.a_id",
    );
    assert_eq!(
        output, "1|alice|10|x\n1|alice|11|y\n2|bob||\n3|carol||\n",
        "unmatched left rows (bob, carol) must appear once with NULL b columns"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a LEFT JOIN b ON a.id = b.a_id",
        "left_join_null_extends_unmatched_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

#[test]
fn cross_join_is_the_full_cartesian_product() {
    let db = join_fixture_db("cross");
    let output = run_query(&db, "SELECT a.id, b.id FROM a CROSS JOIN b");
    assert_eq!(
        output.lines().count(),
        3 * 3,
        "3 rows in `a` x 3 rows in `b` = 9 rows; got: {output}"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, b.id FROM a CROSS JOIN b",
        "cross_join_is_the_full_cartesian_product",
    );
}

/// A 3-way join chain: `a JOIN b ON ... JOIN c ON ...`.
#[test]
fn three_way_inner_join_matches_oracle() {
    let db = join_fixture_db("three_way");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag, c.note FROM a JOIN b ON a.id = b.a_id JOIN c ON b.id = c.b_id",
        "three_way_inner_join_matches_oracle",
    );
}

/// A WHERE clause after the join filters the joined result, not just
/// one side.
#[test]
fn where_clause_filters_the_joined_result() {
    let db = join_fixture_db("where_after_join");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag FROM a JOIN b ON a.id = b.a_id WHERE b.tag = 'y'",
        "where_clause_filters_the_joined_result",
    );
}

/// LIMIT applies to the joined output as a whole.
#[test]
fn limit_applies_to_the_joined_output() {
    let db = join_fixture_db("limit_after_join");
    let output = run_query(
        &db,
        "SELECT a.id, b.id FROM a JOIN b ON a.id = b.a_id LIMIT 1",
    );
    assert_eq!(output, "1|10\n");
}

/// `star`-expansion across a join projects every table's columns, in
/// FROM order.
#[test]
fn star_expands_across_every_joined_table() {
    let db = join_fixture_db("star");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id",
        "star_expands_across_every_joined_table",
    );
}

/// `USING (id)` joins `a` and `b` on their shared `id` column, exactly
/// as if `ON a.id = b.id` had been written — content-wise. This
/// intentionally isn't `a.id = b.a_id` (the FK relationship the other
/// tests use); it just exercises USING's column-name-driven join
/// mechanics against the one column name the fixture tables actually
/// share.
#[test]
fn using_join_matches_oracle() {
    let db = using_natural_fixture_db("using");
    assert_matches_oracle(
        &db,
        "SELECT p.name, q.extra FROM p JOIN q USING (id)",
        "using_join_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// `NATURAL JOIN` between `a` and `b` implicitly joins on every column
/// name they share (`id`), same semantics as `using_join_matches_oracle`.
#[test]
fn natural_join_matches_oracle() {
    let db = using_natural_fixture_db("natural");
    assert_matches_oracle(
        &db,
        "SELECT p.name, q.extra FROM p NATURAL JOIN q",
        "natural_join_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// `SELECT *` across a USING join must de-duplicate the shared `id`
/// column: `p` and `q` have 2 columns each (`id, name` / `id, extra`),
/// so a naive concatenation would produce 4 columns, but real SQLite
/// (and this codegen) emit only 3 — one merged `id` (taking the left
/// table's value) plus `name`, `extra`.
#[test]
fn star_dedup_across_using_join() {
    let db = using_natural_fixture_db("star_dedup_using");
    let output = run_query(&db, "SELECT * FROM p JOIN q USING (id)");
    let first_row = output.lines().next().expect("at least one row");
    assert_eq!(
        first_row.split('|').count(),
        3,
        "USING join's SELECT * must merge the shared `id` column into one \
         (p.id, p.name, q.extra = 3 columns), got: {first_row:?} in {output:?}"
    );
    assert_matches_oracle(
        &db,
        "SELECT * FROM p JOIN q USING (id)",
        "star_dedup_across_using_join",
    );
}

/// Same de-duplication check for `NATURAL JOIN`.
#[test]
fn star_dedup_across_natural_join() {
    let db = using_natural_fixture_db("star_dedup_natural");
    let output = run_query(&db, "SELECT * FROM p NATURAL JOIN q");
    let first_row = output.lines().next().expect("at least one row");
    assert_eq!(
        first_row.split('|').count(),
        3,
        "NATURAL JOIN's SELECT * must merge the shared `id` column into one, \
         got: {first_row:?} in {output:?}"
    );
    assert_matches_oracle(
        &db,
        "SELECT * FROM p NATURAL JOIN q",
        "star_dedup_across_natural_join",
    );
}

/// #250 gave the parser real grammar for comma-style joins, so
/// `FROM a, b` now parses AND compiles (it's synthesized as an
/// unconstrained CROSS JOIN, which codegen already supports) — this is
/// a full cartesian product, same shape as `cross_join_is_the_full_cartesian_product`.
#[test]
fn comma_join_is_cross_join_sugar() {
    let db = join_fixture_db("comma");
    let output = run_query(&db, "SELECT a.id, b.id FROM a, b");
    assert_eq!(
        output.lines().count(),
        3 * 3,
        "3 rows in `a` x 3 rows in `b` = 9 rows; got: {output}"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, b.id FROM a, b",
        "comma_join_is_cross_join_sugar",
    );
}

/// A `FULL JOIN` combined with another join in the same `FROM` clause
/// is still rejected cleanly (#250's codegen only supports a single
/// two-table `FULL JOIN` — see
/// `src/codegen/select.rs::compile_full_join_two_table`'s doc comment)
/// — not panic or silently mis-compile. RIGHT/FULL JOIN's own
/// supported shapes now compile and match the oracle — see
/// `right_join_null_extends_unmatched_left_rows`/
/// `full_join_matches_oracle_both_sides_unmatched` below.
///
/// #243: an inner table's `ON` equality against the *outer* table's
/// rowid (`a`'s `INTEGER PRIMARY KEY`) compiles to a `SeekRowid` point
/// lookup instead of a full `Rewind`/`Next` scan — result must be
/// identical to the oracle regardless of which side of `ON` names which
/// table, and regardless of table order in `FROM`.
#[test]
fn join_on_outer_rowid_uses_seek_and_matches_oracle() {
    let db = join_fixture_db("rowid_seek");
    assert_matches_oracle(
        &db,
        "SELECT * FROM b JOIN a ON b.a_id = a.id",
        "join_on_outer_rowid_uses_seek_and_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #243: `LEFT JOIN`'s null-extension still fires correctly when the
/// inner table's `ON` equality uses the `SeekRowid` fast path — `b`'s
/// `a_id=99` row has no matching `a.id`, so it must still appear once
/// with `a`'s columns NULL (not zero rows).
#[test]
fn left_join_on_rowid_seek_still_null_extends_unmatched_rows() {
    let db = join_fixture_db("left_rowid_seek");
    let output = run_query(
        &db,
        "SELECT b.id, b.a_id, a.id, a.name FROM b LEFT JOIN a ON b.a_id = a.id",
    );
    assert_eq!(
        output, "10|1|1|alice\n11|1|1|alice\n12|99||\n",
        "unmatched b row (a_id=99) must appear once with NULL a columns"
    );
    assert_matches_oracle(
        &db,
        "SELECT b.id, b.a_id, a.id, a.name FROM b LEFT JOIN a ON b.a_id = a.id",
        "left_join_on_rowid_seek_still_null_extends_unmatched_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #243: an inner table's `ON` equality against a `UNIQUE`-indexed
/// (non-rowid) column compiles to a `SeekIndexEq` + `IdxRowid` +
/// `SeekRowid` point lookup instead of a full scan.
#[test]
fn join_on_unique_indexed_column_uses_seek_and_matches_oracle() {
    let db = scratch_db("unique_index_seek");
    let ddls = [
        "CREATE TABLE d(id INTEGER PRIMARY KEY, code TEXT)",
        "CREATE UNIQUE INDEX idx_d_code ON d(code)",
        "CREATE TABLE e(id INTEGER PRIMARY KEY, d_code TEXT)",
    ];
    let rows = [
        "INSERT INTO d VALUES (1, 'x'), (2, 'y'), (3, 'z')",
        "INSERT INTO e VALUES (10, 'x'), (11, 'y'), (12, 'nomatch')",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls.iter().chain(rows.iter()) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls.iter().chain(rows.iter()) {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    assert_matches_oracle(
        &db,
        "SELECT e.id, d.id, d.code FROM e JOIN d ON e.d_code = d.code",
        "join_on_unique_indexed_column_uses_seek_and_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

#[test]
fn still_unsupported_join_forms_fail_cleanly() {
    let db = join_fixture_db("unsupported");
    for sql in [
        "SELECT * FROM a JOIN b ON a.id = b.a_id FULL JOIN c ON b.id = c.b_id",
        "SELECT * FROM a RIGHT JOIN b ON a.id = b.a_id RIGHT JOIN c ON b.id = c.b_id",
    ] {
        let output = Command::new(CLI)
            .arg("query")
            .arg(&db)
            .arg(sql)
            .output()
            .unwrap_or_else(|e| panic!("running {CLI} query {}: {e}", db.display()));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "expected {sql:?} to fail");
        assert!(
            stderr.contains("not yet supported"),
            "expected an unsupported-construct diagnostic for {sql:?}; got: {stderr}"
        );
        assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
    }
}

/// `RIGHT JOIN` is codegen'd by reordering to an equivalent `LEFT
/// JOIN` (`a RIGHT JOIN b` == `b LEFT JOIN a`) — the fixture's
/// `b_id=12, a_id=99` row has no matching row in `a`, so it must still
/// appear once with `a`'s columns NULL, same shape as
/// `left_join_null_extends_unmatched_rows` but with the tables' roles
/// swapped.
#[test]
fn right_join_null_extends_unmatched_rows() {
    let db = join_fixture_db("right");
    let output = run_query(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a RIGHT JOIN b ON a.id = b.a_id",
    );
    assert_eq!(
        output.lines().count(),
        3,
        "2 matching `b` rows plus 1 unmatched (`b_id=12`) = 3 rows; got: {output}"
    );
    assert!(
        output.lines().any(|line| line == "||12|z"),
        "expected the unmatched b row (id=12, a_id=99) to appear with a's columns NULL; got: \
         {output}"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a RIGHT JOIN b ON a.id = b.a_id",
        "right_join_null_extends_unmatched_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// A three-way chain with the `RIGHT JOIN` in the middle
/// (`a JOIN b ... RIGHT JOIN c ...`) still compiles via the same
/// reordering — the whole `a JOIN b` chain becomes `c`'s null-extended
/// side.
#[test]
fn right_join_three_way_chain_matches_oracle() {
    let db = join_fixture_db("right_three_way");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag, c.note FROM a JOIN b ON a.id = b.a_id RIGHT JOIN c ON b.id = c.b_id",
        "right_join_three_way_chain_matches_oracle",
    );
}

/// `A FULL JOIN B ON cond`: unmatched rows from *both* sides must
/// appear — `a.id=3` ('carol') matches no `b` row, and `b.id=12`
/// (`a_id=99`) matches no `a` row.
#[test]
fn full_join_matches_oracle_both_sides_unmatched() {
    let db = join_fixture_db("full");
    let output = run_query(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a FULL JOIN b ON a.id = b.a_id",
    );
    assert!(
        output.lines().any(|line| line == "3|carol||"),
        "expected unmatched `a` row (id=3, carol) with b's columns NULL; got: {output}"
    );
    assert!(
        output.lines().any(|line| line == "||12|z"),
        "expected unmatched `b` row (id=12, a_id=99) with a's columns NULL; got: {output}"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a FULL JOIN b ON a.id = b.a_id",
        "full_join_matches_oracle_both_sides_unmatched",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #250's last piece: `ORDER BY` combined with a JOIN, keyed on a column
/// from the left-hand table.
#[test]
fn order_by_matches_oracle_across_a_join() {
    let db = join_fixture_db("order_by_left");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id ORDER BY a.name",
        "order_by_matches_oracle_across_a_join",
    );
}

/// `ORDER BY` keyed on a column from the right-hand (joined) table.
#[test]
fn order_by_on_joined_table_column_matches_oracle() {
    let db = join_fixture_db("order_by_right");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag FROM a JOIN b ON a.id = b.a_id ORDER BY b.tag DESC",
        "order_by_on_joined_table_column_matches_oracle",
    );
}

/// `ORDER BY` + `LIMIT` combined with a JOIN — checks the sort/limit
/// interaction still picks the correct top-N rows post-sort.
#[test]
fn order_by_with_limit_matches_oracle_across_a_join() {
    let db = join_fixture_db("order_by_limit");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id ORDER BY a.name LIMIT 2",
        "order_by_with_limit_matches_oracle_across_a_join",
    );
}

/// `DISTINCT` combined with a JOIN: `a.id=1` matches twice in `b`
/// (rows 10 and 11), so `DISTINCT a.name` must collapse the duplicate
/// `alice` down to a single row.
#[test]
fn distinct_collapses_duplicate_rows_across_a_join() {
    let db = join_fixture_db("distinct_join");
    let output = run_query(&db, "SELECT DISTINCT a.name FROM a JOIN b ON a.id = b.a_id");
    assert_eq!(
        output, "alice\n",
        "a.id=1 matches b twice (rows 10, 11) — DISTINCT must collapse to one `alice` row"
    );
    assert_matches_oracle(
        &db,
        "SELECT DISTINCT a.name FROM a JOIN b ON a.id = b.a_id",
        "distinct_collapses_duplicate_rows_across_a_join",
    );
}

/// #268: the common anti-join idiom — an outer join followed by
/// `WHERE <unmatched side>.<col> IS NULL` — isolates exactly the rows
/// that the plain LEFT/RIGHT/FULL JOIN tests above show get
/// NULL-extended.
#[test]
fn left_right_full_join_where_is_null_anti_join_matches_oracle() {
    let db = join_fixture_db("anti_join");
    for (label, sql) in [
        (
            "left",
            "SELECT a.id, a.name FROM a LEFT JOIN b ON a.id = b.a_id WHERE b.id IS NULL",
        ),
        (
            "right",
            "SELECT b.id, b.tag FROM a RIGHT JOIN b ON a.id = b.a_id WHERE a.id IS NULL",
        ),
        (
            "full",
            "SELECT a.id, b.id FROM a FULL JOIN b ON a.id = b.a_id \
             WHERE a.id IS NULL OR b.id IS NULL",
        ),
    ] {
        assert_matches_oracle(
            &db,
            sql,
            &format!("left_right_full_join_where_is_null_anti_join_matches_oracle[{label}]"),
        );
    }
    // LEFT JOIN's unmatched `a` rows: `id=2` (bob) and `id=3` (carol) —
    // `b.a_id` is never `2`, and `b.a_id=99` doesn't match any `a.id`.
    let left = run_query(
        &db,
        "SELECT a.id, a.name FROM a LEFT JOIN b ON a.id = b.a_id WHERE b.id IS NULL",
    );
    assert_eq!(left, "2|bob\n3|carol\n");
    // RIGHT JOIN's only unmatched row is `b.id=12` (a_id=99).
    let right = run_query(
        &db,
        "SELECT b.id, b.tag FROM a RIGHT JOIN b ON a.id = b.a_id WHERE a.id IS NULL",
    );
    assert_eq!(right, "12|z\n");
}

/// #288: `FULL JOIN` combined with `ORDER BY` — #268 originally found
/// this combination unsupported; `compile_full_join_two_table` now
/// routes all three of its emission points (matched, left-nulled,
/// right-unmatched) through a sorter pass, so null-extended rows sort
/// correctly alongside matched ones. The `full` fixture's `a` (id=2
/// bob, id=3 carol) has no matching `b` row, and `b`'s `id=12` row
/// (a_id=99) has no matching `a` row, so this exercises both
/// null-extension directions at once.
#[test]
fn full_join_order_by_matches_oracle_both_sides_unmatched() {
    let db = join_fixture_db("full");
    assert_matches_oracle(
        &db,
        "SELECT a.id, b.id FROM a FULL JOIN b ON a.id = b.a_id ORDER BY a.id, b.id",
        "full_join_order_by_matches_oracle_both_sides_unmatched",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #270: `ORDER BY` combined with a JOIN used to silently deoptimize —
/// `compile_join_level_for_sort` was a hand-forked copy of
/// `compile_join_level` that never grew the #243 single-check-access seek
/// optimization, so adding `ORDER BY` to an otherwise-seekable join query
/// quietly downgraded the inner table's `SeekRowid` point lookup to a full
/// `Rewind`/`Next` scan. Now that both paths share one traversal (see
/// `src/codegen/select/joins.rs::compile_join_level_traverse`), the sorted
/// path must report the exact same `SEARCH ... USING INTEGER PRIMARY KEY`
/// plan as the unsorted path in
/// `explain_query_plan_reports_rowid_search_and_scan` below, not `SCAN a`.
#[test]
fn explain_query_plan_reports_rowid_search_with_order_by() {
    let db = join_fixture_db("eqp_rowid_order_by");
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM b JOIN a ON b.a_id = a.id ORDER BY b.tag",
    );
    assert_eq!(
        output, "0|0|0|SCAN b\n1|0|0|SEARCH a USING INTEGER PRIMARY KEY (rowid=?)\n",
        "ORDER BY must not deoptimize the join's #243 seek: outer table b is a full scan, \
         inner table a is still seeked by rowid, same as the unsorted path"
    );
    assert_matches_oracle(
        &db,
        "SELECT * FROM b JOIN a ON b.a_id = a.id ORDER BY b.tag",
        "explain_query_plan_reports_rowid_search_with_order_by",
    );
}

/// #288: `FULL JOIN` combined with `DISTINCT` — `a.id=1` matches two
/// `b` rows (a_id=1 twice), so a plain (non-distinct) projection of
/// `a.id` alone would emit `1` twice; `DISTINCT` must dedup that down
/// to one row, and the dedup must apply to the null-extended rows too
/// (both `a`'s unmatched id=2/3 and `b`'s unmatched-side NULL `a.id`).
#[test]
fn full_join_distinct_matches_oracle_both_sides_unmatched() {
    let db = join_fixture_db("full");
    assert_matches_oracle(
        &db,
        "SELECT DISTINCT a.id FROM a FULL JOIN b ON a.id = b.a_id",
        "full_join_distinct_matches_oracle_both_sides_unmatched",
    );
    let output = run_query(
        &db,
        "SELECT DISTINCT a.id FROM a FULL JOIN b ON a.id = b.a_id",
    );
    assert_eq!(
        output.lines().filter(|l| *l == "1").count(),
        1,
        "a.id=1 (matched twice) must appear exactly once under DISTINCT; got: {output}"
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #288: `FULL JOIN` combined with `LIMIT` (no `ORDER BY`) — verifies
/// the two-pass emitter's short-circuit (`emit_limit_guard` jumping
/// straight to `end_label`, placed after pass 2) actually stops pass 2
/// once the limit is exhausted, rather than continuing to scan `b` a
/// second time for no reason. Row order without `ORDER BY` isn't
/// portably meaningful across engines, so this compares row *count*
/// (matching `LIMIT`) rather than the oracle's exact row order.
#[test]
fn full_join_limit_matches_oracle_both_sides_unmatched() {
    let db = join_fixture_db("full");
    let output = run_query(
        &db,
        "SELECT a.id, b.id FROM a FULL JOIN b ON a.id = b.a_id LIMIT 2",
    );
    assert_eq!(
        output.lines().count(),
        2,
        "LIMIT 2 must cap at 2 rows; got: {output}"
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #288: `FULL JOIN` combined with `ORDER BY` + `LIMIT` together —
/// `LIMIT` must apply post-sort (SQLite's own pipeline order), so this
/// is oracle-comparable row-for-row unlike the `LIMIT`-alone case
/// above.
#[test]
fn full_join_order_by_limit_matches_oracle_both_sides_unmatched() {
    let db = join_fixture_db("full");
    assert_matches_oracle(
        &db,
        "SELECT a.id, b.id FROM a FULL JOIN b ON a.id = b.a_id ORDER BY a.id, b.id LIMIT 2",
        "full_join_order_by_limit_matches_oracle_both_sides_unmatched",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #288: `DISTINCT` combined with `ORDER BY` on a `FULL JOIN` is the
/// one combination that still stays rejected — mirroring the same
/// restriction the ordinary (non-FULL) join tree already enforces for
/// `DISTINCT` + `ORDER BY` + any JOIN.
#[test]
fn full_join_distinct_order_by_still_unsupported() {
    let db = join_fixture_db("full_combinators");
    let sql = "SELECT DISTINCT a.id FROM a FULL JOIN b ON a.id = b.a_id ORDER BY a.id";
    let output = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {}: {e}", db.display()));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected {sql:?} to fail");
    assert!(
        stderr.contains("not yet supported"),
        "expected an unsupported-construct diagnostic for {sql:?}; got: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
}

/// #243: `EXPLAIN QUERY PLAN` reports `SEARCH ... USING INTEGER PRIMARY
/// KEY` for the rowid-seek join path and `SCAN` for tables that still
/// fall back to a full scan (no matching index on the equality side).
#[test]
fn explain_query_plan_reports_rowid_search_and_scan() {
    let db = join_fixture_db("eqp_rowid");
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM b JOIN a ON b.a_id = a.id",
    );
    assert_eq!(
        output, "0|0|0|SCAN b\n1|0|0|SEARCH a USING INTEGER PRIMARY KEY (rowid=?)\n",
        "outer table b is a full scan; inner table a is seeked by rowid"
    );
}

/// #243: `EXPLAIN QUERY PLAN` reports `SEARCH ... USING INDEX <name>`
/// for the unique-secondary-index seek join path.
#[test]
fn explain_query_plan_reports_unique_index_search() {
    let db = scratch_db("eqp_unique_index");
    let ddls = [
        "CREATE TABLE d(id INTEGER PRIMARY KEY, code TEXT)",
        "CREATE UNIQUE INDEX idx_d_code ON d(code)",
        "CREATE TABLE e(id INTEGER PRIMARY KEY, d_code TEXT)",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM e JOIN d ON e.d_code = d.code",
    );
    assert_eq!(
        output,
        "0|0|0|SCAN e\n1|0|0|SEARCH d USING INDEX idx_d_code (code=?)\n",
    );
}

/// #243: without a usable index/rowid equality, `EXPLAIN QUERY PLAN`
/// reports `SCAN` for every joined table — the full-scan fallback must
/// stay observable, not just correct.
#[test]
fn explain_query_plan_reports_full_scan_fallback() {
    let db = join_fixture_db("eqp_full_scan");
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM a JOIN b ON a.name = b.tag",
    );
    assert_eq!(output, "0|0|0|SCAN a\n1|0|0|SCAN b\n");
}
