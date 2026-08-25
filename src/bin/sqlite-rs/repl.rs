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
//! Deliberately minimal, per the issue's explicit scope-down: no
//! `-csv`/`-explain`/`EXPLAIN QUERY PLAN` (those stay `query`-only). A
//! `;` inside a string/blob literal never ends a statement early —
//! `ends_with_semicolon` goes through the real tokenizer, not a
//! newline-oblivious `str::ends_with(';')`.
//!
//! Line editing and history (#551): input is read through a
//! `rustyline` [`DefaultEditor`], which gives up/down arrow history
//! navigation for free and falls back to plain line reads when stdin
//! isn't a tty (piped scripts, as used by every test in this crate).
//! History persists across sessions at [`history_path`]
//! (`~/.sqlite-rs_history`); loading/saving is best-effort — a missing
//! `$HOME` or an unwritable history file never blocks the session.
//!
//! Dot-commands (#478, #495): `.quit`/`.exit`/`.tables` plus `.help`,
//! `.version`, `.schema`, `.dump`, `.headers`, `.mode`, `.databases`,
//! `.indices` — all `sqlite3`-style prefix-matched (`.t`..`.tables`,
//! `.q`..`.quit`, etc.), dispatched from the `if let Some(rest) =
//! trimmed.strip_prefix('.')` block below. `.headers`/`.mode` flip
//! `ReplState` fields the query-result printer (`mode.rs::print_rows`)
//! reads on every subsequent `SELECT`; the rest read from the database
//! (via `dot_commands.rs`) and print immediately.

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_statement, leading_keywords, output_column_names, resolve_from_table_schema,
};
use sqlite_rs::dump;
use sqlite_rs::parser::{ends_with_semicolon, parse_select, split_statements, ParseOutcome};
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::{execute_transaction_step, execute_with_db};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::dot_commands::{
    print_databases, print_dump, print_help, print_indices, print_schema, print_version,
};
use crate::mode::{print_rows, OutputMode};
use crate::pragma_query::{execute_pragma_query, parse_pragma_query};
use crate::query::{compile_select_program, write_list_row, SelectOutcome};
use crate::tables::{list_table_and_view_names, print_table_names};

/// Session state that persists across statements within one `run_repl`
/// call: `.mode`/`.headers` (#495) alongside the pre-existing
/// `autocommit` flag threaded through the transaction-control machinery.
struct ReplState {
    mode: OutputMode,
    headers: bool,
    autocommit: bool,
}

pub fn run_repl(path: &Path) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let pager = Rc::new(RefCell::new(pager));

    let mut editor = match DefaultEditor::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: initializing line editor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let history_file = history_path();
    if let Some(history_file) = &history_file {
        // Best-effort: a fresh install has no history file yet, and
        // that's not an error.
        editor.load_history(history_file).ok();
    }
    let mut state = ReplState {
        mode: OutputMode::List,
        headers: false,
        autocommit: true,
    };
    let mut buffer = String::new();

    loop {
        let line = match editor.readline(prompt_str(&buffer)) {
            Ok(l) => l,
            Err(ReadlineError::Eof) => break, // Ctrl-D, or piped input exhausted.
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: abandon the in-progress line/statement, same
                // as `sqlite3`'s own shell, and start fresh.
                buffer.clear();
                continue;
            }
            Err(e) => {
                eprintln!("error: reading input: {e}");
                break;
            }
        };
        if !line.trim().is_empty() {
            editor.add_history_entry(line.as_str()).ok();
        }

        if buffer.is_empty() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix('.') {
                if trimmed == ".exit" {
                    break;
                }
                let mut parts = rest.splitn(2, char::is_whitespace);
                let cmd = parts.next().unwrap_or("");
                let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());

                if !cmd.is_empty() && "quit".starts_with(cmd) {
                    break;
                }
                if !cmd.is_empty() && "tables".starts_with(cmd) {
                    match list_table_and_view_names(
                        Rc::clone(&pager) as Rc<dyn PageSource>,
                        &header,
                        arg,
                    ) {
                        Ok(names) => print_table_names(&names),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    continue;
                }
                if !cmd.is_empty() && "help".starts_with(cmd) {
                    print_help();
                    continue;
                }
                if !cmd.is_empty() && "version".starts_with(cmd) {
                    print_version();
                    continue;
                }
                if !cmd.is_empty() && "schema".starts_with(cmd) {
                    print_schema(&pager, &header, arg);
                    continue;
                }
                if !cmd.is_empty() && "indices".starts_with(cmd) {
                    print_indices(&pager, &header, arg);
                    continue;
                }
                if !cmd.is_empty() && "databases".starts_with(cmd) {
                    print_databases(path);
                    continue;
                }
                if !cmd.is_empty() && "dump".starts_with(cmd) {
                    print_dump(path, arg);
                    continue;
                }
                if !cmd.is_empty() && "headers".starts_with(cmd) {
                    match arg.map(str::to_ascii_lowercase).as_deref() {
                        Some("on") => state.headers = true,
                        Some("off") => state.headers = false,
                        _ => eprintln!("Error: usage: .headers on|off"),
                    }
                    continue;
                }
                if !cmd.is_empty() && "mode".starts_with(cmd) {
                    match arg.and_then(OutputMode::parse) {
                        Some(m) => state.mode = m,
                        None => eprintln!("Error: usage: .mode csv|column|line|list"),
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
            run_one_statement(&stmt, &pager, header, &mut state, path);
        }
        buffer.clear();
    }

    if let Some(history_file) = &history_file {
        editor.save_history(history_file).ok();
    }

    ExitCode::SUCCESS
}

fn prompt_str(buffer: &str) -> &'static str {
    if buffer.is_empty() {
        "sqlite> "
    } else {
        "   ...> "
    }
}

/// `~/.sqlite-rs_history`, or `None` if `$HOME` isn't set — persisting
/// history is a nice-to-have, never a reason to fail the session.
fn history_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".sqlite-rs_history"))
}

/// Runs one already-complete statement against the session's shared
/// `pager`, printing rows (for a `SELECT`) or an `Error: ...` line to
/// stderr on failure — never panics, never exits the loop, matching
/// `sqlite3`'s own shell behavior of surviving a bad statement.
fn run_one_statement(
    stmt: &str,
    pager: &Rc<RefCell<sqlite_rs::pager::Pager>>,
    header: sqlite_rs::header::DatabaseHeader,
    state: &mut ReplState,
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
        let columns = derive_headers(&select, &schemas);
        match execute_with_db(&program, source, header) {
            Ok(rows) => {
                let mut stdout = io::BufWriter::new(io::stdout().lock());
                if let Err(e) = print_rows(&mut stdout, state.mode, state.headers, &columns, &rows)
                {
                    eprintln!("Error: {e}");
                    return;
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
    match execute_transaction_step(&program, Rc::clone(pager), header, state.autocommit) {
        Ok((_, ac)) => state.autocommit = ac,
        Err(e) => eprintln!("Error: {e}"),
    }
}

/// `.headers on`'s column labels for `select`'s result set: for a
/// single-table, non-compound `SELECT` this is
/// [`output_column_names`]'s "alias, else bare column name, else
/// `columnN`" rule against the resolved `FROM` table; anything the
/// codegen pipeline resolves less directly (no `FROM`, a join, or a
/// compound) falls back to positional `column1..columnN` labels — a
/// scope-cut noted in the issue's write-up rather than plumbing this
/// REPL's header derivation through the full join/compound resolver.
fn derive_headers(
    select: &sqlite_rs::parser::ast::Select,
    schemas: &[sqlite_rs::schema::TableSchema],
) -> Vec<String> {
    let single_table = select.compound.is_empty()
        && select
            .from
            .as_ref()
            .is_some_and(|from| from.joins.is_empty());
    if single_table {
        if let Some(from) = &select.from {
            if let Ok(schema) = resolve_from_table_schema(&from.first, schemas) {
                return output_column_names(select, &schema);
            }
        }
    }
    let count = select.columns.len().max(1);
    (1..=count).map(|i| format!("column{i}")).collect()
}
