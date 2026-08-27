// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! REPL dot-command tests (#495): `.help`, `.version`, `.schema`,
//! `.dump`, `.headers`, `.mode`, `.databases`, `.indices` — driven
//! non-interactively by piping a script of dot-commands + SQL through
//! the `repl` subcommand's stdin and capturing stdout, the same
//! `Command`+`CARGO_BIN_EXE_sqlite-rs` pattern `tests/tiers/tier2.rs`
//! uses for `exec`/`query`.

#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use oracle::pinned_oracle;

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-repl-dotcmd-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

/// Runs one statement against `db` — via the pinned oracle when a fresh
/// `db` doesn't exist yet (`sqlite-rs exec` never creates a database
/// file, only opens an existing one — see `tests/corpus/repl_test.rs`'s
/// own `seed_db`), else via `sqlite-rs exec` itself so later statements
/// in a test still go through this crate's own write path.
fn seed(db: &Path, sql: &str) {
    if !db.exists() {
        if let Some(oracle) = pinned_oracle() {
            let status = Command::new(&oracle).arg(db).arg(sql).status().unwrap();
            assert!(status.success(), "oracle seed {sql:?} failed");
            return;
        }
    }
    let out = Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()));
    assert!(
        out.status.success(),
        "seed {sql:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Pipes `script` (one dot-command/SQL line per script line) through
/// `repl <db>`'s stdin and returns the completed process output.
fn run_repl_script(db: &Path, script: &str) -> Output {
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {CLI} repl {}: {e}", db.display()));
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(script.as_bytes())
        .expect("writing repl script");
    child.wait_with_output().expect("waiting for repl")
}

fn stdout_of(db: &Path, script: &str) -> String {
    let out = run_repl_script(db, script);
    assert!(
        out.status.success(),
        "repl script failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn help_lists_every_dot_command() {
    let db = scratch_db("help");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(&db, ".help\n.quit\n");
    for cmd in [
        ".tables",
        ".quit",
        ".exit",
        ".help",
        ".version",
        ".schema",
        ".dump",
        ".headers",
        ".mode",
        ".databases",
        ".indices",
    ] {
        assert!(out.contains(cmd), "missing {cmd:?} in .help output:\n{out}");
    }
}

#[test]
fn version_reports_crate_and_file_format_version() {
    let db = scratch_db("version");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(&db, ".version\n.quit\n");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    assert!(out.to_ascii_lowercase().contains("sqlite"), "{out}");
}

#[test]
fn schema_prints_create_statements() {
    let db = scratch_db("schema");
    seed(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
    seed(&db, "CREATE INDEX idx_t_b ON t(b)");
    seed(&db, "CREATE TABLE other(x)");

    let all = stdout_of(&db, ".schema\n.quit\n");
    assert!(all.contains("CREATE TABLE t(a INTEGER, b TEXT);"), "{all}");
    assert!(all.contains("CREATE INDEX idx_t_b ON t(b);"), "{all}");
    assert!(all.contains("CREATE TABLE other(x);"), "{all}");

    let one = stdout_of(&db, ".schema t\n.quit\n");
    assert!(one.contains("CREATE TABLE t(a INTEGER, b TEXT);"), "{one}");
    assert!(one.contains("CREATE INDEX idx_t_b ON t(b);"), "{one}");
    assert!(!one.contains("other"), "{one}");
}

#[test]
fn indices_lists_index_names_optionally_filtered() {
    let db = scratch_db("indices");
    seed(&db, "CREATE TABLE t(a, b)");
    seed(&db, "CREATE TABLE u(c)");
    seed(&db, "CREATE INDEX idx_t_a ON t(a)");
    seed(&db, "CREATE INDEX idx_u_c ON u(c)");

    let all = stdout_of(&db, ".indices\n.quit\n");
    assert!(all.contains("idx_t_a"), "{all}");
    assert!(all.contains("idx_u_c"), "{all}");

    let one = stdout_of(&db, ".indices t\n.quit\n");
    assert!(one.contains("idx_t_a"), "{one}");
    assert!(!one.contains("idx_u_c"), "{one}");
}

#[test]
fn databases_lists_the_single_main_database() {
    let db = scratch_db("databases");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(&db, ".databases\n.quit\n");
    assert!(out.contains("main"), "{out}");
}

#[test]
fn dump_emits_valid_sql_with_inserts() {
    let db = scratch_db("dump");
    seed(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
    seed(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y')");

    let out = stdout_of(&db, ".dump\n.quit\n");
    assert!(out.contains("CREATE TABLE t(a INTEGER, b TEXT);"), "{out}");
    assert!(out.contains("INSERT INTO \"t\" VALUES(1,'x');"), "{out}");
    assert!(out.contains("INSERT INTO \"t\" VALUES(2,'y');"), "{out}");
    assert!(out.contains("BEGIN TRANSACTION;"), "{out}");
    assert!(out.contains("COMMIT;"), "{out}");
}

#[test]
fn headers_and_mode_affect_select_rendering() {
    let db = scratch_db("headers-mode");
    seed(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
    seed(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y')");

    // Default: list mode, no headers.
    let default_out = stdout_of(&db, "SELECT * FROM t;\n.quit\n");
    assert!(default_out.contains("1|x\n2|y\n"), "{default_out}");

    // `.headers on` + list mode adds a header row.
    let headered = stdout_of(&db, ".headers on\nSELECT * FROM t;\n.quit\n");
    assert!(headered.contains("a|b\n1|x\n2|y\n"), "{headered}");

    // `.mode csv`.
    let csv = stdout_of(&db, ".mode csv\nSELECT * FROM t;\n.quit\n");
    assert!(csv.contains("1,x\r\n2,y\r\n"), "{csv}");

    // `.mode line`: one `name = value` per column, blank line between rows.
    let line = stdout_of(&db, ".mode line\nSELECT * FROM t;\n.quit\n");
    assert!(line.contains("a = 1\nb = x\n\na = 2\nb = y\n"), "{line}");

    // `.mode column` with headers on: header row, dashes, aligned values.
    let column = stdout_of(&db, ".headers on\n.mode column\nSELECT * FROM t;\n.quit\n");
    assert!(column.contains("a  b\n-  -\n1  x\n2  y\n"), "{column}");
}

#[test]
fn prefix_matching_reaches_new_commands() {
    let db = scratch_db("prefix");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(&db, ".ver\n.quit\n");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
}

#[test]
fn unknown_dot_command_prints_error_and_continues() {
    let db = scratch_db("unknown-cmd");
    seed(&db, "CREATE TABLE t(a)");
    let out = run_repl_script(&db, ".bogus\nSELECT 1;\n.quit\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown command"), "{stderr}");
    assert!(out.status.success());
}

#[test]
fn color_toggle_accepts_on_off_and_rejects_garbage() {
    let db = scratch_db("color");
    seed(&db, "CREATE TABLE t(a)");
    let out = run_repl_script(&db, ".color on\n.color off\n.color sideways\n.quit\n");
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: .color on|off"), "{stderr}");
}

#[test]
fn headers_rejects_invalid_argument() {
    let db = scratch_db("headers-bad");
    seed(&db, "CREATE TABLE t(a)");
    let out = run_repl_script(&db, ".headers sideways\n.quit\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: .headers on|off"), "{stderr}");
}

#[test]
fn mode_rejects_invalid_argument() {
    let db = scratch_db("mode-bad");
    seed(&db, "CREATE TABLE t(a)");
    let out = run_repl_script(&db, ".mode sideways\n.quit\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("usage: .mode csv|column|line|list"),
        "{stderr}"
    );
}

#[test]
fn schema_includes_view_statements() {
    let db = scratch_db("schema-view");
    seed(&db, "CREATE TABLE t(a, b)");
    seed(&db, "CREATE VIEW v AS SELECT a FROM t");

    let all = stdout_of(&db, ".schema\n.quit\n");
    assert!(all.contains("CREATE VIEW v AS SELECT a FROM t;"), "{all}");

    let named = stdout_of(&db, ".schema v\n.quit\n");
    assert!(
        named.contains("CREATE VIEW v AS SELECT a FROM t;"),
        "{named}"
    );
}

#[test]
fn indices_with_no_matching_table_prints_nothing() {
    let db = scratch_db("indices-none");
    seed(&db, "CREATE TABLE t(a)");
    seed(&db, "CREATE INDEX idx_t_a ON t(a)");
    let out = stdout_of(&db, ".indices nosuchtable\n.quit\n");
    assert!(!out.contains("idx_t_a"), "{out}");
}

#[test]
fn dump_filtered_by_table_omits_other_tables() {
    let db = scratch_db("dump-filtered");
    seed(&db, "CREATE TABLE t(a)");
    seed(&db, "CREATE TABLE other(x)");
    seed(&db, "INSERT INTO t VALUES (1)");
    seed(&db, "INSERT INTO other VALUES (2)");

    let out = stdout_of(&db, ".dump t\n.quit\n");
    assert!(out.contains("CREATE TABLE t(a);"), "{out}");
    assert!(out.contains("INSERT INTO \"t\" VALUES(1);"), "{out}");
    assert!(!out.contains("other"), "{out}");
}

#[test]
fn pragma_query_runs_through_the_repl_loop() {
    let db = scratch_db("pragma");
    seed(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
    let out = stdout_of(&db, "PRAGMA table_info(t);\n.quit\n");
    assert!(out.contains('a'), "{out}");
    assert!(out.contains('b'), "{out}");
}

#[test]
fn transaction_control_survives_across_statements() {
    let db = scratch_db("txn");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(
        &db,
        "BEGIN;\nINSERT INTO t VALUES (1);\nSELECT * FROM t;\nCOMMIT;\n.quit\n",
    );
    assert!(out.contains('1'), "{out}");
}

#[test]
fn select_syntax_error_is_reported_and_repl_continues() {
    let db = scratch_db("syntax-error");
    seed(&db, "CREATE TABLE t(a)");
    let out = run_repl_script(&db, "SELECT FROM;\nSELECT 1;\n.quit\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Error: syntax error"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('1'), "{stdout}");
}

#[test]
fn multiple_statements_on_one_line_all_execute() {
    let db = scratch_db("multi-stmt");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(&db, "INSERT INTO t VALUES (1); SELECT * FROM t;\n.quit\n");
    assert!(out.contains('1'), "{out}");
}

#[test]
fn crlf_line_endings_are_trimmed_like_bare_newlines() {
    let db = scratch_db("crlf");
    seed(&db, "CREATE TABLE t(a)");
    let out = stdout_of(
        &db,
        "INSERT INTO t VALUES (1);\r\nSELECT * FROM t;\r\n.quit\r\n",
    );
    assert!(out.contains('1'), "{out}");
}

#[test]
fn select_with_join_falls_back_to_positional_headers() {
    let db = scratch_db("join-headers");
    seed(&db, "CREATE TABLE t(a)");
    seed(&db, "CREATE TABLE u(b)");
    seed(&db, "INSERT INTO t VALUES (1)");
    seed(&db, "INSERT INTO u VALUES (2)");

    let out = stdout_of(
        &db,
        ".headers on\nSELECT t.a, u.b FROM t JOIN u ON 1;\n.quit\n",
    );
    assert!(out.contains("column1|column2"), "{out}");
}
