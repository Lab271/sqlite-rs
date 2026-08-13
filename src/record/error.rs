use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordError {
    #[error("unexpected end of input at byte offset {offset}")]
    UnexpectedEof { offset: usize },

    #[error("serial type {0} is reserved/internal and not valid in a record")]
    ReservedSerialType(u64),

    #[error("invalid UTF-8 in text value")]
    InvalidUtf8,

    #[error("invalid UTF-16 in text value")]
    InvalidUtf16,
}
