//! Black-box tests of `sqlite_rs::record::*` — only public paths, exactly as
//! an external consumer of the crate would see them. Note: `decode_serial_value`
//! is intentionally not used here — it was demoted to `pub(crate)` as internal
//! plumbing (its only caller was `decode_record` itself).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use sqlite_rs::record::{decode_record, decode_varint, RecordError, TextEncoding, Value};

/// Minimal varint encoder for building test payloads (mirrors the decoder's
/// bit layout; only used to construct fixtures — this is test-only code,
/// not a re-implementation of any internal function).
fn varint_bytes(value: u64) -> Vec<u8> {
    if value < 128 {
        return vec![value as u8];
    }
    let mut groups = Vec::new();
    let mut v = value;
    // 8-byte form: 7 bits per byte, big-endian order.
    for _ in 0..8 {
        groups.push((v & 0x7f) as u8);
        v >>= 7;
    }
    groups.reverse();
    let first_significant = groups.iter().position(|&b| b != 0).unwrap_or(7);
    let mut out: Vec<u8> = groups[first_significant..8]
        .iter()
        .map(|&b| b | 0x80)
        .collect();
    *out.last_mut().unwrap() &= 0x7f;
    out
}

fn record_bytes(serial_types_and_bodies: &[(u64, &[u8])]) -> Vec<u8> {
    let mut header = Vec::new();
    for (st, _) in serial_types_and_bodies {
        header.extend(varint_bytes(*st));
    }
    let mut header_len = header.len() + 1;
    loop {
        let hl_bytes = varint_bytes(header_len as u64);
        if hl_bytes.len() + header.len() == header_len {
            let mut out = hl_bytes;
            out.extend(&header);
            for (_, body) in serial_types_and_bodies {
                out.extend(*body);
            }
            return out;
        }
        header_len += 1;
    }
}

#[test]
fn decode_varint_round_trips() {
    assert_eq!(decode_varint(&[0x00]), Ok((0, 1)));
    assert_eq!(decode_varint(&[0x81, 0x00]), Ok((128, 2)));
}

#[test]
fn decode_record_null_value() {
    let payload = record_bytes(&[(0, &[])]);
    assert_eq!(
        decode_record(&payload, TextEncoding::Utf8),
        Ok(vec![Value::Null])
    );
}

/// Spec 003/Req-4 "Constant serial types" scenario: serial types 8 and 9
/// decode as the constant integers 0 and 1 respectively, consuming zero
/// body bytes — dedicated coverage, isolated from the broader
/// integer-widths test in the inline suite.
#[test]
fn constant_serial_types_8_and_9() {
    let payload = record_bytes(&[(8, &[]), (9, &[])]);
    assert_eq!(
        decode_record(&payload, TextEncoding::Utf8),
        Ok(vec![Value::Integer(0), Value::Integer(1)])
    );
}

/// Spec 003/Req-6 "Mixed-type row" scenario: a single record spanning five
/// distinct value kinds, matching the spike 002 fixture row
/// `(42, 'hello', 3.14, X'DEADBEEF', NULL)`.
#[test]
#[allow(clippy::approx_constant)] // 3.14 is the spec's literal fixture value, not an attempted pi
fn mixed_type_row() {
    let payload = record_bytes(&[
        (1, &[42]),                              // INTEGER 42, 1-byte width
        (13 + 2 * 5, b"hello"),                  // TEXT 'hello'
        (7, &3.14f64.to_be_bytes()),             // REAL 3.14
        (12 + 2 * 4, &[0xde, 0xad, 0xbe, 0xef]), // BLOB X'DEADBEEF'
        (0, &[]),                                // NULL
    ]);
    assert_eq!(
        decode_record(&payload, TextEncoding::Utf8),
        Ok(vec![
            Value::Integer(42),
            Value::Text("hello".to_string().into()),
            Value::Real(3.14),
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef].into()),
            Value::Null,
        ])
    );
}

#[test]
fn record_error_variants_are_matchable() {
    let payload = vec![0x80, 0x00]; // encodes header_len = 0 using 2 bytes
    let err = decode_record(&payload, TextEncoding::Utf8).unwrap_err();
    match err {
        RecordError::HeaderTooShort {
            declared,
            varint_len,
        } => {
            assert_eq!(declared, 0);
            assert_eq!(varint_len, 2);
        }
        other => panic!("expected HeaderTooShort, got {other:?}"),
    }

    let mut payload = record_bytes(&[(0, &[])]);
    payload.push(0xff);
    let err = decode_record(&payload, TextEncoding::Utf8).unwrap_err();
    assert!(matches!(err, RecordError::TrailingData { trailing: 1 }));
}

#[test]
fn record_error_is_error_send_sync() {
    fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
    assert_bounds::<RecordError>();
}
