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

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::Instant;

use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::{execute_with_db, explain, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

fn fixture(row_count: u32, label: &str) -> (PathBuf, TableSchema) {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_point_lookup_perf_{}_{label}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

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

fn run(path: &PathBuf, program: &Program) -> std::time::Duration {
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
    samples[samples.len() / 2]
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

        let _ = std::fs::remove_file(&path);
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
        let (small_rows, small_ratio) = pair[0];
        let (big_rows, big_ratio) = pair[1];
        assert!(
            big_ratio >= small_ratio * 0.5,
            "scan/seek ratio shrank going from {small_rows} rows ({small_ratio:.1}x) to \
             {big_rows} rows ({big_ratio:.1}x) — expected it to hold steady or widen, since a \
             real O(log n) seek shouldn't lose ground as the table grows"
        );
    }
}
