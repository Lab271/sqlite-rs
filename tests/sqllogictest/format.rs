//! Parser for the sqllogictest text format (`statement ok/error`,
//! `query <types> <sort> [label]` + SQL + `----` + expected block),
//! as used by the vendored files under
//! `tests/corpus/sql/vendor/sqllogictest/test/` (#70). See
//! `.openspec/specs/004-corpus/spec.md` Requirement 4.
//!
//! `onlyif <engine>`/`skipif <engine>` conditionals are resolved here,
//! not left for the runner: this crate only ever plays the role of
//! `sqlite` (the pinned oracle *is* `sqlite3`), so a block gated to
//! another engine is dropped from the returned record list entirely.
//! `hash-threshold N` directives are recognized and discarded — this
//! runner never generates an expected block, it only compares against
//! whichever representation (literal values or a hash) the file
//! already committed to, so the threshold itself is never consulted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    NoSort,
    RowSort,
    ValueSort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    /// Literal expected values, one per (flattened, row-major) output line.
    Values(Vec<String>),
    /// `"<count> values hashing to <digest>"`.
    Hash { count: usize, digest: String },
}

#[derive(Debug, Clone)]
pub struct StatementRecord {
    pub expect_ok: bool,
    pub sql: String,
}

#[derive(Debug, Clone)]
pub struct QueryRecord {
    pub line: usize,
    pub type_string: String,
    pub sort_mode: SortMode,
    pub sql: String,
    pub expected: Expected,
}

#[derive(Debug, Clone)]
pub enum Record {
    Statement(StatementRecord),
    Query(QueryRecord),
}

/// `onlyif`/`skipif` name a target engine; trailing `# comment` text is
/// allowed and ignored (observed in the vendored corpus, e.g. `onlyif
/// sqlite # empty RHS`).
fn engine_condition_holds(directive: &str, rest: &str) -> bool {
    let engine = rest.split('#').next().unwrap_or("").trim().to_lowercase();
    match directive {
        "onlyif" => engine == "sqlite",
        "skipif" => engine != "sqlite",
        _ => true,
    }
}

/// Splits `text` into blank-line-separated blocks, each block's lines
/// still in order. Comment lines (`# ...`) standing alone between
/// blocks are dropped; a leading `#`-comment *inside* a block (there
/// are none in the vendored corpus) would be preserved as a literal
/// line, since only whole-line comments between records are noise.
fn blocks(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_start = 0usize;
    for (i, line) in text.lines().enumerate() {
        let line_no = i.saturating_add(1);
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push((current_start, std::mem::take(&mut current)));
            }
            continue;
        }
        if current.is_empty() && line.starts_with('#') {
            continue;
        }
        if current.is_empty() {
            current_start = line_no;
        }
        current.push(line);
    }
    if !current.is_empty() {
        out.push((current_start, current));
    }
    out
}

fn parse_sort_mode(s: &str) -> Option<SortMode> {
    match s {
        "nosort" => Some(SortMode::NoSort),
        "rowsort" => Some(SortMode::RowSort),
        "valuesort" => Some(SortMode::ValueSort),
        _ => None,
    }
}

/// Parses a single `"<n> values hashing to <hex>"` expected line, or
/// falls back to treating every line of `lines` as a literal expected
/// value.
fn parse_expected(lines: &[&str]) -> Expected {
    if let [only] = lines {
        if let Some((count_str, digest)) = only.split_once(" values hashing to ") {
            if let Ok(count) = count_str.trim().parse::<usize>() {
                return Expected::Hash {
                    count,
                    digest: digest.trim().to_string(),
                };
            }
        }
    }
    Expected::Values(lines.iter().map(|s| s.to_string()).collect())
}

/// Parses the full text of one vendored `.test` file into its
/// applicable-to-`sqlite` records, in file order. Line numbers are
/// 1-based and point at the record's header line (`statement ...` /
/// `query ...`), for diagnostics.
pub fn parse_script(text: &str) -> Vec<Record> {
    let mut records = Vec::new();

    for (block_start_line, block) in blocks(text) {
        // A conditional/`hash-threshold` directive shares this block
        // only when it was authored on its own line directly above the
        // header (the vendored corpus's convention); walk past any of
        // those to find the real header line, tracking how many were
        // consumed so `header_line` still points at it.
        let mut consumed = 0usize;
        let mut applicable = true;
        while let Some(&line) = block.get(consumed) {
            let mut words = line.split_whitespace();
            match words.next() {
                Some(directive @ ("onlyif" | "skipif")) => {
                    let rest_of_line = line.splitn(2, char::is_whitespace).nth(1).unwrap_or("");
                    if !engine_condition_holds(directive, rest_of_line) {
                        applicable = false;
                    }
                    consumed = consumed.saturating_add(1);
                }
                Some("hash-threshold") => {
                    consumed = consumed.saturating_add(1);
                }
                _ => break,
            }
        }
        let header_line = block_start_line.saturating_add(consumed);
        let Some((&header, rest)) = block[consumed..].split_first() else {
            continue;
        };
        if !applicable {
            continue;
        }

        let mut words = header.split_whitespace();
        match words.next() {
            Some("statement") => {
                let expect_ok = match words.next() {
                    Some("ok") => true,
                    Some("error") => false,
                    _ => continue,
                };
                let sql = rest.join("\n");
                records.push(Record::Statement(StatementRecord { expect_ok, sql }));
            }
            Some("query") => {
                let Some(type_string) = words.next() else {
                    continue;
                };
                let Some(sort_mode) = words.next().and_then(parse_sort_mode) else {
                    continue;
                };
                // An optional trailing label (e.g. `label-0`) is
                // otherwise unused here — this runner only compares
                // per-file, per-line results, and doesn't currently
                // cross-check same-label queries against each other.

                let Some(separator_pos) = rest.iter().position(|line| *line == "----") else {
                    continue;
                };
                let sql = rest[..separator_pos].join("\n");
                let expected = parse_expected(&rest[separator_pos.saturating_add(1)..]);

                records.push(Record::Query(QueryRecord {
                    line: header_line,
                    type_string: type_string.to_string(),
                    sort_mode,
                    sql,
                    expected,
                }));
            }
            _ => {}
        }
    }

    records
}
