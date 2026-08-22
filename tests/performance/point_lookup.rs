#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Quick wall-clock evidence for #137: `WHERE rowid = <const>` compiles
//! to `SeekRowid` (O(log n)) instead of the `Rewind`/`Next` full-table
//! scan (O(n)). Unlike `tests/performance/engine.rs` (a criterion bench
//! needing `tools/bench_env.sh`, rusqlite linked against the pinned
//! oracle, and pre-generated fixtures), this is a plain `#[test]`:
//! self-contained, no external bench harness, fast enough to run as
//! part of the normal suite — a scaling *demonstration*, not a
//! micro-benchmark with statistical rigor.
//!
//! What it shows: a full scan's cost grows with row count, but a point
//! lookup's does not — so the ratio between them widens as the table
//! grows. That widening ratio is the O(n) vs O(log n) signature; a
//! single fixed threshold on one fixture size couldn't distinguish
//! "point lookup is a bit faster" from "point lookup doesn't scan the
//! table at all."
//!
//! Also covers V4's join-level counterpart: whether a `JOIN`'s inner
//! table gets a `SeekIndexEq` point lookup per outer row (when the join
//! column has a `UNIQUE` index, #243) instead of a `Rewind`/`Next` scan
//! per outer row. See `join_lookup_indexed_beats_unindexed_at_every_outer_table_size`
//! below.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Instant;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select, compile_select_joined, resolve_from_table_schema};
use sqlite_rs::dump;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::ast::Select;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, explain, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

fn fixture(row_count: u32, label: &str) -> (PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_point_lookup_perf_{}_{label}.db",
        std::process::id()
    ));
    std::fs::remove_file(&path).ok();

    let mut sql = String::from("CREATE TABLE t(id INTEGER PRIMARY KEY, payload TEXT);\n");
    sql.push_str("INSERT INTO t(payload) SELECT 'row-' || value FROM generate_series(1, ");
    sql.push_str(&row_count.to_string());
    sql.push_str(");\n");

    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(sql)
        .status()
        .expect("invoking sqlite3 to build the fixture (requires sqlite3 on PATH)");
    assert!(status.success(), "fixture creation failed");

    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["id".to_string(), "payload".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
    };
    (path, schema)
}

fn compile(schema: &TableSchema, sql: &str) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling {sql:?}: {e}"))
}

fn run(path: &Path, program: &Program) -> std::time::Duration {
    let vfs = UnixVfs;
    let file = vfs.open_read(path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source: Rc<dyn PageSource> =
        Rc::new(VfsPageSource::open(&vfs, path, header.page_size).unwrap());

    // Median of a few runs — smooths OS/page-cache jitter without
    // pulling in criterion for what's meant to stay a quick, dependency-
    // free demonstration.
    let mut samples = Vec::with_capacity(5);
    for _ in 0..5 {
        let start = Instant::now();
        let rows = execute_with_db(program, Rc::clone(&source), header).expect("execute");
        samples.push(start.elapsed());
        std::hint::black_box(rows);
    }
    samples.sort();
    let mid = samples.len() / 2;
    samples
        .get(mid)
        .copied()
        .expect("samples is non-empty (5 iterations pushed above)")
}

/// #137's headline claim, made concrete: as the table grows, a full
/// scan gets slower (it visits every row) while a `SeekRowid` point
/// lookup does not (it's a b-tree descent, ~log n page reads regardless
/// of table size). Asserting the *ratio* widens with row count — rather
/// than a fixed "seek must be under Xms" threshold — is what actually
/// distinguishes O(log n) from "a constant factor faster."
#[test]
fn point_lookup_scan_ratio_widens_with_table_size() {
    if Command::new("sqlite3").arg("-version").output().is_err() {
        eprintln!("skipping point_lookup_scan_ratio_widens_with_table_size: no sqlite3 on PATH");
        return;
    }

    let mut ratios = Vec::new();
    for &row_count in &[2_000u32, 20_000u32, 200_000u32] {
        let (path, schema) = fixture(row_count, &row_count.to_string());

        let scan_program = compile(&schema, "SELECT id, payload FROM t");
        let seek_program = compile(
            &schema,
            &format!("SELECT id, payload FROM t WHERE rowid = {row_count}"),
        );

        // Sanity-check the shape this test depends on, not just the
        // timing: no Rewind/Next in the seek program's opcode stream.
        let seek_rows = explain(&seek_program);
        assert!(
            seek_rows.iter().any(|r| r.opcode == "SeekRowid"),
            "expected SeekRowid in the compiled point-lookup program"
        );
        assert!(
            !seek_rows.iter().any(|r| r.opcode == "Rewind"),
            "point-lookup program must not also emit a full scan"
        );

        let scan_time = run(&path, &scan_program);
        let seek_time = run(&path, &seek_program);
        let ratio = scan_time.as_secs_f64() / seek_time.as_secs_f64().max(f64::EPSILON);
        eprintln!("rows={row_count}: full_scan={scan_time:?} seek={seek_time:?} ratio={ratio:.1}x");
        ratios.push((row_count, ratio));

        std::fs::remove_file(&path).ok();
    }

    // A generous floor (not the ~2500x the issue's 1M-row estimate
    // implies) to stay stable on a loaded CI box: the point is that the
    // *smallest* fixture already shows a real gap, not that this test
    // pins an exact multiplier.
    for (row_count, ratio) in &ratios {
        assert!(
            *ratio > 3.0,
            "expected the {row_count}-row full scan to be meaningfully slower than the \
             SeekRowid point lookup, got only {ratio:.1}x — has the fast path regressed?"
        );
    }

    // The scan/seek ratio should not shrink as the table grows 10x at a
    // time — if anything it should grow, since the scan's cost scales
    // with row count and the seek's does not. A shrinking ratio would
    // mean the "fast path" is secretly still paying an O(n) cost
    // somewhere. Checked across every consecutive pair, not just the
    // endpoints, so a regression at any one fixture size is caught.
    for pair in ratios.windows(2) {
        let (Some(&(small_rows, small_ratio)), Some(&(big_rows, big_ratio))) =
            (pair.first(), pair.get(1))
        else {
            continue;
        };
        assert!(
            big_ratio >= small_ratio * 0.5,
            "scan/seek ratio shrank going from {small_rows} rows ({small_ratio:.1}x) to \
             {big_rows} rows ({big_ratio:.1}x) — expected it to hold steady or widen, since a \
             real O(log n) seek shouldn't lose ground as the table grows"
        );
    }
}

/// Builds a `bench_data`/`bench_lookup`-shaped JOIN fixture (same shape
/// as `tests/performance/engine.rs`'s `join` scenario, #301): `bench_data`
/// has a `bucket` column joined against `bench_lookup.code`. `bench_lookup`
/// is a plain rowid table (not `WITHOUT ROWID` — this crate's table scan
/// doesn't yet handle index-organized tables) with a `TEXT` `code` column
/// so a plain rowid lookup can't apply. When `indexed` is true,
/// `bench_lookup.code` gets an explicit `CREATE UNIQUE INDEX` —
/// `choose_join_access` only considers `UNIQUE` single-column indexes,
/// see `src/codegen/select/join_access.rs` — so the join's inner side is
/// a `SeekIndexEq`, not a `Rewind`/`Next` scan.
fn join_fixture(row_count: u32, indexed: bool, label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_point_lookup_join_perf_{}_{label}.db",
        std::process::id()
    ));
    std::fs::remove_file(&path).ok();

    let mut sql = String::from(
        "CREATE TABLE bench_lookup(code TEXT, label TEXT);\n\
         INSERT INTO bench_lookup SELECT 'code-' || value, 'label-' || value \
         FROM generate_series(0, 199);\n",
    );
    if indexed {
        sql.push_str("CREATE UNIQUE INDEX bench_lookup_code ON bench_lookup(code);\n");
    }
    sql.push_str("CREATE TABLE bench_data(id INTEGER PRIMARY KEY, bucket TEXT);\n");
    sql.push_str(
        "INSERT INTO bench_data SELECT value, 'code-' || (value % 200) \
         FROM generate_series(1, ",
    );
    sql.push_str(&row_count.to_string());
    sql.push_str(");\n");

    let status = Command::new("sqlite3")
        .arg(&path)
        .arg(sql)
        .status()
        .expect("invoking sqlite3 to build the join fixture (requires sqlite3 on PATH)");
    assert!(status.success(), "join fixture creation failed");
    path
}

fn compile_join(sql: &str, catalog: &[TableSchema]) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    compile_join_select(&select, catalog)
}

fn compile_join_select(select: &Select, catalog: &[TableSchema]) -> Program {
    let from = select.from.as_ref().expect("bench SQL has no FROM clause");
    let resolve = |table_ref: &sqlite_rs::parser::ast::TableRef| {
        resolve_from_table_schema(table_ref, catalog)
            .unwrap_or_else(|e| panic!("resolve table {table_ref:?}: {e}"))
    };
    let mut schemas = vec![resolve(&from.first)];
    schemas.extend(from.joins.iter().map(|j| resolve(&j.table)));
    compile_select_joined(select, &schemas, catalog)
        .unwrap_or_else(|e| panic!("compile {select:?}: {e}"))
}

fn run_join(path: &Path, program: &Program) -> std::time::Duration {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut samples = Vec::with_capacity(5);
    for _ in 0..5 {
        let start = Instant::now();
        let rows = execute_with_db(program, Rc::clone(&source), header).expect("execute");
        samples.push(start.elapsed());
        std::hint::black_box(rows);
    }
    samples.sort();
    let mid = samples.len() / 2;
    samples
        .get(mid)
        .copied()
        .expect("samples is non-empty (5 iterations pushed above)")
}

fn catalog_of(path: &Path) -> Vec<TableSchema> {
    let (header, pager) =
        dump::open(&UnixVfs, path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let source: Rc<dyn PageSource> = Rc::new(pager);
    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    read_schema(&mut schema_cursor, header.text_encoding)
        .unwrap_or_else(|e| panic!("read_schema {path:?}: {e}"))
}

/// V4's join-path counterpart to
/// `point_lookup_scan_ratio_widens_with_table_size` above: instead of a
/// single table's `rowid` seek, this is `bench_data JOIN bench_lookup ON
/// bench_data.bucket = bench_lookup.code` — every outer row does one
/// lookup into `bench_lookup`, so an unindexed `code` forces a full
/// `Rewind`/`Next` scan of the ~200-row lookup table *per outer row*
/// (O(outer * inner)), while an index on `code` collapses that per-row
/// lookup to a `SeekIndexEq` (O(outer * log inner)). Unlike the rowid-seek
/// test above, both sides here scale linearly with `bench_data`'s row
/// count (only the *inner* table is fixed-size), so the indexed/unindexed
/// ratio is expected to stay roughly steady rather than widen as
/// `bench_data` grows — this test pins that the gap is real and durable,
/// not that it widens.
#[test]
fn join_lookup_indexed_beats_unindexed_at_every_outer_table_size() {
    if Command::new("sqlite3").arg("-version").output().is_err() {
        eprintln!(
            "skipping join_lookup_indexed_beats_unindexed_at_every_outer_table_size: \
             no sqlite3 on PATH"
        );
        return;
    }

    let sql = "SELECT bench_data.id, bench_lookup.label FROM bench_data \
               JOIN bench_lookup ON bench_data.bucket = bench_lookup.code";

    let mut ratios = Vec::new();
    for &row_count in &[200u32, 800u32, 3_200u32] {
        let unindexed_path = join_fixture(row_count, false, &format!("{row_count}_noidx"));
        let indexed_path = join_fixture(row_count, true, &format!("{row_count}_idx"));

        let indexed_catalog = catalog_of(&indexed_path);
        let indexed_program = compile_join(sql, &indexed_catalog);
        let explained = explain(&indexed_program);
        assert!(
            explained.iter().any(|r| r.opcode == "SeekIndexEq"),
            "expected the indexed join to compile a SeekIndexEq into bench_lookup"
        );

        let unindexed_catalog = catalog_of(&unindexed_path);
        let unindexed_program = compile_join(sql, &unindexed_catalog);

        let unindexed_time = run_join(&unindexed_path, &unindexed_program);
        let indexed_time = run_join(&indexed_path, &indexed_program);
        let ratio = unindexed_time.as_secs_f64() / indexed_time.as_secs_f64().max(f64::EPSILON);
        eprintln!(
            "outer_rows={row_count}: unindexed_join={unindexed_time:?} \
             indexed_join={indexed_time:?} ratio={ratio:.1}x"
        );
        ratios.push((row_count, ratio));

        std::fs::remove_file(&unindexed_path).ok();
        std::fs::remove_file(&indexed_path).ok();
    }

    // Same generous-floor rationale as the rowid-seek test above: pin
    // that the indexed join is *meaningfully* faster, not an exact
    // multiplier that would make this test flaky on a loaded CI box.
    for (row_count, ratio) in &ratios {
        assert!(
            *ratio > 1.5,
            "expected the indexed join ({row_count} outer rows) to be meaningfully faster than \
             the unindexed join, got only {ratio:.1}x — has the join index-seek path regressed?"
        );
    }
}
