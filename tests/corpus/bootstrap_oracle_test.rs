// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Does a database file *this crate creates from scratch* satisfy stock
//! SQLite? (spec 013 Requirement 2 prerequisite.)
//!
//! Nothing in the tree answered that before this file, and the reason is a
//! structural blind spot rather than an oversight. Both bootstrap-adjacent
//! suites seed their fixtures like this:
//!
//! ```text
//! if let Some(oracle) = pinned_oracle() { /* oracle builds the file */ }
//! else { /* our CLI builds the file */ }
//! ```
//!
//! — `tests/tiers/tier2.rs`'s `seed_db` and `tests/corpus/cli_write_test.rs`'s
//! `seed_db`. They *prefer* the oracle and fall back to our own path only
//! when no oracle is installed. So when the oracle is present it creates
//! the file and our creation path is never exercised; when it is absent our
//! path runs but there is no oracle left to check the result. The two never
//! run together, and the claim "a file we create is a valid SQLite database"
//! went unverified for the life of the write path.
//!
//! An embedding consumer makes this load-bearing: spec 013's `Connection::open`
//! creates the database if it does not exist, and SQE's catalog is a file
//! nothing else has ever touched. If our page 1 were subtly wrong, every
//! consumer's first file would be malformed and only a third-party tool
//! would ever say so.
//!
//! `tests/unit/vdbe_integrity_check_test.rs` is not this test: it hand-rolls
//! page 1 and checks it with *our* integrity checker, which shares any
//! misconception the writer has.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sqlite_rs::header::DatabaseHeader;

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-bootstrap-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn oracle_says(bin: &Path, db: &Path, sql: &str) -> String {
    let output = Command::new(bin)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("oracle failed to run {sql:?}: {e}"));
    assert!(
        output.status.success(),
        "oracle rejected {sql:?} against {}: {}",
        db.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn our_exec(db: &Path, sql: &str) {
    let output = Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {sql:?}: {e}"));
    assert!(
        output.status.success(),
        "our CLI rejected {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `DatabaseHeader::new_empty_page1` is the bootstrap: the bytes written to
/// a brand-new file before its first statement runs. Every supported page
/// size must produce a file stock SQLite accepts as an empty database.
///
/// 65536 is included deliberately — it is the one size that cannot be
/// stored literally in the 16-bit page-size field and is encoded as `1`
/// (and whose cell-content-area offset wraps to 0), so it exercises the
/// only branch in the function.
#[test]
fn an_empty_database_we_build_is_valid_at_every_page_size() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("an_empty_database_we_build_is_valid_at_every_page_size");
        return;
    };
    let dir = scratch_dir("empty");

    for page_size in [512u32, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
        let db = dir.join(format!("empty_{page_size}.db"));
        std::fs::write(&db, DatabaseHeader::new_empty_page1(page_size)).unwrap();

        assert_eq!(
            oracle_says(&bin, &db, "PRAGMA integrity_check;"),
            "ok",
            "page 1 built for page_size={page_size} is malformed to stock sqlite3"
        );
        assert_eq!(
            oracle_says(&bin, &db, "PRAGMA page_size;"),
            page_size.to_string(),
            "the oracle read back a different page size for page_size={page_size}"
        );
        assert_eq!(
            oracle_says(&bin, &db, "SELECT count(*) FROM sqlite_master;"),
            "0",
            "a freshly built database should have an empty schema"
        );
        // And it must be *usable*, not merely well-formed: the oracle has
        // to be able to grow it.
        oracle_says(
            &bin,
            &db,
            "CREATE TABLE probe(x); INSERT INTO probe VALUES (1);",
        );
        assert_eq!(
            oracle_says(&bin, &db, "PRAGMA integrity_check;"),
            "ok",
            "the oracle's own write to our page_size={page_size} file left it malformed"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The end-to-end claim: our CLI creates a file that never existed, runs a
/// DDL + DML sequence through our write path only, and the result is a
/// valid database whose contents the oracle agrees with.
///
/// The comparison file is built by the oracle from the identical sequence,
/// so a divergence points at our writer rather than at the SQL.
#[test]
fn a_database_we_create_from_scratch_survives_writes_and_matches_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("a_database_we_create_from_scratch_survives_writes_and_matches_the_oracle");
        return;
    };
    let dir = scratch_dir("scratch");
    let ours = dir.join("ours.db");
    let theirs = dir.join("theirs.db");

    // Deliberately no composite PRIMARY KEY: a declared composite PK is
    // #687, still open, and would fail here for an unrelated reason.
    let statements = [
        "CREATE TABLE t(a INTEGER, b TEXT)",
        "CREATE INDEX t_a ON t(a)",
        "CREATE UNIQUE INDEX t_b ON t(b)",
        "INSERT INTO t VALUES (1, 'x')",
        "INSERT INTO t VALUES (2, 'y')",
        "INSERT INTO t VALUES (3, 'z')",
        "UPDATE t SET b = 'q' WHERE a = 2",
        "DELETE FROM t WHERE a = 1",
    ];

    for sql in statements {
        our_exec(&ours, sql);
        oracle_says(&bin, &theirs, &format!("{sql};"));
    }

    assert_eq!(
        oracle_says(&bin, &ours, "PRAGMA integrity_check;"),
        "ok",
        "a database built entirely by our write path is malformed to stock sqlite3"
    );

    // The oracle must also *agree with itself* about the two files: same
    // schema, same rows, same index contents.
    for probe in [
        "SELECT type, name, tbl_name FROM sqlite_master ORDER BY name;",
        "SELECT a, b FROM t ORDER BY a;",
        "SELECT a FROM t WHERE a = 3;",
        "SELECT b FROM t WHERE b = 'q';",
        "SELECT count(*) FROM t;",
    ] {
        assert_eq!(
            oracle_says(&bin, &ours, probe),
            oracle_says(&bin, &theirs, probe),
            "the oracle read different results from our file and its own for {probe:?}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Byte-level comparison of a fresh header we wrote against a fresh header
/// the oracle wrote, for the same DDL.
///
/// This is the test that would catch an omitted header field that
/// `DatabaseHeader::parse` happens to tolerate — the class of bug
/// `integrity_check` can miss, because `integrity_check` validates the
/// b-tree structure rather than every header byte.
///
/// Exactly three ranges are expected to differ, and all three are fields
/// this crate does not model at all (they are absent from
/// `DatabaseHeader`), so they read as zero in a file we create:
///
/// | offset | field | ours | why it is not a validity problem |
/// |---|---|---|---|
/// | 24..28 | change counter | 0 | self-consistent with 92..96, so the cached page count at 28 stays trusted |
/// | 92..96 | version-valid-for | 0 | must equal the change counter, and does |
/// | 96..100 | SQLite version | 0 | advisory; a file last written by another writer legitimately has its own value |
///
/// The stuck change counter is a real interop limitation rather than a
/// cosmetic one — another SQLite connection that already holds a cached
/// image of this file has no way to learn our writes happened — but it is
/// not a malformation, which is precisely why it needs asserting here
/// instead of being left to `integrity_check`. Asserting the divergence set
/// *exhaustively* means fixing the counter, or drifting any other header
/// field, both show up as a failure here.
#[test]
fn our_fresh_header_differs_from_the_oracles_only_in_fields_we_do_not_model() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("our_fresh_header_differs_from_the_oracles_only_in_fields_we_do_not_model");
        return;
    };
    let dir = scratch_dir("header");
    let ours = dir.join("ours.db");
    let theirs = dir.join("theirs.db");

    let ddl = "CREATE TABLE t(a INTEGER, b TEXT)";
    our_exec(&ours, ddl);
    oracle_says(&bin, &theirs, &format!("{ddl};"));

    let a = std::fs::read(&ours).unwrap();
    let b = std::fs::read(&theirs).unwrap();
    assert!(a.len() >= 100 && b.len() >= 100);

    // Everything before the change counter: magic, page size, file format
    // versions, reserved space, and all three payload fractions.
    assert_eq!(
        &a[0..24],
        &b[0..24],
        "header bytes 0..24 diverge — magic, page size, format versions or payload fractions"
    );

    // Everything from the page count to the version-valid-for field:
    // page count, freelist, schema cookie, schema format, cache size,
    // auto-vacuum root, text encoding, user version, incremental vacuum,
    // application id, and the reserved expansion space.
    assert_eq!(
        &a[28..92],
        &b[28..92],
        "header bytes 28..92 diverge — page count, freelist, schema cookie/format, \
         text encoding or one of the version/id fields"
    );

    // The three we do not model read as zero. Asserted positively so this
    // test states the divergence rather than merely tolerating it.
    assert_eq!(
        &a[24..28],
        &[0, 0, 0, 0],
        "we appear to write a change counter now — update this test and the \
         interop note attached to it"
    );
    assert_eq!(
        &a[92..96],
        &[0, 0, 0, 0],
        "we appear to write version-valid-for now"
    );
    assert_eq!(
        &a[96..100],
        &[0, 0, 0, 0],
        "we appear to write a SQLite version now"
    );

    // And the oracle really does populate them, so the comparison above is
    // meaningful rather than comparing two sets of zeroes.
    assert_ne!(
        &b[24..28],
        &[0, 0, 0, 0],
        "the oracle left the change counter at zero — this test proves nothing"
    );
    assert_ne!(
        &b[96..100],
        &[0, 0, 0, 0],
        "the oracle left its version number at zero — this test proves nothing"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Writing to a file the *oracle* created must preserve the fields we do
/// not model rather than zeroing them — a writer that re-serialised the
/// header from its own struct would silently drop the oracle's change
/// counter and version number.
#[test]
fn writing_to_an_oracle_created_file_preserves_the_fields_we_do_not_model() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("writing_to_an_oracle_created_file_preserves_the_fields_we_do_not_model");
        return;
    };
    let dir = scratch_dir("preserve");
    let db = dir.join("mixed.db");

    oracle_says(
        &bin,
        &db,
        "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x');",
    );
    let before = std::fs::read(&db).unwrap();
    let version_before = &before[96..100].to_vec();
    assert_ne!(version_before.as_slice(), &[0, 0, 0, 0]);

    our_exec(&db, "INSERT INTO t VALUES (2, 'y')");

    let after = std::fs::read(&db).unwrap();
    assert_eq!(
        &after[96..100],
        version_before.as_slice(),
        "our write zeroed the oracle's SQLite version number"
    );
    assert_eq!(
        oracle_says(&bin, &db, "PRAGMA integrity_check;"),
        "ok",
        "our write left an oracle-created file malformed"
    );
    assert_eq!(
        oracle_says(&bin, &db, "SELECT a, b FROM t ORDER BY a;"),
        "1|x\n2|y"
    );

    std::fs::remove_dir_all(&dir).ok();
}
