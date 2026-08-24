use thiserror::Error;

use crate::pager::PagerError;
use crate::record::RecordError;
use crate::vfs::PageError;

/// Errors arising from b-tree page decoding, cursor traversal, and
/// row/index mutation.
#[derive(Debug, Error)]
pub enum BtreeError {
    #[error("decoding a key record: {0}")]
    /// A cell's key record failed to decode.
    InvalidKeyRecord(#[from] RecordError),

    #[error("reading page {page_num}: {source}")]
    /// The pager/VFS failed to read the given page.
    PageSource {
        /// Page that failed to load.
        page_num: u32,
        #[source]
        /// Underlying VFS read failure.
        source: PageError,
    },

    #[error("page {page_num} is too short ({len} bytes) to contain a b-tree page header")]
    /// A page's byte buffer is smaller than a b-tree page header requires.
    PageTooShort {
        /// Page that is too short.
        page_num: u32,
        /// Actual length of the page buffer, in bytes.
        len: usize,
    },

    #[error("{operation} called on a cursor that was never positioned by {required}")]
    /// A cursor operation was invoked before the cursor was positioned.
    CursorNotPositioned {
        /// Operation that required a positioned cursor.
        operation: &'static str,
        /// Positioning call the caller must make first.
        required: &'static str,
    },

    #[error("page {page_num} has unexpected b-tree page type {page_type:#x}")]
    /// A page's type byte does not match any known b-tree page type.
    UnexpectedPageType {
        /// Page with the unexpected type.
        page_num: u32,
        /// Raw page-type byte found on the page.
        page_type: u8,
    },

    #[error("page {page_num} cell pointer at index {index} is out of bounds")]
    /// A cell pointer array index resolved outside the page's cell content area.
    InvalidCellPointer {
        /// Page containing the invalid cell pointer.
        page_num: u32,
        /// Index into the cell pointer array that was out of bounds.
        index: usize,
    },

    #[error("page {page_num} cell varint decode failed: {source}")]
    /// A varint embedded in a cell (payload length or rowid) failed to decode.
    InvalidCellVarint {
        /// Page containing the cell.
        page_num: u32,
        #[source]
        /// Underlying varint/record decode failure.
        source: RecordError,
    },

    #[error("page {page_num} cell payload is shorter than its declared local size")]
    /// A cell's payload bytes end before its declared local payload size.
    PayloadTooShort {
        /// Page containing the truncated cell.
        page_num: u32,
    },

    #[error("page {page_num} declares an implausible payload length {payload_len}")]
    /// A cell declares a payload length exceeding what the file format allows.
    PayloadTooLarge {
        /// Page containing the offending cell.
        page_num: u32,
        /// Declared payload length, in bytes.
        payload_len: u64,
    },

    #[error("overflow chain from page {page_num} exceeded {max} pages (possible cycle)")]
    /// An overflow chain was followed for more than `max` pages without terminating.
    OverflowChainTooLong {
        /// First page of the overflow chain.
        page_num: u32,
        /// Maximum chain length allowed before this is treated as a cycle.
        max: usize,
    },

    #[error("overflow chain from page {page_num} revisited page {revisited_page} (cycle)")]
    /// An overflow chain revisited a page it had already traversed.
    OverflowChainCycle {
        /// First page of the overflow chain.
        page_num: u32,
        /// Page that was visited twice.
        revisited_page: u32,
    },

    #[error("overflow chain from page {page_num} ended before all payload bytes were read")]
    /// An overflow chain terminated before yielding all declared payload bytes.
    OverflowChainTruncated {
        /// First page of the overflow chain.
        page_num: u32,
    },

    #[error("b-tree traversal visited more than {max} pages (possible cycle)")]
    /// A cursor traversal visited more than `max` pages, indicating a likely cycle.
    TraversalTooLong {
        /// Maximum number of pages allowed in a single traversal.
        max: usize,
    },

    #[error("pager error: {0}")]
    /// A lower-level pager operation failed.
    Pager(#[from] PagerError),

    #[error("cannot insert duplicate rowid {rowid}")]
    /// An insert targeted a rowid that already exists in the table.
    DuplicateRowid {
        /// Rowid that already exists.
        rowid: i64,
    },

    #[error("interior page {page_num} has no routing entry for child page {child}")]
    /// An interior page's cells contain no entry routing to the given child page.
    MissingChildRoute {
        /// Interior page missing the routing entry.
        page_num: u32,
        /// Child page that could not be located.
        child: u32,
    },

    #[error("cannot delete rowid {rowid}: no such row")]
    /// A delete targeted a rowid that does not exist in the table.
    RowidNotFound {
        /// Rowid that could not be found.
        rowid: i64,
    },

    #[error("cannot insert duplicate index key")]
    /// An insert into a unique index targeted a key that already exists.
    DuplicateKey,

    #[error("cannot delete index key: no such entry")]
    /// A delete targeted an index key that does not exist.
    KeyNotFound,

    #[error("sqlite_master entry {name:?} has out-of-range rootpage {rootpage}")]
    /// A `sqlite_master` entry names a rootpage number outside the valid page range.
    InvalidRootPage {
        /// Name of the offending `sqlite_master` entry.
        name: String,
        /// Out-of-range rootpage value.
        rootpage: i64,
    },

    #[error("cannot delete sqlite_master entry {name:?}: no such entry")]
    /// A delete targeted a `sqlite_master` entry that does not exist.
    MasterEntryNotFound {
        /// Name of the entry that could not be found.
        name: String,
    },

    #[error("internal invariant violated: {0}")]
    /// An internal invariant was violated; the message describes what was expected.
    Internal(&'static str),
}
