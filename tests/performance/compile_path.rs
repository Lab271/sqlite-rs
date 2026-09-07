// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #590: an internal, relative benchmark (sqlite-rs vs sqlite-rs across
//! revisions, not vs oracle — same shape as `skip_scan.rs`) for the
//! Tier 2 *compile* path: tokenize -> parse -> codegen. Deliberately
//! separate from `engine.rs`, which times query *execution* against the
//! oracle on a real fixture; there, statement compilation is a rounding
//! error next to B-tree/IO work, so a compile-path change is invisible.
//! This bench isolates it instead.
//!
//! No fixture and no oracle: every input is a string literal and the
//! only state is one hand-built `TableSchema`, so this bench needs
//! neither `tools/bench_env.sh` nor rusqlite. Run it directly:
//!
//! ```text
//! cargo bench --bench compile_path
//! ```
//!
//! To compare two revisions, criterion's own baselines are the reliable
//! route — same machine, same target dir, back to back:
//!
//! ```text
//! cargo bench --bench compile_path -- --save-baseline before
//! # ... change code ...
//! cargo bench --bench compile_path -- --baseline before
//! ```
//!
//! Read the numbers as a *relative* signal between revisions. Absolute
//! nanoseconds here are meaningless as a parity claim: there is no
//! oracle arm, because stock sqlite3 exposes no comparable
//! "tokenize/parse/compile only, don't run it" entry point to time
//! against.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use sqlite_rs::codegen::{compile_select_with_catalog, expand_with_clause};
use sqlite_rs::parser::tokenizer::Tokenizer;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Collation;
use sqlite_rs::schema::TableSchema;

/// A short, extremely common statement shape — the case where per-call
/// fixed costs (source materialization, output `Vec` growth) dominate.
const SHORT: &str = "SELECT a, b FROM t WHERE x > 10";

/// A wider statement: more identifiers and keywords, so per-token costs
/// (keyword lookup, identifier slicing, `Token` size) dominate instead.
const LONG: &str = "SELECT customers.name, customers.email, orders.total, orders.placed_at \
                    FROM customers \
                    JOIN orders ON customers.id = orders.customer_id \
                    WHERE orders.total > 100 AND customers.name LIKE 'A%' \
                    ORDER BY orders.placed_at DESC \
                    LIMIT 50";

/// Literal-heavy input: string literals with `''` escapes, blobs, and
/// bind parameters — the scanners that used to rebuild every token
/// char-by-char, and the two `TokenKind` variants that used to sit
/// inline rather than boxed.
const LITERALS: &str = "SELECT 'it''s here', x'48454C4C4F', :name, @var, $p, ?1, 1.5e-3, 0xFF \
                        FROM t WHERE label = 'plain string with no escapes at all'";

/// Exercises the no-`WITH`-clause path through `expand_with_clause` —
/// the common case, where there is nothing to rewrite and so nothing
/// that needs an owned copy of the AST.
const NO_CTE: &str = "SELECT a, b, c FROM t WHERE x > 10 ORDER BY a LIMIT 5";

/// Same query shape, but behind a `WITH` clause, so the rewrite genuinely
/// runs and must produce an owned, substituted AST. Keeps the
/// no-rewrite fast path honest: this arm should *not* get faster.
const WITH_CTE: &str =
    "WITH src AS (SELECT a, b, c FROM t WHERE x > 10) SELECT a, b, c FROM src ORDER BY a LIMIT 5";

/// Minimal single-table catalog entry the codegen arms compile against.
/// Field-for-field a literal (rather than decoded from a fixture) so
/// this bench stays fixture-free.
fn bench_schema() -> TableSchema {
    TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 2,
        columns: vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "x".to_string(),
        ],
        column_types: vec![
            "INTEGER".to_string(),
            "TEXT".to_string(),
            "REAL".to_string(),
            "INTEGER".to_string(),
        ],
        column_collations: vec![
            Collation::Binary,
            Collation::Binary,
            Collation::Binary,
            Collation::Binary,
        ],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: "CREATE TABLE t(a INTEGER, b TEXT, c REAL, x INTEGER)".to_string(),
        indexes: vec![],
        rowid_alias: None,
    }
}

/// Same fail-loudly-not-panic shape as `engine.rs`/`v6.rs`/
/// `skip_scan.rs`'s `fail` — a bare `panic!` is denied crate-wide by
/// `Cargo.toml`'s `[lints.clippy]`.
fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("bench error: {msg}");
    std::process::exit(1);
}

fn parsed(sql: &str) -> sqlite_rs::parser::ast::Select {
    match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => fail(format!("bench input must parse: {sql:?} -> {other:?}")),
    }
}

/// Stage 1 in isolation: source text -> `Vec<Token>`.
fn bench_tokenize(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile_path/tokenize");
    for (name, sql) in [("short", SHORT), ("long", LONG), ("literals", LITERALS)] {
        group.bench_function(name, |b| {
            b.iter(|| black_box(Tokenizer::tokenize(black_box(sql))));
        });
    }
    group.finish();
}

/// Stages 1+2: source text -> AST. Measured together because the parser
/// consumes the tokenizer's output directly; `tier2/tokenize` above is
/// what separates the two contributions.
fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("compile_path/parse");
    for (name, sql) in [("short", SHORT), ("long", LONG), ("literals", LITERALS)] {
        group.bench_function(name, |b| {
            b.iter(|| black_box(parse_select(black_box(sql))));
        });
    }
    group.finish();
}

/// The `WITH`-clause rewrite, on an already-parsed AST: `no_cte` is the
/// nothing-to-rewrite case, `with_cte` the case that genuinely rewrites.
fn bench_expand(c: &mut Criterion) {
    let no_cte = parsed(NO_CTE);
    let with_cte = parsed(WITH_CTE);
    let mut group = c.benchmark_group("compile_path/expand_with_clause");
    // Bound to a local and then borrowed, rather than returned from the
    // closure: the return type differs across the revisions this bench
    // compares (owned `Select` vs `Cow<Select>`), and `&local` is valid
    // for both where returning the value itself would not be.
    group.bench_function("no_cte", |b| {
        b.iter(|| {
            let expanded = expand_with_clause(black_box(&no_cte));
            black_box(&expanded);
        });
    });
    group.bench_function("with_cte", |b| {
        b.iter(|| {
            let expanded = expand_with_clause(black_box(&with_cte));
            black_box(&expanded);
        });
    });
    group.finish();
}

/// The whole Tier 2 pipeline end to end: text -> tokens -> AST ->
/// `Program`. This is the number that matters for "how long does it take
/// to get a statement ready to run".
fn bench_compile(c: &mut Criterion) {
    let schema = bench_schema();
    let mut group = c.benchmark_group("compile_path/compile_full");
    for (name, sql) in [("short", SHORT), ("no_cte", NO_CTE)] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let select = match parse_select(black_box(sql)) {
                    ParseOutcome::Accepted(s) => *s,
                    other => fail(format!("bench input must parse: {other:?}")),
                };
                let expanded = expand_with_clause(&select);
                black_box(compile_select_with_catalog(&expanded, &schema, &[]))
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_tokenize,
    bench_parse,
    bench_expand,
    bench_compile
);
criterion_main!(benches);
