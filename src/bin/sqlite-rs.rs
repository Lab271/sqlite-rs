//! `sqlite-rs` CLI: `dump`, `export`, and `query` subcommands (issues
//! #37, #95 — the V1 and V2 acceptance gates). Data goes to stdout
//! (`dump`, `query`) or disk (`export`); anything gracefully skipped
//! goes to stderr as a warning. Dot-commands, a REPL, and `.import` are
//! explicit non-goals (CLI level 3, a later value block) — see the
//! issue bodies. `query`'s own flags (`-csv`, `-explain`) deliberately
//! use `sqlite3`'s single-dash option style rather than GNU `--long`
//! flags, matching the interface it stays parity with.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_create_index, compile_create_table, compile_delete_with_catalog, compile_drop_index,
    compile_drop_table, compile_insert, compile_select_compound, compile_select_joined,
    compile_select_with_catalog, compile_update_with_catalog, explain_query_plan, CodegenError,
};
use sqlite_rs::dump::{self, dump_database};
use sqlite_rs::format::{csv_quote, format_csv_value, format_list_value, format_query_value};
use sqlite_rs::parser::error::{
    parse_create_index, parse_create_table, parse_delete, parse_drop_index, parse_drop_table,
    parse_insert, parse_update,
};
use sqlite_rs::parser::{parse_explain, parse_select, ParseOutcome};
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::{execute_with_db, execute_with_writable_db, explain};
use sqlite_rs::vfs::{PageSource, UnixVfs};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("sqlite-rs {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("dump") => match args.next() {
            Some(path) => run_dump(Path::new(&path)),
            None => usage_error("dump <file>"),
        },
        Some("export") => match args.next() {
            Some(path) => run_export(Path::new(&path)),
            None => usage_error("export <file>"),
        },
        Some("query") => run_query(args.collect()),
        Some("tables") => match args.next() {
            Some(path) => run_tables(Path::new(&path)),
            None => usage_error("tables <file>"),
        },
        Some("exec") => {
            let (Some(path), Some(sql)) = (args.next(), args.next()) else {
                return usage_error("exec <file> \"<SQL>\"");
            };
            run_exec(Path::new(&path), &sql)
        }
        _ => usage_error("[--version] <dump|export|query|tables|exec> <file>"),
    }
}

/// `sqlite3`'s `-csv` mode terminates every row — header included — with
/// CRLF, per RFC 4180, and `export` matches it so its output is
/// byte-identical to the oracle's. This is purely a CLI output-layer
/// convention: SQLite's storage engine is line-ending agnostic (TEXT and
/// BLOB bytes are stored and returned verbatim), so this terminator is
/// only ever *appended between* values and never rewrites a value's own
/// embedded CR/LF bytes.
///
/// Note `-list` mode (what `dump` emits) uses a bare LF instead — the two
/// modes genuinely differ, verified against the pinned oracle.
const CSV_ROW_TERMINATOR: &str = "\r\n";

fn usage_error(expected: &str) -> ExitCode {
    eprintln!("usage: sqlite-rs {expected}");
    ExitCode::from(2)
}

fn run_dump(path: &Path) -> ExitCode {
    let result = match dump_database(&UnixVfs, path) {
        Ok(r) => r,
        Err(e) => return fatal(path, &e),
    };

    for table in &result.tables {
        println!("{}", table.sql);
        for row in &table.rows {
            let rendered: Vec<String> = row.iter().map(format_list_value).collect();
            println!("{}", rendered.join("|"));
        }
    }

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    degraded_exit_code(result.warnings.is_empty())
}

fn run_export(path: &Path) -> ExitCode {
    let result = match dump_database(&UnixVfs, path) {
        Ok(r) => r,
        Err(e) => return fatal(path, &e),
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    let mut clean = result.warnings.is_empty();

    for table in &result.tables {
        let out_path: PathBuf = dir.join(format!(
            "{}_{stem}.csv",
            sanitize_filename_component(&table.name)
        ));
        let mut out = String::new();
        out.push_str(
            &table
                .columns
                .iter()
                .map(|c| csv_quote(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str(CSV_ROW_TERMINATOR);
        for row in &table.rows {
            let rendered: Vec<String> = row.iter().map(format_csv_value).collect();
            out.push_str(&rendered.join(","));
            out.push_str(CSV_ROW_TERMINATOR);
        }
        if let Err(e) = std::fs::write(&out_path, out) {
            eprintln!("warning: table {:?}: writing {out_path:?}: {e}", table.name);
            clean = false;
            continue;
        }
        eprintln!("wrote {} ({} rows)", out_path.display(), table.rows.len());
    }

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    degraded_exit_code(clean)
}

/// Maps a `sqlite_master` table name to a safe filesystem path component.
/// Table names come verbatim from the (possibly untrusted) database being
/// exported, so they cannot be trusted as path segments — a crafted name
/// containing `..`/`/`/an absolute path could otherwise let `export` write
/// outside the target directory or overwrite an arbitrary file. Only
/// ASCII alphanumerics and `_` pass through unchanged.
fn sanitize_filename_component(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "table".to_string()
    } else {
        sanitized
    }
}

/// `dump`/`export` still print/write everything they successfully read
/// even when some tables were gracefully skipped — but a caller checking
/// only the exit code needs a way to detect that the output is partial.
fn degraded_exit_code(clean: bool) -> ExitCode {
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn fatal(path: &Path, e: &impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {}: {e}", path.display());
    ExitCode::FAILURE
}

/// `tables <file>`: list table names from sqlite_master, sorted
/// alphabetically, excluding internal `sqlite_%` tables. Simple precursor
/// to full `.tables [PATTERN]` REPL support (see #177). Note: views not
/// yet included (requires schema extension).
fn run_tables(path: &Path) -> ExitCode {
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

    let mut names: Vec<&str> = schemas
        .iter()
        .filter(|s| !s.name.starts_with("sqlite_"))
        .map(|s| s.name.as_str())
        .collect();
    names.sort_unstable();

    for name in names {
        println!("{name}");
    }
    ExitCode::SUCCESS
}

/// `query <file> "<SQL>"`: parse -> resolve the `FROM` table's schema ->
/// compile -> execute -> render, read-only, through the same
/// `dump::open` (safe-reader locking, WAL-pending visible, hot-journal
/// refusal) `dump`/`export` use. `-explain` prints the compiled
/// bytecode (spec 009 Requirement 10) instead of running it; `-csv`
/// switches row rendering to CSV — matching plain `sqlite3 file "sql"`,
/// neither mode prints a header row.
fn run_query(raw_args: Vec<String>) -> ExitCode {
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
        return usage_error("query [-csv] [-explain] <file> \"<SQL>\"");
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
    let Some(from) = &select.from else {
        return fatal(path, &CodegenError::NoFromClause);
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
    let find_schema = |name: &str| {
        schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
    };
    let Some(schema) = find_schema(&from.first.name) else {
        return fatal(path, &format!("no such table: {}", from.first.name));
    };

    if eqp_mode {
        let mut joined_schemas = vec![schema];
        for join in &from.joins {
            let Some(s) = find_schema(&join.table.name) else {
                return fatal(path, &format!("no such table: {}", join.table.name));
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
            let Some(s) = find_schema(&arm_from.first.name) else {
                return fatal(path, &format!("no such table: {}", arm_from.first.name));
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
            let Some(s) = find_schema(&join.table.name) else {
                return fatal(path, &format!("no such table: {}", join.table.name));
            };
            joined_schemas.push(s);
        }
        match compile_select_joined(&select, &joined_schemas) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        }
    };

    if explain_flag {
        for row in explain(&program) {
            println!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                row.addr, row.opcode, row.p1, row.p2, row.p3, row.p4, row.p5, row.comment
            );
        }
        return ExitCode::SUCCESS;
    }

    let rows = match execute_with_db(&program, source, header) {
        Ok(r) => r,
        Err(e) => return fatal(path, &e),
    };

    let mut stdout = io::stdout().lock();
    for row in &rows {
        if csv {
            let rendered: Vec<String> = row.iter().map(format_csv_value).collect();
            print!("{}{CSV_ROW_TERMINATOR}", rendered.join(","));
        } else {
            // `-list` mode: raw blob bytes may not be valid UTF-8, so this
            // writes bytes directly rather than going through `String`.
            let rendered: Vec<Vec<u8>> = row.iter().map(format_query_value).collect();
            if let Err(e) = write_list_row(&mut stdout, &rendered) {
                return fatal(path, &e);
            }
        }
    }
    ExitCode::SUCCESS
}

/// `exec <file> "<SQL>"`: runs one INSERT/UPDATE/DELETE/CREATE TABLE/DROP
/// TABLE/CREATE INDEX/DROP INDEX statement against a writable `Pager`
/// (#215's write-path CLI surface — Phase 4 of the V3 epic, #161).
/// Matches stock `sqlite3`'s CLI behavior of printing nothing on success
/// for a bare DML/DDL statement (no `.echo`/`-changes` flag requested).
fn run_exec(path: &Path, sql: &str) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };

    let schemas = {
        let mut schema_cursor = TableCursor::new(&pager, &header, 1);
        match read_schema(&mut schema_cursor, header.text_encoding) {
            Ok(s) => s,
            Err(e) => return fatal(path, &e),
        }
    };

    let program = match compile_statement(path, sql, &schemas) {
        Ok(p) => p,
        Err(code) => return code,
    };

    match execute_with_writable_db(&program, pager, header) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fatal(path, &e),
    }
}

/// The first one or two whitespace-separated words of `sql`, uppercased
/// — enough to pick which statement-specific parser to hand `sql` to
/// (`CREATE TABLE` vs `CREATE INDEX`/`CREATE UNIQUE INDEX`, `DROP TABLE`
/// vs `DROP INDEX`), without re-tokenizing the whole statement twice.
fn leading_keywords(sql: &str) -> Vec<String> {
    sql.split_whitespace()
        .take(3)
        .map(|w| w.to_ascii_uppercase())
        .collect()
}

fn compile_statement(
    path: &Path,
    sql: &str,
    schemas: &[sqlite_rs::schema::TableSchema],
) -> Result<sqlite_rs::vdbe::Program, ExitCode> {
    let find_schema = |name: &str| -> Result<&sqlite_rs::schema::TableSchema, ExitCode> {
        schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| fatal(path, &format!("no such table: {name}")))
    };
    let find_index_root = |name: &str| -> Result<u32, ExitCode> {
        schemas
            .iter()
            .flat_map(|s| &s.indexes)
            .find(|idx| idx.name.eq_ignore_ascii_case(name))
            .map(|idx| idx.root_page)
            .ok_or_else(|| fatal(path, &format!("no such index: {name}")))
    };

    let keywords = leading_keywords(sql);
    let kw = |i: usize| keywords.get(i).map(String::as_str).unwrap_or("");

    match kw(0) {
        "INSERT" => match parse_insert(sql) {
            ParseOutcome::Accepted(insert) => {
                let schema = find_schema(&insert.table)?;
                let select_schemas: Option<Vec<sqlite_rs::schema::TableSchema>> =
                    match &insert.source {
                        sqlite_rs::parser::ast::InsertSource::Select(select) => {
                            let Some(from) = &select.from else {
                                return Err(fatal(path, &"SELECT has no FROM clause".to_string()));
                            };
                            let mut joined_schemas = vec![find_schema(&from.first.name)?.clone()];
                            for join in &from.joins {
                                joined_schemas.push(find_schema(&join.table.name)?.clone());
                            }
                            Some(joined_schemas)
                        }
                        sqlite_rs::parser::ast::InsertSource::Values(_)
                        | sqlite_rs::parser::ast::InsertSource::DefaultValues => None,
                    };
                compile_insert(&insert, schema, select_schemas.as_deref())
                    .map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "UPDATE" => match parse_update(sql) {
            ParseOutcome::Accepted(update) => {
                let schema = find_schema(&update.table)?;
                compile_update_with_catalog(&update, schema, &schemas).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "DELETE" => match parse_delete(sql) {
            ParseOutcome::Accepted(delete) => {
                let schema = find_schema(&delete.table)?;
                compile_delete_with_catalog(&delete, schema, &schemas).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "CREATE" if kw(1) == "TABLE" => match parse_create_table(sql) {
            ParseOutcome::Accepted(create) => {
                compile_create_table(&create, sql).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "CREATE" if kw(1) == "INDEX" || kw(1) == "UNIQUE" => match parse_create_index(sql) {
            ParseOutcome::Accepted(ci) => {
                let schema = find_schema(&ci.table)?;
                compile_create_index(&ci, schema, sql).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "DROP" if kw(1) == "TABLE" => match parse_drop_table(sql) {
            ParseOutcome::Accepted(drop) => {
                let schema = find_schema(&drop.name)?;
                compile_drop_table(&drop, schema).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        "DROP" if kw(1) == "INDEX" => match parse_drop_index(sql) {
            ParseOutcome::Accepted(di) => {
                let root_page = find_index_root(&di.name)?;
                compile_drop_index(&di, root_page).map_err(|e| fatal(path, &e))
            }
            other => Err(fatal(path, &format!("{other:?}"))),
        },
        other => Err(fatal(
            path,
            &format!("unsupported or unrecognized statement: {other:?} ..."),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_component_strips_path_traversal() {
        assert_eq!(sanitize_filename_component("normal_name"), "normal_name");
        assert_eq!(
            sanitize_filename_component("../../etc/passwd"),
            "______etc_passwd"
        );
        assert_eq!(sanitize_filename_component("/etc/passwd"), "_etc_passwd");
        assert_eq!(sanitize_filename_component(""), "table");
        assert_eq!(sanitize_filename_component("..."), "___");
    }
}
