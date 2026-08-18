//! Rollback-journal on-disk format: header layout and the per-page
//! checksum, matching stock SQLite's `pager.c` byte-for-byte (#172) so a
//! journal we write is recoverable by a real `sqlite3`, and vice versa.
//!
//! Header layout (28 bytes, `pager.c`'s `writeJournalHdr`), followed by
//! zero padding out to [`JournalHeader::sector_size`] bytes before the
//! first page record:
//!
//! | offset | len | field                                    |
//! |--------|-----|-------------------------------------------|
//! | 0      | 8   | magic (`crate::pager::JOURNAL_MAGIC`)      |
//! | 8      | 4   | `n_rec` — number of page records that follow |
//! | 12     | 4   | `nonce` — checksum salt (`cksumInit`)      |
//! | 16     | 4   | `initial_page_count` — db size before the txn |
//! | 20     | 4   | `sector_size`                              |
//! | 24     | 4   | `page_size`                                |
//!
//! Each page record is `4 + page_size + 4` bytes: big-endian page number,
//! the page's original content, then [`page_checksum`] of that content.

use thiserror::Error;

pub const JOURNAL_HEADER_LEN: usize = 28;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal header too short: expected at least {JOURNAL_HEADER_LEN} bytes, got {0}")]
    HeaderTooShort(usize),

    #[error("journal header magic mismatch")]
    BadMagic,

    #[error(
        "journal record {index} checksum mismatch: expected {expected:#010x}, computed {computed:#010x}"
    )]
    ChecksumMismatch {
        index: u32,
        expected: u32,
        computed: u32,
    },

    #[error("journal record {index} truncated: expected {expected} bytes, got {got}")]
    RecordTruncated {
        index: u32,
        expected: usize,
        got: usize,
    },
}

/// A parsed/serialized rollback-journal header. See the module doc for the
/// byte layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalHeader {
    pub n_rec: u32,
    pub nonce: u32,
    pub initial_page_count: u32,
    pub sector_size: u32,
    pub page_size: u32,
}

impl JournalHeader {
    /// Parses the fixed 28-byte header from `buf`'s start. Does not
    /// validate the magic — callers that already branched on "is this
    /// journal hot" (`Pager::open`) have typically checked it already;
    /// [`crate::pager::JOURNAL_MAGIC`] is exposed for callers that haven't.
    pub fn parse(buf: &[u8]) -> Result<Self, JournalError> {
        let bytes: &[u8; JOURNAL_HEADER_LEN] = buf
            .get(..JOURNAL_HEADER_LEN)
            .and_then(|s| s.try_into().ok())
            .ok_or(JournalError::HeaderTooShort(buf.len()))?;
        #[allow(
            clippy::indexing_slicing,
            reason = "fixed literal ranges into a 28-byte array, checked by the compiler"
        )]
        let be32 = |range: std::ops::Range<usize>| -> u32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[range]);
            u32::from_be_bytes(b)
        };
        Ok(JournalHeader {
            n_rec: be32(8..12),
            nonce: be32(12..16),
            initial_page_count: be32(16..20),
            sector_size: be32(20..24),
            page_size: be32(24..28),
        })
    }

    /// Serializes the 28-byte header proper (magic + the five fields).
    /// Callers pad out to `sector_size` themselves before writing the
    /// first record — see `pager.rs`'s journal writer.
    pub fn serialize(&self, magic: [u8; 8]) -> [u8; JOURNAL_HEADER_LEN] {
        let mut out = [0u8; JOURNAL_HEADER_LEN];
        out[..8].copy_from_slice(&magic);
        out[8..12].copy_from_slice(&self.n_rec.to_be_bytes());
        out[12..16].copy_from_slice(&self.nonce.to_be_bytes());
        out[16..20].copy_from_slice(&self.initial_page_count.to_be_bytes());
        out[20..24].copy_from_slice(&self.sector_size.to_be_bytes());
        out[24..28].copy_from_slice(&self.page_size.to_be_bytes());
        out
    }
}

/// SQLite's `pager_cksum`: samples one byte every 200 bytes, starting at
/// `page.len() - 200` and walking down to (but not past) index 0, summing
/// into `nonce` with wrapping add. Deliberately not a "real" checksum
/// (SQLite's own comment: "it is not a real hashing function... fast to
/// compute and unlikely to collide with a valid page") — replicated
/// exactly so our journal records validate against a stock `sqlite3` and
/// vice versa.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "all subtraction is saturating_sub; index is checked against page.len() before indexing"
)]
pub fn page_checksum(nonce: u32, page: &[u8]) -> u32 {
    let mut cksum = nonce;
    let mut idx = page.len().saturating_sub(200);
    while idx > 0 && idx < page.len() {
        #[allow(
            clippy::indexing_slicing,
            reason = "idx < page.len() is checked by the loop guard"
        )]
        {
            cksum = cksum.wrapping_add(u32::from(page[idx]));
        }
        idx = idx.saturating_sub(200);
    }
    cksum
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

    #[test]
    fn header_roundtrips() {
        let header = JournalHeader {
            n_rec: 3,
            nonce: 0xdead_beef,
            initial_page_count: 7,
            sector_size: 512,
            page_size: 4096,
        };
        let bytes = header.serialize(MAGIC);
        assert_eq!(&bytes[..8], &MAGIC);
        let parsed = JournalHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn header_too_short_is_an_error() {
        let bytes = [0u8; 20];
        assert!(matches!(
            JournalHeader::parse(&bytes),
            Err(JournalError::HeaderTooShort(20))
        ));
    }

    #[test]
    fn checksum_matches_sqlite_pager_cksum_reference_vector() {
        // Hand-computed reference: a 512-byte page of all 0x01 bytes,
        // nonce 0. Sampled indices: 312, 112 (512-200=312, 312-200=112,
        // 112-200=-88 stops). Two samples of 0x01 each -> nonce + 2.
        let page = vec![1u8; 512];
        assert_eq!(page_checksum(0, &page), 2);
    }

    #[test]
    fn checksum_depends_on_nonce() {
        let page = vec![1u8; 512];
        assert_ne!(page_checksum(0, &page), page_checksum(1, &page));
    }

    #[test]
    fn checksum_of_short_page_below_200_bytes_is_just_the_nonce() {
        let page = vec![0xffu8; 100];
        assert_eq!(page_checksum(42, &page), 42);
    }
}
