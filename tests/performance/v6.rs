//! V6 (epic #354) benchmarks (#391): four scenarios from the ticket,
//! adapted to what this codebase can actually measure honestly today —
//! see each `bench_*` function's own doc comment for what changed from
//! the ticket's literal wording and why.
//!
//! Unlike `engine.rs`/`crud.rs` (sqlite-rs vs the pinned oracle on the
//! *same* workload), three of these four benchmarks are internal,
//! relative comparisons — journal mode vs WAL mode, both sqlite-rs, or
//! CTE vs inline subquery, both sqlite-rs — matching the ticket's own
//! "Expected Results" table (Journal (V5) vs WAL (V6), no oracle
//! column). `insert_batch_wal` additionally reports the oracle's own
//! journal-vs-WAL numbers as a sanity check, since the pinned-oracle
//! harness (`tools/bench_env.sh`) already exists and a real `sqlite3`
//! WAL-vs-rollback-journal comparison is a natural one to have alongside
//! ours.
//!
//! Run via `make bench-v6` (sources `tools/bench_env.sh` first) or
//! directly: `source tools/bench_env.sh && cargo bench --bench v6`.

use std::cell::RefCell;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use rusqlite::Connection;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_joined, compile_select_with_catalog, compile_statement, expand_with_clause,
    resolve_from_table_schema,
};
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::checkpoint::checkpoint_passive;
use sqlite_rs::pager::wal::{WalHeader, WalWriter};
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::ast::{Select, TableRef};
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_transaction_step, execute_with_db, Program};
use sqlite_rs::vfs::{companion_path, AnyVfs, PageSource, UnixVfs};

/// Same literal-not-imported-from-Cargo.toml shape as `engine.rs`'s own
/// `ORACLE_VERSION`: needed at compile time (`starts_with` below), and
/// held to the pin by `tools/version_pin.py` so it can't drift silently.
pub const ORACLE_VERSION: &str = "3.53.4";

/// Aborts the bench run on a setup/execution failure — same
/// fail-loudly-not-panic shape as `engine.rs::fail` (a bare `panic!` is
/// denied crate-wide by `Cargo.toml`'s `[lints.clippy]`).
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

fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-bench-v6-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format!("mkdir {dir:?}: {e}")));
    dir
}

/// A fresh scratch copy of `fixture`, in its own directory (so cleanup
/// is one `remove_dir_all`) — mirrors `engine.rs::scratch_copy`.
fn scratch_copy(fixture: &Path) -> PathBuf {
    let dir = scratch_dir("tx");
    let dst = dir.join("bench.db");
    std::fs::copy(fixture, &dst).unwrap_or_else(|e| fail(format!("copy fixture: {e}")));
    dst
}

fn cleanup_scratch(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::remove_dir_all(dir).unwrap_or(());
    }
}

fn header_of(path: &Path) -> DatabaseHeader {
    let (header, _pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| fail(format!("open {path:?}: {e}")));
    header
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

fn oracle_script(stmts: &[String]) -> String {
    stmts
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads `bench_data`'s schema off an already-open `pager` — used to
/// compile further statements against it without re-opening the file.
fn read_schemas(pager: &Rc<RefCell<Pager>>, header: DatabaseHeader) -> Vec<TableSchema> {
    let borrowed = pager.borrow();
    let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
    read_schema(&mut schema_cursor, header.text_encoding)
        .unwrap_or_else(|e| fail(format!("read_schema: {e}")))
}

fn run_stmt(
    pager: &Rc<RefCell<Pager>>,
    header: DatabaseHeader,
    schemas: &[TableSchema],
    sql: &str,
    autocommit: bool,
) -> bool {
    let program = compile_statement(sql, schemas, &[])
        .unwrap_or_else(|e| fail(format!("compile {sql:?}: {e}")));
    let (rows, ac) = execute_transaction_step(&program, Rc::clone(pager), header, autocommit)
        .unwrap_or_else(|e| fail(format!("execute {sql:?}: {e}")));
    black_box(rows);
    ac
}

/// Switches `pager` to `journal_mode=WAL` through the real SQL entry
/// point (`compile_statement`/`execute_transaction_step`) — the same
/// machinery #390 proved drives `PRAGMA`/`INSERT` end to end through
/// `Pager::flush`'s WAL path, not `Pager::set_journal_mode` called
/// directly. A one-time setup cost, always excluded from a benchmark's
/// timed closure.
fn switch_to_wal(pager: &Rc<RefCell<Pager>>, header: DatabaseHeader) {
    run_stmt(pager, header, &[], "PRAGMA journal_mode=WAL", true);
}

fn run_session(
    pager: &Rc<RefCell<Pager>>,
    header: DatabaseHeader,
    schemas: &[TableSchema],
    stmts: &[String],
) {
    let mut autocommit = true;
    for stmt in stmts {
        autocommit = run_stmt(pager, header, schemas, stmt, autocommit);
    }
}

fn insert_batch_stmts(n: usize) -> Vec<String> {
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

// ---------------------------------------------------------------------
// 1. insert_batch_wal — same batch-INSERT workload the ticket's own
//    `insert_batch_1000` example names, run through the real SQL-level
//    path (`compile_statement`/`execute_transaction_step`, #390's
//    confirmed entry point) once in default (rollback-journal) mode and
//    once after an SQL-level `PRAGMA journal_mode=WAL` switch (excluded
//    from the timed closure — a one-time mode-switch cost, not part of
//    steady-state commit cost). Per ADR-0026, the WAL path rescans the
//    whole `-wal` file on every flush, so this number reflects that
//    real per-flush cost, not a hypothetical cached-writer design.
// ---------------------------------------------------------------------

#[derive(Clone, Copy)]
enum WalMode {
    Journal,
    Wal,
}

impl WalMode {
    fn label(self) -> &'static str {
        match self {
            WalMode::Journal => "journal",
            WalMode::Wal => "wal",
        }
    }
}

fn bench_insert_batch_scenario(c: &mut Criterion, fixture: &Path, stmts: &[String], mode: WalMode) {
    let group_name = format!("insert_batch_wal_{}/bench_1mb.db", mode.label());
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
                if matches!(mode, WalMode::Wal) {
                    switch_to_wal(&pager, header);
                }
                let schemas = read_schemas(&pager, header);
                (path, header, pager, schemas)
            },
            |(path, header, pager, schemas)| {
                run_session(&pager, header, &schemas, stmts);
                drop(pager);
                cleanup_scratch(&path);
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
                if matches!(mode, WalMode::Wal) {
                    conn.execute_batch("PRAGMA journal_mode=WAL;")
                        .unwrap_or_else(|e| fail(format!("oracle PRAGMA journal_mode=WAL: {e}")));
                }
                (path, conn)
            },
            |(path, conn)| {
                conn.execute_batch(&script)
                    .unwrap_or_else(|e| fail(format!("oracle execute_batch {script:?}: {e}")));
                drop(conn);
                cleanup_scratch(&path);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

fn bench_insert_batch_wal(c: &mut Criterion) {
    let fixture = fixture_path("bench_1mb.db");
    let stmts = insert_batch_stmts(1000);
    bench_insert_batch_scenario(c, &fixture, &stmts, WalMode::Journal);
    bench_insert_batch_scenario(c, &fixture, &stmts, WalMode::Wal);
}

// ---------------------------------------------------------------------
// 2. concurrent_read_write — honesty note: this is a single-process
//    Criterion micro-bench, not a real multi-threaded/multi-process
//    harness (#390's `tests/corpus/wal_concurrent_interop_test.rs`
//    already proves the actual non-blocking property against a live
//    second `sqlite3` process; this bench's job is throughput/latency
//    numbers, not re-proving correctness). It measures a deterministic
//    *sequential* interleaving: a reader `Pager` opened once (pinning a
//    WAL snapshot per #389/ADR-0026) alongside a writer `Pager` that
//    commits `CYCLES` single-row transactions, with one reader scan
//    after every writer commit. This is two sequential costs measured
//    back to back, not genuine wall-clock parallelism — it does *not*
//    claim to prove the writer never blocks the reader (or vice versa),
//    only to report what each costs when interleaved this way in WAL
//    mode, including any per-flush cost growth as the WAL file grows
//    (ADR-0026's reopen-and-rescan design).
// ---------------------------------------------------------------------

const CONCURRENT_CYCLES: usize = 20;

fn bench_concurrent_read_write(c: &mut Criterion) {
    let fixture = fixture_path("bench_1mb.db");
    let group_name = "concurrent_read_write/bench_1mb.db";
    let mut group = c.benchmark_group(group_name);

    group.bench_function("writer_commit_then_reader_scan_x20", |b| {
        b.iter_batched(
            || {
                let path = scratch_copy(&fixture);
                let header = header_of(&path);
                let writer = Rc::new(RefCell::new(
                    Pager::open(&UnixVfs, &path, header.page_size)
                        .unwrap_or_else(|e| fail(format!("open writer {path:?}: {e}"))),
                ));
                switch_to_wal(&writer, header);
                let schemas = read_schemas(&writer, header);

                // The reader opens *after* the WAL switch (so there is a
                // WAL-mode database to read) but *before* any of the
                // timed writer commits below — its snapshot is pinned
                // right here (#389/ADR-0026's snapshot isolation), so
                // every scan in the timed closure reads that same
                // pre-write view, never the writer's new rows.
                let reader = Rc::new(RefCell::new(
                    Pager::open(&UnixVfs, &path, header.page_size)
                        .unwrap_or_else(|e| fail(format!("open reader {path:?}: {e}"))),
                ));

                let select = match parse_select("SELECT id, n, x, f, s FROM bench_data") {
                    ParseOutcome::Accepted(select) => *select,
                    other => fail(format!("bench SQL failed to parse: {other:?}")),
                };
                let bench_data_schema = schemas
                    .iter()
                    .find(|s| s.name == "bench_data")
                    .cloned()
                    .unwrap_or_else(|| fail("no bench_data table in fixture"));
                let read_program =
                    compile_select_with_catalog(&select, &bench_data_schema, &schemas)
                        .unwrap_or_else(|e| fail(format!("compile read query: {e}")));

                (path, header, writer, reader, schemas, read_program)
            },
            |(path, header, writer, reader, schemas, read_program)| {
                for i in 0..CONCURRENT_CYCLES {
                    let insert = format!(
                        "INSERT INTO bench_data(id, n, x, f, s, bucket) \
                         VALUES (NULL, {i}, {i}, {i}.0, 'row{i}', {})",
                        i % 100
                    );
                    run_stmt(&writer, header, &schemas, "BEGIN", true);
                    run_stmt(&writer, header, &schemas, &insert, false);
                    run_stmt(&writer, header, &schemas, "COMMIT", false);

                    let read_source: Rc<dyn PageSource> = reader.clone();
                    let rows = execute_with_db(&read_program, Rc::clone(&read_source), header)
                        .unwrap_or_else(|e| fail(format!("reader scan: {e}")));
                    black_box(rows);
                }
                drop(writer);
                drop(reader);
                cleanup_scratch(&path);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------
// 3. checkpoint_10mb — time to `checkpoint_passive` a WAL file holding
//    ~10MB of accumulated frames. Built directly with `WalWriter`
//    (rather than via #388/#389's SQL-level path) so the frame count is
//    exact and the benchmark measures `checkpoint_passive` itself, not
//    however many rows a real INSERT workload happens to dirty per
//    page — the frame-header size (24 bytes) is SQLite's fixed WAL wire
//    format, hardcoded here the same way `tools/gen_fixtures.sh`'s WAL
//    parsing snippet already does, since `wal::FRAME_HEADER_LEN` is
//    `pub(crate)` and not visible from this external test crate. Page
//    size (4096) matches `bench_1mb.db`/`bench_50mb.db`'s own default
//    (SQLite's modern default, unset by `tools/gen_fixtures.sh --bench`).
//    All frames form a single ~10MB commit (only the last frame carries
//    a non-zero commit size) so `checkpoint_passive` backfills the
//    whole thing in one pass — no `-shm` file is created, so there are
//    no active readers to bound it.
// ---------------------------------------------------------------------

const CHECKPOINT_PAGE_SIZE: u32 = 4096;
const WAL_FRAME_HEADER_LEN: usize = 24;
const CHECKPOINT_TARGET_BYTES: usize = 10 * 1024 * 1024;

fn build_10mb_wal() -> (AnyVfs, PathBuf, PathBuf) {
    let frame_size = WAL_FRAME_HEADER_LEN.saturating_add(CHECKPOINT_PAGE_SIZE as usize);
    let frame_count = (CHECKPOINT_TARGET_BYTES / frame_size) as u32;

    let dir = scratch_dir("checkpoint");
    let db_path = dir.join("checkpoint.db");
    std::fs::write(&db_path, vec![0u8; CHECKPOINT_PAGE_SIZE as usize])
        .unwrap_or_else(|e| fail(format!("write {db_path:?}: {e}")));

    let vfs = AnyVfs::new(UnixVfs);
    let wal_path = companion_path(&db_path, "-wal");
    let header = WalHeader::new(true, CHECKPOINT_PAGE_SIZE, 0xC0FF_EE01, 0xC0FF_EE02, 1);
    let mut writer = WalWriter::create(&vfs, &wal_path, header)
        .unwrap_or_else(|e| fail(format!("WalWriter::create {wal_path:?}: {e}")));

    for page_num in 1..=frame_count {
        let page_data = vec![(page_num % 251) as u8; CHECKPOINT_PAGE_SIZE as usize];
        let commit_size = if page_num == frame_count {
            frame_count
        } else {
            0
        };
        writer
            .append_frame(page_num, &page_data, commit_size)
            .unwrap_or_else(|e| fail(format!("append_frame {page_num}: {e}")));
    }
    writer
        .sync()
        .unwrap_or_else(|e| fail(format!("WalWriter::sync: {e}")));

    (vfs, db_path, dir)
}

fn bench_checkpoint_10mb(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_10mb");
    group.bench_function("checkpoint_passive", |b| {
        b.iter_batched(
            build_10mb_wal,
            |(vfs, db_path, dir)| {
                let result = checkpoint_passive(&vfs, &db_path, CHECKPOINT_PAGE_SIZE)
                    .unwrap_or_else(|e| fail(format!("checkpoint_passive: {e}")));
                black_box(result);
                std::fs::remove_dir_all(&dir).unwrap_or(());
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

// ---------------------------------------------------------------------
// 4. cte_reuse_10x — NOT WAL-related: measures the V6.1 CTE
//    materialization benefit (#375/#376), a `WITH`-clause CTE
//    referenced 10 times in one query vs. the equivalent query with the
//    underlying subquery repeated 10 times inline (both self-joined on
//    `id`, matching `tests/corpus/cte_test.rs`'s
//    `with_clause_cte_referenced_twice_self_join_matches_oracle`
//    scenario, extended from 2 references to 10).
//
//    Finding worth flagging up front: `src/codegen/subquery/cte.rs`'s
//    `expand_with_clause` rewrites *every* CTE reference into its own
//    independent `TableRefKind::Subquery(cte.query.clone())` — the
//    exact same AST shape writing the subquery out inline 10 times
//    produces (confirmed here: both variants compile to materializing
//    the underlying query 10 separate times, and this benchmark's own
//    numbers below bear that out). There is currently no shared
//    "materialize once, scan N times" optimization for a multi-reference
//    CTE — using `WITH` costs the same as inline repetition, no more
//    and no less. This benchmark exists to make that measurable, not to
//    demonstrate a speedup that doesn't exist yet (see the ticket's
//    close-out notes for a proposed follow-up).
//
//    A 10-way UNION ALL of `SELECT count(*) FROM cte` was tried first
//    and rejected: it hits a real codegen gap (a compound arm beyond
//    the first fails to resolve a `TableRefKind::Subquery` reference,
//    "table cte has an invalid root page (0)") — also worth a follow-up,
//    not fixed here (out of scope for a benchmark ticket; see the
//    ticket's close-out notes).
// ---------------------------------------------------------------------

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

/// `WITH`/view-expansion-then-resolve-then-compile pipeline mirroring
/// `src/bin/sqlite-rs/query.rs::compile_select_program` (the `sqlite-rs
/// query` CLI's real dispatch) closely enough for this bench's needs —
/// that function itself is `pub(crate)` to the binary crate and not
/// callable from an external test/bench crate, so the CTE-expansion +
/// resolve + single-table/joined dispatch is reproduced here rather than
/// reused. No view expansion: none of this file's SQL references a view.
fn compile_ours_select(select: &Select, catalog: &[TableSchema]) -> Program {
    let expanded = expand_with_clause(select);
    let from = expanded
        .from
        .as_ref()
        .unwrap_or_else(|| fail("bench SQL has no FROM clause"));
    let resolve = |table_ref: &TableRef| {
        resolve_from_table_schema(table_ref, catalog)
            .unwrap_or_else(|e| fail(format!("resolve table {table_ref:?}: {e}")))
    };
    let schema = resolve(&from.first);
    if from.joins.is_empty() {
        compile_select_with_catalog(&expanded, &schema, catalog)
            .unwrap_or_else(|e| fail(format!("compile {expanded:?}: {e}")))
    } else {
        let mut joined_schemas = vec![schema];
        joined_schemas.extend(from.joins.iter().map(|j| resolve(&j.table)));
        compile_select_joined(&expanded, &joined_schemas, catalog)
            .unwrap_or_else(|e| fail(format!("compile {expanded:?}: {e}")))
    }
}

fn parse_ours(sql: &str) -> Select {
    match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => fail(format!("bench SQL failed to parse: {sql:?}: {other:?}")),
    }
}

/// A 10-way self-join of `source` on `id` — bounded to roughly the
/// `bucket = 5` filter's own row count (~1/100th of the table), not a
/// combinatorial blow-up, since every join condition ties back to `c1`.
fn n_way_join_sql(source: &str) -> String {
    let mut sql = format!("SELECT count(*) FROM {source} c1");
    for i in 2..=10 {
        sql.push_str(&format!(" JOIN {source} c{i} ON c{i}.id = c1.id"));
    }
    sql
}

fn bench_cte_reuse_10x(c: &mut Criterion) {
    let path = fixture_path("bench_1mb.db");
    let ours = open_ours(&path);

    let cte_sql = format!(
        "WITH cte AS (SELECT id, x FROM bench_data WHERE bucket = 5) {}",
        n_way_join_sql("cte")
    );
    let inline_sql = n_way_join_sql("(SELECT id, x FROM bench_data WHERE bucket = 5)");

    let mut group = c.benchmark_group("cte_reuse_10x/bench_1mb.db");

    let cte_program = compile_ours_select(&parse_ours(&cte_sql), &ours.catalog);
    group.bench_function("cte", |b| {
        b.iter(|| {
            let rows = execute_with_db(&cte_program, Rc::clone(&ours.source), ours.header)
                .unwrap_or_else(|e| fail(format!("execute {cte_sql:?}: {e}")));
            black_box(rows)
        });
    });

    let inline_program = compile_ours_select(&parse_ours(&inline_sql), &ours.catalog);
    group.bench_function("inline", |b| {
        b.iter(|| {
            let rows = execute_with_db(&inline_program, Rc::clone(&ours.source), ours.header)
                .unwrap_or_else(|e| fail(format!("execute {inline_sql:?}: {e}")));
            black_box(rows)
        });
    });

    group.finish();
}

fn bench_all(c: &mut Criterion) {
    bench_insert_batch_wal(c);
    bench_concurrent_read_write(c);
    bench_checkpoint_10mb(c);
    bench_cte_reuse_10x(c);
}

criterion_group!(benches, bench_all);
criterion_main!(benches);
