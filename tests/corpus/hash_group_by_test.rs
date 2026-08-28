// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #631 acceptance: an explicit `GROUP BY` with no covering index
//! compiles to the Sorter-backed `compile_grouped_scan` pipeline
//! (`SorterOpen`/`SorterInsert`/`SorterSort`/`SorterData`/
//! `SorterNext`) rather than #570's single-pass hash aggregation
//! (`HashAggOpen`/...), and produces byte-for-byte the same rows as
//! the pinned oracle. `try_compile_hash_grouped_scan` (hash.rs) is
//! kept intact but no longer wired into GROUP BY dispatch — see
//! `src/codegen/select/entry.rs`.
//!
//! The correctness risk this file exists to cover is *group identity*:
//! hash grouping decides which rows share a group by canonical key
//! bytes rather than by the sort strategy's adjacent-row `Eq` chain, so
//! every way SQLite calls two key values equal (merged numeric class,
//! collation, affinity) or unequal (NULL) needs an oracle diff.
//! Mirrors `index_ordered_group_by_test.rs`'s scratch-db-plus-oracle
//! pattern.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-hash-group-by-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(db: &Path) -> u32 {
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let page_size = u16::from_be_bytes([header_buf[16], header_buf[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn read_header(db: &Path, page_size: u32) -> DatabaseHeader {
    let vfs = UnixVfs;
    let pager = sqlite_rs::pager::Pager::open(&vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn table_schema(db: &Path, header: &DatabaseHeader, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = sqlite_rs::btree::TableCursor::new(source, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
}

fn oracle_rows(oracle: &PathBuf, db: &PathBuf, sql: &str) -> String {
    let out = Command::new(oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn our_rows(db: &Path, header: &DatabaseHeader, schema: &TableSchema, sql: &str) -> String {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let vfs = UnixVfs;
    let source: Rc<dyn PageSource> =
        Rc::new(VfsPageSource::open(&vfs, db, header.page_size).unwrap());
    let rows = execute_with_db(&program, source, *header).unwrap_or_else(|e| panic!("exec: {e}"));
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    sqlite_rs::record::Value::Null => String::new(),
                    sqlite_rs::record::Value::Integer(i) => i.to_string(),
                    // The CLI's own REAL rendering (`100.0`, not
                    // Rust's `100`) — the oracle's `-list` output is
                    // what this is diffed against.
                    sqlite_rs::record::Value::Real(r) => sqlite_rs::format::format_real(*r),
                    sqlite_rs::record::Value::Text(s) => s.to_string(),
                    sqlite_rs::record::Value::Blob(_) => "<blob>".to_string(),
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Confirms the sorter path was actually taken (not merely correct by
/// accident via hash aggregation): the compiled program opens a
/// sorter and never a hash-aggregation table.
fn assert_sorter_grouped(schema: &TableSchema, sql: &str) {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        opcodes.contains(&Opcode::SorterOpen),
        "expected a sorter-backed GROUP BY for {sql:?}, got: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&Opcode::HashAggOpen),
        "expected no hash aggregation for a sorter-backed GROUP BY {sql:?}, got: {opcodes:?}"
    );
}

/// Compiles, checks the sorter path fired, and diffs against the oracle.
fn check(oracle: &PathBuf, db: &PathBuf, header: &DatabaseHeader, schema: &TableSchema, sql: &str) {
    assert_sorter_grouped(schema, sql);
    assert_eq!(
        our_rows(db, header, schema, sql),
        oracle_rows(oracle, db, sql),
        "row mismatch for {sql:?}"
    );
}

/// The bread-and-butter case: several aggregate kinds folded side by
/// side over one hash table, each into its own accumulator slot.
#[test]
fn multiple_aggregates_in_one_query_match_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("multi_agg");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, count(*), sum(x), avg(x), min(x), max(x) FROM t GROUP BY bucket",
    );
}

/// Two key columns: the canonical key encoding has to keep them
/// unambiguous (a naive concatenation would merge `('a','bc')` with
/// `('ab','c')`).
#[test]
fn multi_column_group_by_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("multi_col");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b TEXT, x INTEGER); \
         INSERT INTO t(a, b, x) VALUES \
           ('a', 'bc', 1), ('ab', 'c', 2), ('a', 'bc', 3), ('ab', 'c', 4), ('a', 'c', 5);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT a, b, count(*), sum(x) FROM t GROUP BY a, b",
    );
}

/// NULL is its own group (all NULLs together, never merged with any
/// other value) — the one place `GROUP BY` equality deliberately
/// disagrees with `=`.
#[test]
fn null_group_keys_match_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("nulls");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) VALUES \
           (NULL, 1), (1, 2), (NULL, 3), (2, 4), (NULL, NULL), (1, NULL);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, count(*), count(x), sum(x) FROM t GROUP BY bucket",
    );
}

/// A `COLLATE NOCASE` column groups case-insensitively — the key
/// encoding has to fold before hashing, not compare after.
#[test]
fn nocase_collated_text_group_keys_match_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("nocase");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE, x INTEGER); \
         INSERT INTO t(name, x) VALUES \
           ('Ann', 1), ('ann', 2), ('ANN', 3), ('bob', 4), ('Bob', 5), ('carol', 6);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT name, count(*), sum(x) FROM t GROUP BY name",
    );
}

/// SQLite merges INTEGER and REAL into one numeric class for
/// comparison, so `1` and `1.0` are the same group — the canonical key
/// encoding must not separate them by storage class.
#[test]
fn integer_and_real_group_keys_that_compare_equal_share_a_group() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("numeric");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, k, x INTEGER); \
         INSERT INTO t(k, x) VALUES \
           (1, 10), (1.0, 20), (2, 30), (2.0, 40), (2.5, 50), (1, 60);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT k, count(*), sum(x) FROM t GROUP BY k",
    );
}

/// A declared-type column applies its comparison affinity before the
/// key is hashed, so numeric-looking text lands with its number — the
/// same coercion the sort strategy's boundary `Eq` performs.
#[test]
fn numeric_affinity_group_keys_match_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("affinity");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, k INTEGER, x INTEGER); \
         INSERT INTO t(k, x) VALUES (1, 10), ('1', 20), (2, 30), ('2', 40), ('abc', 50);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT k, count(*), sum(x) FROM t GROUP BY k",
    );
}

/// An explicit `GROUP BY` matching no rows produces zero groups — not
/// the one all-NULL row an aggregate with no `GROUP BY` would.
#[test]
fn empty_result_set_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("empty");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket, count(*) FROM t GROUP BY bucket";
    assert_sorter_grouped(&schema, sql);
    assert_eq!(our_rows(&db, &header, &schema, sql), "");
    assert_eq!(oracle_rows(&oracle, &db, sql), "");

    // Same, but with every row filtered out by a WHERE clause rather
    // than by the table being empty.
    let db2 = scratch_db("empty_where");
    seed(
        &oracle,
        &db2,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) VALUES (1, 1), (2, 2);",
    );
    let page_size2 = page_size_of(&db2);
    let header2 = read_header(&db2, page_size2);
    let schema2 = table_schema(&db2, &header2, "t");
    let sql2 = "SELECT bucket, count(*) FROM t WHERE x > 100 GROUP BY bucket";
    assert_sorter_grouped(&schema2, sql2);
    assert_eq!(our_rows(&db2, &header2, &schema2, sql2), "");
    assert_eq!(oracle_rows(&oracle, &db2, sql2), "");
}

/// `HAVING` (and `LIMIT`) run at flush time against the finalized
/// aggregates, unchanged from the sort strategy — this confirms the
/// shared `flush_group` really is shared.
#[test]
fn group_by_with_having_and_limit_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("having");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, count(*) FROM t GROUP BY bucket HAVING count(*) > 18",
    );
    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, sum(x) FROM t GROUP BY bucket HAVING sum(x) > 1800 LIMIT 3",
    );
}

/// A `WHERE`-filtered scan and a computed (non-bare-column) `GROUP BY`
/// expression — the two shapes the index-ordered fast path declines,
/// which is what makes them this path's bread and butter.
#[test]
fn where_filtered_and_computed_group_keys_match_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("where_expr");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, count(*), sum(x) FROM t WHERE x > 50 GROUP BY bucket",
    );
    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT x % 7, count(*) FROM t GROUP BY x % 7",
    );
}

/// A plain (non-aggregate, non-grouped-by) result column takes an
/// "arbitrary row" from its group — SQLite picks the group's first row,
/// and so must the hash table's retained one.
#[test]
fn plain_column_takes_the_same_arbitrary_row_as_the_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("arbitrary");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, label TEXT); \
         INSERT INTO t(bucket, label) VALUES \
           (1, 'first-1'), (2, 'first-2'), (1, 'second-1'), (2, 'second-2'), (1, 'third-1');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    check(
        &oracle,
        &db,
        &header,
        &schema,
        "SELECT bucket, label, count(*) FROM t GROUP BY bucket",
    );
}

/// A `DISTINCT` aggregate needs a per-group dedup set the hash strategy
/// does not model, so it deliberately falls back to the sorter — still
/// correct, just not hashed. Guards the narrowing documented on
/// `try_compile_hash_grouped_scan`.
#[test]
fn distinct_aggregate_falls_back_to_the_sorter_and_still_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("hash_group_by");
        return;
    };
    let db = scratch_db("distinct_agg");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         INSERT INTO t(bucket, x) VALUES \
           (1, 5), (1, 5), (1, 7), (2, 9), (2, 9), (2, 9);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket, count(DISTINCT x) FROM t GROUP BY bucket";
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        !opcodes.contains(&Opcode::HashAggOpen),
        "expected the sorter fallback for a DISTINCT aggregate, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}
