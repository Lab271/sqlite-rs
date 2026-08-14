//! `sqlite-rs` CLI: `dump` and `export` subcommands (issue #37, V1 step
//! 9 — the acceptance gate). Data goes to stdout (`dump`) or disk
//! (`export`); anything gracefully skipped goes to stderr as a warning.
//! Dot-commands, a REPL, and `.import` are explicit non-goals (CLI
//! level 3, a later value block) — see the issue body.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sqlite_rs::dump::{dump_database, DumpError};
use sqlite_rs::format::{csv_quote, format_csv_value, format_list_value};
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

fn fatal(path: &Path, e: &DumpError) -> ExitCode {
    eprintln!("error: {}: {e}", path.display());
    ExitCode::FAILURE
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
