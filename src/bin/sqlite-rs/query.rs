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
    explain_query_plan, resolve_from_table_schema, CodegenError,
};
use sqlite_rs::dump;
use sqlite_rs::format::{format_csv_value, format_query_value};
use sqlite_rs::parser::{parse_explain, parse_select, ParseOutcome};
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::{execute_with_db, explain};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::common::{fatal, CSV_ROW_TERMINATOR};

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
    let resolve_table = |table_ref: &sqlite_rs::parser::ast::TableRef| {
        resolve_from_table_schema(table_ref, &schemas)
    };
    // A FROM-less SELECT (#260, e.g. `SELECT sqlite_version();`) has no
    // table to resolve at all — `compile_select_with_catalog` handles
    // this itself once `select.from` is `None`, so this path skips
    // straight to codegen with a throwaway schema/catalog neither one
    // is read.
    let Some(from) = &select.from else {
        if eqp_mode {
            return fatal(
                path,
                &"EXPLAIN QUERY PLAN requires a FROM clause".to_string(),
            );
        }
        let no_table = sqlite_rs::schema::TableSchema {
            name: String::new(),
            root_page: 0,
            columns: vec![],
            column_types: vec![],
            without_rowid: false,
            strict: false,
            is_virtual: false,
            sql: String::new(),
            indexes: vec![],
        };
        let program = match compile_select_with_catalog(&select, &no_table, &[]) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        };
        return finish_query(path, &program, source, header, explain_flag, csv);
    };

    let schema = match resolve_table(&from.first) {
        Ok(s) => s,
        Err(e) => return fatal(path, &e),
    };

    if eqp_mode {
        let mut joined_schemas = vec![schema];
        for join in &from.joins {
            let s = match resolve_table(&join.table) {
                Ok(s) => s,
                Err(e) => return fatal(path, &e),
            };
            joined_schemas.push(s);
        }
        let rows = match explain_query_plan(&select, &joined_schemas) {
            Ok(rows) => rows,
            Err(e) => return fatal(path, &e),
        };
        for row in rows {
            println!("{}|{}|{}|{}", row.id, row.parent, row.notused, row.detail);
        }
        return ExitCode::SUCCESS;
    }

    let program = if !select.compound.is_empty() {
        let mut arm_schemas = Vec::with_capacity(select.compound.len());
        for arm in &select.compound {
            let Some(arm_from) = &arm.from else {
                return fatal(path, &CodegenError::NoFromClause);
            };
            let s = match resolve_table(&arm_from.first) {
                Ok(s) => s,
                Err(e) => return fatal(path, &e),
            };
            arm_schemas.push(s);
        }
        match compile_select_compound(&select, &schema, &arm_schemas, &schemas) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        }
    } else if from.joins.is_empty() {
        match compile_select_with_catalog(&select, &schema, &schemas) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        }
    } else {
        let mut joined_schemas = vec![schema];
        for join in &from.joins {
            let s = match resolve_table(&join.table) {
                Ok(s) => s,
                Err(e) => return fatal(path, &e),
            };
            joined_schemas.push(s);
        }
        match compile_select_joined(&select, &joined_schemas, &schemas) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        }
    };

    finish_query(path, &program, source, header, explain_flag, csv)
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

fn write_list_row(out: &mut impl Write, values: &[Vec<u8>]) -> io::Result<()> {
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.write_all(b"|")?;
        }
        out.write_all(v)?;
    }
    out.write_all(b"\n")
}
