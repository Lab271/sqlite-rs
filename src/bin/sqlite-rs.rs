//! `sqlite-rs` CLI: `dump` and `export` subcommands (issue #37, V1 step
//! 9 — the acceptance gate). Data goes to stdout (`dump`) or disk
//! (`export`); anything gracefully skipped goes to stderr as a warning.
//! Dot-commands, a REPL, and `.import` are explicit non-goals (CLI
//! level 3, a later value block) — see the issue body.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sqlite_rs::dump::{dump_database, DumpError};
use sqlite_rs::format::{format_csv_value, format_list_value};
use sqlite_rs::vfs::UnixVfs;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("dump") => match args.next() {
            Some(path) => run_dump(Path::new(&path)),
            None => usage_error("dump <file>"),
        },
        Some("export") => match args.next() {
            Some(path) => run_export(Path::new(&path)),
            None => usage_error("export <file>"),
        },
        _ => usage_error("<dump|export> <file>"),
    }
}

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
    ExitCode::SUCCESS
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

    for table in &result.tables {
        let out_path: PathBuf = dir.join(format!("{}_{stem}.csv", table.name));
        let mut out = String::new();
        out.push_str(&table.columns.join(","));
        out.push('\n');
        for row in &table.rows {
            let rendered: Vec<String> = row.iter().map(format_csv_value).collect();
            out.push_str(&rendered.join(","));
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&out_path, out) {
            eprintln!("warning: table {:?}: writing {out_path:?}: {e}", table.name);
            continue;
        }
        eprintln!("wrote {} ({} rows)", out_path.display(), table.rows.len());
    }

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    ExitCode::SUCCESS
}

fn fatal(path: &Path, e: &DumpError) -> ExitCode {
    eprintln!("error: {}: {e}", path.display());
    ExitCode::FAILURE
}
