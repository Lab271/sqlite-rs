//! `repl <file>`: a minimal read-eval-print loop (#365) — the CLI
//! surface transaction control (#356/#360) actually needs. `exec`
//! (#358) already runs a `;`-separated multi-statement *script* in one
//! shot; this is the same session machinery (one shared
//! `Rc<RefCell<Pager>>` + autocommit flag, `split_statements`,
//! `execute_transaction_step`) driven interactively from stdin instead,
//! so `BEGIN`/a write/`SELECT`/`COMMIT`/`ROLLBACK` can be typed one at a
//! time and see each other's effects — including an uncommitted write,
//! which `exec`'s one-shot-per-process model has no way to demonstrate
//! (`SELECT` here reads through the *same* shared `Pager`, not a fresh
//! read-only one, precisely so that's true).
//!
//! Deliberately minimal, per the issue's explicit scope-down: only
//! `.quit`/`.exit`/`.tables` as dot-commands (#478 adds `.tables` and
//! `sqlite3`-style prefix matching — `.t`..`.tables`, `.q`..`.quit` — to
//! the `.quit`/`.exit` this module started with), no readline/history, no
//! `-csv`/`-explain`/`EXPLAIN QUERY PLAN` (those stay `query`-only). A `;`
//! inside a string/blob literal never ends a statement early —
//! `ends_with_semicolon` goes through the real tokenizer, not a
//! newline-oblivious `str::ends_with(';')`.

use std::cell::RefCell;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_statement, leading_keywords};
use sqlite_rs::dump;
use sqlite_rs::format::format_query_value;
use sqlite_rs::parser::{ends_with_semicolon, parse_select, split_statements, ParseOutcome};
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::{execute_transaction_step, execute_with_db};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::pragma_query::{execute_pragma_query, parse_pragma_query};
use crate::query::{compile_select_program, write_list_row, SelectOutcome};
use crate::tables::{list_table_and_view_names, print_table_names};

pub fn run_repl(path: &Path) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let pager = Rc::new(RefCell::new(pager));

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut autocommit = true;
    let mut buffer = String::new();

    loop {
        print_prompt(&buffer);
        let Some(line) = lines.next() else {
            break; // EOF (e.g. piped input, or Ctrl-D) ends the session.
        };
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error: reading stdin: {e}");
                break;
            }
        };

        if buffer.is_empty() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix('.') {
                // sqlite3-style prefix matching: `.t`/`.ta`/.../`.tables`
                // all resolve to `.tables`, `.q`/`.qu`/.../`.quit` to
                // `.quit`; `.exit` stays its own exact alias.
                if trimmed == ".exit" || (!rest.is_empty() && "quit".starts_with(rest)) {
                    break;
                }
                if !rest.is_empty() && "tables".starts_with(rest) {
                    match list_table_and_view_names(
                        Rc::clone(&pager) as Rc<dyn PageSource>,
                        &header,
                        None,
                    ) {
                        Ok(names) => print_table_names(&names),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    continue;
                }
                eprintln!("Error: unknown command {trimmed:?}");
                continue;
            }
        }

        buffer.push_str(&line);
        buffer.push('\n');
        if !ends_with_semicolon(&buffer) {
            continue;
        }

        for stmt in split_statements(&buffer) {
            run_one_statement(&stmt, &pager, header, &mut autocommit, path);
        }
        buffer.clear();
    }

    ExitCode::SUCCESS
}

fn print_prompt(buffer: &str) {
    let prompt = if buffer.is_empty() {
        "sqlite> "
    } else {
        "   ...> "
    };
    print!("{prompt}");
    // Best-effort: a flush failure here (e.g. a closed pipe) isn't
    // worth aborting the loop over — the next read from stdin will
    // surface the real problem if there is one.
    io::stdout().flush().ok();
}

/// Runs one already-complete statement against the session's shared
/// `pager`, printing rows (for a `SELECT`) or an `Error: ...` line to
/// stderr on failure — never panics, never exits the loop, matching
/// `sqlite3`'s own shell behavior of surviving a bad statement.
fn run_one_statement(
    stmt: &str,
    pager: &Rc<RefCell<sqlite_rs::pager::Pager>>,
    header: sqlite_rs::header::DatabaseHeader,
    autocommit: &mut bool,
    db_path: &Path,
) {
    // #489: checked before anything else, same as `query.rs`'s
    // `run_query` — a `PRAGMA` outside these 9 recognized names (e.g.
    // `journal_mode`) falls through unrecognized and hits the ordinary
    // `compile_statement` write-pragma path below, unchanged.
    if let Some(pragma) = parse_pragma_query(stmt) {
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            match read_schema(&mut schema_cursor, header.text_encoding) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return;
                }
            }
        };
        let views = {
            let borrowed = pager.borrow();
            let mut view_cursor = TableCursor::new(&*borrowed, &header, 1);
            match read_views(&mut view_cursor, header.text_encoding) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return;
                }
            }
        };
        match execute_pragma_query(&pragma, &schemas, &views, &header, db_path) {
            Ok(rows) => {
                let mut stdout = io::BufWriter::new(io::stdout().lock());
                for row in rows {
                    let rendered: Vec<Vec<u8>> = row.into_iter().map(String::into_bytes).collect();
                    if let Err(e) = write_list_row(&mut stdout, &rendered) {
                        eprintln!("Error: {e}");
                        return;
                    }
                }
                stdout.flush().ok();
            }
            Err(e) => eprintln!("Error: {e}"),
        }
        return;
    }

    let schemas = {
        let borrowed = pager.borrow();
        let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
        match read_schema(&mut schema_cursor, header.text_encoding) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return;
            }
        }
    };
    let views = {
        let borrowed = pager.borrow();
        let mut view_cursor = TableCursor::new(&*borrowed, &header, 1);
        match read_views(&mut view_cursor, header.text_encoding) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {e}");
                return;
            }
        }
    };
    let stats_by_table = {
        let borrowed = pager.borrow();
        sqlite_rs::planner::load_stats(&*borrowed, &header, &schemas)
    };

    let keywords = leading_keywords(stmt);
    let is_select = keywords.first().is_some_and(|kw| kw.as_str() == "SELECT");

    if is_select {
        let select = match parse_select(stmt) {
            ParseOutcome::Accepted(select) => *select,
            ParseOutcome::Unsupported { message, span } => {
                eprintln!(
                    "Error: not yet supported (line {}, column {}): {message}",
                    span.line, span.column
                );
                return;
            }
            ParseOutcome::Invalid { message, span } => {
                eprintln!(
                    "Error: syntax error (line {}, column {}): {message}",
                    span.line, span.column
                );
                return;
            }
        };
        let program =
            match compile_select_program(&select, false, &schemas, &views, &stats_by_table) {
                Ok(SelectOutcome::Program(p)) => p,
                // `eqp_mode` is always `false` above, so `Eqp` never comes back.
                Ok(SelectOutcome::Eqp(_)) => unreachable!("eqp_mode was false"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    return;
                }
            };
        // Reads through the same shared `Pager` the write path uses
        // (`Rc<RefCell<Pager>>` implements `PageSource`, per
        // `src/pager.rs`) — an uncommitted write earlier in this same
        // transaction must be visible here, not just what's on disk.
        let source: Rc<dyn PageSource> = Rc::clone(pager) as Rc<dyn PageSource>;
        match execute_with_db(&program, source, header) {
            Ok(rows) => {
                let mut stdout = io::BufWriter::new(io::stdout().lock());
                for row in &rows {
                    let rendered: Vec<Vec<u8>> = row.iter().map(format_query_value).collect();
                    if let Err(e) = write_list_row(&mut stdout, &rendered) {
                        eprintln!("Error: {e}");
                        return;
                    }
                }
                stdout.flush().ok();
            }
            Err(e) => eprintln!("Error: {e}"),
        }
        return;
    }

    let program = match compile_statement(stmt, &schemas, &views) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            return;
        }
    };
    match execute_transaction_step(&program, Rc::clone(pager), header, *autocommit) {
        Ok((_, ac)) => *autocommit = ac,
        Err(e) => eprintln!("Error: {e}"),
    }
}
