// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Helpers for driving a real, stock `sqlite3` CLI process as the "other side"
//! of each locking experiment.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Run `sqlite3 <db> "<sql>"` to completion and capture stdout/stderr/status.
pub fn run_sql(db: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(sql)
        .output()
        .expect("failed to spawn sqlite3");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A long-lived `sqlite3 -batch <db>` session we drive one line at a time,
/// so we can hold a transaction open across our own lock probes.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    pub fn spawn(db: &str) -> Self {
        let mut child = Command::new("sqlite3")
            .arg("-batch")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sqlite3 session");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            stdin,
            stdout,
        }
    }

    pub fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to sqlite3 stdin failed");
        self.stdin.flush().ok();
    }

    /// Send `line`, then block until sqlite3 has actually executed it by
    /// following it with a `SELECT` sentinel and reading that sentinel back —
    /// this is what proves the preceding statement (e.g. BEGIN EXCLUSIVE)
    /// really completed, not just that we wrote bytes to a pipe.
    pub fn send_and_sync(&mut self, line: &str, sentinel: &str) {
        self.send(line);
        self.send(&format!("SELECT '{sentinel}';"));
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self
                .stdout
                .read_line(&mut buf)
                .expect("read sqlite3 stdout failed");
            if n == 0 {
                panic!("sqlite3 session closed stdout before sentinel {sentinel:?}");
            }
            if buf.trim() == sentinel {
                return;
            }
        }
    }

    /// Close stdin and wait for the session to exit. Callers are responsible
    /// for sending any COMMIT/ROLLBACK themselves before calling this.
    pub fn wait(self) -> std::process::ExitStatus {
        let Session {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        child.wait().expect("sqlite3 session wait failed")
    }
}

/// Re-exec this same test binary as a detached probe process, so a lock
/// check genuinely comes from a second OS process, not a second fd in ours.
pub fn probe_in_subprocess(mode: &str, db: &str) -> String {
    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new(exe)
        .arg("--probe")
        .arg(mode)
        .arg(db)
        .output()
        .expect("failed to spawn probe subprocess");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
