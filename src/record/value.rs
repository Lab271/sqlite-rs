use std::rc::Rc;

/// A single decoded column value, per SQLite's dynamic type system.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(Rc<str>),
    Blob(Rc<[u8]>),
}

/// The database's text encoding, from database header byte 56.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}
