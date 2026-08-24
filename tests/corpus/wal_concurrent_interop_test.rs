//! #390 acceptance tests for the V6 demo (epic #354's stated goal):
//! *"sqlite-rs writing while stock sqlite3 reads the same WAL-mode file
//! live."* One level up from `wal_write_interop_test.rs`'s byte-level
//! WAL-frame tests and #389's `concurrent_writer_is_refused_the_wal_write_lock`
//! (two *sqlite-rs* writers serializing against each other): this file
//! proves two genuinely *different* engines — this crate and a real,
//! live `sqlite3` subprocess — round-trip SQL-level writes through each
//! other's WAL frames correctly.
//!
//! "Concurrent" here is a deterministic interleaving (spawn/wait each
//! step in sequence, never a `sleep`), matching this repo's existing
//! oracle-parity style (`wal_write_interop_test.rs`,
//! `transaction_oracle_test.rs`) rather than a timing-based race —
//! sufficient to exercise the same byte-level WAL interop a genuinely
//! concurrent workload would, per CLAUDE.md's test layout conventions.
//!
//! ## SQL-level entry point
//!
//! sqlite-rs drives every statement (including `PRAGMA journal_mode =
//! WAL`) through [`execute_transaction_step`]/[`compile_statement`] on
//! one shared [`Pager`] — the exact machinery `transaction_oracle_test.rs`
//! already uses for BEGIN/COMMIT/ROLLBACK, and what `sqlite-rs exec`
//! (`src/bin/sqlite-rs/exec.rs`) is a thin CLI wrapper over. No new
//! plumbing was needed: #388 (mode switching) and #389 (WAL-mode
//! `flush`) already made `INSERT`/`UPDATE` reach `Pager::flush` through
//! the WAL when `journal_mode = WAL` is active; this file is the first
//! to prove it through the *SQL* surface instead of `get_page_mut`/
//! `flush` called directly.
//!
//! ## Two things found while writing this file (neither a blocker,
//! both worth a follow-up ticket — see the #390 close-out report)
//!
//! - Neither `Pager::open` nor the CLI's `dump::open` can create a
//!   brand-new empty database file from nothing (`Vfs::open_read`/
//!   `open_write` on `UnixVfs` both require the file to already exist).
//!   Whichever engine writes *first* in a given scenario below must
//!   therefore be the oracle (a real `sqlite3 <newfile>` invocation
//!   always creates the file) — the same seed-via-oracle-when-available
//!   shape `cli_write_test.rs`'s `seed_db` already uses.
//! - Stock `sqlite3` auto-checkpoints — and, on full success, deletes
//!   the `-wal`/`-shm` files entirely — when the *last* connection to a
//!   WAL database closes. A one-shot `sqlite3 db "PRAGMA
//!   journal_mode=WAL; INSERT ...;"` invocation therefore leaves no
//!   `-wal` file behind by the time the process exits, which would have
//!   broken a same-process-reads-the-WAL-overlay assertion. It no
//!   longer breaks a *subsequent* sqlite-rs commit, though:
//!   `Pager::flush_wal_locked` now recreates a fresh `-wal`/`-shm` pair
//!   when `WalWriter::open_existing` reports the file missing rather
//!   than propagating the error (#422, fixed —
//!   `flush_in_wal_mode_recovers_when_wal_and_shm_vanish` in
//!   `src/pager.rs`). [`OracleSession`] below (a genuinely live,
//!   still-open second process) still sidesteps the same-process
//!   overlay assertion issue by construction: it's the more faithful
//!   reproduction of this ticket's "two terminals, both stay running"
//!   acceptance sketch anyway.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::dump::dump_database;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::checkpoint::checkpoint_passive;
use sqlite_rs::pager::Pager;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::{AnyVfs, PageSource, UnixVfs};

use crate::oracle::{pinned_oracle, skip_no_oracle};
use crate::wal_write_interop_test::{declared_page_size, scratch_db};

fn oracle_exec(oracle: &Path, db: &Path, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success(), "oracle script failed: {sql}");
}

fn oracle_select_t(oracle: &Path, db: &Path) -> String {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg(db)
        .arg("SELECT * FROM t ORDER BY a;")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn oracle_journal_mode(oracle: &Path, db: &Path) -> String {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg(db)
        .arg("PRAGMA journal_mode;")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A real, *live* `sqlite3` process whose connection to `db` stays open
/// across multiple statements — needed whenever a test wants sqlite-rs
/// to observe the oracle's WAL frames before they'd be backfilled and
/// removed. Stock SQLite auto-checkpoints (and, on success, deletes the
/// `-wal`/`-shm` files) when the *last* connection to a WAL database
/// closes — verified empirically while writing this file: a one-shot
/// `sqlite3 db "PRAGMA journal_mode=WAL; INSERT ..."` invocation leaves
/// no `-wal` file behind at all once the process exits. Keeping the
/// connection open (same spawn/pipe technique
/// `wal_write_interop_test.rs`'s `HeldLock` uses for a live second
/// process, but driving SQL instead of holding a lock) sidesteps that
/// entirely, and is arguably the more faithful reproduction of this
/// ticket's "two-terminal, both processes stay running" acceptance
/// sketch anyway. Each statement is followed by a `SELECT` of a unique
/// marker string and a blocking read for that same line on stdout — a
/// deterministic "has sqlite3 actually finished this statement yet?"
/// sync point, not a `sleep`, matching this repo's no-timing-races
/// convention.
struct OracleSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl OracleSession {
    fn spawn(oracle: &Path, db: &Path) -> Self {
        let mut child = Command::new(oracle)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        OracleSession {
            child,
            stdin,
            stdout,
        }
    }

    /// Runs `sql` against this session's still-open connection, blocking
    /// until sqlite3 has actually executed it (see the struct doc for
    /// why this can't just be "write and move on").
    fn exec(&mut self, sql: &str) {
        const MARKER: &str = "oracle-session-step-done";
        writeln!(self.stdin, "{sql}").unwrap();
        writeln!(self.stdin, "SELECT '{MARKER}';").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "oracle session exited unexpectedly mid-script");
            if line.trim() == MARKER {
                break;
            }
        }
    }

    /// Like [`OracleSession::exec`], but returns every line of output
    /// `sql` itself produced (the marker line excluded) — for a `SELECT`
    /// read through this same still-open connection.
    fn query(&mut self, sql: &str) -> String {
        const MARKER: &str = "oracle-session-step-done";
        writeln!(self.stdin, "{sql}").unwrap();
        writeln!(self.stdin, "SELECT '{MARKER}';").unwrap();
        self.stdin.flush().unwrap();
        let mut out = String::new();
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "oracle session exited unexpectedly mid-script");
            if line.trim() == MARKER {
                break;
            }
            out.push_str(&line);
        }
        out.trim_end().to_string()
    }

    /// Closes stdin (EOF, sqlite3's normal way to end a piped script)
    /// and waits for the process to exit — only now may it auto-
    /// checkpoint and remove the `-wal`/`-shm` files.
    fn close(mut self) {
        drop(self.stdin);
        self.child.wait().unwrap();
    }
}

fn header_of(vfs: &UnixVfs, db: &Path, page_size: u32) -> DatabaseHeader {
    let source = Pager::open(vfs, db, page_size).unwrap();
    let bytes = source.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&bytes[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

/// Runs `stmts` (each a full SQL statement — `PRAGMA journal_mode=WAL`
/// included) through our engine against `db`'s one shared `Pager`,
/// re-reading the schema before every statement
/// (`src/bin/sqlite-rs/exec.rs::run_exec`'s pattern) so a script that
/// both switches `journal_mode` and writes sees its own earlier
/// effects. `header`'s `page_size`/`text_encoding` fields (the only
/// ones `compile_statement`/`execute_transaction_step` consult) are
/// unaffected by the journal-mode switch, so — unlike `schemas` — it's
/// safe to compute once up front.
fn run_our_session(vfs: &UnixVfs, db: &Path, page_size: u32, stmts: &[&str]) {
    let header = header_of(vfs, db, page_size);
    let pager = Rc::new(RefCell::new(Pager::open(vfs, db, page_size).unwrap()));
    let mut autocommit = true;
    for stmt in stmts {
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            read_schema(&mut schema_cursor, header.text_encoding).unwrap()
        };
        let program = compile_statement(stmt, &schemas, &[]).unwrap();
        let (_, ac) =
            execute_transaction_step(&program, Rc::clone(&pager), header, autocommit).unwrap();
        autocommit = ac;
    }
}

/// Reads every row of table `t` through our own engine
/// ([`dump_database`], which opens via `Pager::open` — WAL-frame overlay
/// included, spec 007 Requirement 3) and renders it the same `"a|b"`
/// shape the oracle's own default (list-mode) CLI output uses, ordered
/// by `a` so row order never depends on b-tree insertion order.
fn our_select_t(vfs: &UnixVfs, db: &Path) -> String {
    let result = dump_database(vfs, db).unwrap();
    let table = result.tables.iter().find(|t| t.name == "t").unwrap();
    let mut rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(sqlite_rs::format::format_list_value)
                .collect()
        })
        .collect();
    rows.sort_by(|a, b| {
        a[0].parse::<i64>()
            .unwrap()
            .cmp(&b[0].parse::<i64>().unwrap())
    });
    rows.into_iter()
        .map(|row| row.join("|"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// **Scenario 1**: sqlite-rs writes a WAL-mode database, a real
/// `sqlite3` process reads it back — proving stock SQLite auto-detects
/// WAL mode from the file header alone (per SQLite's own docs) rather
/// than needing `PRAGMA journal_mode=WAL` re-issued from its own side.
#[test]
fn sqlite_rs_writes_wal_and_oracle_reads_live() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("sqlite_rs_writes_wal_and_oracle_reads_live");
        return;
    };

    let db = scratch_db("rs-writes-oracle-reads");
    oracle_exec(&oracle, &db, "CREATE TABLE t(a INTEGER, b TEXT);");

    let vfs = UnixVfs;
    let page_size = declared_page_size(&vfs, &db);
    run_our_session(
        &vfs,
        &db,
        page_size,
        &[
            "PRAGMA journal_mode = WAL",
            "INSERT INTO t VALUES (1, 'from-sqlite-rs')",
        ],
    );

    assert!(
        sqlite_rs::vfs::companion_path(&db, "-wal").exists(),
        "a -wal file must exist once sqlite-rs has committed a WAL-mode write"
    );
    assert_eq!(
        oracle_journal_mode(&oracle, &db),
        "wal",
        "stock sqlite3 must auto-detect WAL mode from the header, no PRAGMA needed on its side"
    );
    assert_eq!(oracle_select_t(&oracle, &db), "1|from-sqlite-rs");
}

/// **Scenario 2**: the reverse direction — a real, still-*live*
/// `sqlite3` process writes a WAL-mode database, sqlite-rs reads the
/// committed rows back through the WAL overlay while that connection is
/// still open (see [`OracleSession`] for why the connection must stay
/// open for there to be anything in the WAL left to overlay).
#[test]
fn oracle_writes_wal_and_sqlite_rs_reads_live() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("oracle_writes_wal_and_sqlite_rs_reads_live");
        return;
    };

    let db = scratch_db("oracle-writes-rs-reads");
    let mut session = OracleSession::spawn(&oracle, &db);
    session.exec("PRAGMA journal_mode=WAL;");
    session.exec("CREATE TABLE t(a INTEGER, b TEXT);");
    session.exec("INSERT INTO t VALUES (2, 'from-sqlite3');");

    let vfs = UnixVfs;
    assert!(
        sqlite_rs::vfs::companion_path(&db, "-wal").exists(),
        "the oracle's still-open WAL write must leave a -wal file behind"
    );
    assert_eq!(our_select_t(&vfs, &db), "2|from-sqlite3");

    session.close();
}

/// **Scenario 3**: both engines alternate commits against the same
/// file — the strongest correctness proof, since each side must
/// correctly resume from (and read back through) WAL frames the *other*
/// engine appended. Exercises ADR-0026's rescan-on-every-flush design
/// (`WalWriter::open_existing`) against genuinely foreign frames, not
/// just our own earlier commits — and, since the oracle's connection
/// stays open throughout via [`OracleSession`], its second and third
/// writes must themselves notice (through the wal-index/`-shm`, exactly
/// the mechanism real concurrent `sqlite3` connections rely on) frames
/// sqlite-rs appended out-of-process in between.
#[test]
fn both_engines_alternate_writes_converge_on_the_same_final_state() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("both_engines_alternate_writes_converge_on_the_same_final_state");
        return;
    };

    let db = scratch_db("alternating-writers");
    let vfs = UnixVfs;

    // Step 1: the oracle creates the file, switches it to WAL, and
    // writes row 1 — the oracle goes first because sqlite-rs's own
    // `Pager::open` (unlike stock `sqlite3`) can't create a brand-new
    // database file from nothing (see this file's module doc).
    let mut session = OracleSession::spawn(&oracle, &db);
    session.exec("PRAGMA journal_mode=WAL;");
    session.exec("CREATE TABLE t(a INTEGER, b TEXT);");
    session.exec("INSERT INTO t VALUES (1, 'sqlite3');");

    // Step 2: sqlite-rs appends row 2 onto the WAL the oracle started —
    // must resume past the oracle's frame from step 1, not start fresh.
    let page_size = declared_page_size(&vfs, &db);
    run_our_session(&vfs, &db, page_size, &["INSERT INTO t VALUES (2, 'rs')"]);

    // Step 3: the oracle's still-open connection appends row 3 — must
    // itself notice sqlite-rs's out-of-process frame from step 2.
    session.exec("INSERT INTO t VALUES (3, 'sqlite3');");

    // Step 4: sqlite-rs appends row 4 onto the oracle's step-3 frame.
    run_our_session(&vfs, &db, page_size, &["INSERT INTO t VALUES (4, 'rs')"]);

    let expected = "1|sqlite3\n2|rs\n3|sqlite3\n4|rs";
    assert_eq!(our_select_t(&vfs, &db), expected);
    assert_eq!(
        session.query("SELECT * FROM t ORDER BY a;"),
        expected,
        "the oracle's own still-open connection must see every row too"
    );

    session.close();
}

/// **Scenario 4a**: sqlite-rs runs a passive checkpoint (backfilling
/// WAL frames — including ones the oracle itself wrote — into the main
/// file), then the oracle reads the data back correctly.
#[test]
fn sqlite_rs_checkpoints_then_oracle_reads_backfilled_data() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("sqlite_rs_checkpoints_then_oracle_reads_backfilled_data");
        return;
    };

    let db = scratch_db("rs-checkpoints-oracle-reads");
    oracle_exec(
        &oracle,
        &db,
        "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT); \
         INSERT INTO t VALUES (1, 'before-checkpoint');",
    );

    let vfs = UnixVfs;
    let page_size = declared_page_size(&vfs, &db);
    let any_vfs = AnyVfs::new(UnixVfs);
    let result = checkpoint_passive(&any_vfs, &db, page_size).unwrap();
    assert!(
        result.checkpoint_complete,
        "no concurrent reader is pinning a frame, so a passive checkpoint must fully complete"
    );

    assert_eq!(oracle_select_t(&oracle, &db), "1|before-checkpoint");
}

/// **Scenario 4b**: the reverse direction — the oracle runs `PRAGMA
/// wal_checkpoint(PASSIVE)`, sqlite-rs reads the backfilled data back
/// (through the main file, since the checkpoint emptied the WAL's
/// unbackfilled tail).
#[test]
fn oracle_checkpoints_then_sqlite_rs_reads_backfilled_data() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("oracle_checkpoints_then_sqlite_rs_reads_backfilled_data");
        return;
    };

    let db = scratch_db("oracle-checkpoints-rs-reads");
    oracle_exec(
        &oracle,
        &db,
        "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT); \
         INSERT INTO t VALUES (1, 'before-checkpoint');",
    );
    oracle_exec(&oracle, &db, "PRAGMA wal_checkpoint(PASSIVE);");

    let vfs = UnixVfs;
    assert_eq!(our_select_t(&vfs, &db), "1|before-checkpoint");
}
