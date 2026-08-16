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
use sqlite_rs::codegen::compile_select;
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs};

pub const ORACLE_VERSION: &str = "3.53.4";

/// The six tier-1 scenarios from #111/#112, run against the same
/// `bench_data` table (see `tools/gen_fixtures.sh --bench`) at every
/// fixture size. `prepare_only` measures parse+codegen (ours) / `prepare`
/// (rusqlite) alone — no execution.
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
    schema: TableSchema,
}

fn open_ours(path: &Path) -> OursFixture {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let schemas = read_schema(&mut schema_cursor, header.text_encoding)
        .unwrap_or_else(|e| fail(format!("read_schema {path:?}: {e}")));
    let schema = schemas
        .into_iter()
        .find(|s| s.name == "bench_data")
        .unwrap_or_else(|| fail(format!("{path:?}: no bench_data table")));

    OursFixture {
        source,
        header,
        schema,
    }
}

fn compile_ours(schema: &TableSchema, sql: &str) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => fail(format!("bench SQL failed to parse: {sql:?}: {other:?}")),
    };
    compile_select(&select, schema).unwrap_or_else(|e| fail(format!("compile {sql:?}: {e}")))
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
                b.iter(|| black_box(compile_ours(&ours.schema, sql)));
            });
            group.bench_function("oracle", |b| {
                b.iter(|| black_box(theirs.prepare(sql).unwrap_or_else(|e| fail(format!("{e}")))));
            });
            group.finish();
            continue;
        }

        let program = compile_ours(&ours.schema, sql);
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
