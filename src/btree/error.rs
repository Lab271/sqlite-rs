use thiserror::Error;

use crate::record::RecordError;
use crate::vfs::PageError;

#[derive(Debug, Error)]
pub enum BtreeError {
    #[error("decoding a key record: {0}")]
    InvalidKeyRecord(#[from] RecordError),

    #[error("reading page {page_num}: {source}")]
    PageSource {
        page_num: u32,
        #[source]
        source: PageError,
    },

    #[error("page {page_num} is too short ({len} bytes) to contain a b-tree page header")]
    PageTooShort { page_num: u32, len: usize },

    #[error("page {page_num} has unexpected b-tree page type {page_type:#x}")]
    UnexpectedPageType { page_num: u32, page_type: u8 },

    #[error("page {page_num} cell pointer at index {index} is out of bounds")]
    InvalidCellPointer { page_num: u32, index: usize },

    #[error("page {page_num} cell varint decode failed: {source}")]
    InvalidCellVarint {
        page_num: u32,
        #[source]
        source: RecordError,
    },

    #[error("page {page_num} cell payload is shorter than its declared local size")]
    PayloadTooShort { page_num: u32 },

    #[error("page {page_num} declares an implausible payload length {payload_len}")]
    PayloadTooLarge { page_num: u32, payload_len: u64 },

    #[error("overflow chain from page {page_num} exceeded {max} pages (possible cycle)")]
    OverflowChainTooLong { page_num: u32, max: usize },

    #[error("overflow chain from page {page_num} revisited page {revisited_page} (cycle)")]
    OverflowChainCycle { page_num: u32, revisited_page: u32 },

    #[error("overflow chain from page {page_num} ended before all payload bytes were read")]
    OverflowChainTruncated { page_num: u32 },

    #[error("b-tree traversal visited more than {max} pages (possible cycle)")]
    TraversalTooLong { max: usize },
}
