//! #485 phase 4: an internal, relative benchmark (sqlite-rs vs
//! sqlite-rs, not vs oracle — same shape as `v6.rs`'s
//! `bench_cte_reuse_10x`) comparing a skip-scan plan against the plain
//! full table scan it replaces, at a low-cardinality leading-column
//! composite index. Builds its own scratch fixture via rusqlite (linked
//! against the pinned oracle, same as `engine.rs`/`v6.rs`) rather than
//! extending the shared `tools/gen_fixtures.sh --bench` fixtures, since
//! this scenario's schema (a composite index with a specific
//! leading-column cardinality) is narrow to this one benchmark.
//!
//! `src/codegen/select/limit_scan.rs`'s `try_compile_skip_scan_index`
//! doc comment already flags the honest expectation here:
//! `IndexCursor::seek` (`src/btree/index.rs`) is a documented Tier 0
//! linear scan, not a real B-tree binary descent, so this skip-scan
//! walks every index entry rather than truly skipping past a large
//! group once it stops matching. The measured win, if any, comes from
//! decoding narrower index rows and only touching the table for
//! genuine matches — not from sub-linear seeking the way real SQLite's
//! skip-scan wins. Report the number as measured, not as a claimed
//! parity ratio with oracle sqlite3's own (structurally different)
//! skip-scan implementation.
//!
//! Run via `make bench-skip-scan` (sources `tools/bench_env.sh` first)
//! or directly: `source tools/bench_env.sh && cargo bench --bench skip_scan`.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use criterion::{criterion_group, criterion_main, Criterion};
use rusqlite::Connection;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_with_catalog, compile_select_with_catalog_and_stats};
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::planner::{load_stats, Stats};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs};

/// Same fail-loudly-not-panic shape as `engine.rs`/`v6.rs`'s `fail` — a
/// bare `panic!` is denied crate-wide by `Cargo.toml`'s
/// `[lints.clippy]`.
fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("bench error: {msg}");
    std::process::exit(1);
}

fn build_fixture() -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-bench-skip-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format!("mkdir {dir:?}: {e}")));
    let path = dir.join("fixture.db");
    let conn = Connection::open(&path).unwrap_or_else(|e| fail(format!("rusqlite open: {e}")));
    conn.execute_batch(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, category TEXT, price INTEGER); \
         CREATE INDEX idx ON t(category, price); \
         INSERT INTO t(category, price) \
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 200000) \
         SELECT 'cat' || (x % 3), x FROM cnt; \
         ANALYZE;",
    )
    .unwrap_or_else(|e| fail(format!("rusqlite seed: {e}")));
    drop(conn);
    path
}

fn table_schema_and_stats(path: &Path) -> (TableSchema, Stats) {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    let source: Rc<dyn PageSource> = Rc::new(pager);
    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let catalog = read_schema(&mut schema_cursor, header.text_encoding)
        .unwrap_or_else(|e| fail(format!("read_schema {path:?}: {e}")));
    let schema = catalog
        .iter()
        .find(|s| s.name == "t")
        .unwrap_or_else(|| fail("no schema for table t"))
        .clone();
    let stats_by_table = load_stats(Rc::clone(&source), &header, &catalog);
    let stats = stats_by_table.get("t").cloned().unwrap_or_default();
    (schema, stats)
}

fn compile(sql: &str, schema: &TableSchema, stats: &Stats) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => fail(format!("bench SQL failed to parse: {sql:?}: {other:?}")),
    };
    compile_select_with_catalog_and_stats(&select, schema, std::slice::from_ref(schema), stats)
        .unwrap_or_else(|e| fail(format!("compile {sql:?}: {e}")))
}

fn compile_without_stats(sql: &str, schema: &TableSchema) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => fail(format!("bench SQL failed to parse: {sql:?}: {other:?}")),
    };
    compile_select_with_catalog(&select, schema, std::slice::from_ref(schema))
        .unwrap_or_else(|e| fail(format!("compile {sql:?}: {e}")))
}

fn open_source(path: &Path) -> (Rc<dyn PageSource>, DatabaseHeader) {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    (Rc::new(pager), header)
}

/// `full_scan` forces the pre-#485 plan (`Stats::default()`, matching
/// every caller before this ticket) by compiling with
/// `compile_select_with_catalog` directly rather than the stats-aware
/// entrypoint. `skip_scan` compiles the exact same SQL with this
/// fixture's real `ANALYZE`-derived stats, taking #485's new dispatch
/// branch. Both walk the identical on-disk data — the only variable is
/// which plan gets chosen.
fn bench_skip_scan_vs_full_scan(c: &mut Criterion) {
    let path = build_fixture();
    let (schema, stats) = table_schema_and_stats(&path);
    let sql = "SELECT id, price FROM t WHERE price = 100000";

    let full_scan_program = compile_without_stats(sql, &schema);
    let skip_scan_program = compile(sql, &schema, &stats);
    assert!(
        !full_scan_program
            .instructions
            .iter()
            .any(|i| i.opcode == sqlite_rs::vdbe::Opcode::IdxRewind),
        "expected full_scan_program to NOT use IdxRewind"
    );
    assert!(
        skip_scan_program
            .instructions
            .iter()
            .any(|i| i.opcode == sqlite_rs::vdbe::Opcode::IdxRewind),
        "expected skip_scan_program to use IdxRewind"
    );

    let mut group = c.benchmark_group("skip_scan/low_cardinality_leading_column");

    group.bench_function("full_scan", |b| {
        let (source, header) = open_source(&path);
        b.iter(|| {
            let rows = execute_with_db(&full_scan_program, Rc::clone(&source), header)
                .unwrap_or_else(|e| fail(format!("execute full_scan: {e}")));
            black_box(rows)
        });
    });

    group.bench_function("skip_scan", |b| {
        let (source, header) = open_source(&path);
        b.iter(|| {
            let rows = execute_with_db(&skip_scan_program, Rc::clone(&source), header)
                .unwrap_or_else(|e| fail(format!("execute skip_scan: {e}")));
            black_box(rows)
        });
    });

    group.finish();
}

fn bench_all(c: &mut Criterion) {
    bench_skip_scan_vs_full_scan(c);
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
