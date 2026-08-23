//! WAL (write-ahead log) frame reading: merges committed frames from an
//! uncheckpointed `-wal` file over the main database's pages. Read-only,
//! quiescent-file recovery only — no `-shm` file, no read-locks, no
//! live-writer coexistence (validated as needed by spike 005/#8, closed;
//! implementation tracked as #45). Byte
//! layout and the checksum-endianness gotcha (finding 2) are as
//! established by spike #7 (`tests/spike/004_wal_reading/src/wal.rs`,
//! validated against real `sqlite3`-produced files); see
//! `.openspec/specs/007-pager/spec.md` Requirement 3.
//!
//! WAL header (32 bytes):
//!   0..4   magic: 0x377f0682 (native-endian checksums) or 0x377f0683
//!          (always big-endian) — 0x82 is the common/default case, not
//!          0x83, despite what the name suggests (spike #7 finding 2)
//!   4..8   file format version
//!   8..12  page size
//!   12..16 checkpoint sequence number
//!   16..20 salt-1
//!   20..24 salt-2
//!   24..28 checksum-1 (of bytes 0..24)
//!   28..32 checksum-2
//!
//! Frame header (24 bytes), immediately followed by `page_size` bytes of
//! page content:
//!   0..4   page number
//!   4..8   size of the database in pages, after this frame, if this frame
//!          committed a transaction — 0 if this frame did not commit
//!   8..12  salt-1 (copied from the WAL header)
//!   12..16 salt-2 (copied from the WAL header)
//!   16..20 checksum-1 (running, continues from the previous frame's, or
//!          the header's if this is the first frame)
//!   20..24 checksum-2

use std::collections::HashMap;
use std::path::Path;

use thiserror::Error;

use crate::vfs::{AnyVfs, AnyVfsFile, VfsError};

pub const HEADER_LEN: usize = 32;
pub(crate) const FRAME_HEADER_LEN: usize = 24;

/// SQLite's WAL file-format version (`pager.c`'s `WAL_MAX_VERSION`),
/// unchanged since the format's introduction — written by
/// [`WalHeader::new`], not currently validated on parse.
const FORMAT_VERSION: u32 = 3_007_000;

#[derive(Debug, Error)]
pub enum WalError {
    #[error("WAL header is {len} bytes, need at least {HEADER_LEN}")]
    HeaderTooShort { len: usize },

    #[error("invalid WAL magic {magic:#010x} (must be 0x377f0682 or 0x377f0683)")]
    InvalidMagic { magic: u32 },

    #[error("invalid WAL page size {page_size} (must be a power of two from 512 to 65536)")]
    InvalidPageSize { page_size: u32 },

    #[error(
        "WAL header checksum mismatch: stored {stored:?}, computed {computed:?} — corrupt WAL"
    )]
    HeaderChecksumMismatch {
        stored: (u32, u32),
        computed: (u32, u32),
    },

    #[error(transparent)]
    Vfs(#[from] VfsError),
}

/// A parsed/serialized 32-byte WAL header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalHeader {
    /// `true` if frame/header checksums are the host's native byte order
    /// (magic `0x377f0682`, the common case); `false` if always
    /// big-endian (magic `0x377f0683`).
    pub native_checksum: bool,
    pub page_size: u32,
    pub salt1: u32,
    pub salt2: u32,
    format_version: u32,
    checkpoint_seq: u32,
    header_checksum: (u32, u32),
}

/// Reads a big-endian `u32` at `offset..offset+4`. `buf` is assumed
/// already length-checked by the caller; this still returns `Err` rather
/// than indexing directly, so the bound never has to be re-proven by
/// inspection.
fn read_u32(buf: &[u8], offset: usize) -> Result<u32, WalError> {
    let end = offset.saturating_add(4);
    let bytes: [u8; 4] = buf
        .get(offset..end)
        .ok_or(WalError::HeaderTooShort { len: buf.len() })?
        .try_into()
        .map_err(|_| WalError::HeaderTooShort { len: buf.len() })?;
    Ok(u32::from_be_bytes(bytes))
}

impl WalHeader {
    /// Parses the 32-byte WAL header from the start of a `-wal` file's
    /// bytes. `bytes` may be longer (the rest is frame data). Never
    /// panics: malformed input returns `Err`.
    pub fn parse(bytes: &[u8]) -> Result<Self, WalError> {
        if bytes.len() < HEADER_LEN {
            return Err(WalError::HeaderTooShort { len: bytes.len() });
        }

        let magic = read_u32(bytes, 0)?;
        let native_checksum = match magic {
            0x377f_0682 => true,
            0x377f_0683 => false,
            _ => return Err(WalError::InvalidMagic { magic }),
        };

        let page_size = read_u32(bytes, 8)?;
        if page_size < 512 || !page_size.is_power_of_two() || page_size > 65536 {
            return Err(WalError::InvalidPageSize { page_size });
        }

        let format_version = read_u32(bytes, 4)?;
        let checkpoint_seq = read_u32(bytes, 12)?;
        let salt1 = read_u32(bytes, 16)?;
        let salt2 = read_u32(bytes, 20)?;
        let stored = (read_u32(bytes, 24)?, read_u32(bytes, 28)?);
        let checksummed = bytes
            .get(0..24)
            .ok_or(WalError::HeaderTooShort { len: bytes.len() })?;
        let computed = checksum(native_checksum, checksummed, (0, 0));
        if computed != stored {
            return Err(WalError::HeaderChecksumMismatch { stored, computed });
        }

        Ok(WalHeader {
            native_checksum,
            page_size,
            salt1,
            salt2,
            format_version,
            checkpoint_seq,
            header_checksum: stored,
        })
    }

    /// Builds a fresh header for a brand-new WAL file — `checkpoint_seq`
    /// is the generation counter a checkpoint bumps (#386); a first-ever
    /// WAL for a database starts at 1, matching stock SQLite.
    pub fn new(
        native_checksum: bool,
        page_size: u32,
        salt1: u32,
        salt2: u32,
        checkpoint_seq: u32,
    ) -> Self {
        let mut header = WalHeader {
            native_checksum,
            page_size,
            salt1,
            salt2,
            format_version: FORMAT_VERSION,
            checkpoint_seq,
            header_checksum: (0, 0),
        };
        let bytes = header.serialize();
        #[allow(
            clippy::indexing_slicing,
            reason = "serialize() always returns exactly HEADER_LEN bytes"
        )]
        let checksummed = &bytes[0..24];
        header.header_checksum = checksum(native_checksum, checksummed, (0, 0));
        header
    }

    /// Serializes this header back to its 32-byte on-disk form. Round-trips
    /// with [`WalHeader::parse`] once `header_checksum` is set (see
    /// [`WalHeader::new`], which computes it before returning).
    pub fn serialize(&self) -> [u8; HEADER_LEN] {
        let magic: u32 = if self.native_checksum {
            0x377f_0682
        } else {
            0x377f_0683
        };
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&magic.to_be_bytes());
        out[4..8].copy_from_slice(&self.format_version.to_be_bytes());
        out[8..12].copy_from_slice(&self.page_size.to_be_bytes());
        out[12..16].copy_from_slice(&self.checkpoint_seq.to_be_bytes());
        out[16..20].copy_from_slice(&self.salt1.to_be_bytes());
        out[20..24].copy_from_slice(&self.salt2.to_be_bytes());
        out[24..28].copy_from_slice(&self.header_checksum.0.to_be_bytes());
        out[28..32].copy_from_slice(&self.header_checksum.1.to_be_bytes());
        out
    }
}

/// SQLite's WAL checksum: `data` must be a multiple of 8 bytes, read as
/// pairs of 32-bit words — native byte order if `native_checksum`,
/// big-endian otherwise. Continues a running `(s1, s2)` pair; pass
/// `(0, 0)` to start. Never panics: a `data` length not a multiple of 8
/// (never produced by this module's own callers, but reachable if called
/// directly) simply ignores the trailing partial word via `chunks_exact`.
fn checksum(native_checksum: bool, data: &[u8], init: (u32, u32)) -> (u32, u32) {
    let (mut s1, mut s2) = init;
    for chunk in data.as_chunks::<8>().0 {
        let (w0_bytes, w1_bytes) = chunk.split_at(4);
        let w0 = read_word(native_checksum, w0_bytes);
        let w1 = read_word(native_checksum, w1_bytes);
        s1 = s1.wrapping_add(w0).wrapping_add(s2);
        s2 = s2.wrapping_add(w1).wrapping_add(s1);
    }
    (s1, s2)
}

/// `b` is always exactly 4 bytes here (`chunks_exact(8)` + `split_at(4)`
/// in `checksum`'s only caller), so `unwrap_or_default` never actually
/// falls back — it just avoids asserting that by an unchecked `unwrap()`.
fn read_word(native_checksum: bool, b: &[u8]) -> u32 {
    let arr: [u8; 4] = b.try_into().unwrap_or_default();
    if native_checksum {
        u32::from_ne_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    }
}

/// `read_u32`, but returning `None` on a short buffer instead of an `Err`
/// — for callers (like the frame walk below) where a bounds miss is just
/// "stop, this isn't a full frame" rather than a reportable error.
fn read_u32_opt(buf: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let bytes: [u8; 4] = buf.get(offset..end)?.try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// Walks every frame in `wal_bytes` (past the 32-byte header) and returns
/// the page map as of the last committed transaction, plus that commit's
/// declared database size in pages (0 if no frame ever committed).
///
/// A page's mapping is only published into the returned map when a commit
/// frame (db-size-if-commit != 0) is reached — frames from a transaction
/// that never committed update a scratch candidate map but are never
/// published, so they fall away naturally. Scanning stops the instant a
/// frame's salts don't match the header's (a foreign frame from a
/// different WAL generation) or its checksum doesn't verify (corrupt or
/// incomplete tail) — neither is an error, since a torn tail is the
/// normal shape of a WAL file mid-write; whatever was last published
/// survives. Never panics on any input, including a `wal_bytes` shorter
/// than one frame (the loop simply doesn't execute).
pub fn committed_pages(header: &WalHeader, wal_bytes: &[u8]) -> (HashMap<u32, Vec<u8>>, u32) {
    let frame_size = FRAME_HEADER_LEN.saturating_add(header.page_size as usize);
    let mut offset = HEADER_LEN;
    let mut running = header.header_checksum;
    let mut candidate: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut committed: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut committed_db_size = 0u32;

    while offset.saturating_add(frame_size) <= wal_bytes.len() {
        // A torn/incomplete tail isn't malformed input in the error sense
        // (see the module doc) — a bounds miss here just stops the scan
        // (`break`), the same as the salt/checksum mismatch checks below.
        let Some(fh) = wal_bytes.get(offset..offset.saturating_add(FRAME_HEADER_LEN)) else {
            break;
        };
        let (Some(page_number), Some(db_size), Some(salt1), Some(salt2)) = (
            read_u32_opt(fh, 0),
            read_u32_opt(fh, 4),
            read_u32_opt(fh, 8),
            read_u32_opt(fh, 12),
        ) else {
            break;
        };
        let (Some(c1), Some(c2)) = (read_u32_opt(fh, 16), read_u32_opt(fh, 20)) else {
            break;
        };
        let stored_checksum = (c1, c2);

        if salt1 != header.salt1 || salt2 != header.salt2 {
            break;
        }

        let Some(page_content) = wal_bytes
            .get(offset.saturating_add(FRAME_HEADER_LEN)..offset.saturating_add(frame_size))
        else {
            break;
        };
        let Some(fh_header_bytes) = fh.get(0..8) else {
            break;
        };
        let after_frame_header = checksum(header.native_checksum, fh_header_bytes, running);
        let after_page = checksum(header.native_checksum, page_content, after_frame_header);

        if after_page != stored_checksum {
            break;
        }
        running = after_page;

        candidate.insert(page_number, page_content.to_vec());

        if db_size != 0 {
            committed = candidate.clone();
            committed_db_size = db_size;
        }

        offset = offset.saturating_add(frame_size);
    }

    (committed, committed_db_size)
}

/// Appends frames to a `-wal` file: writes the header once on
/// [`WalWriter::create`], then each [`WalWriter::append_frame`] call
/// carries the running checksum forward exactly as [`committed_pages`]
/// verifies it on read — a page written then read back via this pair
/// round-trips by construction. `commit_db_size` is `0` for a frame that
/// does not end a transaction, matching the read path's convention.
pub struct WalWriter {
    file: AnyVfsFile,
    header: WalHeader,
    running: (u32, u32),
    offset: u64,
}

impl WalWriter {
    /// Creates (or reopens) the `-wal` file at `path` and writes `header`.
    pub fn create(vfs: &AnyVfs, path: &Path, header: WalHeader) -> Result<Self, WalError> {
        let file = vfs.create_or_open_write(path)?;
        file.write_at(&header.serialize(), 0)?;
        Ok(WalWriter {
            file,
            running: header.header_checksum,
            offset: HEADER_LEN as u64,
            header,
        })
    }

    /// Appends one frame (24-byte header + `page_data`) at the writer's
    /// current offset, continuing the running checksum chain.
    pub fn append_frame(
        &mut self,
        page_num: u32,
        page_data: &[u8],
        commit_db_size: u32,
    ) -> Result<(), WalError> {
        let mut frame_header = [0u8; FRAME_HEADER_LEN];
        frame_header[0..4].copy_from_slice(&page_num.to_be_bytes());
        frame_header[4..8].copy_from_slice(&commit_db_size.to_be_bytes());
        frame_header[8..12].copy_from_slice(&self.header.salt1.to_be_bytes());
        frame_header[12..16].copy_from_slice(&self.header.salt2.to_be_bytes());

        #[allow(
            clippy::indexing_slicing,
            reason = "frame_header is a fixed 24-byte array"
        )]
        let after_header = checksum(
            self.header.native_checksum,
            &frame_header[0..8],
            self.running,
        );
        let after_page = checksum(self.header.native_checksum, page_data, after_header);
        frame_header[16..20].copy_from_slice(&after_page.0.to_be_bytes());
        frame_header[20..24].copy_from_slice(&after_page.1.to_be_bytes());

        let mut buf = Vec::with_capacity(FRAME_HEADER_LEN.saturating_add(page_data.len()));
        buf.extend_from_slice(&frame_header);
        buf.extend_from_slice(page_data);
        self.file.write_at(&buf, self.offset)?;

        self.running = after_page;
        self.offset = self.offset.saturating_add(buf.len() as u64);
        Ok(())
    }

    /// Flushes every frame written so far to durable storage.
    pub fn sync(&self) -> Result<(), WalError> {
        self.file.sync()?;
        Ok(())
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
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new("tests/corpus/fixtures/journalstates").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
    }

    #[test]
    fn native_checksum_header_parses() {
        let bytes = fixture("wal_pending.db-wal");
        let header = WalHeader::parse(&bytes).unwrap();
        assert!(header.native_checksum);
        assert_eq!(header.page_size, 4096);
    }

    #[test]
    fn bigendian_checksum_header_parses() {
        let bytes = fixture("wal_pending_bigendian.db-wal");
        let header = WalHeader::parse(&bytes).unwrap();
        assert!(!header.native_checksum);
        assert_eq!(header.page_size, 4096);
    }

    #[test]
    fn too_short_is_err_not_panic() {
        assert!(matches!(
            WalHeader::parse(&[0u8; 10]),
            Err(WalError::HeaderTooShort { len: 10 })
        ));
    }

    #[test]
    fn bad_magic_is_err() {
        let mut bytes = fixture("wal_pending.db-wal");
        bytes[0..4].copy_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(
            WalHeader::parse(&bytes),
            Err(WalError::InvalidMagic { magic: 0 })
        ));
    }

    #[test]
    fn corrupted_header_checksum_is_err() {
        let mut bytes = fixture("wal_pending.db-wal");
        bytes[16] ^= 0xff; // flip a salt byte without fixing up the checksum
        assert!(matches!(
            WalHeader::parse(&bytes),
            Err(WalError::HeaderChecksumMismatch { .. })
        ));
    }

    #[test]
    fn trailing_spilled_frames_are_ignored() {
        let bytes = fixture("wal_pending_trailing.db-wal");
        let header = WalHeader::parse(&bytes).unwrap();
        let (pages, db_size) = committed_pages(&header, &bytes);
        // The pre-existing "committed-before" row was flushed to the main
        // db file by the checkpoint that ran before this WAL generation
        // started (see tools/gen_fixtures.sh); every frame in this WAL is
        // an uncommitted spill from the ~1999-row transaction that was
        // then rolled back, so no frame here ever commits (db-size stays
        // 0) and nothing is published — the pre-existing row must come
        // from the main file, not from this WAL merge.
        assert_eq!(db_size, 0);
        assert!(pages.is_empty());
    }

    #[test]
    fn stale_foreign_frame_is_rejected_on_salt_mismatch() {
        let bytes = fixture("wal_pending_stale.db-wal");
        let header = WalHeader::parse(&bytes).unwrap();
        let (pages, _) = committed_pages(&header, &bytes);
        for page in pages.values() {
            let text = String::from_utf8_lossy(page);
            assert!(!text.contains("STALE-FRAME-MUST-NOT-APPEAR"));
        }
    }

    #[test]
    fn garbage_input_never_panics() {
        for len in 0..40 {
            let bytes = vec![0xaau8; len];
            WalHeader::parse(&bytes).ok();
        }
        let header = WalHeader {
            native_checksum: true,
            page_size: 4096,
            salt1: 0,
            salt2: 0,
            format_version: FORMAT_VERSION,
            checkpoint_seq: 0,
            header_checksum: (0, 0),
        };
        for len in 0..100 {
            let bytes = vec![0x55u8; len];
            let _ = committed_pages(&header, &bytes);
        }
    }

    #[test]
    fn write_then_read_round_trips_committed_page() {
        use crate::vfs::{AnyVfs, MemoryVfs};

        let memory = MemoryVfs::new();
        let vfs = AnyVfs::new(memory);
        let path = Path::new("/test.db-wal");

        let header = WalHeader::new(true, 512, 0xAAAA_1111, 0xBBBB_2222, 1);
        let mut writer = WalWriter::create(&vfs, path, header).unwrap();

        let page1 = vec![0x11u8; 512];
        writer.append_frame(1, &page1, 0).unwrap();
        let page2 = vec![0x22u8; 512];
        writer.append_frame(2, &page2, 2).unwrap();
        writer.sync().unwrap();

        let file = vfs.open_read(path).unwrap();
        let size = file.size().unwrap();
        let mut bytes = vec![0u8; size as usize];
        file.read_at(&mut bytes, 0).unwrap();

        let parsed = WalHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, header);
        let (pages, db_size) = committed_pages(&parsed, &bytes);
        assert_eq!(db_size, 2);
        assert_eq!(pages.get(&1), Some(&page1));
        assert_eq!(pages.get(&2), Some(&page2));
    }

    #[test]
    fn uncommitted_trailing_frame_is_not_published() {
        use crate::vfs::{AnyVfs, MemoryVfs};

        let memory = MemoryVfs::new();
        let vfs = AnyVfs::new(memory);
        let path = Path::new("/test.db-wal");

        let header = WalHeader::new(true, 512, 1, 2, 1);
        let mut writer = WalWriter::create(&vfs, path, header).unwrap();
        let page1 = vec![0x33u8; 512];
        writer.append_frame(1, &page1, 0).unwrap();
        writer.sync().unwrap();

        let file = vfs.open_read(path).unwrap();
        let size = file.size().unwrap();
        let mut bytes = vec![0u8; size as usize];
        file.read_at(&mut bytes, 0).unwrap();

        let parsed = WalHeader::parse(&bytes).unwrap();
        let (pages, db_size) = committed_pages(&parsed, &bytes);
        assert_eq!(db_size, 0);
        assert!(pages.is_empty());
    }
}
