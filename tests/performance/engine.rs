//! Tier 1 (engine-to-engine) bench, per #111/#112: sqlite-rs vs libsqlite3
//! via rusqlite, both in-process, prepared statements reused, output sunk
//! into `black_box` (formatting excluded — see spec below).
//!
//! rusqlite is deliberately built without its `bundled` feature (see
//! Cargo.toml) so it links the *pinned* oracle via
//! `tools/bench_env.sh` (`SQLITE3_LIB_DIR`/`SQLITE3_INCLUDE_DIR`), not
//! whatever SQLite version it happens to vendor — [`ORACLE_VERSION`] below
//! is asserted against the linked library at the top of every benchmark
//! run. Kept as a literal (not read from Cargo.toml) for the same reason
//! `tests/corpus/oracle.rs`'s const is: this needs the value at compile
//! time. `tools/version_pin.py` holds it to the pin so it can't drift
//! silently.
//!
//! Run via `make bench` (sources `tools/bench_env.sh` first) or directly:
//! `source tools/bench_env.sh && cargo bench --bench engine`.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::Connection;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_joined, compile_select_with_catalog, resolve_from_table_schema,
};
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::ast::Select;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs};

pub const ORACLE_VERSION: &str = "3.53.4";

/// The tier-1 scenarios from #111/#112 (single-table V2) plus #301's
/// V4 join/aggregate/subquery additions, run against the fixture tables
/// (`bench_data` and, for the #301 scenarios, `bench_lookup`; see
/// `tools/gen_fixtures.sh --bench`) at every fixture size. `prepare_only`
/// measures parse+codegen (ours) / `prepare` (rusqlite) alone — no
/// execution.
const SCENARIOS: &[(&str, &str)] = &[
    ("full_scan", "SELECT id, n, x, f, s FROM bench_data"),
    (
        "point_lookup",
        "SELECT id, n, x, f, s FROM bench_data WHERE id = 4200",
    ),
    (
        "filter_scan",
        "SELECT id, n, x, f, s FROM bench_data WHERE x > 50000",
    ),
    (
        "order_by_limit",
        "SELECT id, n, x, f, s FROM bench_data ORDER BY x DESC LIMIT 100",
    ),
    // Single result column, deliberately: mixing a multi-register
    // (compound) expression with any other result column isn't yet
    // supported by this V2-scope codegen (documented in
    // src/codegen/select.rs's own error message, a real scope boundary —
    // not the separate function-call codegen bug filed as #125, which
    // this also avoids by using no function calls at all).
    (
        "expr_heavy",
        "SELECT (n + x) * 2 - (x - n) * 3 + f / 2.0 FROM bench_data",
    ),
    ("prepare_only", "SELECT id, n, x, f, s FROM bench_data"),
    // #301 (V4 phase 1 landed: JOIN/aggregate/subquery) — same fixtures,
    // plus `bench_lookup` (a fixed-size ~1000-row dimension table, see
    // `tools/gen_fixtures.sh --bench`) joined on `bench_data.bucket`.
    (
        "join",
        "SELECT bench_data.id, bench_data.x, bench_lookup.label FROM bench_data \
         JOIN bench_lookup ON bench_data.bucket = bench_lookup.code",
    ),
    (
        "group_by_agg",
        "SELECT bucket, COUNT(*), SUM(x) FROM bench_data GROUP BY bucket",
    ),
    // A scalar subquery, deliberately cheap on its own (`bench_lookup`'s
    // PK lookup, one row): #301's bench run found this codegen
    // re-executes the subquery on *every* outer row instead of caching
    // an uncorrelated result once (filed as #306) — an `IN (SELECT ...)`
    // form, or even this same scalar form matching a `code` value other
    // than the lookup table's first row, multiplies that by the
    // subquery's own linear table scan and blows the VDBE step cap on
    // the 830k-row fixture before criterion can even measure it.
    // `code = 0` is the lookup cursor's first row after `Rewind`, so the
    // per-outer-row rescan is O(1) rather than O(lookup table size) —
    // the scenario stays measurable while still surfacing the
    // per-row-reexecution ratio itself (#306).
    (
        "subquery",
        "SELECT id, x FROM bench_data WHERE bucket > (SELECT code FROM bench_lookup WHERE code = 0)",
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

struct OursFixture {
    source: Rc<dyn PageSource>,
    header: DatabaseHeader,
    catalog: Vec<TableSchema>,
}

fn open_ours(path: &Path) -> OursFixture {
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
/// exactly like `src/bin/sqlite-rs/query.rs::run_query` does — the
/// production dispatch this bench must mirror, not a bench-only
/// shortcut, so a #301 scenario measures what a user's query actually
/// runs through.
fn compile_ours(select: &Select, catalog: &[TableSchema]) -> Program {
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

fn parse_ours(sql: &str) -> Select {
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

fn bench_fixture(c: &mut Criterion, fixture_name: &str) {
    let path = fixture_path(fixture_name);
    let ours = open_ours(&path);
    let theirs = open_theirs(&path);

    for (scenario, sql) in SCENARIOS {
        let group_name = format!("{scenario}/{fixture_name}");
        let mut group = c.benchmark_group(group_name);

        if *scenario == "prepare_only" {
            group.bench_function("ours", |b| {
                b.iter(|| black_box(compile_ours(&parse_ours(sql), &ours.catalog)));
            });
            group.bench_function("oracle", |b| {
                b.iter(|| black_box(theirs.prepare(sql).unwrap_or_else(|e| fail(format!("{e}")))));
            });
            group.finish();
            continue;
        }

        let program = compile_ours(&parse_ours(sql), &ours.catalog);
        group.bench_function("ours", |b| {
            b.iter(|| {
                let rows = execute_with_db(&program, Rc::clone(&ours.source), ours.header)
                    .unwrap_or_else(|e| fail(format!("execute {sql:?}: {e}")));
                black_box(rows)
            });
        });

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
        group.finish();
    }
}

fn bench_all(c: &mut Criterion) {
    for fixture_name in ["bench_1mb.db", "bench_50mb.db"] {
        bench_fixture(c, fixture_name);
    }
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
