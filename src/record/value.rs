// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

/// A single decoded column value, per SQLite's dynamic type system.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A signed integer, stored as 1/2/3/4/6/8 bytes on disk per the serial type.
    Integer(i64),
    /// An 8-byte IEEE 754 floating-point value.
    Real(f64),
    /// A text value, decoded according to the database's `TextEncoding`.
    Text(Arc<str>),
    /// An uninterpreted byte sequence.
    Blob(Arc<[u8]>),
}

/// `Value` must stay `Send + Sync` (#688): the embedding API's
/// connection handle hands result rows to another thread, and a row is
/// a `Vec<Value>`. `Rc` payloads made that impossible, and nothing but a
/// compile-time check keeps it from silently regressing — swapping
/// either payload back to `Rc` would otherwise only fail much later, in
/// whichever consumer tried to cross a thread.
///
/// This is deliberately *not* a claim about `Pager`/`PageSource`, which
/// stay `Rc` per ADR-0013 and ADR-0017. See ADR-0039.
const fn assert_value_send_sync<T: Send + Sync>() {}
const _: () = assert_value_send_sync::<Value>();

/// The database's text encoding, from database header byte 56.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    /// UTF-8.
    Utf8,
    /// UTF-16 little-endian.
    Utf16Le,
    /// UTF-16 big-endian.
    Utf16Be,
}
