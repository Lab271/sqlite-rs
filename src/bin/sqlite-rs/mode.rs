// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `.mode`/`.headers` REPL state (#495): a small `OutputMode` enum plus
//! one dispatch function (`print_rows`) the REPL's own result-set
//! printer routes every `SELECT` through, replacing the single
//! hardcoded `write_list_row` call `run_repl` used before this ticket.
//!
//! This only affects the REPL's interactive printing — `query`/`exec`
//! (one-shot CLI entry points, their own `-csv` flag already) are
//! untouched, per the issue's explicit scope-down.

use std::io::{self, Write};

use sqlite_rs::format::{csv_quote, format_csv_value, format_query_value};
use sqlite_rs::record::Value;

use crate::common::CSV_ROW_TERMINATOR;
use crate::query::write_list_row;

/// The REPL's `.mode` setting. `List` is the pre-existing default
/// (pipe-delimited, `write_list_row`); the other three are new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    #[default]
    List,
    Csv,
    Column,
    Line,
}

impl OutputMode {
    /// Parses a `.mode` argument (`list`/`csv`/`column`/`line`,
    /// case-insensitive) — `None` for anything else, left to the
    /// caller to report as an unknown mode.
    pub fn parse(arg: &str) -> Option<Self> {
        match arg.to_ascii_lowercase().as_str() {
            "list" => Some(Self::List),
            "csv" => Some(Self::Csv),
            "column" => Some(Self::Column),
            "line" => Some(Self::Line),
            _ => None,
        }
    }
}

/// Renders `v` for `column`/`line` mode display: reuses
/// `format_query_value`'s `-list`-mode rendering (empty string for
/// `NULL`, NUL-truncated text/blobs) converted lossily to `String` —
/// both modes are terminal-display renderers, not `-list`'s
/// binary-safe byte stream, so lossy UTF-8 is an acceptable
/// approximation here.
fn cell_string(v: &Value) -> String {
    String::from_utf8_lossy(&format_query_value(v)).into_owned()
}

/// Prints one REPL result set (`columns` — see
/// `crate::repl::derive_headers` — and `rows`) in `mode`, with a
/// leading header row when `headers` is set. The single entry point
/// `repl.rs`'s `run_one_statement` routes every `SELECT`'s output
/// through, so `.mode`/`.headers` state lives in exactly one place.
pub fn print_rows(
    out: &mut impl Write,
    mode: OutputMode,
    headers: bool,
    columns: &[String],
    rows: &[Vec<Value>],
) -> io::Result<()> {
    match mode {
        OutputMode::List => print_list(out, headers, columns, rows),
        OutputMode::Csv => print_csv(out, headers, columns, rows),
        OutputMode::Column => print_column(out, headers, columns, rows),
        OutputMode::Line => print_line(out, columns, rows),
    }
}

fn print_list(
    out: &mut impl Write,
    headers: bool,
    columns: &[String],
    rows: &[Vec<Value>],
) -> io::Result<()> {
    if headers {
        let rendered: Vec<Vec<u8>> = columns.iter().map(|c| c.clone().into_bytes()).collect();
        write_list_row(out, &rendered)?;
    }
    for row in rows {
        let rendered: Vec<Vec<u8>> = row.iter().map(format_query_value).collect();
        write_list_row(out, &rendered)?;
    }
    Ok(())
}

fn print_csv(
    out: &mut impl Write,
    headers: bool,
    columns: &[String],
    rows: &[Vec<Value>],
) -> io::Result<()> {
    if headers {
        let rendered: Vec<String> = columns.iter().map(|c| csv_quote(c)).collect();
        write!(out, "{}{CSV_ROW_TERMINATOR}", rendered.join(","))?;
    }
    for row in rows {
        let rendered: Vec<String> = row.iter().map(format_csv_value).collect();
        write!(out, "{}{CSV_ROW_TERMINATOR}", rendered.join(","))?;
    }
    Ok(())
}

/// Fixed-width column rendering: each column's width is the longest
/// rendered value in it (header included, when shown), padded with a
/// 2-space gap. This is a reasonable approximation of `sqlite3`'s own
/// `.mode column` (which auto-sizes from sampled row data with its own
/// heuristics) rather than a byte-exact match — no attempt is made to
/// reproduce its default truncation-at-a-fixed-width behavior.
fn print_column(
    out: &mut impl Write,
    headers: bool,
    columns: &[String],
    rows: &[Vec<Value>],
) -> io::Result<()> {
    let num_cols = columns.len();
    let mut widths = vec![0usize; num_cols];
    if headers {
        for (w, c) in widths.iter_mut().zip(columns.iter()) {
            *w = (*w).max(c.len());
        }
    }
    let rendered_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(cell_string).collect())
        .collect();
    for row in &rendered_rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }

    let write_row = |out: &mut dyn Write, cells: &[String]| -> io::Result<()> {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let width = widths.get(i).copied().unwrap_or(0);
            if i.saturating_add(1) == num_cols {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  "));
            }
        }
        writeln!(out, "{line}")
    };

    if headers {
        write_row(out, columns)?;
        let dashes: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
        write_row(out, &dashes)?;
    }
    for row in &rendered_rows {
        write_row(out, row)?;
    }
    Ok(())
}

/// One `column_name = value` line per column, a blank line between
/// rows (not `.headers`-gated — `.mode line` always labels every
/// value with its column name, matching stock `sqlite3`).
fn print_line(out: &mut impl Write, columns: &[String], rows: &[Vec<Value>]) -> io::Result<()> {
    let name_width = columns.iter().map(String::len).max().unwrap_or(0);
    for (row_i, row) in rows.iter().enumerate() {
        if row_i > 0 {
            writeln!(out)?;
        }
        for (i, value) in row.iter().enumerate() {
            let name = columns.get(i).map(String::as_str).unwrap_or("");
            writeln!(out, "{name:<name_width$} = {}", cell_string(value))?;
        }
    }
    Ok(())
}
