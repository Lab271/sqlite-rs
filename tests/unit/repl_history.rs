#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! REPL history persistence (#551): `~/.sqlite-rs_history` survives
//! across sessions. Driven non-interactively via piped stdin, same
//! `Command`+`CARGO_BIN_EXE_sqlite-rs` pattern as
//! `tests/unit/repl_dot_commands.rs`, with `$HOME` overridden to a
//! scratch directory so the real user's history file is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-repl-history-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh scratch db with `t(a)`, created via `sqlite-rs exec` — `repl`
/// only opens an existing database file, never creates one.
fn seed_db(dir: &Path) -> PathBuf {
    let db = dir.join("scratch.db");
    let status = Command::new(CLI)
        .arg("exec")
        .arg(&db)
        .arg("CREATE TABLE t(a)")
        .status()
        .unwrap();
    assert!(status.success());
    db
}

fn run_repl_script(home: &Path, db: &Path, script: &str) {
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(db)
        .env("HOME", home)
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
    let out = child.wait_with_output().expect("waiting for repl");
    assert!(
        out.status.success(),
        "repl script failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn history_persists_across_sessions() {
    let home = scratch_dir("persists");
    let db = seed_db(&home);
    let history_file = home.join(".sqlite-rs_history");

    run_repl_script(&home, &db, "SELECT 1;\n.quit\n");
    assert!(
        history_file.exists(),
        "expected {} to be written",
        history_file.display()
    );
    let contents = std::fs::read_to_string(&history_file).unwrap();
    assert!(
        contents.contains("SELECT 1;"),
        "history file missing the submitted statement: {contents:?}"
    );

    run_repl_script(&home, &db, ".headers on\n.quit\n");
    let contents = std::fs::read_to_string(&history_file).unwrap();
    assert!(
        contents.contains("SELECT 1;") && contents.contains(".headers on"),
        "second session should append to, not replace, existing history: {contents:?}"
    );
}

#[test]
fn missing_home_does_not_fail_the_session() {
    let home = scratch_dir("no-home");
    let db = seed_db(&home);
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(&db)
        .env_remove("HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {CLI} repl {}: {e}", db.display()));
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(b"SELECT 1;\n.quit\n")
        .expect("writing repl script");
    let out = child.wait_with_output().expect("waiting for repl");
    assert!(
        out.status.success(),
        "repl script failed without $HOME: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
