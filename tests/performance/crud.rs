//! Tier 1 (engine-to-engine) CRUD bench: sqlite-rs vs libsqlite3 (via
//! rusqlite) on 15 scenarios spanning Create/Read/Update/Delete at
//! varying complexity (single-row/PK vs range/multi-row vs indexed-column
//! vs join/aggregate), so it's clear per scenario what SQL ran and how
//! ours compares to the oracle on it.
//!
//! Reads run against `bench_1mb.db` read-only, same as `engine.rs`.
//! Writes (INSERT/UPDATE/DELETE) mutate the database file, so each
//! write iteration gets its own fresh copy of the fixture via
//! `Criterion::iter_batched` — otherwise a repeated DELETE would run out
//! of rows, and a repeated INSERT would grow the file across iterations,
//! skewing later iterations relative to earlier ones. Both `ours` and
//! `oracle` pay the same fresh-copy setup cost, so the comparison stays
//! apples-to-apples; only `bench_1mb.db` is used for writes (not the
//! 50MB fixture) to keep the per-iteration copy cheap.
//!
//! Run via `make -C tests/performance crud` (needs `make fixtures-bench`
//! and `tools/bench_env.sh` first, same prerequisites as `engine.rs`) or
//! directly: `source tools/bench_env.sh && cargo bench --bench crud`.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use rusqlite::Connection;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_joined, compile_select_with_catalog, compile_statement,
    resolve_from_table_schema,
};
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::ast::Select;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, execute_with_writable_db, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs};

pub const ORACLE_VERSION: &str = "3.53.4";

/// Statement kind, so `bench_scenario` knows whether it needs read-only
/// setup (once) or a fresh per-iteration copy (writes).
#[derive(Clone, Copy)]
enum Kind {
    Read,
    Write,
}

/// 15 scenarios: 5 reads (PK/indexed-range/full-scan/join/aggregate),
/// 3 inserts (single row, batch of 10, no-explicit-PK), 4 updates
/// (PK, filtered range, indexed-column, multi-column SET), 3 deletes
/// (PK, filtered range, equality on a non-indexed column) — the same
/// "how does complexity scale" spread as `engine.rs`'s SELECT
/// scenarios, extended to the write path.
const SCENARIOS: &[(&str, Kind, &str)] = &[
    (
        "read_pk",
        Kind::Read,
        "SELECT id, n, x, f, s FROM bench_data WHERE id = 4200",
    ),
    (
        "read_indexed_range",
        Kind::Read,
        "SELECT id, n, x, f, s FROM bench_data WHERE x > 50000",
    ),
    (
        "read_full_scan",
        Kind::Read,
        "SELECT id, n, x, f, s FROM bench_data",
    ),
    (
        "read_join",
        Kind::Read,
        "SELECT bench_data.id, bench_data.x, bench_lookup.label FROM bench_data \
         JOIN bench_lookup ON bench_data.bucket = bench_lookup.code",
    ),
    (
        "read_group_by_agg",
        Kind::Read,
        "SELECT bucket, COUNT(*), SUM(x) FROM bench_data GROUP BY bucket",
    ),
    (
        "insert_single",
        Kind::Write,
        "INSERT INTO bench_data(id, n, x, f, s, bucket) \
         VALUES (900001, 1, 2, 3.5, 'benched-row', 7)",
    ),
    (
        "insert_batch_10",
        Kind::Write,
        "INSERT INTO bench_data(id, n, x, f, s, bucket) VALUES \
         (900001, 1, 2, 3.5, 'row-0', 7), (900002, 1, 2, 3.5, 'row-1', 7), \
         (900003, 1, 2, 3.5, 'row-2', 7), (900004, 1, 2, 3.5, 'row-3', 7), \
         (900005, 1, 2, 3.5, 'row-4', 7), (900006, 1, 2, 3.5, 'row-5', 7), \
         (900007, 1, 2, 3.5, 'row-6', 7), (900008, 1, 2, 3.5, 'row-7', 7), \
         (900009, 1, 2, 3.5, 'row-8', 7), (900010, 1, 2, 3.5, 'row-9', 7)",
    ),
    (
        "insert_no_explicit_pk",
        Kind::Write,
        "INSERT INTO bench_data(n, x, f, s, bucket) VALUES (1, 2, 3.5, 'auto-rowid', 7)",
    ),
    (
        "update_pk",
        Kind::Write,
        "UPDATE bench_data SET n = n + 1 WHERE id = 4200",
    ),
    (
        "update_filtered_range",
        Kind::Write,
        "UPDATE bench_data SET n = n + 1 WHERE x > 50000",
    ),
    (
        "update_indexed_column",
        Kind::Write,
        "UPDATE bench_data SET x = x + 1 WHERE id = 4200",
    ),
    (
        "update_multi_column",
        Kind::Write,
        "UPDATE bench_data SET n = n + 1, s = 'updated' WHERE id = 4200",
    ),
    (
        "delete_pk",
        Kind::Write,
        "DELETE FROM bench_data WHERE id = 4200",
    ),
    (
        "delete_filtered_range",
        Kind::Write,
        "DELETE FROM bench_data WHERE x > 90000",
    ),
    (
        "delete_equality_bucket",
        Kind::Write,
        "DELETE FROM bench_data WHERE bucket = 500",
    ),
];

/// Aborts the bench run on a setup/execution failure. A bare `panic!` is
/// denied crate-wide (`Cargo.toml`'s `[lints.clippy]`); this is the
/// same "fail loudly, no swallowed error" behavior without it.
fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("bench error: {msg}");
    std::process::exit(1);
}

fn fixture_path(name: &str) -> PathBuf {
    let dir = std::env::var("BENCH_FIXTURES_DIR").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/target/bench-fixtures").to_string()
    });
    Path::new(&dir).join(name)
}

/// Unique scratch path for one write iteration's fixture copy, under
/// `target/` so it's gitignored and cleaned by `cargo clean`.
fn scratch_copy_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-fixtures/tmp");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format!("mkdir {dir:?}: {e}")));
    dir.join(format!("crud-{tag}-{}-{n}.db", std::process::id()))
}

struct OursFixture {
    source: Rc<dyn PageSource>,
    header: DatabaseHeader,
    catalog: Vec<TableSchema>,
}

fn open_ours_readonly(path: &Path) -> OursFixture {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let catalog = read_schema(&mut schema_cursor, header.text_encoding)
        .unwrap_or_else(|e| fail(format!("read_schema {path:?}: {e}")));
    if !catalog.iter().any(|s| s.name == "bench_data") {
        fail(format!("{path:?}: no bench_data table"));
    }

    OursFixture {
        source,
        header,
        catalog,
    }
}

/// Compiles `sql` against `catalog`, dispatching single-table vs `JOIN`
/// exactly like `src/bin/sqlite-rs/query.rs::run_query` does.
fn compile_select(select: &Select, catalog: &[TableSchema]) -> Program {
    let from = select
        .from
        .as_ref()
        .unwrap_or_else(|| fail("bench SQL has no FROM clause"));
    let resolve = |table_ref: &sqlite_rs::parser::ast::TableRef| {
        resolve_from_table_schema(table_ref, catalog)
            .unwrap_or_else(|e| fail(format!("resolve table {table_ref:?}: {e}")))
    };
    let schema = resolve(&from.first);
    if from.joins.is_empty() {
        compile_select_with_catalog(select, &schema, catalog)
            .unwrap_or_else(|e| fail(format!("compile {select:?}: {e}")))
    } else {
        let mut joined_schemas = vec![schema];
        joined_schemas.extend(from.joins.iter().map(|j| resolve(&j.table)));
        compile_select_joined(select, &joined_schemas, catalog)
            .unwrap_or_else(|e| fail(format!("compile {select:?}: {e}")))
    }
}

fn parse_select_sql(sql: &str) -> Select {
    match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => fail(format!("bench SQL failed to parse: {sql:?}: {other:?}")),
    }
}

fn open_theirs(path: &Path) -> Connection {
    let linked = rusqlite::version();
    if !linked.starts_with(ORACLE_VERSION) {
        fail(format!(
            "rusqlite linked against sqlite3 {linked}, expected the pinned oracle \
             {ORACLE_VERSION} — did you `source tools/bench_env.sh` before building?"
        ));
    }
    Connection::open(path).unwrap_or_else(|e| fail(format!("rusqlite open {path:?}: {e}")))
}

fn bench_read(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    sql: &str,
    ours: &OursFixture,
    fixture_path: &Path,
) {
    let program = compile_select(&parse_select_sql(sql), &ours.catalog);
    group.bench_function("ours", |b| {
        b.iter(|| {
            let rows = execute_with_db(&program, Rc::clone(&ours.source), ours.header)
                .unwrap_or_else(|e| fail(format!("execute {sql:?}: {e}")));
            black_box(rows)
        });
    });

    let theirs = open_theirs(fixture_path);
    let mut stmt = theirs
        .prepare(sql)
        .unwrap_or_else(|e| fail(format!("rusqlite prepare {sql:?}: {e}")));
    let column_count = stmt.column_count();
    group.bench_function("oracle", |b| {
        b.iter(|| {
            let rows: Vec<Vec<rusqlite::types::Value>> = stmt
                .query_map([], |row| {
                    (0..column_count)
                        .map(|i| row.get::<_, rusqlite::types::Value>(i))
                        .collect()
                })
                .unwrap_or_else(|e| fail(format!("query {sql:?}: {e}")))
                .collect::<Result<_, _>>()
                .unwrap_or_else(|e| fail(format!("row {sql:?}: {e}")));
            black_box(rows)
        });
    });
}

fn bench_write(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    scenario: &str,
    sql: &str,
    catalog: &[TableSchema],
    fixture_path: &Path,
) {
    let program =
        compile_statement(sql, catalog).unwrap_or_else(|e| fail(format!("compile {sql:?}: {e}")));

    group.bench_function("ours", |b| {
        b.iter_batched(
            || {
                let tmp = scratch_copy_path(&format!("{scenario}-ours"));
                std::fs::copy(fixture_path, &tmp)
                    .unwrap_or_else(|e| fail(format!("copy fixture to {tmp:?}: {e}")));
                let (header, pager) = dump::open(&UnixVfs, &tmp)
                    .unwrap_or_else(|e| fail(format!("open {tmp:?}: {e}")));
                (tmp, header, pager)
            },
            |(tmp, header, pager)| {
                let rows = execute_with_writable_db(&program, pager, header)
                    .unwrap_or_else(|e| fail(format!("execute {sql:?}: {e}")));
                std::fs::remove_file(&tmp).ok();
                black_box(rows)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("oracle", |b| {
        b.iter_batched(
            || {
                let tmp = scratch_copy_path(&format!("{scenario}-oracle"));
                std::fs::copy(fixture_path, &tmp)
                    .unwrap_or_else(|e| fail(format!("copy fixture to {tmp:?}: {e}")));
                let conn = open_theirs(&tmp);
                (tmp, conn)
            },
            |(tmp, conn)| {
                let changes = conn
                    .execute(sql, [])
                    .unwrap_or_else(|e| fail(format!("rusqlite execute {sql:?}: {e}")));
                std::fs::remove_file(&tmp).ok();
                black_box(changes)
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_all(c: &mut Criterion) {
    let fixture_name = "bench_1mb.db";
    let path = fixture_path(fixture_name);
    let ours = open_ours_readonly(&path);

    for (scenario, kind, sql) in SCENARIOS {
        let group_name = format!("{scenario}/{fixture_name}");
        let mut group = c.benchmark_group(group_name);
        match kind {
            Kind::Read => bench_read(&mut group, sql, &ours, &path),
            Kind::Write => bench_write(&mut group, scenario, sql, &ours.catalog, &path),
        }
        group.finish();
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
