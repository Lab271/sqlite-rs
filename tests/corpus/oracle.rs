//! Pinned-oracle constants, plus the shared helpers for invoking a live
//! read-only `sqlite3` as a differential oracle.
//!
//! Version/codec enforcement happens in `tools/gen_fixtures.sh` at
//! fixture-generation time, not here: `harness.rs` reads only committed
//! fixtures and never shells out. The live-oracle helpers below are the
//! scoped exception to that, used by `dump_oracle_test.rs` (library-level
//! diff) and `cli_e2e_test.rs` (through the CLI binary). They are always
//! `-readonly` so a fixture can never be mutated, and callers are
//! expected to gate on [`sqlite3_available`] and skip rather than fail
//! when no oracle is present. See `.openspec/specs/004-corpus/spec.md`
//! Requirement 1.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Must equal Cargo.toml's `[package.metadata.oracle] version` — a
/// `const` cannot read it at run time, so `make version-pin` enforces
/// the agreement.
pub const ORACLE_VERSION: &str = "3.53.4";

pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/fixtures")
}

pub fn gen_fixtures_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/gen_fixtures.sh")
}

pub fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/support")
}

/// Locates a `sqlite3` binary whose version matches [`ORACLE_VERSION`],
/// searching the same candidates as `tools/gen_fixtures.sh`'s
/// `find_oracle()` and honouring the same `ORACLE_SQLITE3` override.
///
/// Pinning matters more than convenience here. A bare `sqlite3` off
/// `PATH` is whatever the OS ships — on macOS that is `/usr/bin/sqlite3`,
/// an Apple build that `gen_fixtures.sh` explicitly refuses (it is
/// codec-enabled) and whose `-csv` mode terminates rows with `\n` where
/// upstream 3.53.4 uses `\r\n`. Diffing against it silently passed a real
/// line-ending bug in this crate's CSV output for the entire life of
/// `dump_oracle_test.rs`. An oracle that isn't the pinned oracle isn't an
/// oracle.
pub fn pinned_oracle() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(env_override) = std::env::var("ORACLE_SQLITE3") {
        candidates.push(PathBuf::from(env_override));
    }
    candidates.push(PathBuf::from("/opt/homebrew/opt/sqlite/bin/sqlite3"));
    candidates.push(PathBuf::from("/usr/local/opt/sqlite/bin/sqlite3"));
    candidates.push(PathBuf::from("sqlite3"));

    candidates.into_iter().find(|candidate| {
        let version_matches = Command::new(candidate)
            .arg("-version")
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).starts_with(ORACLE_VERSION)
            });
        version_matches && !is_codec_build(candidate)
    })
}

/// Rejects codec-enabled builds (e.g. macOS's system `/usr/bin/sqlite3`)
/// even when their reported version happens to match [`ORACLE_VERSION`].
/// A version match alone isn't sufficient: codec builds are a distinct
/// binary lineage that can diverge in output formatting (see the
/// `\n`-vs-`\r\n` CSV bug documented on [`pinned_oracle`]), so `PRAGMA
/// compile_options` is checked the same way `tools/harvest_opcodes.py`'s
/// `find_oracle()` does.
fn is_codec_build(candidate: &Path) -> bool {
    Command::new(candidate)
        .arg("-readonly")
        .arg(":memory:")
        .arg("PRAGMA compile_options;")
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("codec")
        })
}

/// Prints a uniform skip notice for tests gated on [`pinned_oracle`].
/// Tests skip green rather than failing when the pinned oracle is absent,
/// so a machine without it still passes `make test-corpus` — but note
/// that makes the oracle diffs only as strong as the environment they run
/// in, which is why CI ought to install the pinned build.
pub fn skip_no_oracle(test: &str) {
    eprintln!("skipping {test}: no sqlite3 {ORACLE_VERSION} found (set ORACLE_SQLITE3)");
}

/// A `SELECT` list naming each of `columns`, wrapping BLOB-valued columns
/// in `quote()` so they come back as `X'..'` literals — `-list`/`-csv`
/// mode cannot print raw bytes at all. The choice is made per row via
/// `typeof()`, not from the column's *declared* type, since a declared
/// type doesn't constrain each row's dynamic storage class (e.g. the
/// serial-type-8/9 REAL-as-integer-constant case `src/dump.rs` also
/// accounts for).
pub fn blob_coercing_select_list(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| format!("(case when typeof(\"{c}\")='blob' then quote(\"{c}\") else \"{c}\" end)"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Runs `sqlite3 -readonly <mode_args> <db> <sql>` and returns its stdout.
/// Always `-readonly`: the oracle must never be able to mutate a
/// committed fixture the way an accidental read-write open could.
pub fn run_oracle(oracle: &Path, db: &Path, mode_args: &[&str], sql: &str) -> String {
    let output = Command::new(oracle)
        .arg("-readonly")
        .args(mode_args)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running sqlite3 oracle on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "sqlite3 oracle failed on {}: {}",
        db.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The oracle's `-list` rendering of every row of `table`.
pub fn oracle_list_output(oracle: &Path, db: &Path, table: &str, columns: &[String]) -> String {
    let sql = format!(
        "select {} from \"{table}\"",
        blob_coercing_select_list(columns)
    );
    run_oracle(
        oracle,
        db,
        &["-list", "-separator", "|", "-nullvalue", "NULL"],
        &sql,
    )
}

/// The oracle's `-csv` rendering of every row of `table`, without a header
/// row.
pub fn oracle_csv_output(oracle: &Path, db: &Path, table: &str, columns: &[String]) -> String {
    let sql = format!(
        "select {} from \"{table}\"",
        blob_coercing_select_list(columns)
    );
    run_oracle(oracle, db, &["-csv"], &sql)
}

/// The oracle's `-csv -header` rendering of `table` — header row included,
/// i.e. exactly what the `sqlite-rs export` subcommand writes to disk.
///
/// The header must come from `AS` aliases: the `SELECT` list wraps each
/// column in a `case` expression, and without an alias `sqlite3` would
/// name the header column after that whole expression rather than the
/// column. Aliasing restores the real column names, so the header line
/// this returns is the one a plain `select *` would produce.
pub fn oracle_csv_with_header_output(
    oracle: &Path,
    db: &Path,
    table: &str,
    columns: &[String],
) -> String {
    let select_list = columns
        .iter()
        .map(|c| {
            format!(
                "(case when typeof(\"{c}\")='blob' then quote(\"{c}\") else \"{c}\" end) as \"{c}\""
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("select {select_list} from \"{table}\"");
    run_oracle(oracle, db, &["-csv", "-header"], &sql)
}
