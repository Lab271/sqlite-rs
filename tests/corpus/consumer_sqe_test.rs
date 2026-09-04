// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! SQE's catalog as a consumer fixture family (spec 013 Requirement 6).
//!
//! Req 6 says acceptance for the embedding API is spec 004's harness, not
//! a new one: "a consumer statement set becomes a fixture family, diffed
//! against pinned `sqlite3`... The first family is *SQE*'s catalog". This
//! is that family.
//!
//! It lives here rather than in `tests/spike/014_embedding_api/` on
//! purpose. ADR-0008: spike code is disposed, the evidence survives and
//! "test material is committed as acceptance corpus for the real
//! implementation — the ratchet". Spikes also do not run in CI
//! (`make test-spikes` lists 001-009 and `ci.yml` mentions none), so a
//! regression guard kept there would rot unnoticed.
//!
//! `catalog.sql` is SQE's schema **as it is today**: a plain table plus a
//! named `CREATE UNIQUE INDEX`, not a declared composite `PRIMARY KEY`.
//! That is not a simplification for the test — it is the workaround spec
//! 013 records SQE having adopted, and the reason is #687: a table *this
//! crate creates* with a declared composite PK gets no
//! `sqlite_autoindex_*`, and the oracle then calls the file
//! `database disk image is malformed` immediately, before any write.
//! `catalog_declared_pk.sql` is the schema SQE wants back, pinned as
//! #687's ratchet.
//!
//! Four things SQE needs are known-broken and pinned as `#[ignore]` with
//! the ticket that flips them, the same shape as `tests/tiers/`. Each is
//! written to pass once fixed, so closing a ticket un-ignores a test
//! rather than needing one authored:
//!
//! - `catalog_ddl_is_idempotent` — #697
//! - `a_column_named_key_is_usable` — #696
//! - `catalog_sql_file_runs_verbatim` — #698
//! - `declared_composite_pk_creates_a_valid_file` — #687

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/fixtures/consumers/sqe")
        .join(name)
}

fn catalog_path() -> PathBuf {
    fixture("catalog.sql")
}

fn catalog_sql() -> String {
    std::fs::read_to_string(catalog_path()).expect("catalog.sql")
}

/// The same SQL with `--` comment lines removed.
///
/// Needed because a statement beginning with a comment is a parse error
/// here and is accepted by the oracle (#698) — the fixture's own header
/// is enough to trip it. The oracle is always fed the file verbatim; only
/// our side gets the stripped form, and `catalog_sql_file_runs_verbatim`
/// pins the real behaviour so this crutch disappears when #698 closes.
fn catalog_sql_without_comments() -> String {
    catalog_sql()
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sqlite-rs-sqe-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The `sqlite-rs` binary under test. Built by `cargo test` because the
/// corpus target depends on it being present; `env!("CARGO_BIN_EXE_...")`
/// is not available to a `test = false` target, so resolve it next to the
/// test executable.
fn cli() -> PathBuf {
    let mut p = std::env::current_exe().expect("test exe path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("sqlite-rs")
}

/// Feeds the oracle over **stdin**, not `argv`. A leading `--` in an
/// argument is read as a CLI option by `sqlite3` (and by us), so the
/// fixture's header comment would otherwise be a spurious failure that
/// says nothing about SQL.
fn oracle_script(bin: &Path, db: &Path, sql: &str) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(bin)
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(sql.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "oracle rejected the script: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn oracle_run(bin: &Path, db: &Path, sql: &str) -> String {
    let out = Command::new(bin).arg(db).arg(sql).output().unwrap();
    assert!(
        out.status.success(),
        "oracle rejected {sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn ours_query(db: &Path, sql: &str) -> String {
    let out = Command::new(cli())
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sqlite-rs rejected {sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn ours_exec(db: &Path, sql: &str) {
    let out = Command::new(cli())
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sqlite-rs rejected {sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The queries SQE issues against its catalog: existence probes with
/// `LIMIT 1`, a namespace `UNION`, a point lookup on the composite key.
const QUERIES: &[&str] = &[
    "SELECT metadata_location FROM iceberg_tables WHERE catalog_name='cat' AND table_namespace='ns' AND table_name='t1' LIMIT 1",
    "SELECT metadata_location FROM iceberg_tables WHERE catalog_name='cat' AND table_namespace='ns' AND table_name='absent' LIMIT 1",
    "SELECT table_name FROM iceberg_tables WHERE catalog_name='cat' AND table_namespace='ns' ORDER BY table_name",
    "SELECT namespace FROM iceberg_namespace_properties WHERE catalog_name='cat' UNION SELECT table_namespace FROM iceberg_tables WHERE catalog_name='cat' ORDER BY 1",
    "SELECT property_value FROM iceberg_namespace_properties WHERE catalog_name='cat' AND namespace='ns' AND property_key='owner' LIMIT 1",
    "SELECT count(*) FROM iceberg_tables",
    "SELECT count(*) FROM iceberg_namespace_properties",
];

/// Both engines build the catalog from the same `.sql`, then answer the
/// same queries. This is the claim "sqlite-rs works for SQE's read path",
/// stated as a diff rather than an assertion.
#[test]
fn catalog_queries_match_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("catalog_queries_match_the_oracle");
        return;
    };
    let dir = workdir("read");
    let (theirs_db, ours_db) = (dir.join("o.db"), dir.join("m.db"));
    oracle_script(&bin, &theirs_db, &catalog_sql());
    for stmt in sqlite_rs::parser::split_statements(&catalog_sql_without_comments()) {
        ours_exec(&ours_db, &stmt);
    }

    for q in QUERIES {
        assert_eq!(
            ours_query(&ours_db, q),
            oracle_run(&bin, &theirs_db, q),
            "diverged on {q}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// SQE's write path: a conditional `UPDATE` swapping a metadata pointer
/// (its optimistic-concurrency step), a `DELETE`, and the resulting file
/// still being one the oracle will read.
#[test]
fn catalog_writes_match_the_oracle_and_leave_a_valid_file() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("catalog_writes_match_the_oracle_and_leave_a_valid_file");
        return;
    };
    let dir = workdir("write");
    let (theirs_db, ours_db) = (dir.join("o.db"), dir.join("m.db"));
    oracle_script(&bin, &theirs_db, &catalog_sql());
    for stmt in sqlite_rs::parser::split_statements(&catalog_sql_without_comments()) {
        ours_exec(&ours_db, &stmt);
    }

    let writes = [
        // The compare-and-swap SQE treats zero rows affected as a lost race.
        "UPDATE iceberg_tables SET metadata_location='s3://bucket/t1/meta2.json' WHERE catalog_name='cat' AND table_namespace='ns' AND table_name='t1' AND metadata_location='s3://bucket/t1/meta.json'",
        // The same statement again: now a no-op.
        "UPDATE iceberg_tables SET metadata_location='s3://bucket/t1/meta3.json' WHERE catalog_name='cat' AND table_namespace='ns' AND table_name='t1' AND metadata_location='s3://bucket/t1/meta.json'",
        "DELETE FROM iceberg_tables WHERE catalog_name='cat' AND table_namespace='other'",
    ];
    for w in writes {
        oracle_run(&bin, &theirs_db, w);
        ours_exec(&ours_db, w);
    }

    for q in [
        "SELECT catalog_name, table_namespace, table_name, metadata_location FROM iceberg_tables ORDER BY table_name",
        "SELECT count(*) FROM iceberg_tables",
    ] {
        assert_eq!(
            ours_query(&ours_db, q),
            oracle_run(&bin, &theirs_db, q),
            "diverged on {q}"
        );
    }

    // The file we wrote must be one stock SQLite considers sound — the
    // check that caught #685 and #697.
    assert_eq!(
        oracle_run(&bin, &ours_db, "PRAGMA integrity_check;"),
        "ok",
        "our catalog file is not one the oracle will read"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// SQE runs its catalog DDL on **every** startup — that is what `IF NOT
/// EXISTS` is for. Today the second run appends a duplicate
/// `sqlite_master` row and leaks a root page, so the oracle calls the
/// file malformed.
#[test]
#[ignore = "#697: CREATE TABLE IF NOT EXISTS ignores its guard, corrupting the file on the second run"]
fn catalog_ddl_is_idempotent() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("catalog_ddl_is_idempotent");
        return;
    };
    let dir = workdir("idem");
    let ours_db = dir.join("m.db");
    let sql = catalog_sql_without_comments();

    for stmt in sqlite_rs::parser::split_statements(&sql) {
        ours_exec(&ours_db, &stmt);
    }
    // Second startup: the DDL only, as SQE would issue it.
    for stmt in sqlite_rs::parser::split_statements(&sql) {
        if stmt.to_ascii_uppercase().starts_with("CREATE TABLE") {
            ours_exec(&ours_db, &stmt);
        }
    }

    assert_eq!(
        oracle_run(
            &bin,
            &ours_db,
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='iceberg_tables';"
        ),
        "1",
        "duplicate sqlite_master row after re-running IF NOT EXISTS"
    );
    assert_eq!(oracle_run(&bin, &ours_db, "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        ours_query(&ours_db, "SELECT count(*) FROM iceberg_tables"),
        "3",
        "rows written before the second DDL run must survive it"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `key` is one of 89 keywords we reserve that SQLite treats as an
/// identifier. Whether SQE's namespace-properties table literally names a
/// column `key` is Jacob's to confirm — but a consumer cannot be told to
/// avoid `key`, so this is pinned regardless.
#[test]
#[ignore = "#696: 89 keywords are reserved that SQLite's parse.y %fallback treats as identifiers"]
fn a_column_named_key_is_usable() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("a_column_named_key_is_usable");
        return;
    };
    let dir = workdir("key");
    let ours_db = dir.join("m.db");

    ours_exec(
        &ours_db,
        "CREATE TABLE props (namespace TEXT, key TEXT, value TEXT, PRIMARY KEY (namespace, key))",
    );
    ours_exec(
        &ours_db,
        "INSERT INTO props VALUES ('ns','owner','data-eng')",
    );
    assert_eq!(
        ours_query(&ours_db, "SELECT key, value FROM props"),
        "owner|data-eng"
    );
    assert_eq!(oracle_run(&bin, &ours_db, "PRAGMA integrity_check;"), "ok");
    std::fs::remove_dir_all(&dir).ok();
}

/// The fixture file, comments and all, run through this crate exactly as
/// the oracle runs it. A consumer ships its schema as a commented `.sql`
/// file; that has to work.
#[test]
#[ignore = "#698: a statement beginning with a SQL comment is a parse error"]
fn catalog_sql_file_runs_verbatim() {
    let dir = workdir("verbatim");
    let ours_db = dir.join("m.db");
    for stmt in sqlite_rs::parser::split_statements(&catalog_sql()) {
        ours_exec(&ours_db, &stmt);
    }
    assert_eq!(
        ours_query(&ours_db, "SELECT count(*) FROM iceberg_tables"),
        "3"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The schema SQE actually wants: a declared composite `PRIMARY KEY`
/// rather than a named unique index.
///
/// Blocked in the direction that matters most — a table *we* create is
/// malformed to the oracle straight away, with no write involved, because
/// no `sqlite_autoindex_*` is emitted. #685 fixed adopting a
/// stock-created file; this is the same gap in the direction we control,
/// and it is why SQE carries the named-index workaround at all.
#[test]
#[ignore = "#687: CREATE TABLE emits no sqlite_autoindex_* for a declared composite PRIMARY KEY"]
fn declared_composite_pk_creates_a_valid_file() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("declared_composite_pk_creates_a_valid_file");
        return;
    };
    let dir = workdir("declpk");
    let ours_db = dir.join("m.db");
    let sql = std::fs::read_to_string(fixture("catalog_declared_pk.sql")).unwrap();
    for stmt in sqlite_rs::parser::split_statements(&sql) {
        ours_exec(&ours_db, &stmt);
    }

    assert_eq!(
        oracle_run(&bin, &ours_db, "PRAGMA integrity_check;"),
        "ok",
        "a file we created with a declared composite PK is not one the oracle will read"
    );
    // And the constraint it declares must actually be enforced.
    let dup = Command::new(cli())
        .arg("exec")
        .arg(&ours_db)
        .arg("INSERT INTO iceberg_tables VALUES ('cat','ns','t1','s3://elsewhere')")
        .output()
        .unwrap();
    assert!(
        !dup.status.success(),
        "the declared composite PRIMARY KEY did not reject a duplicate"
    );
    std::fs::remove_dir_all(&dir).ok();
}
