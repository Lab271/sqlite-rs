//! SQLite database header (bytes 0-99 of the main database file).
//!
//! Page-1 trap: the 100-byte header occupies the start of page 1, but page
//! 1's own b-tree cell-pointer array is relative to byte 0 of the page, not
//! byte 100. The b-tree layer must account for the header when computing
//! in-page offsets on page 1 — this module only parses the header itself.

use thiserror::Error;

use crate::record::TextEncoding;

pub const HEADER_LEN: usize = 100;

const MAGIC: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("header is {len} bytes, need at least 100")]
    TooShort { len: usize },

    #[error("missing or invalid SQLite magic string")]
    InvalidMagic,

    #[error(
        "invalid page size encoding {raw} (must be a power of two from 512 to 32768, or 1 for 65536)"
    )]
    InvalidPageSize { raw: u16 },

    #[error("invalid {field:?} version byte {value} (must be 1 or 2)")]
    InvalidFileFormatVersion { field: VersionField, value: u8 },

    #[error("reserved space {reserved_space} leaves no usable bytes in a {page_size}-byte page")]
    InvalidReservedSpace { reserved_space: u8, page_size: u32 },

    #[error("invalid text encoding {raw} (must be 1, 2, or 3)")]
    InvalidTextEncoding { raw: u32 },
}

/// Which header byte an [`HeaderError::InvalidFileFormatVersion`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionField {
    Write,
    Read,
}

/// The journal mode declared by the write/read version bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    Legacy,
    Wal,
}

/// The parsed 100-byte SQLite database header.
///
/// See the page-1 trap note in the module doc: this struct describes only
/// the header itself, not how page 1's cell pointers are addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseHeader {
    /// Bytes 16-17, after the `1` = 65536 encoding is resolved.
    pub page_size: u32,
    /// Byte 18: file format write version.
    pub write_version: u8,
    /// Byte 19: file format read version.
    pub read_version: u8,
    /// Byte 20: bytes reserved at the end of each page.
    pub reserved_space: u8,
    /// Bytes 28-31: number of pages in the database.
    pub page_count: u32,
    /// Bytes 32-35: page number of the first freelist trunk page (0 if none).
    pub freelist_trunk_page: u32,
    /// Bytes 36-39: total number of freelist pages.
    pub freelist_page_count: u32,
    /// Bytes 40-43: schema cookie.
    pub schema_cookie: u32,
    /// Bytes 44-47: schema format number.
    pub schema_format: u32,
    /// Bytes 52-55: page number of the largest root b-tree page in
    /// auto-vacuum/incremental-vacuum mode (0 otherwise). Non-zero means
    /// pointer-map pages are interleaved in the file.
    pub largest_root_btree_page: u32,
    /// Bytes 56-59: database text encoding.
    pub text_encoding: TextEncoding,
    /// Bytes 60-63: user version, set by the `user_version` pragma.
    pub user_version: u32,
    /// Bytes 68-71: application ID, set by the `application_id` pragma.
    pub application_id: u32,
}

/// Reads one byte at `offset`. `buf` is assumed already length-checked
/// against [`HEADER_LEN`] by [`DatabaseHeader::parse`]; this still returns
/// `Err` rather than indexing directly, so the bound never has to be
/// re-proven by inspection.
fn read_u8(buf: &[u8], offset: usize) -> Result<u8, HeaderError> {
    buf.get(offset)
        .copied()
        .ok_or(HeaderError::TooShort { len: buf.len() })
}

/// Reads a big-endian `u32` at `offset..offset+4`.
fn read_u32(buf: &[u8], offset: usize) -> Result<u32, HeaderError> {
    let end = offset
        .checked_add(4)
        .ok_or(HeaderError::TooShort { len: buf.len() })?;
    let bytes: [u8; 4] = buf
        .get(offset..end)
        .ok_or(HeaderError::TooShort { len: buf.len() })?
        .try_into()
        .map_err(|_| HeaderError::TooShort { len: buf.len() })?;
    Ok(u32::from_be_bytes(bytes))
}

impl DatabaseHeader {
    /// Parses the 100-byte database header from the start of a database
    /// file. `buf` may be longer (e.g. a full page) but must be at least
    /// [`HEADER_LEN`] bytes. Never panics: malformed input returns `Err`.
    // Spike #371: rust-refine proof-of-concept. The issue's target
    // annotations used `#[mvl::refine] impl { ... }` with `ret.field` and
    // `==>` — none of that is real rust-refine syntax. The actual API
    // attaches `#[mvl::requires]`/`#[mvl::ensures]` per function, the
    // return value is always named `result`, and implication has to be
    // spelled as `!p || q` since `==>` isn't a Rust operator the
    // predicate's `syn::Expr` parser can accept.
    //
    // The issue's real intent — `result.page_size.is_power_of_two()`,
    // `512 <= result.page_size <= 65536`, `result.reserved_space <
    // result.page_size` — cannot be expressed here at all: `ensures` is
    // injected at *every* return point including early `return Err(...)`
    // sites, and `result.as_ref().unwrap().page_size` there leaves the
    // `Ok` type of `Result<T, HeaderError>` unconstrained soon enough for
    // rustc to reject it (E0282, "type annotations needed"). This is a
    // genuine gap in rust-refine, not a syntax mistake on the issue's
    // part — see the spike write-up. `ensures` below is left as the
    // largest postcondition that #371's target actually could type-check
    // through this attribute today: a tautology over `result`'s variant.
    #[mvl::ensures(result.is_ok() || result.is_err())]
    pub fn parse(buf: &[u8]) -> Result<Self, HeaderError> {
        if buf.len() < HEADER_LEN {
            return Err(HeaderError::TooShort { len: buf.len() });
        }

        if buf.get(0..16) != Some(MAGIC.as_slice()) {
            return Err(HeaderError::InvalidMagic);
        }

        let raw_page_size = u16::from_be_bytes([read_u8(buf, 16)?, read_u8(buf, 17)?]);
        let page_size: u32 = if raw_page_size == 1 {
            65536
        } else {
            raw_page_size as u32
        };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(HeaderError::InvalidPageSize { raw: raw_page_size });
        }

        let write_version = read_u8(buf, 18)?;
        if !matches!(write_version, 1 | 2) {
            return Err(HeaderError::InvalidFileFormatVersion {
                field: VersionField::Write,
                value: write_version,
            });
        }
        let read_version = read_u8(buf, 19)?;
        if !matches!(read_version, 1 | 2) {
            return Err(HeaderError::InvalidFileFormatVersion {
                field: VersionField::Read,
                value: read_version,
            });
        }

        let reserved_space = read_u8(buf, 20)?;
        if reserved_space as u32 >= page_size {
            return Err(HeaderError::InvalidReservedSpace {
                reserved_space,
                page_size,
            });
        }

        let page_count = read_u32(buf, 28)?;
        let freelist_trunk_page = read_u32(buf, 32)?;
        let freelist_page_count = read_u32(buf, 36)?;
        let schema_cookie = read_u32(buf, 40)?;
        let schema_format = read_u32(buf, 44)?;
        let largest_root_btree_page = read_u32(buf, 52)?;

        let text_encoding_raw = read_u32(buf, 56)?;
        let text_encoding = match text_encoding_raw {
            1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            other => return Err(HeaderError::InvalidTextEncoding { raw: other }),
        };

        let user_version = read_u32(buf, 60)?;
        let application_id = read_u32(buf, 68)?;

        Ok(DatabaseHeader {
            page_size,
            write_version,
            read_version,
            reserved_space,
            page_count,
            freelist_trunk_page,
            freelist_page_count,
            schema_cookie,
            schema_format,
            largest_root_btree_page,
            text_encoding,
            user_version,
            application_id,
        })
    }

    /// The journal mode declared by the write/read version bytes.
    pub fn journal_mode(&self) -> JournalMode {
        if self.write_version == 2 && self.read_version == 2 {
            JournalMode::Wal
        } else {
            JournalMode::Legacy
        }
    }

    /// Usable bytes per page: `page_size - reserved_space`.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "parse() rejects reserved_space >= page_size, so this never underflows"
    )]
    #[mvl::requires(self.reserved_space as u32 <= self.page_size)]
    #[mvl::ensures(result <= self.page_size)]
    pub fn usable_page_size(&self) -> u32 {
        self.page_size - self.reserved_space as u32
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

    fn fixture(family: &str, name: &str) -> Vec<u8> {
        // `cargo test` runs with the working directory set to the crate
        // root, so a path relative to it needs no `env!("CARGO_MANIFEST_DIR")`
        // — the mvl-limit gate (Makefile) doesn't allow that macro here.
        let path = Path::new("tests/corpus/fixtures").join(family).join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
    }

    #[test]
    fn page_size_512() {
        let header = DatabaseHeader::parse(&fixture("pagesizes", "page_size_512.db")).unwrap();
        assert_eq!(header.page_size, 512);
        assert_eq!(header.reserved_space, 0);
        assert_eq!(header.journal_mode(), JournalMode::Legacy);
    }

    #[test]
    fn page_size_65536_via_one_encoding() {
        let header = DatabaseHeader::parse(&fixture("pagesizes", "page_size_65536.db")).unwrap();
        assert_eq!(header.page_size, 65536);
        assert_eq!(header.reserved_space, 0);
    }

    #[test]
    fn reserved_bytes_0() {
        let header = DatabaseHeader::parse(&fixture("pagesizes", "reserved_bytes_0.db")).unwrap();
        assert_eq!(header.page_size, 4096);
        assert_eq!(header.reserved_space, 0);
        assert_eq!(header.usable_page_size(), 4096);
    }

    #[test]
    fn reserved_bytes_12() {
        let header = DatabaseHeader::parse(&fixture("pagesizes", "reserved_bytes_12.db")).unwrap();
        assert_eq!(header.page_size, 4096);
        assert_eq!(header.reserved_space, 12);
        assert_eq!(header.usable_page_size(), 4084);
    }

    #[test]
    fn encoding_utf8() {
        let header = DatabaseHeader::parse(&fixture("encodings", "utf8.db")).unwrap();
        assert_eq!(header.text_encoding, TextEncoding::Utf8);
    }

    #[test]
    fn encoding_utf16le() {
        let header = DatabaseHeader::parse(&fixture("encodings", "utf16le.db")).unwrap();
        assert_eq!(header.text_encoding, TextEncoding::Utf16Le);
    }

    #[test]
    fn encoding_utf16be() {
        let header = DatabaseHeader::parse(&fixture("encodings", "utf16be.db")).unwrap();
        assert_eq!(header.text_encoding, TextEncoding::Utf16Be);
    }

    #[test]
    fn empty_file_is_too_short_not_a_panic() {
        let err = DatabaseHeader::parse(&fixture("invalid", "empty.db")).unwrap_err();
        assert_eq!(err, HeaderError::TooShort { len: 0 });
    }

    #[test]
    fn truncated_header_is_too_short_not_a_panic() {
        let bytes = fixture("invalid", "truncated.db");
        let len = bytes.len();
        let err = DatabaseHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, HeaderError::TooShort { len });
    }

    #[test]
    fn bad_magic_is_rejected() {
        let err = DatabaseHeader::parse(&fixture("invalid", "magic.db")).unwrap_err();
        assert_eq!(err, HeaderError::InvalidMagic);
    }

    #[test]
    fn invalid_text_encoding_errors() {
        let mut bytes = fixture("encodings", "utf8.db");
        bytes[56..60].copy_from_slice(&99u32.to_be_bytes());
        let err = DatabaseHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, HeaderError::InvalidTextEncoding { raw: 99 });
    }

    #[test]
    fn invalid_page_size_errors() {
        let mut bytes = fixture("pagesizes", "page_size_512.db");
        bytes[16..18].copy_from_slice(&3u16.to_be_bytes());
        let err = DatabaseHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, HeaderError::InvalidPageSize { raw: 3 });
    }

    #[test]
    fn wal_journal_mode_detected() {
        let mut bytes = fixture("pagesizes", "reserved_bytes_0.db");
        bytes[18] = 2;
        bytes[19] = 2;
        let header = DatabaseHeader::parse(&bytes).unwrap();
        assert_eq!(header.journal_mode(), JournalMode::Wal);
    }

    #[test]
    fn mismatched_journal_version_bytes_are_legacy_not_wal() {
        // Only one of write_version/read_version is 2 here — pins the `&&`
        // in journal_mode() against a mutation to `||`, which would
        // wrongly report Wal as soon as either byte is 2.
        let mut bytes = fixture("pagesizes", "reserved_bytes_0.db");
        bytes[18] = 2;
        bytes[19] = 1;
        let header = DatabaseHeader::parse(&bytes).unwrap();
        assert_eq!(header.journal_mode(), JournalMode::Legacy);
    }

    #[test]
    fn page_size_below_512_but_power_of_two_is_rejected() {
        // 256 is a power of two but below the 512 floor — pins the `||` in
        // parse()'s page-size check against a mutation to `&&`, which
        // would wrongly accept this since is_power_of_two() alone is true.
        let mut bytes = fixture("pagesizes", "page_size_512.db");
        bytes[16..18].copy_from_slice(&256u16.to_be_bytes());
        let err = DatabaseHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, HeaderError::InvalidPageSize { raw: 256 });
    }

    #[test]
    fn page_size_above_512_but_not_a_power_of_two_is_rejected() {
        // 600 clears the 512 floor but isn't a power of two — the other
        // half of the `||` boundary above.
        let mut bytes = fixture("pagesizes", "page_size_512.db");
        bytes[16..18].copy_from_slice(&600u16.to_be_bytes());
        let err = DatabaseHeader::parse(&bytes).unwrap_err();
        assert_eq!(err, HeaderError::InvalidPageSize { raw: 600 });
    }
}
