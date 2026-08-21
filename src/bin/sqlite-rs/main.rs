//! `sqlite-rs` CLI: `dump`, `export`, and `query` subcommands (issues
//! #37, #95 — the V1 and V2 acceptance gates). Data goes to stdout
//! (`dump`, `query`) or disk (`export`); anything gracefully skipped
//! goes to stderr as a warning. Dot-commands, a REPL, and `.import` are
//! explicit non-goals (CLI level 3, a later value block) — see the
//! issue bodies. `query`'s own flags (`-csv`, `-explain`) deliberately
//! use `sqlite3`'s single-dash option style rather than GNU `--long`
//! flags, matching the interface it stays parity with.

mod common;
mod dump;
mod exec;
mod query;
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
        _ => usage_error("[--version] <dump|export|query|tables|exec> <file>"),
    }
}
