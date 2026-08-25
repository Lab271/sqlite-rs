//! `query [-csv] [-explain] <file> "<SQL>"`: parse -> resolve the `FROM`
//! table's schema -> compile -> execute -> render, read-only, through
//! the same `dump::open` (safe-reader locking, WAL-pending visible,
//! hot-journal refusal) `dump`/`export` use. `-explain` prints the
//! compiled bytecode (spec 009 Requirement 10) instead of running it;
//! `-csv` switches row rendering to CSV — matching plain
//! `sqlite3 file "sql"`, neither mode prints a header row.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_compound, compile_select_joined, compile_select_with_catalog,
    compile_select_with_catalog_and_stats, expand_views, expand_with_clause, explain_query_plan,
    resolve_from_table_schema, resolve_views, CodegenError, EqpRow,
};
use sqlite_rs::dump;
use sqlite_rs::format::{format_csv_value, format_query_value};
use sqlite_rs::parser::ast::Select;
use sqlite_rs::parser::{parse_explain, parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, read_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::{execute_with_db, explain, Program};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::common::{fatal, CSV_ROW_TERMINATOR};
use crate::pragma_query::{execute_pragma_query, parse_pragma_query};

/// What [`compile_select_program`] produced: either `EXPLAIN QUERY
/// PLAN`'s rows (nothing further to compile — there's no bytecode to
/// run or `-explain`) or an ordinary compiled `Program`.
pub(crate) enum SelectOutcome {
    Eqp(Vec<EqpRow>),
    Program(Program),
}

/// Parses (already done by the caller — `select`/`eqp_mode` come from
/// `parse_select`/`parse_explain`), resolves every table `select`
/// touches against `schemas`, and compiles it: FROM-less (#260),
/// single-table, joined (#237), or `UNION ALL` compound (#240),
/// whichever `select`'s shape calls for. Shared by `run_query` (a
/// fresh read-only `Pager` per invocation) and the REPL (#365, one
/// shared read/write `Pager` per session) — both need exactly this
/// parse-resolve-compile pipeline, just against a different
/// `PageSource`.
pub(crate) fn compile_select_program(
    select: &Select,
    eqp_mode: bool,
    schemas: &[TableSchema],
    views: &[ViewSchema],
    stats_by_table: &std::collections::HashMap<String, sqlite_rs::planner::Stats>,
) -> Result<SelectOutcome, String> {
    // #376: a `WITH` clause is rewritten away before any table
    // resolution happens — every CTE reference in `FROM`/`JOIN` becomes
    // a `TableRefKind::Subquery` wrapping that CTE's own query, so the
    // rest of this pipeline (and #257's subquery-in-FROM codegen) needs
    // no CTE-specific handling at all.
    let cte_expanded = expand_with_clause(select);
    // #380: every catalog-view reference in `FROM`/`JOIN` is rewritten
    // away next, the same shape as the CTE rewrite above — into a
    // `TableRefKind::Subquery` wrapping the view's own stored query,
    // reusing #257's subquery-in-FROM codegen unchanged. Runs *after*
    // the CTE rewrite (rather than before) so it also reaches into any
    // `TableRefKind::Subquery` the CTE rewrite just produced — a CTE
    // whose own body references a view is resolved this way, without
    // `expand_views` needing any CTE-specific handling of its own; a
    // CTE also shadows a same-named view for the scope of its declaring
    // `SELECT`, matching how it already shadows a same-named real table.
    let resolved_views = resolve_views(views);
    let expanded = expand_views(&cte_expanded, &resolved_views).map_err(|e| e.to_string())?;
    let select = &expanded;

    let resolve_table = |table_ref: &sqlite_rs::parser::ast::TableRef| {
        resolve_from_table_schema(table_ref, schemas)
    };

    let Some(from) = &select.from else {
        if eqp_mode {
            return Err("EXPLAIN QUERY PLAN requires a FROM clause".to_string());
        }
        let no_table = TableSchema {
            name: String::new(),
            root_page: 0,
            columns: vec![],
            column_types: vec![],
            column_collations: vec![],
            without_rowid: false,
            strict: false,
            is_virtual: false,
            sql: String::new(),
            indexes: vec![],
        };
        let program =
            compile_select_with_catalog(select, &no_table, &[]).map_err(|e| e.to_string())?;
        return Ok(SelectOutcome::Program(program));
    };

    let schema = resolve_table(&from.first).map_err(|e| e.to_string())?;

    if eqp_mode {
        let mut joined_schemas = vec![schema];
        for join in &from.joins {
            joined_schemas.push(resolve_table(&join.table).map_err(|e| e.to_string())?);
        }
        let rows = explain_query_plan(select, &joined_schemas, stats_by_table)
            .map_err(|e| e.to_string())?;
        return Ok(SelectOutcome::Eqp(rows));
    }

    let program = if !select.compound.is_empty() {
        let mut arm_schemas = Vec::with_capacity(select.compound.len());
        for arm in &select.compound {
            let Some(arm_from) = &arm.from else {
                return Err(CodegenError::NoFromClause.to_string());
            };
            arm_schemas.push(resolve_table(&arm_from.first).map_err(|e| e.to_string())?);
        }
        compile_select_compound(select, &schema, &arm_schemas, schemas)
            .map_err(|e| e.to_string())?
    } else if from.joins.is_empty() {
        let stats = stats_by_table
            .get(&schema.name)
            .cloned()
            .unwrap_or_default();
        compile_select_with_catalog_and_stats(select, &schema, schemas, &stats)
            .map_err(|e| e.to_string())?
    } else {
        let mut joined_schemas = vec![schema];
        for join in &from.joins {
            joined_schemas.push(resolve_table(&join.table).map_err(|e| e.to_string())?);
        }
        compile_select_joined(select, &joined_schemas, schemas, stats_by_table)
            .map_err(|e| e.to_string())?
    };
    Ok(SelectOutcome::Program(program))
}

pub fn run_query(raw_args: Vec<String>) -> ExitCode {
    let mut csv = false;
    let mut explain_flag = false;
    let mut positional = Vec::new();
    for arg in raw_args {
        match arg.as_str() {
            "-csv" => csv = true,
            "-explain" => explain_flag = true,
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let (Some(path), Some(sql)) = (positional.next(), positional.next()) else {
        return crate::common::usage_error("query [-csv] [-explain] <file> \"<SQL>\"");
    };
    let path = Path::new(&path);

    // #489: the 9 read-only introspection pragmas (`table_info`,
    // `table_list`, `index_list`, `index_info`, `database_list`,
    // `schema_version`, `user_version`, `page_size`, `page_count`) are
    // recognized by a hand-rolled parser entirely separate from
    // `parse_select`/`parse_explain` — checked first so a `PRAGMA`
    // statement never gets fed into either. A `PRAGMA` that isn't one
    // of these 9 (e.g. `journal_mode`, the `#388` write-pragma path)
    // falls through unrecognized here and hits the ordinary
    // `parse_select` error path below, same as before this existed.
    if let Some(pragma) = parse_pragma_query(&sql) {
        return run_pragma_query(path, &pragma);
    }

    // #243: `EXPLAIN QUERY PLAN <select>` is parsed by a dedicated entry
    // point (`parse_explain`, grammar V4) rather than `parse_select` —
    // only checked when the statement actually starts with `EXPLAIN`,
    // so an ordinary `SELECT` never pays for the extra parse attempt.
    let starts_with_explain = sql
        .trim_start()
        .get(..7)
        .is_some_and(|head| head.eq_ignore_ascii_case("explain"));
    let (select, eqp_mode) = if starts_with_explain {
        match parse_explain(&sql) {
            ParseOutcome::Accepted(explain) => (*explain.select, explain.query_plan),
            ParseOutcome::Unsupported { message, span } => {
                return fatal(
                    path,
                    &format!(
                        "not yet supported (line {}, column {}): {message}",
                        span.line, span.column
                    ),
                );
            }
            ParseOutcome::Invalid { message, span } => {
                return fatal(
                    path,
                    &format!(
                        "syntax error (line {}, column {}): {message}",
                        span.line, span.column
                    ),
                );
            }
        }
    } else {
        match parse_select(&sql) {
            ParseOutcome::Accepted(select) => (*select, false),
            ParseOutcome::Unsupported { message, span } => {
                return fatal(
                    path,
                    &format!(
                        "not yet supported (line {}, column {}): {message}",
                        span.line, span.column
                    ),
                );
            }
            ParseOutcome::Invalid { message, span } => {
                return fatal(
                    path,
                    &format!(
                        "syntax error (line {}, column {}): {message}",
                        span.line, span.column
                    ),
                );
            }
        }
    };
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let schemas = match read_schema(&mut schema_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(e) => return fatal(path, &e),
    };
    let mut view_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let views = match read_views(&mut view_cursor, header.text_encoding) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let stats_by_table = sqlite_rs::planner::load_stats(Rc::clone(&source), &header, &schemas);

    match compile_select_program(&select, eqp_mode, &schemas, &views, &stats_by_table) {
        Ok(SelectOutcome::Eqp(rows)) => {
            for row in rows {
                println!("{}|{}|{}|{}", row.id, row.parent, row.notused, row.detail);
            }
            ExitCode::SUCCESS
        }
        Ok(SelectOutcome::Program(program)) => {
            finish_query(path, &program, source, header, explain_flag, csv)
        }
        Err(e) => fatal(path, &e),
    }
}

/// `EXPLAIN`-render-or-execute-and-print tail shared by every `run_query`
/// codegen path (single-table, joined, compound, and #260's FROM-less).
fn finish_query(
    path: &Path,
    program: &sqlite_rs::vdbe::Program,
    source: Rc<dyn PageSource>,
    header: sqlite_rs::header::DatabaseHeader,
    explain_flag: bool,
    csv: bool,
) -> ExitCode {
    if explain_flag {
        for row in explain(program) {
            println!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                row.addr, row.opcode, row.p1, row.p2, row.p3, row.p4, row.p5, row.comment
            );
        }
        return ExitCode::SUCCESS;
    }

    let rows = match execute_with_db(program, source, header) {
        Ok(r) => r,
        Err(e) => return fatal(path, &e),
    };

    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for row in &rows {
        if csv {
            let rendered: Vec<String> = row.iter().map(format_csv_value).collect();
            if let Err(e) = write!(stdout, "{}{CSV_ROW_TERMINATOR}", rendered.join(",")) {
                return fatal(path, &e);
            }
        } else {
            // `-list` mode: raw blob bytes may not be valid UTF-8, so this
            // writes bytes directly rather than going through `String`.
            let rendered: Vec<Vec<u8>> = row.iter().map(format_query_value).collect();
            if let Err(e) = write_list_row(&mut stdout, &rendered) {
                return fatal(path, &e);
            }
        }
    }
    if let Err(e) = stdout.flush() {
        return fatal(path, &e);
    }
    ExitCode::SUCCESS
}

/// Opens `path` fresh (a one-shot `query` invocation has no shared
/// session `Pager`, unlike the repl's), loads schemas/views/header, and
/// executes an already-recognized introspection `PRAGMA` end to end —
/// used only by `run_query`. `repl.rs` loads schemas/views/header
/// itself each statement (its existing per-statement pattern) and
/// calls [`execute_pragma_query`] directly against its session's
/// already-open shared `Pager`, reusing the same execution/rendering
/// logic without this fresh-open step.
fn run_pragma_query(path: &Path, pragma: &crate::pragma_query::PragmaQuery) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let source: Rc<dyn PageSource> = Rc::new(pager);
    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let schemas = match read_schema(&mut schema_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(e) => return fatal(path, &e),
    };
    let mut view_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let views = match read_views(&mut view_cursor, header.text_encoding) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    print_pragma_rows(path, pragma, &schemas, &views, &header)
}

/// Executes `pragma` against already-loaded `schemas`/`views`/`header`
/// and prints its rows pipe-delimited via [`write_list_row`], the same
/// rendering `SelectOutcome::Eqp` and ordinary query rows use.
pub(crate) fn print_pragma_rows(
    path: &Path,
    pragma: &crate::pragma_query::PragmaQuery,
    schemas: &[TableSchema],
    views: &[ViewSchema],
    header: &sqlite_rs::header::DatabaseHeader,
) -> ExitCode {
    match execute_pragma_query(pragma, schemas, views, header, path) {
        Ok(rows) => {
            let mut stdout = io::BufWriter::new(io::stdout().lock());
            for row in rows {
                let rendered: Vec<Vec<u8>> = row.into_iter().map(String::into_bytes).collect();
                if let Err(e) = write_list_row(&mut stdout, &rendered) {
                    return fatal(path, &e);
                }
            }
            if let Err(e) = stdout.flush() {
                return fatal(path, &e);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fatal(path, &e),
    }
}

pub(crate) fn write_list_row(out: &mut impl Write, values: &[Vec<u8>]) -> io::Result<()> {
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.write_all(b"|")?;
        }
        out.write_all(v)?;
    }
    out.write_all(b"\n")
}
