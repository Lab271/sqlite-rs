// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `dump <file>` and `export <file>` subcommands: `sqlite_master`-driven
//! table dump to stdout (`-list`-style, LF-terminated) and per-table CSV
//! export to disk, both through [`dump_database`].

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sqlite_rs::dump::dump_database;
use sqlite_rs::format::{csv_quote, format_csv_value, format_list_value};
use sqlite_rs::vfs::UnixVfs;

use crate::common::{degraded_exit_code, fatal, CSV_ROW_TERMINATOR};

pub fn run_dump(path: &Path) -> ExitCode {
    let result = match dump_database(&UnixVfs, path) {
        Ok(r) => r,
        Err(e) => return fatal(path, &e),
    };

    let mut out = BufWriter::new(io::stdout().lock());
    for table in &result.tables {
        if let Err(e) = writeln!(out, "{}", table.sql) {
            return fatal(path, &e);
        }
        for row in &table.rows {
            let rendered: Vec<String> = row.iter().map(format_list_value).collect();
            if let Err(e) = writeln!(out, "{}", rendered.join("|")) {
                return fatal(path, &e);
            }
        }
    }
    if let Err(e) = out.flush() {
        return fatal(path, &e);
    }

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    degraded_exit_code(result.warnings.is_empty())
}

pub fn run_export(path: &Path) -> ExitCode {
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
