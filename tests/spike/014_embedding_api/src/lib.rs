//! Spike 014 (#682): can spec 013's embedding API be a layer over the
//! engine, or does it need engine surgery?
//!
//! Three prototypes of the same operation — run a `SELECT` and read its
//! rows — differing only in where the engine lives and how rows reach
//! the caller:
//!
//! 1. [`batch`] — control. `execute_with_db_and_params` on the caller's
//!    thread, exactly as `examples/query.rs` does it. No engine change.
//! 2. [`worker_batch`] — the engine's `Rc` graph lives on a thread the
//!    connection owns; the whole result set crosses a channel per
//!    statement. Isolates spec 013/Req-4 (`Send + Sync`).
//! 3. [`worker_stream`] — same worker, but rows cross one at a time
//!    through a `sync_channel(1)`, so the caller's `next()` applies real
//!    backpressure to the VDBE. Exercises Req-4 and Req-7 together.
//!
//! Prototype 3 is the one that needed the engine change: `Execution`
//! (`src/vdbe/exec.rs`, this branch only). Prototypes 1 and 2 use only
//! the public API as it already shipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_with_catalog, resolve_from_table_schema};
use sqlite_rs::dump;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::{Execution, Vm};
use sqlite_rs::vfs::{PageSource, UnixVfs};

/// A row as it exists *inside* the engine: `Value` carries `Rc`.
pub type EngineRow = Vec<Value>;

/// A row as it crosses the worker boundary.
pub type Row = Vec<SendValue>;

/// A `Value` that can cross a thread boundary.
///
/// **This type is spike 014's central finding.** Spec 013/Req-4 and
/// ADR-0034 both locate the `Send` problem in the pager
/// (`Rc<dyn PageSource>`, `Rc<RefCell<Pager>>`) and conclude that a
/// worker thread which "creates its `Rc` graph and never lets it leave"
/// is sufficient. It is not: `Value::Text(Rc<str>)` and
/// `Value::Blob(Rc<[u8]>)` (`src/record/value.rs:15-17`) make `Value`
/// itself `!Send`, and a *result row is a `Vec<Value>`*. Rows are
/// exactly the thing that has to leave the worker, so the boundary needs
/// an owned representation and pays a copy for every text and blob.
///
/// Integers, reals and nulls cross free — the copy is proportional to
/// text/blob payload, not to row count.
#[derive(Debug, Clone, PartialEq)]
pub enum SendValue {
    /// SQL `NULL`.
    Null,
    /// A signed integer.
    Integer(i64),
    /// An IEEE 754 double.
    Real(f64),
    /// Text, copied out of its `Rc<str>`.
    Text(String),
    /// A blob, copied out of its `Rc<[u8]>`.
    Blob(Vec<u8>),
}

impl SendValue {
    /// Copies `value` into an owned, `Send` form.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Integer(i) => Self::Integer(*i),
            Value::Real(r) => Self::Real(*r),
            Value::Text(t) => Self::Text(t.to_string()),
            Value::Blob(b) => Self::Blob(b.to_vec()),
        }
    }

    /// Rebuilds an engine-side `Value`. Used to bind parameters that
    /// arrived from another thread, and by the agreement test to compare
    /// the control against the worker prototypes.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(i) => Value::Integer(*i),
            Self::Real(r) => Value::Real(*r),
            Self::Text(t) => Value::Text(t.as_str().into()),
            Self::Blob(b) => Value::Blob(b.as_slice().into()),
        }
    }
}

/// Converts an engine row for transport.
#[must_use]
pub fn send_row(row: &[Value]) -> Row {
    row.iter().map(SendValue::from_value).collect()
}

/// Errors are stringified at the worker boundary: `ExecError` is not
/// `Send`-bound by contract and a spike does not need a typed error to
/// answer its question. A real facade would define its own.
pub type SpikeResult<T> = Result<T, String>;

// ---------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------

/// Builds (once) a fixture database with `rows` rows via the stock
/// `sqlite3` on `PATH`, the same oracle the corpus harness uses.
///
/// Deliberately `INTEGER PRIMARY KEY` (a rowid alias) and a *named*
/// unique index rather than a composite `PRIMARY KEY`: the composite
/// shape triggers the `sqlite_autoindex_*` corruption of spec 010/Req-8,
/// and this spike must not measure a known bug.
pub fn fixture(rows: usize) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("bench_{rows}.db"));
    if path.exists() {
        return path;
    }
    // Cargo runs a binary's tests in parallel and the two test binaries
    // in parallel with each other, so this is contended two ways. The
    // mutex covers threads inside one binary; building into a
    // pid-unique temporary and renaming covers separate processes,
    // since POSIX `rename` is atomic. Without both, concurrent
    // `sqlite3` writers race and lose with "database is locked (5)".
    static FIXTURE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if path.exists() {
        return path;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let sql = format!(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, bucket INTEGER NOT NULL);
         INSERT INTO t (id, name, bucket)
           WITH RECURSIVE seq(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM seq WHERE i < {rows})
           SELECT i, 'name-' || i, i % 100 FROM seq;
         CREATE INDEX t_bucket ON t (bucket);"
    );
    let out = Command::new("sqlite3")
        .arg(&tmp)
        .arg(&sql)
        .output()
        .expect("stock sqlite3 must be on PATH to build the spike fixture");
    assert!(
        out.status.success(),
        "sqlite3 failed to build fixture: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::rename(&tmp, &path).expect("publishing the fixture must be atomic");
    path
}

// ---------------------------------------------------------------------
// Shared engine setup
// ---------------------------------------------------------------------

/// Opens `path` and compiles `sql`, returning everything the VDBE needs.
///
/// This is the glue `examples/README.md` describes every consumer
/// writing by hand today, and is identical in all three prototypes —
/// which is the point: the prototypes differ in *delivery*, not in
/// compilation.
fn open_and_compile(
    path: &Path,
    sql: &str,
) -> SpikeResult<(
    Rc<dyn PageSource>,
    sqlite_rs::header::DatabaseHeader,
    sqlite_rs::vdbe::Program,
)> {
    let (header, pager) = dump::open(&UnixVfs, path).map_err(|e| e.to_string())?;
    let schemas = {
        let mut schema_cursor = TableCursor::new(&pager, &header, 1);
        read_schema(&mut schema_cursor, header.text_encoding).map_err(|e| e.to_string())?
    };
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        _ => return Err(format!("failed to parse: {sql}")),
    };
    let from = select.from.as_ref().ok_or("SELECT has no FROM clause")?;
    let table = resolve_from_table_schema(&from.first, &schemas).map_err(|e| e.to_string())?;
    let program =
        compile_select_with_catalog(&select, &table, &schemas).map_err(|e| e.to_string())?;
    let source: Rc<dyn PageSource> = Rc::new(pager);
    Ok((source, header, program))
}

// ---------------------------------------------------------------------
// 1. batch — control
// ---------------------------------------------------------------------

pub mod batch {
    use super::{open_and_compile, send_row, EngineRow, Row, SpikeResult, Value};
    use sqlite_rs::vdbe::execute_with_db_and_params;
    use std::path::Path;

    /// Today's shape: the whole result set materializes before the
    /// caller sees row 0. No thread, no channel, no engine change.
    ///
    /// Returns engine-side rows: on one thread there is no boundary, so
    /// no copy is owed. That asymmetry is the point — the control does
    /// not pay what the worker prototypes pay.
    pub fn query(path: &Path, sql: &str, params: Vec<Value>) -> SpikeResult<Vec<EngineRow>> {
        let (source, header, program) = open_and_compile(path, sql)?;
        execute_with_db_and_params(&program, source, header, params).map_err(|e| e.to_string())
    }

    /// The control's rows in transport form, for comparing against the
    /// worker prototypes in the agreement test.
    pub fn query_sendable(path: &Path, sql: &str, params: Vec<Value>) -> SpikeResult<Vec<Row>> {
        Ok(query(path, sql, params)?.iter().map(|r| send_row(r)).collect())
    }
}

// ---------------------------------------------------------------------
// Worker protocol, shared by prototypes 2 and 3
// ---------------------------------------------------------------------

enum Cmd {
    /// Collect every row, then send the whole set once.
    Batch {
        sql: String,
        params: Row,
        reply: SyncSender<SpikeResult<Vec<Row>>>,
    },
    /// Send rows one at a time; `Ok(None)` terminates the stream.
    Stream {
        sql: String,
        params: Row,
        reply: SyncSender<SpikeResult<Option<Row>>>,
    },
    /// Send rows in groups of `chunk`; `Ok(None)` terminates the stream.
    Chunked {
        sql: String,
        params: Row,
        chunk: usize,
        reply: SyncSender<SpikeResult<Option<Vec<Row>>>>,
    },
}

/// A `Send + Sync` handle to an engine that is itself neither.
///
/// The `Rc` graph is created on the worker thread and never leaves it;
/// only `Cmd`s and `Row`s cross the boundary. `sqlx`'s own SQLite driver
/// does this for a C `sqlite3*` — see ADR-0034's citation of
/// `sqlx-sqlite-0.9.0/src/connection/worker.rs`.
///
/// `Mutex<Sender<_>>` rather than a bare `Sender`: `mpsc::Sender<T>` is
/// `Send` but **not** `Sync`, so a bare field would make `Connection`
/// un-shareable behind an `Arc` and Req-4's "shared across threads"
/// scenario unimplementable.
pub struct Connection {
    cmd: Mutex<mpsc::Sender<Cmd>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Connection {
    /// Spawns the worker and opens `path` on it.
    pub fn open(path: &Path) -> SpikeResult<Self> {
        let path = path.to_path_buf();
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<SpikeResult<()>>();
        let worker = std::thread::Builder::new()
            .name("sqlite-rs-conn".into())
            .spawn(move || worker_loop(&path, &rx, &ready_tx))
            .map_err(|e| e.to_string())?;
        // Surface an open failure to the caller instead of letting the
        // worker die silently and every later send look like a closed
        // channel.
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(format!("worker died before signalling ready: {e}")),
        }
        Ok(Self {
            cmd: Mutex::new(tx),
            worker: Mutex::new(Some(worker)),
        })
    }

    fn send(&self, cmd: Cmd) -> SpikeResult<()> {
        self.cmd
            .lock()
            .map_err(|_| "connection mutex poisoned".to_string())?
            .send(cmd)
            .map_err(|_| "engine is gone".to_string())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Dropping the only `Sender` ends the worker's `recv` loop; the
        // join then guarantees the engine's files are closed before
        // `Connection::drop` returns, which is what makes a
        // reopen-immediately-after-drop test deterministic.
        if let Ok(mut guard) = self.cmd.lock() {
            let (dead, _) = mpsc::channel();
            *guard = dead;
        }
        if let Ok(mut guard) = self.worker.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

fn worker_loop(path: &Path, rx: &Receiver<Cmd>, ready: &mpsc::Sender<SpikeResult<()>>) {
    // Prove the file opens before reporting ready, so `open` can fail
    // loudly. The pager/`Rc` graph below is rebuilt per command: one
    // `Rc<dyn PageSource>` cached across commands is the real design,
    // but it is not what this spike is measuring and it would confound
    // the streaming numbers with page-cache warmth.
    if let Err(e) = dump::open(&UnixVfs, path) {
        let _ = ready.send(Err(e.to_string()));
        return;
    }
    let _ = ready.send(Ok(()));

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Batch { sql, params, reply } => {
                let bound = params.iter().map(SendValue::to_value).collect();
                let _ = reply.send(batch::query_sendable(path, &sql, bound));
            }
            Cmd::Stream { sql, params, reply } => {
                stream_one(path, &sql, params, &reply);
            }
            Cmd::Chunked {
                sql,
                params,
                chunk,
                reply,
            } => {
                stream_chunked(path, &sql, params, chunk, &reply);
            }
        }
    }
}

/// Drives one statement, sending each row as it is produced.
///
/// A failed `send` means the caller dropped the receiving end
/// mid-iteration. Breaking out drops `Execution` — and with it the `Vm`
/// and its open cursors — which is Req-7's "abandoning a statement
/// releases it", enforced by control flow rather than by a comment.
fn stream_one(
    path: &Path,
    sql: &str,
    params: Row,
    reply: &SyncSender<SpikeResult<Option<Row>>>,
) {
    let (source, header, program) = match open_and_compile(path, sql) {
        Ok(parts) => parts,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    let mut vm = Vm::with_db(source, header);
    vm.bind_params(params.iter().map(SendValue::to_value).collect());
    let mut execution = Execution::new(vm, &program);
    loop {
        match execution.next_row() {
            Ok(Some(row)) => {
                // The copy Req-4 actually owes, paid here.
                if reply.send(Ok(Some(send_row(&row)))).is_err() {
                    return; // caller abandoned the stream
                }
            }
            Ok(None) => {
                let _ = reply.send(Ok(None));
                return;
            }
            Err(e) => {
                let _ = reply.send(Err(e.to_string()));
                return;
            }
        }
    }
}

/// Drives one statement, sending rows in groups of `chunk`.
///
/// Spike finding: prototype 3's one-row-per-message design costs ~4.5us
/// per row in thread handoff, which makes a full scan 42x slower than
/// the control. Amortizing the handoff over `chunk` rows is the fix, and
/// it costs nothing in memory: the in-flight bound becomes `chunk` rows
/// instead of 1, still constant in result size.
fn stream_chunked(
    path: &Path,
    sql: &str,
    params: Row,
    chunk: usize,
    reply: &SyncSender<SpikeResult<Option<Vec<Row>>>>,
) {
    let (source, header, program) = match open_and_compile(path, sql) {
        Ok(parts) => parts,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    let mut vm = Vm::with_db(source, header);
    vm.bind_params(params.iter().map(SendValue::to_value).collect());
    let mut execution = Execution::new(vm, &program);
    let chunk = chunk.max(1);
    let mut buffer: Vec<Row> = Vec::with_capacity(chunk);
    loop {
        match execution.next_row() {
            Ok(Some(row)) => {
                buffer.push(send_row(&row));
                if buffer.len() >= chunk {
                    if reply.send(Ok(Some(std::mem::take(&mut buffer)))).is_err() {
                        return; // caller abandoned the stream
                    }
                    buffer = Vec::with_capacity(chunk);
                }
            }
            Ok(None) => {
                if !buffer.is_empty() {
                    let _ = reply.send(Ok(Some(buffer)));
                }
                let _ = reply.send(Ok(None));
                return;
            }
            Err(e) => {
                let _ = reply.send(Err(e.to_string()));
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------
// 2. worker_batch — Req 4 alone
// ---------------------------------------------------------------------

impl Connection {
    /// Whole result set in one channel message. Isolates the cost of
    /// the thread boundary from the cost of streaming.
    pub fn query_batch(&self, sql: &str, params: Row) -> SpikeResult<Vec<Row>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Batch {
            sql: sql.to_string(),
            params,
            reply,
        })?;
        rx.recv().map_err(|_| "engine is gone".to_string())?
    }

    // -----------------------------------------------------------------
    // 3. worker_stream — Req 4 + Req 7
    // -----------------------------------------------------------------

    /// One row at a time, with backpressure: `sync_channel(1)` blocks
    /// the VDBE until the caller takes the previous row, so a consumer
    /// that stops consuming stops the engine.
    pub fn query_stream(&self, sql: &str, params: Row) -> SpikeResult<RowStream> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Stream {
            sql: sql.to_string(),
            params,
            reply,
        })?;
        Ok(RowStream { rx })
    }

    /// Rows in groups of `chunk`, amortizing the thread handoff.
    /// Memory stays bounded: `chunk` rows in flight rather than 1.
    pub fn query_chunked(
        &self,
        sql: &str,
        params: Row,
        chunk: usize,
    ) -> SpikeResult<ChunkedRowStream> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Chunked {
            sql: sql.to_string(),
            params,
            chunk,
            reply,
        })?;
        Ok(ChunkedRowStream {
            rx,
            buffer: std::collections::VecDeque::new(),
            done: false,
        })
    }
}

/// The caller's end of a chunked result set, flattened back to rows so
/// the chunk size stays an implementation detail — which is the point:
/// a real `Statement::next_row` would expose exactly this API.
pub struct ChunkedRowStream {
    rx: Receiver<SpikeResult<Option<Vec<Row>>>>,
    buffer: std::collections::VecDeque<Row>,
    done: bool,
}

impl Iterator for ChunkedRowStream {
    type Item = SpikeResult<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.buffer.pop_front() {
                return Some(Ok(row));
            }
            if self.done {
                return None;
            }
            match self.rx.recv() {
                Ok(Ok(Some(chunk))) => self.buffer.extend(chunk),
                Ok(Ok(None)) => {
                    self.done = true;
                    return None;
                }
                Ok(Err(e)) => {
                    self.done = true;
                    return Some(Err(e));
                }
                Err(_) => {
                    self.done = true;
                    return Some(Err("engine is gone".to_string()));
                }
            }
        }
    }
}

/// The caller's end of a streamed result set.
pub struct RowStream {
    rx: Receiver<SpikeResult<Option<Row>>>,
}

impl Iterator for RowStream {
    type Item = SpikeResult<Row>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.rx.recv() {
            Ok(Ok(Some(row))) => Some(Ok(row)),
            Ok(Ok(None)) => None,
            Ok(Err(e)) => Some(Err(e)),
            Err(_) => Some(Err("engine is gone".to_string())),
        }
    }
}

// ---------------------------------------------------------------------
// Compile-time proof of Req 4
// ---------------------------------------------------------------------

/// Req-4 asks for a `Send + Sync` handle. Asserting it in prose is
/// worthless; this fails the build if it ever stops being true.
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<Connection>();
/// `SendValue` is what makes the boundary possible. The engine's own
/// `Value` is deliberately absent from this list: adding
/// `assert_send_sync::<Value>()` does not compile, which is the finding.
const _: () = assert_send_sync::<SendValue>();

/// `RowStream` is `Send` (it can be handed to another thread) but is
/// deliberately not asserted `Sync`: a `Receiver` is not, and a shared
/// cursor into a result set has no coherent meaning anyway.
const fn assert_send<T: Send>() {}
const _: () = assert_send::<RowStream>();
const _: () = assert_send::<ChunkedRowStream>();
