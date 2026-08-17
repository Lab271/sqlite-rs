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
//! `Pager::open` acquires a journal-mode SHARED lock (#50) and, if a
//! `-shm` file is present, a WAL reader-mark lock (#45), before serving
//! any page — both released when the `Pager` drops. Spike 005 (#8,
//! closed) validated that this obligation is real and that byte-identical
//! `fcntl` locks interoperate correctly with a live stock `sqlite3`
//! process, including its checkpointer backing off on a held reader-mark.
//! Lock contention on either surfaces as [`crate::vfs::VfsError::Locked`].
//! The per-inode fd-cache for the `close()`-drops-all-locks trap remains
//! deferred (#45) — nothing here yet opens two fds to the same path, so
//! there is no bug for it to fix; see #45 for when that changes.

mod error;
pub mod freelist;
pub mod wal;

pub use error::PagerError;
pub use freelist::TrunkPage;

use std::collections::HashMap;
use std::path::Path;

use crate::vfs::{companion_path, FileLock, PageError, PageSource, Vfs, WritablePageSource};

/// The 8-byte magic that opens a valid rollback-journal header (SQLite
/// file-format reference, "The Rollback Journal"). A `-journal` file with
/// different leading bytes (e.g. all zero, from `PRAGMA
/// journal_mode=PERSIST`'s post-commit zeroing, or a short/empty file) is
/// not hot — it is safe to open the main file alongside it.
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// A source of whole database pages, refusing to open a database with a
/// hot rollback journal rather than risk serving pre-rollback pages as
/// committed data (001-architecture Req-4's "hot journal is never ignored"
/// scenario), and transparently overlaying any committed WAL frames from
/// an adjacent `-wal` file (Req-4's "Read a database with uncheckpointed
/// WAL" scenario).
pub struct Pager {
    /// Held only for its `Drop`, which releases the SHARED lock acquired
    /// in `open`. Declared before `source`: struct fields drop in
    /// declaration order, and the lock must be released while `source`'s
    /// file handle is still open — POSIX `close()` silently drops all
    /// `fcntl` locks on that fd, so unlocking a fd number the kernel may
    /// already have reused for something else would be a real bug.
    #[allow(dead_code, reason = "held only for its Drop side effect")]
    lock: FileLock,
    /// Held only for its `Drop`, which releases the WAL `-shm`
    /// reader-mark lock claimed in `open` (#45) — `None` when there is no
    /// `-shm` file to coordinate through (no live WAL writer has ever
    /// opened this database). Owns its own file handle (`WalReadLock` in
    /// `src/vfs/shm.rs`), so no interaction with `source`'s fd/drop
    /// ordering.
    #[allow(dead_code, reason = "held only for its Drop side effect")]
    wal_lock: Option<FileLock>,
    source: WritablePageSource,
    wal_pages: HashMap<u32, Vec<u8>>,
    /// Pages fetched via [`Pager::get_page_mut`] since the last
    /// [`Pager::flush`], keyed by page number (#166). Also consulted by
    /// [`Pager::read_page`] ahead of `wal_pages`/`source` so an
    /// unflushed write is visible to a subsequent read through the same
    /// `Pager`.
    dirty: HashMap<u32, Vec<u8>>,
    /// The page size this database was opened with (#167) — needed by
    /// [`Pager::allocate_page`] to size a freshly-extended page and by
    /// [`Pager::deallocate_page`] to compute a trunk page's leaf capacity,
    /// without re-deriving it from `source` on every call.
    page_size: u32,
}

/// Byte offsets of the three header fields ([`crate::header::DatabaseHeader`])
/// that freelist allocate/deallocate mutate: page count (bytes 28-31),
/// freelist trunk page (32-35), freelist page count (36-39). Patched
/// in-place on page 1's raw buffer rather than round-tripping through a
/// full header serializer, since no such serializer exists yet (#167).
const PAGE_COUNT_OFFSET: usize = 28;
const FREELIST_TRUNK_PAGE_OFFSET: usize = 32;
const FREELIST_PAGE_COUNT_OFFSET: usize = 36;

fn read_be_u32(buf: &[u8], offset: usize) -> Result<u32, freelist::FreelistError> {
    let end = offset.saturating_add(4);
    let bytes: [u8; 4] = buf
        .get(offset..end)
        .ok_or(freelist::FreelistError::PageTooShort {
            offset,
            len: buf.len(),
        })?
        .try_into()
        .map_err(|_| freelist::FreelistError::PageTooShort {
            offset,
            len: buf.len(),
        })?;
    Ok(u32::from_be_bytes(bytes))
}

fn write_be_u32(buf: &mut [u8], offset: usize, value: u32) -> Result<(), freelist::FreelistError> {
    let end = offset.saturating_add(4);
    let len = buf.len();
    let slice = buf
        .get_mut(offset..end)
        .ok_or(freelist::FreelistError::PageTooShort { offset, len })?;
    slice.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

impl Pager {
    /// Opens `path` (page size `page_size`) through `vfs`. Returns
    /// [`PagerError::HotJournal`] if an adjacent `-journal` file has a
    /// valid rollback-journal header, or [`PagerError::Wal`] if an
    /// adjacent non-empty `-wal` file's header is malformed or declares a
    /// page size that doesn't match `page_size`.
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

        // Claimed before reading WAL frames below, so a live checkpointer
        // that starts backfilling/truncating mid-read still backs off on
        // this reader's slot (#45) — pinning happens before, not after,
        // the read it protects.
        let wal_lock = vfs.claim_wal_read_lock(path)?;
        let wal_pages = read_wal_pages(vfs, path, page_size)?;

        let source = WritablePageSource::open(vfs, path, page_size)?;
        let lock = source.lock_shared()?;
        Ok(Pager {
            lock,
            wal_lock,
            source,
            wal_pages,
            dirty: HashMap::new(),
            page_size,
        })
    }

    /// Returns a mutable buffer for page `page_num` (1-based), reading it
    /// first if it isn't already dirty. Mutations are visible to
    /// subsequent [`PageSource::read_page`] calls on this same `Pager`
    /// immediately, but only reach disk once [`Pager::flush`] runs.
    pub fn get_page_mut(&mut self, page_num: u32) -> Result<&mut Vec<u8>, PagerError> {
        match self.dirty.entry(page_num) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let page = read_page(&self.wal_pages, &self.source, page_num)?;
                Ok(entry.insert(page))
            }
        }
    }

    /// Writes every dirty page back to the underlying file, in ascending
    /// page-number order, then `fsync`s and clears the dirty set. Ascending
    /// order isn't a correctness requirement of the plain rollback-journal
    /// path here (no partial-flush recovery exists yet in this pager — see
    /// #172), just a deterministic, easy-to-reason-about default.
    pub fn flush(&mut self) -> Result<(), PagerError> {
        let mut page_nums: Vec<u32> = self.dirty.keys().copied().collect();
        page_nums.sort_unstable();
        for page_num in page_nums {
            if let Some(bytes) = self.dirty.get(&page_num) {
                self.source.write_page(page_num, bytes)?;
            }
        }
        self.source.sync()?;
        self.dirty.clear();
        Ok(())
    }

    /// Allocates a page: pops one off the freelist if it's non-empty,
    /// otherwise extends the database by one page. Returns the allocated
    /// page's (1-based) number. Updates the freelist trunk/count fields
    /// (and, when extending, the page-count field) on page 1 in the same
    /// call, so a subsequent `flush` persists both the allocation and the
    /// header bookkeeping together.
    pub fn allocate_page(&mut self) -> Result<u32, PagerError> {
        let header = self.read_page(1)?;
        let page_count = read_be_u32(&header, PAGE_COUNT_OFFSET)?;
        let freelist_trunk_page = read_be_u32(&header, FREELIST_TRUNK_PAGE_OFFSET)?;
        let freelist_page_count = read_be_u32(&header, FREELIST_PAGE_COUNT_OFFSET)?;

        if freelist_trunk_page == 0 {
            let new_page_num = page_count.saturating_add(1);
            self.dirty
                .insert(new_page_num, vec![0u8; self.page_size as usize]);
            let page1 = self.get_page_mut(1)?;
            write_be_u32(page1, PAGE_COUNT_OFFSET, new_page_num)?;
            return Ok(new_page_num);
        }

        let trunk_buf = self.read_page(freelist_trunk_page)?;
        let mut trunk = TrunkPage::parse(&trunk_buf)?;

        let (allocated, new_trunk_page) = if let Some(leaf) = trunk.leaves.pop() {
            let trunk_buf = self.get_page_mut(freelist_trunk_page)?;
            trunk.write(trunk_buf)?;
            (leaf, freelist_trunk_page)
        } else {
            (freelist_trunk_page, trunk.next_trunk)
        };

        let page1 = self.get_page_mut(1)?;
        write_be_u32(page1, FREELIST_TRUNK_PAGE_OFFSET, new_trunk_page)?;
        write_be_u32(
            page1,
            FREELIST_PAGE_COUNT_OFFSET,
            freelist_page_count.saturating_sub(1),
        )?;
        Ok(allocated)
    }

    /// Returns `page_num` to the freelist: appended to the current trunk
    /// page's leaf array if it has room, otherwise `page_num` itself
    /// becomes the new trunk page (pointing at the old one). Updates the
    /// freelist trunk/count fields on page 1 in the same call.
    pub fn deallocate_page(&mut self, page_num: u32) -> Result<(), PagerError> {
        let header = self.read_page(1)?;
        let freelist_trunk_page = read_be_u32(&header, FREELIST_TRUNK_PAGE_OFFSET)?;
        let freelist_page_count = read_be_u32(&header, FREELIST_PAGE_COUNT_OFFSET)?;

        let max_leaves = freelist::max_leaves_per_trunk(self.page_size) as usize;
        let new_trunk_page = if freelist_trunk_page != 0 {
            let trunk_buf = self.read_page(freelist_trunk_page)?;
            let mut trunk = TrunkPage::parse(&trunk_buf)?;
            if trunk.leaves.len() < max_leaves {
                trunk.leaves.push(page_num);
                let trunk_buf = self.get_page_mut(freelist_trunk_page)?;
                trunk.write(trunk_buf)?;
                freelist_trunk_page
            } else {
                let new_trunk = TrunkPage {
                    next_trunk: freelist_trunk_page,
                    leaves: vec![],
                };
                let buf = self.get_page_mut(page_num)?;
                new_trunk.write(buf)?;
                page_num
            }
        } else {
            let new_trunk = TrunkPage {
                next_trunk: 0,
                leaves: vec![],
            };
            let buf = self.get_page_mut(page_num)?;
            new_trunk.write(buf)?;
            page_num
        };

        let page1 = self.get_page_mut(1)?;
        write_be_u32(page1, FREELIST_TRUNK_PAGE_OFFSET, new_trunk_page)?;
        write_be_u32(
            page1,
            FREELIST_PAGE_COUNT_OFFSET,
            freelist_page_count.saturating_add(1),
        )?;
        Ok(())
    }
}

/// Shared by [`Pager::read_page`] and [`Pager::get_page_mut`]: WAL overlay
/// first, then the underlying file.
fn read_page(
    wal_pages: &HashMap<u32, Vec<u8>>,
    source: &WritablePageSource,
    page_num: u32,
) -> Result<Vec<u8>, PageError> {
    if let Some(page) = wal_pages.get(&page_num) {
        return Ok(page.clone());
    }
    source.read_page(page_num)
}

/// Reads and merges committed WAL frames from `path`'s adjacent `-wal`
/// file, if one exists and is large enough to hold a header. A missing,
/// empty, or sub-header-length `-wal` file (the common case: a fully
/// checkpointed WAL truncates to empty) is not an error and yields no
/// overlay pages.
fn read_wal_pages<V: Vfs>(
    vfs: &V,
    path: &Path,
    page_size: u32,
) -> Result<HashMap<u32, Vec<u8>>, PagerError> {
    let wal_path = companion_path(path, "-wal");
    if !vfs.exists(&wal_path)? {
        return Ok(HashMap::new());
    }

    let wal_file = vfs.open_read(&wal_path)?;
    let size = wal_file.size()?;
    if size < wal::HEADER_LEN as u64 {
        return Ok(HashMap::new());
    }

    let mut bytes = vec![0u8; size as usize];
    let n = wal_file.read_at(&mut bytes, 0)?;
    bytes.truncate(n);
    if bytes.len() < wal::HEADER_LEN {
        return Ok(HashMap::new());
    }

    let to_pager_error = |source| PagerError::Wal {
        path: wal_path.display().to_string(),
        source,
    };

    let header = wal::WalHeader::parse(&bytes).map_err(to_pager_error)?;
    if header.page_size != page_size {
        return Err(to_pager_error(wal::WalError::InvalidPageSize {
            page_size: header.page_size,
        }));
    }

    let (pages, _committed_db_size) = wal::committed_pages(&header, &bytes);
    Ok(pages)
}

impl PageSource for Pager {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        if let Some(page) = self.dirty.get(&page_num) {
            return Ok(page.clone());
        }
        read_page(&self.wal_pages, &self.source, page_num)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
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
    fn wal_file_exactly_at_header_len_is_parsed_not_skipped_as_too_short() {
        // A `-wal` file of exactly `wal::HEADER_LEN` (32) bytes must be
        // handed to WalHeader::parse, not treated as "too short to hold a
        // header" — pins read_wal_pages's two `size < HEADER_LEN` checks
        // against mutation to `==`/`<=`, which would wrongly skip this
        // length instead of parsing it. All-zero bytes make an invalid
        // magic, so a real parse attempt surfaces as PagerError::Wal
        // rather than the Ok(empty-overlay) that skipping would produce.
        let (mut vfs, path) = db_with_journal(None);
        vfs.insert("/test.db-wal", vec![0u8; 32]);
        let result = Pager::open(&vfs, &path, 512);
        assert!(matches!(result, Err(PagerError::Wal { .. })));
    }

    /// 001-architecture Req-4's "Reader takes a SHARED lock before
    /// serving pages" scenario: a live `Pager` must hold the journal-mode
    /// SHARED lock (blocking a concurrent EXCLUSIVE lock attempt from
    /// another process) and release it once dropped.
    #[test]
    fn open_acquires_shared_lock_released_on_drop() {
        use crate::vfs::lock::exclusive_lock_available;
        use crate::vfs::UnixVfs;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sqlite-rs-pager-lock-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        std::fs::write(&path, vec![0u8; 512]).unwrap();

        let vfs = UnixVfs;
        let pager = Pager::open(&vfs, &path, 512).unwrap();

        assert!(
            !exclusive_lock_available(&path),
            "an open Pager must hold a SHARED lock blocking a concurrent EXCLUSIVE lock"
        );

        drop(pager);

        assert!(
            exclusive_lock_available(&path),
            "dropping the Pager must release the SHARED lock"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 001-architecture Req-4's WAL reader-mark scenario (#45): opening a
    /// `Pager` against a db with an adjacent `-shm` file must claim a WAL
    /// reader-mark slot, blocking a concurrent EXCLUSIVE lock attempt on
    /// that slot from another process, and release it on drop — the same
    /// shape as the journal-mode SHARED lock test above, one layer up.
    #[test]
    fn open_claims_wal_read_lock_when_shm_present_released_on_drop() {
        use crate::vfs::shm::slot_is_free_test_only;
        use crate::vfs::UnixVfs;
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "sqlite-rs-pager-wal-lock-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        std::fs::write(&path, vec![0u8; 512]).unwrap();
        let shm_path = dir.join("test.db-shm");
        std::fs::write(&shm_path, vec![0u8; 32768]).unwrap();

        let vfs = UnixVfs;
        let pager = Pager::open(&vfs, &path, 512).unwrap();

        let claimed_slot = (1..=4)
            .find(|&slot| !slot_is_free_test_only(&shm_path, slot))
            .expect("Pager::open must claim exactly one reader-mark slot");

        drop(pager);

        assert!(
            slot_is_free_test_only(&shm_path, claimed_slot),
            "dropping the Pager must release the WAL reader-mark lock"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 007-pager write-path Requirement 4/5's core roundtrip: a page
    /// mutated via `get_page_mut` reads back the new bytes immediately
    /// (before flush), and is still readable identically after `flush`
    /// clears the dirty set — from both `Pager::read_page` and a fresh
    /// `Pager::open` over the same underlying file.
    #[test]
    fn get_page_mut_then_flush_roundtrips() {
        let mut vfs = MemoryVfs::new();
        let mut contents = vec![1u8; 512];
        contents.extend(vec![2u8; 512]);
        vfs.insert("/test.db", contents);

        let mut pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();

        let page = pager.get_page_mut(2).unwrap();
        page.fill(9u8);
        assert_eq!(pager.read_page(2).unwrap(), vec![9u8; 512]);
        // Untouched page is unaffected.
        assert_eq!(pager.read_page(1).unwrap(), vec![1u8; 512]);

        pager.flush().unwrap();

        assert_eq!(pager.read_page(2).unwrap(), vec![9u8; 512]);

        let reopened = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();
        assert_eq!(reopened.read_page(2).unwrap(), vec![9u8; 512]);
        assert_eq!(reopened.read_page(1).unwrap(), vec![1u8; 512]);
    }

    #[test]
    fn flush_with_no_dirty_pages_is_a_no_op() {
        let mut vfs = MemoryVfs::new();
        vfs.insert("/test.db", vec![7u8; 512]);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();
        pager.flush().unwrap();
        assert_eq!(pager.read_page(1).unwrap(), vec![7u8; 512]);
    }

    /// A one-page database (empty freelist) allocates by extending the
    /// file, bumping the header's page-count field.
    #[test]
    fn allocate_with_empty_freelist_extends_file() {
        let mut vfs = MemoryVfs::new();
        let mut header = vec![0u8; 512];
        write_be_u32(&mut header, PAGE_COUNT_OFFSET, 1).unwrap();
        vfs.insert("/test.db", header);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();

        let allocated = pager.allocate_page().unwrap();
        assert_eq!(allocated, 2);
        let new_header = pager.read_page(1).unwrap();
        assert_eq!(read_be_u32(&new_header, PAGE_COUNT_OFFSET).unwrap(), 2);
        assert_eq!(pager.read_page(2).unwrap(), vec![0u8; 512]);
    }

    /// Deallocating a page with no existing freelist makes it the sole
    /// trunk page; allocating again pops that same page straight back
    /// off, without touching the page-count field.
    #[test]
    fn deallocate_then_allocate_round_trips_single_page() {
        let mut vfs = MemoryVfs::new();
        let mut contents = vec![0u8; 512 * 3];
        write_be_u32(&mut contents, PAGE_COUNT_OFFSET, 3).unwrap();
        vfs.insert("/test.db", contents);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();

        pager.deallocate_page(3).unwrap();
        let after_dealloc = pager.read_page(1).unwrap();
        assert_eq!(
            read_be_u32(&after_dealloc, FREELIST_TRUNK_PAGE_OFFSET).unwrap(),
            3
        );
        assert_eq!(
            read_be_u32(&after_dealloc, FREELIST_PAGE_COUNT_OFFSET).unwrap(),
            1
        );

        let allocated = pager.allocate_page().unwrap();
        assert_eq!(allocated, 3);
        let after_alloc = pager.read_page(1).unwrap();
        assert_eq!(
            read_be_u32(&after_alloc, FREELIST_TRUNK_PAGE_OFFSET).unwrap(),
            0
        );
        assert_eq!(
            read_be_u32(&after_alloc, FREELIST_PAGE_COUNT_OFFSET).unwrap(),
            0
        );
        // Page count untouched — this allocation came from the freelist,
        // not from extending the file.
        assert_eq!(read_be_u32(&after_alloc, PAGE_COUNT_OFFSET).unwrap(), 3);
    }

    /// A second deallocated page joins the existing trunk's leaf array
    /// instead of becoming a new trunk, and allocation pops leaves before
    /// ever consuming the trunk page itself.
    #[test]
    fn deallocate_appends_to_existing_trunk_leaves() {
        let mut vfs = MemoryVfs::new();
        let mut contents = vec![0u8; 512 * 4];
        write_be_u32(&mut contents, PAGE_COUNT_OFFSET, 4).unwrap();
        vfs.insert("/test.db", contents);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();

        pager.deallocate_page(3).unwrap();
        pager.deallocate_page(4).unwrap();
        let after_dealloc = pager.read_page(1).unwrap();
        assert_eq!(
            read_be_u32(&after_dealloc, FREELIST_TRUNK_PAGE_OFFSET).unwrap(),
            3
        );
        assert_eq!(
            read_be_u32(&after_dealloc, FREELIST_PAGE_COUNT_OFFSET).unwrap(),
            2
        );
        let trunk = TrunkPage::parse(&pager.read_page(3).unwrap()).unwrap();
        assert_eq!(trunk.leaves, vec![4]);

        // Leaf pops first...
        assert_eq!(pager.allocate_page().unwrap(), 4);
        // ...then the trunk page itself, once its leaf array is empty.
        assert_eq!(pager.allocate_page().unwrap(), 3);
        let after_alloc = pager.read_page(1).unwrap();
        assert_eq!(
            read_be_u32(&after_alloc, FREELIST_TRUNK_PAGE_OFFSET).unwrap(),
            0
        );
        assert_eq!(
            read_be_u32(&after_alloc, FREELIST_PAGE_COUNT_OFFSET).unwrap(),
            0
        );
    }

    /// Once a trunk page's leaf array is full, the next deallocated page
    /// becomes a new trunk pointing at the old one, chaining trunks
    /// instead of overflowing the array.
    #[test]
    fn deallocate_overflows_into_new_trunk_when_full() {
        // Pre-fill trunk page 3 at exactly `max_leaves_per_trunk(512)`
        // capacity, so the next deallocation must overflow into a new
        // trunk rather than requiring hundreds of individual calls here.
        let page_size = 512u32;
        let max_leaves = freelist::max_leaves_per_trunk(page_size);
        let full_trunk = TrunkPage {
            next_trunk: 0,
            leaves: (100..100 + max_leaves).collect(),
        };
        let mut vfs = MemoryVfs::new();
        let mut contents = vec![0u8; page_size as usize * 4];
        write_be_u32(&mut contents, PAGE_COUNT_OFFSET, 4).unwrap();
        write_be_u32(&mut contents, FREELIST_TRUNK_PAGE_OFFSET, 3).unwrap();
        write_be_u32(&mut contents, FREELIST_PAGE_COUNT_OFFSET, max_leaves).unwrap();
        let trunk_start = page_size as usize * 2;
        full_trunk
            .write(&mut contents[trunk_start..trunk_start + page_size as usize])
            .unwrap();
        vfs.insert("/test.db", contents);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        pager.deallocate_page(4).unwrap();

        let after = pager.read_page(1).unwrap();
        assert_eq!(read_be_u32(&after, FREELIST_TRUNK_PAGE_OFFSET).unwrap(), 4);
        assert_eq!(
            read_be_u32(&after, FREELIST_PAGE_COUNT_OFFSET).unwrap(),
            max_leaves + 1
        );
        let new_trunk = TrunkPage::parse(&pager.read_page(4).unwrap()).unwrap();
        assert_eq!(new_trunk.next_trunk, 3);
        assert!(new_trunk.leaves.is_empty());
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

        fn int(v: &Value) -> i64 {
            match v {
                Value::Integer(i) => *i,
                other => panic!("expected integer, got {other:?}"),
            }
        }

        /// Opens `name` and returns every row of table `t` as `(a, b)`,
        /// discovering `t`'s root page via `read_schema` (never
        /// hardcoded) and merging any pending WAL frames through `Pager`.
        fn read_table_t(name: &str) -> Vec<(i64, String)> {
            let vfs = UnixVfs;
            let path = Path::new("tests/corpus/fixtures/journalstates").join(name);
            let header = header_of(&vfs, &path);

            let schema_pager = Pager::open(&vfs, &path, header.page_size).unwrap();
            let mut schema_cursor = TableCursor::new(schema_pager, &header, 1);
            let schemas = read_schema(&mut schema_cursor, header.text_encoding).unwrap();
            let t = schemas
                .iter()
                .find(|s| s.name == "t")
                .expect("table t in sqlite_master");

            let pager = Pager::open(&vfs, &path, header.page_size).unwrap();
            let mut cursor = TableCursor::new(pager, &header, t.root_page);
            let mut rows = Vec::new();
            let mut row = cursor.first().unwrap();
            while let Some(r) = row {
                let values = decode_record(&r.payload, header.text_encoding).unwrap();
                rows.push((int(&values[0]), text(&values[1]).to_string()));
                row = cursor.next().unwrap();
            }
            rows
        }

        /// 001-architecture Req-4's "Read a database with uncheckpointed
        /// WAL" scenario: three separate commits to the same page, none
        /// checkpointed into the main file — all three rows must be
        /// visible, matching `sqlite3` (see tools/gen_fixtures.sh).
        #[test]
        fn wal_pending_fixture_shows_uncheckpointed_rows() {
            assert_eq!(
                read_table_t("wal_pending.db"),
                vec![
                    (1, "one".to_string()),
                    (2, "two".to_string()),
                    (3, "three".to_string()),
                ]
            );
        }

        /// Both checksum-endianness paths must decode identically — this
        /// fixture is `wal_pending.db`'s content with magic flipped to
        /// 0x377f0683 and every checksum recomputed in big-endian
        /// arithmetic (spike #7 finding 2).
        #[test]
        fn wal_pending_bigendian_fixture_decodes_identically() {
            assert_eq!(
                read_table_t("wal_pending_bigendian.db"),
                read_table_t("wal_pending.db")
            );
        }

        /// A committed frame lifted from an unrelated WAL generation
        /// (different salts) is appended after this fixture's own last
        /// commit — it must never surface, and the WAL's own two
        /// legitimate rows must still be visible.
        #[test]
        fn wal_pending_stale_fixture_rejects_foreign_frame() {
            let rows = read_table_t("wal_pending_stale.db");
            assert_eq!(
                rows,
                vec![(10, "ten".to_string()), (11, "eleven".to_string())]
            );
            assert!(!rows.iter().any(|(_, b)| b.contains("STALE-FRAME")));
        }

        /// A big transaction spills dirty pages into the WAL as non-commit
        /// frames, then rolls back — none of the ~1999 rolled-back rows
        /// may surface; only the pre-existing committed row (already
        /// flushed to the main file by an earlier checkpoint, before this
        /// WAL generation began) is visible.
        #[test]
        fn wal_pending_trailing_fixture_shows_only_committed_row() {
            assert_eq!(
                read_table_t("wal_pending_trailing.db"),
                vec![(1, "committed-before".to_string())]
            );
        }
    }
}
