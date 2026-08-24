use thiserror::Error;

/// Errors from decoding a SQLite record (the payload format used by table and index B-tree
/// cells).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordError {
    /// The record buffer ended before decoding could complete.
    #[error("unexpected end of input at byte offset {offset}")]
    UnexpectedEof {
        /// Byte offset into the record where the read past the end started.
        offset: usize,
    },

    /// The declared header length is too small to even contain the header-length varint itself.
    #[error(
        "record header length {declared} is shorter than its own header-length varint ({varint_len} bytes)"
    )]
    HeaderTooShort {
        /// The header length declared by the header-length varint.
        declared: usize,
        /// The size in bytes of the header-length varint itself.
        varint_len: usize,
    },

    /// A serial-type varint in the header read past the declared header length.
    #[error("record header entry at offset {offset} extends past the declared header length {header_len}")]
    HeaderOverrun {
        /// Byte offset of the header entry that overran.
        offset: usize,
        /// The declared total header length.
        header_len: usize,
    },

    /// Bytes remained in the record buffer after all header-declared columns were decoded.
    #[error("record has {trailing} unconsumed trailing byte(s) after decoding all columns")]
    TrailingData {
        /// Number of unconsumed trailing bytes.
        trailing: usize,
    },

    /// A text value's bytes were not valid UTF-8 under a UTF-8 `TextEncoding`.
    #[error("invalid UTF-8 in text value")]
    InvalidUtf8,

    /// A text value's bytes were not valid UTF-16 under a UTF-16 `TextEncoding`.
    #[error("invalid UTF-16 in text value")]
    InvalidUtf16,
}
