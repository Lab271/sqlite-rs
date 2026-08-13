use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordError {
    #[error("unexpected end of input at byte offset {offset}")]
    UnexpectedEof { offset: usize },

    #[error(
        "record header length {declared} is shorter than its own header-length varint ({varint_len} bytes)"
    )]
    HeaderTooShort { declared: usize, varint_len: usize },

    #[error("record header entry at offset {offset} extends past the declared header length {header_len}")]
    HeaderOverrun { offset: usize, header_len: usize },

    #[error("record has {trailing} unconsumed trailing byte(s) after decoding all columns")]
    TrailingData { trailing: usize },

    #[error("invalid UTF-8 in text value")]
    InvalidUtf8,

    #[error("invalid UTF-16 in text value")]
    InvalidUtf16,
}
