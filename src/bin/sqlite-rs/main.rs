//! `sqlite-rs` CLI: `dump`, `export`, and `query` subcommands (issues
//! #37, #95 — the V1 and V2 acceptance gates). Data goes to stdout
//! (`dump`, `query`) or disk (`export`); anything gracefully skipped
//! goes to stderr as a warning. `query`'s own flags (`-csv`,
//! `-explain`) deliberately use `sqlite3`'s single-dash option style
//! rather than GNU `--long` flags, matching the interface it stays
//! parity with.
//!
//! `repl` (#365) is a deliberately minimal read-eval-print loop, added
//! once V5's transaction control (#356/#360) needed a session to be
//! observable in at all — `.import` and full dot-command parity with
//! the stock `sqlite3` shell remain non-goals (see `repl.rs`'s module
//! doc for exactly what's in/out of scope). #478 makes it the *default*
//! mode too: `sqlite-rs <file>` with no recognized subcommand enters the
//! REPL directly, matching `sqlite3 <file>`.

mod common;
mod dump;
mod exec;
mod pragma_query;
mod query;
mod repl;
mod tables;

use std::path::Path;
use std::process::ExitCode;

use common::usage_error;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("sqlite-rs {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("dump") => match args.next() {
            Some(path) => dump::run_dump(Path::new(&path)),
            None => usage_error("dump <file>"),
        },
        Some("export") => match args.next() {
            Some(path) => dump::run_export(Path::new(&path)),
            None => usage_error("export <file>"),
        },
        Some("query") => query::run_query(args.collect()),
        Some("tables") => match args.next() {
            Some(path) => tables::run_tables(Path::new(&path), args.next().as_deref()),
            None => usage_error("tables <file> [PATTERN]"),
        },
        Some("exec") => {
            let (Some(path), Some(sql)) = (args.next(), args.next()) else {
                return usage_error("exec <file> \"<SQL>\"");
            };
            exec::run_exec(Path::new(&path), &sql)
        }
        Some("repl") => match args.next() {
            Some(path) => repl::run_repl(Path::new(&path)),
            None => usage_error("repl <file>"),
        },
        // No recognized subcommand: treat a single bare argument as a
        // database file and enter the REPL directly, matching
        // `sqlite3 <file>` — but a second stray argument (not a real
        // invocation shape) still reports the usage error rather than
        // silently discarding it.
        Some(path) if args.next().is_none() => repl::run_repl(Path::new(&path)),
        _ => usage_error("[--version] <dump|export|query|tables|exec|repl> <file>"),
    }
}
