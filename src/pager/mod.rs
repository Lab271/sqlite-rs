//! The page-access layer between the [`Vfs`] and the b-tree cursor. See
//! `.openspec/specs/007-pager/spec.md` for the requirements this
//! implements.
//!
//! [`Pager`] implements [`PageSource`] directly, so `TableCursor<Pager>` /
//! `IndexCursor<Pager>` behave exactly like `TableCursor<VfsPageSource>` on
//! every already-covered fixture (007-pager Requirement 1's "zero behavior
//! change" scenario) — `Pager::open` only adds a check that runs once,
//! before any page is read.
//!
//! Freelist / pointer-map pages need no special handling here: the b-tree
//! cursor only ever visits pages reachable by following explicit child
//! pointers out of a b-tree page, and freelist/pointer-map pages are never
//! part of that structure (they're addressed only by a raw sequential scan,
//! which this read path never performs) — see `autovacuum_fixture_reads_identically`.
//!
//! Locking is out of scope: spike 004 (#8), which would decide whether a
//! safe reader needs a SHARED `fcntl` lock before reading, is still open.
//! Deferred until #8 lands, per this module's own ticket (#35).

mod error;

pub use error::PagerError;

use std::path::Path;

use crate::vfs::{companion_path, PageError, PageSource, Vfs, VfsPageSource};

/// The 8-byte magic that opens a valid rollback-journal header (SQLite
/// file-format reference, "The Rollback Journal"). A `-journal` file with
/// different leading bytes (e.g. all zero, from `PRAGMA
/// journal_mode=PERSIST`'s post-commit zeroing, or a short/empty file) is
/// not hot — it is safe to open the main file alongside it.
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// A source of whole database pages, refusing to open a database with a
/// hot rollback journal rather than risk serving pre-rollback pages as
/// committed data (001-architecture Req-4's "hot journal is never ignored"
/// scenario).
pub struct Pager {
    source: VfsPageSource,
}

impl Pager {
    /// Opens `path` (page size `page_size`) through `vfs`. Returns
    /// [`PagerError::HotJournal`] if an adjacent `-journal` file has a
    /// valid rollback-journal header.
    pub fn open<V: Vfs>(vfs: &V, path: &Path, page_size: u32) -> Result<Self, PagerError> {
        let journal_path = companion_path(path, "-journal");
        if vfs.exists(&journal_path)? {
            let journal = vfs.open_read(&journal_path)?;
            let mut magic = [0u8; JOURNAL_MAGIC.len()];
            let n = journal.read_at(&mut magic, 0)?;
            if n == JOURNAL_MAGIC.len() && magic == JOURNAL_MAGIC {
                return Err(PagerError::HotJournal {
                    path: journal_path.display().to_string(),
                });
            }
        }
        let source = VfsPageSource::open(vfs, path, page_size)?;
        Ok(Pager { source })
    }
}

impl PageSource for Pager {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        self.source.read_page(page_num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MemoryVfs;
    use std::path::PathBuf;

    fn db_with_journal(journal_bytes: Option<&[u8]>) -> (MemoryVfs, PathBuf) {
        let mut vfs = MemoryVfs::new();
        let page = vec![0u8; 512];
        vfs.insert("/test.db", page);
        if let Some(bytes) = journal_bytes {
            vfs.insert("/test.db-journal", bytes.to_vec());
        }
        (vfs, PathBuf::from("/test.db"))
    }

    #[test]
    fn no_journal_opens_cleanly() {
        let (vfs, path) = db_with_journal(None);
        assert!(Pager::open(&vfs, &path, 512).is_ok());
    }

    #[test]
    fn hot_journal_is_refused() {
        let (vfs, path) = db_with_journal(Some(&JOURNAL_MAGIC));
        let result = Pager::open(&vfs, &path, 512);
        assert!(matches!(result, Err(PagerError::HotJournal { .. })));
    }

    #[test]
    fn zeroed_persist_mode_journal_is_not_hot() {
        let (vfs, path) = db_with_journal(Some(&[0u8; 8]));
        assert!(Pager::open(&vfs, &path, 512).is_ok());
    }

    #[test]
    fn empty_journal_file_is_not_hot() {
        let (vfs, path) = db_with_journal(Some(&[]));
        assert!(Pager::open(&vfs, &path, 512).is_ok());
    }

    #[test]
    fn short_journal_file_is_not_hot() {
        let (vfs, path) = db_with_journal(Some(&JOURNAL_MAGIC[..4]));
        assert!(Pager::open(&vfs, &path, 512).is_ok());
    }

    #[test]
    fn pager_reads_pages_identically_to_vfs_page_source() {
        let mut vfs = MemoryVfs::new();
        let mut contents = vec![1u8; 512];
        contents.extend(vec![2u8; 512]);
        vfs.insert("/test.db", contents);

        let pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();
        assert_eq!(pager.read_page(1).unwrap(), vec![1u8; 512]);
        assert_eq!(pager.read_page(2).unwrap(), vec![2u8; 512]);
    }

    mod fixtures {
        use super::*;
        use crate::btree::TableCursor;
        use crate::header::DatabaseHeader;
        use crate::record::{decode_record, TextEncoding, Value};
        use crate::schema::read_schema;
        use crate::vfs::UnixVfs;

        fn header_of(vfs: &UnixVfs, path: &Path) -> DatabaseHeader {
            let file = vfs.open_read(path).unwrap();
            let mut buf = [0u8; 100];
            file.read_at(&mut buf, 0).unwrap();
            DatabaseHeader::parse(&buf).unwrap()
        }

        fn text(v: &Value) -> &str {
            match v {
                Value::Text(s) => s,
                other => panic!("expected text, got {other:?}"),
            }
        }

        /// 001-architecture Req-4's "Hot journal is never ignored" scenario:
        /// the fixture's main file already has ~1999 uncommitted, spilled
        /// rows written into it (see tools/gen_fixtures.sh) — a reader that
        /// ignored the journal would see that wrong state. `Pager::open`
        /// must refuse before any page is read.
        #[test]
        fn hot_journal_fixture_is_refused() {
            let vfs = UnixVfs;
            let path = Path::new("tests/corpus/fixtures/journalstates/hot_journal.db");
            let header = header_of(&vfs, path);
            let result = Pager::open(&vfs, path, header.page_size);
            assert!(matches!(result, Err(PagerError::HotJournal { .. })));
        }

        /// "Zero behavior change on at-rest fixtures": the same assertions
        /// `src/btree/mod.rs`'s `single_page_table_first_row` makes against
        /// `VfsPageSource` must hold identically through `Pager`.
        #[test]
        fn table_single_page_fixture_reads_identically_through_pager() {
            let vfs = UnixVfs;
            let path = Path::new("tests/corpus/fixtures/btrees/table_single_page.db");
            let header = header_of(&vfs, path);
            let pager = Pager::open(&vfs, path, header.page_size).unwrap();
            let mut cursor = TableCursor::new(pager, &header, 1);

            let row = cursor.first().unwrap().unwrap();
            let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
            assert_eq!(text(&values[0]), "table");
            assert_eq!(text(&values[1]), "t");
            assert_eq!(text(&values[4]), "CREATE TABLE t(a INTEGER, b TEXT)");
        }

        /// Freelist/pointer-map awareness: the auto-vacuum fixture's
        /// pointer-map page (page 2) sits between sqlite_master (page 1)
        /// and table `t`'s actual root — root page is discovered via
        /// `read_schema`, never hardcoded, so this proves the b-tree
        /// cursor's pointer-following traversal is unaffected by the
        /// interleaved pointer-map page when run through `Pager`.
        #[test]
        fn autovacuum_fixture_reads_identically_through_pager() {
            let vfs = UnixVfs;
            let path = Path::new("tests/corpus/fixtures/features/autovacuum.db");
            let header = header_of(&vfs, path);

            let schema_pager = Pager::open(&vfs, path, header.page_size).unwrap();
            let mut schema_cursor = TableCursor::new(schema_pager, &header, 1);
            let schemas = read_schema(&mut schema_cursor, header.text_encoding).unwrap();
            let t = schemas
                .iter()
                .find(|s| s.name == "t")
                .expect("table t in sqlite_master");

            let pager = Pager::open(&vfs, path, header.page_size).unwrap();
            let mut cursor = TableCursor::new(pager, &header, t.root_page);
            let row = cursor.first().unwrap().unwrap();
            let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
            assert_eq!(text(&values[1]), "auto-vacuum full");
        }
    }
}
