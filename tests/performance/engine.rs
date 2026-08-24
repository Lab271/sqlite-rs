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

use std::cell::RefCell;
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
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::ast::Select;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_transaction_step, execute_with_db, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

pub const ORACLE_VERSION: &str = "3.53.4";

/// The tier-1 scenarios from #111/#112 (single-table V2) plus #301's
/// V4 join/aggregate/subquery additions, run against the fixture tables
/// (`bench_data` and, for the #301 scenarios, `bench_lookup`; see
/// `tools/gen_fixtures.sh --bench`) at every fixture size. `prepare_only`
/// measures parse+codegen (ours) / `prepare` (rusqlite) alone — no
/// execution.
const SCENARIOS: &[(&str, &str)] = &[
    ("full_scan", "SELECT id, n, x, f, s FROM bench_data"),
    // Column-count variants (#465): isolate ResultRow's per-column cost
    // (register clone/move) from its per-row cost (Vec allocation) by
    // holding the row count fixed and varying the projected column
    // count. `full_scan` above is the 5-column ceiling of this series.
    ("full_scan_1col", "SELECT id FROM bench_data"),
    ("full_scan_3col", "SELECT id, n, x FROM bench_data"),
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
    // #303: the correlated counterpart of the `subquery` scenario above
    // — `code = bench_data.bucket` instead of a fixed literal, so the
    // inner subquery references the outer row and is genuinely
    // correlated, making it ineligible for #306's uncorrelated-subquery
    // hoist. Originally re-scanned `bench_lookup` (~1000 rows) once per
    // outer row rather than once total (ADR-0021's "materialization
    // only, no coroutines" cost) — 785x slower than the oracle (#434)
    // and unmeasurable against `bench_50mb.db` (blew the 50M-step VDBE
    // guard rail before criterion could even measure it). #434 fixed
    // this the same way the oracle itself handles this exact shape
    // (confirmed via its own `EXPLAIN`): `bench_lookup.code` is an
    // `INTEGER PRIMARY KEY`, so `code = bench_data.bucket` compiles to
    // a single `SeekRowid` point lookup
    // (`join_access::choose_join_access`, reused from #243's join-level
    // access strategy) instead of a per-outer-row full scan — no
    // caching involved, so this now runs measurably fast against both
    // fixtures.
    (
        "correlated_subquery",
        "SELECT id, x FROM bench_data \
         WHERE bucket > (SELECT code FROM bench_lookup WHERE code = bench_data.bucket)",
    ),
    // #322: an uncorrelated *aggregate* subquery (#304) in the WHERE
    // clause of an outer query that is *itself* aggregate/GROUP BY
    // (here, `count(*)` with no GROUP BY — #287's implicit whole-table
    // group). #306's hoist was wired into the plain-scan codegen
    // (`compile_direct_scan`/`compile_sorted_scan`) but not into the
    // aggregate scan (`compile_grouped_scan`), so this exact shape
    // re-ran the inner `AVG(x)` full-table scan once per WHERE-matching
    // outer row — O(n^2), severe enough to blow the 50M-step VDBE guard
    // rail before #322's fix landed.
    (
        "agg_subquery",
        "SELECT count(*) FROM bench_data WHERE x > (SELECT avg(x) FROM bench_data)",
    ),
    // #323: the `IN (SELECT ...)` counterpart of `agg_subquery` above —
    // same `compile_grouped_scan` hoist gap (#322), different subquery
    // shape. `hoist_uncorrelated_where_subqueries` already recognizes
    // `InSubquery` conjuncts, so #322's fix covers this shape too with
    // no further codegen change.
    (
        "in_subquery_agg_outer",
        "SELECT count(*) FROM bench_data \
         WHERE bucket IN (SELECT code FROM bench_lookup WHERE code < 10)",
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
        compile_select_joined(
            select,
            &joined_schemas,
            catalog,
            &std::collections::HashMap::new(),
        )
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

/// #373: transaction-batching scenarios, using `bench_1mb.db` as the
/// starting state for every iteration. Each iteration gets a fresh
/// scratch copy of the fixture (via `iter_batched`'s `setup`, excluded
/// from timing) so N iterations don't compound row growth or diverge
/// from each other — only the `BEGIN`/statement(s)/`COMMIT` sequence
/// itself is measured. Mirrors `tests/corpus/transaction_oracle_test.rs`'s
/// `run_our_session` pattern: one shared `Pager` + threaded `autocommit`
/// flag across statements, each compiled fresh via `compile_statement`
/// (dispatches BEGIN/COMMIT/INSERT/UPDATE from raw SQL, same as the CLI
/// driver).
fn scratch_copy(fixture: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sqlite-rs-bench-tx-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format!("mkdir {dir:?}: {e}")));
    let dst = dir.join("bench.db");
    std::fs::copy(fixture, &dst).unwrap_or_else(|e| fail(format!("copy fixture: {e}")));
    dst
}

fn run_our_session(pager: &Rc<RefCell<Pager>>, header: DatabaseHeader, stmts: &[String]) {
    let schemas = {
        let borrowed = pager.borrow();
        let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
        read_schema(&mut schema_cursor, header.text_encoding)
            .unwrap_or_else(|e| fail(format!("read_schema: {e}")))
    };
    let mut autocommit = true;
    for stmt in stmts {
        let program = compile_statement(stmt, &schemas, &[])
            .unwrap_or_else(|e| fail(format!("compile {stmt:?}: {e}")));
        let (rows, ac) = execute_transaction_step(&program, Rc::clone(pager), header, autocommit)
            .unwrap_or_else(|e| fail(format!("execute {stmt:?}: {e}")));
        black_box(rows);
        autocommit = ac;
    }
}

/// #436: DELETE vs WAL journal mode, paralleling `v6.rs`'s own
/// `WalMode`/`switch_to_wal` — kept as a separate copy here (rather than
/// shared across the two bench binaries) since criterion benches don't
/// share a support crate. Switching mode is a one-time setup cost, always
/// excluded from the timed closure via `iter_batched`'s `setup`.
#[derive(Clone, Copy)]
enum JournalMode {
    Delete,
    Wal,
}

impl JournalMode {
    fn suffix(self) -> &'static str {
        match self {
            JournalMode::Delete => "",
            JournalMode::Wal => "_wal",
        }
    }
}

fn switch_to_wal(pager: &Rc<RefCell<Pager>>, header: DatabaseHeader) {
    run_our_session(pager, header, &["PRAGMA journal_mode=WAL".to_string()]);
}

fn insert_stmts(n: usize) -> Vec<String> {
    let mut stmts = Vec::with_capacity(n.saturating_add(2));
    stmts.push("BEGIN".to_string());
    for i in 0..n {
        stmts.push(format!(
            "INSERT INTO bench_data(id, n, x, f, s, bucket) \
             VALUES (NULL, {i}, {i}, {i}.0, 'row{i}', {})",
            i % 100
        ));
    }
    stmts.push("COMMIT".to_string());
    stmts
}

fn header_of(path: &Path) -> DatabaseHeader {
    let (header, _pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    header
}

/// Joins `stmts` into a single `;`-separated script for
/// `rusqlite::Connection::execute_batch` — the oracle-side counterpart of
/// `run_our_session`'s per-statement loop. `execute_batch` runs the whole
/// script non-parameterized, same as our own `compile_statement` path
/// here, so both sides pay for the identical SQL text.
fn oracle_script(stmts: &[String]) -> String {
    stmts
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs `stmts` (an explicit `BEGIN`/…/`COMMIT` script) against a fresh
/// scratch copy of `fixture` on both engines, one `criterion` group per
/// scenario — mirrors `bench_fixture`'s ours/oracle pairing above and
/// `crud.rs`'s `bench_write`, but threading multiple statements through
/// one session/transaction instead of one autocommit statement.
fn bench_tx_scenario(
    c: &mut Criterion,
    label: &str,
    fixture: &Path,
    stmts: &[String],
    mode: JournalMode,
) {
    let group_name = format!("{label}{}/bench_1mb.db", mode.suffix());
    let mut group = c.benchmark_group(group_name);

    group.bench_function("ours", |b| {
        b.iter_batched(
            || {
                let path = scratch_copy(fixture);
                let header = header_of(&path);
                let pager = Rc::new(RefCell::new(
                    Pager::open(&UnixVfs, &path, header.page_size)
                        .unwrap_or_else(|e| fail(format!("open {path:?}: {e}"))),
                ));
                if matches!(mode, JournalMode::Wal) {
                    switch_to_wal(&pager, header);
                }
                (path, header, pager)
            },
            |(path, header, pager)| {
                run_our_session(&pager, header, stmts);
                drop(pager);
                if let Some(dir) = path.parent() {
                    std::fs::remove_dir_all(dir).unwrap_or(());
                }
            },
            BatchSize::LargeInput,
        );
    });

    let script = oracle_script(stmts);
    group.bench_function("oracle", |b| {
        b.iter_batched(
            || {
                let path = scratch_copy(fixture);
                let conn = open_theirs(&path);
                if matches!(mode, JournalMode::Wal) {
                    conn.execute_batch("PRAGMA journal_mode=WAL;")
                        .unwrap_or_else(|e| fail(format!("oracle PRAGMA journal_mode=WAL: {e}")));
                }
                (path, conn)
            },
            |(path, conn)| {
                conn.execute_batch(&script)
                    .unwrap_or_else(|e| fail(format!("oracle execute_batch {script:?}: {e}")));
                drop(conn);
                if let Some(dir) = path.parent() {
                    std::fs::remove_dir_all(dir).unwrap_or(());
                }
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

fn bench_transactions(c: &mut Criterion) {
    let fixture = fixture_path("bench_1mb.db");
    let update_stmts = [
        "BEGIN".to_string(),
        "UPDATE bench_data SET x = x + 1 WHERE bucket = 5".to_string(),
        "COMMIT".to_string(),
    ];
    for mode in [JournalMode::Delete, JournalMode::Wal] {
        bench_tx_scenario(c, "insert_single_tx", &fixture, &insert_stmts(1), mode);
        bench_tx_scenario(c, "insert_batch_tx_100", &fixture, &insert_stmts(100), mode);
        bench_tx_scenario(
            c,
            "insert_batch_tx_1000",
            &fixture,
            &insert_stmts(1000),
            mode,
        );
        bench_tx_scenario(c, "update_batch_tx", &fixture, &update_stmts, mode);
    }
}

/// #469: micro-benchmark for the `Payload::Owned` overflow-chain
/// reassembly path #467 introduced (multi-page-overflow rows can't
/// borrow — there's no single page range to point into — so this is the
/// one payload-decoding path that still allocates and copies), so a
/// regression reintroducing extra allocation or a slower reassembly walk
/// is measurable. Deliberately bypasses the full SQL/VDBE `SCENARIOS`
/// pipeline above: none of its bench-scale fixtures (`tools/gen_fixtures.sh
/// --bench`) have an overflow-forcing column, and adding one is a bigger
/// change than this test-only ticket's scope — this instead drives
/// `TableCursor` directly against the small, already-committed
/// `overflow_multi_page.db` corpus fixture (`src/btree.rs`'s own
/// `overflow_multi_page_payload_is_byte_identical_to_oracle` test uses the
/// same file), timing exactly the `first_row` -> `reassemble_payload`
/// walk across its 14-page overflow chain.
fn bench_overflow_payload(c: &mut Criterion) {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/corpus/fixtures/btrees/overflow_multi_page.db"
    ));
    let vfs = UnixVfs;
    let file = vfs
        .open_read(path)
        .unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0)
        .unwrap_or_else(|e| fail(format!("read header {path:?}: {e}")));
    let header = DatabaseHeader::parse(&header_buf)
        .unwrap_or_else(|e| fail(format!("parse header {path:?}: {e}")));

    let mut group = c.benchmark_group("overflow_payload_reassembly");
    group.bench_function("multi_page", |b| {
        b.iter(|| {
            let source = VfsPageSource::open(&vfs, path, header.page_size)
                .unwrap_or_else(|e| fail(format!("open page source {path:?}: {e}")));
            // Root page 2, matching `src/btree.rs`'s `open_cursor` test
            // helper — these corpus fixtures are single-table synthetic
            // b-trees, not full databases with a `sqlite_master` catalog.
            let mut cursor = TableCursor::new(source, &header, 2);
            let row = cursor
                .first_row()
                .unwrap_or_else(|e| fail(format!("first_row: {e}")))
                .unwrap_or_else(|| fail("expected a row in overflow_multi_page.db"));
            black_box(row);
        });
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    for fixture_name in ["bench_1mb.db", "bench_50mb.db"] {
        bench_fixture(c, fixture_name);
    }
    bench_transactions(c);
    bench_overflow_payload(c);
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
