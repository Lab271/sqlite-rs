//! Small helpers shared by every subcommand module: exit-code plumbing
//! and the CSV row terminator convention.

use std::path::Path;
use std::process::ExitCode;

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
pub const CSV_ROW_TERMINATOR: &str = "\r\n";

pub fn usage_error(expected: &str) -> ExitCode {
    eprintln!("usage: sqlite-rs {expected}");
    ExitCode::from(2)
}

pub fn fatal(path: &Path, e: &impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {}: {e}", path.display());
    ExitCode::FAILURE
}

/// `dump`/`export` still print/write everything they successfully read
/// even when some tables were gracefully skipped — but a caller checking
/// only the exit code needs a way to detect that the output is partial.
pub fn degraded_exit_code(clean: bool) -> ExitCode {
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
