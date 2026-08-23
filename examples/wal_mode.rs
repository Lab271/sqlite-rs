//! Switches a database to WAL journal mode, writes and reads through it,
//! then checkpoints the WAL back into the main file.
//!
//! True multi-process concurrent readers/writer is out of scope for a
//! single-binary example — see `tests/corpus/wal_concurrent_interop_test.rs`
//! and `tests/corpus/wal_write_interop_test.rs` for that.
//!
//! Run with: `cargo run --example wal_mode`

use std::cell::RefCell;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::dump;
use sqlite_rs::header::JournalMode;
use sqlite_rs::pager::checkpoint::checkpoint_passive;
use sqlite_rs::parser::split_statements;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::{AnyVfs, UnixVfs};

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fixtures/empty.db");
    let scratch_dir =
        std::env::temp_dir().join(format!("sqlite-rs-wal-example-{}", std::process::id()));
    std::fs::create_dir_all(&scratch_dir)?;
    let scratch_db = scratch_dir.join("wal.db");
    std::fs::copy(&fixture, &scratch_db)?;

    let (_, mut pager) = dump::open(&UnixVfs, &scratch_db)?;
    pager.set_journal_mode(JournalMode::Wal)?;
    drop(pager);

    // Header bytes were flipped on disk by `set_journal_mode`; re-open to
    // pick up the now-WAL header rather than reusing the stale in-memory copy.
    let (header, pager) = dump::open(&UnixVfs, &scratch_db)?;
    println!("journal mode: {:?}", header.journal_mode());

    let pager = Rc::new(RefCell::new(pager));

    let script = "
        CREATE TABLE events(id INTEGER PRIMARY KEY, payload TEXT);
        INSERT INTO events(id, payload) VALUES (1, 'first write');
    ";

    let mut autocommit = true;
    for stmt in split_statements(script) {
        let (schemas, views) = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            let schemas = read_schema(&mut schema_cursor, header.text_encoding)?;
            let mut view_cursor = TableCursor::new(&*borrowed, &header, 1);
            let views = read_views(&mut view_cursor, header.text_encoding)?;
            (schemas, views)
        };
        let program = compile_statement(&stmt, &schemas, &views).map_err(|e| e.to_string())?;
        let (_, ac) = execute_transaction_step(&program, Rc::clone(&pager), header, autocommit)
            .map_err(|e| e.to_string())?;
        autocommit = ac;
    }

    println!("wrote through WAL; checkpointing back into the main file...");
    let vfs = AnyVfs::new(UnixVfs);
    let result = checkpoint_passive(&vfs, &scratch_db, header.page_size)?;
    println!(
        "checkpointed {} of {} frames (complete: {})",
        result.backfilled_frames, result.total_frames, result.checkpoint_complete
    );

    std::fs::remove_dir_all(&scratch_dir).ok();
    Ok(())
}
