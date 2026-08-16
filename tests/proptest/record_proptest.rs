//! Property-based tests for varint and record-decoding roundtrips.
//!
//! Lives outside `src/` rather than alongside the hand-picked example
//! tests in `src/record/varint.rs` / `src/record/decode.rs`: those files
//! are in the qualified subset (issue #23, enforced by `make mvl-limit`),
//! whose curated macro allowlist doesn't include proptest's `proptest!`
//! macro expansion.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use sqlite_rs::record::{decode_record, decode_varint, TextEncoding, Value};

/// Minimal-length varint encoder mirroring `decode_varint`'s bit layout
/// (7 bits/byte, big-endian, high-bit continuation flag; the 9-byte
/// form's last byte carries a full 8 bits, and its first 8 bytes encode
/// `value >> 8` as 8 continuation-flagged 7-bit groups — not `value`
/// itself shifted 7 bits at a time, which only coincidentally matches
/// for all-one-bits values like `u64::MAX`). Test-only — the crate has
/// no production encoder since it doesn't write files yet.
fn encode_varint(value: u64) -> Vec<u8> {
    if value < (1u64 << 56) {
        let mut groups = Vec::new();
        let mut v = value;
        loop {
            groups.push((v & 0x7f) as u8);
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        groups.reverse(); // most-significant group first
        let last = groups.len() - 1;
        groups
            .iter()
            .enumerate()
            .map(|(i, &g)| if i == last { g } else { g | 0x80 })
            .collect()
    } else {
        let low_byte = (value & 0xff) as u8;
        let upper56 = value >> 8;
        let mut bytes: Vec<u8> = (0..8)
            .rev()
            .map(|shift| (((upper56 >> (shift * 7)) & 0x7f) as u8) | 0x80)
            .collect();
        bytes.push(low_byte);
        bytes
    }
}

/// Builds a record payload from `(serial_type, body_bytes)` pairs — same
/// shape as `src/record/decode.rs`'s test-only helper of the same name.
fn record_bytes(serial_types_and_bodies: &[(u64, &[u8])]) -> Vec<u8> {
    let mut header = Vec::new();
    for (st, _) in serial_types_and_bodies {
        header.extend(encode_varint(*st));
    }
    // header_len includes its own varint's length; try lengths until stable.
    let mut header_len = header.len() + 1;
    loop {
        let hl_bytes = encode_varint(header_len as u64);
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

proptest! {
    // `FileFailurePersistence`'s default (`SourceParallel`) anchors the
    // regression file's path to a sibling `src/lib.rs`/`main.rs`, which
    // this file (an integration test under `tests/`, not `src/`) doesn't
    // have — it would silently skip persisting failing seeds. Pointing
    // it at a fixed path keeps regression seeds working here too.
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/proptest-regressions/record_proptest.txt"
        ))),
        ..ProptestConfig::default()
    })]

    /// `decode_varint(encode_varint(v)) == (v, encode_varint(v).len())`
    /// for arbitrary `u64` — generalizes
    /// `every_length_from_1_to_9_bytes`'s hand-picked boundary cases to
    /// the full value space.
    #[test]
    fn encode_decode_varint_roundtrip(value: u64) {
        let bytes = encode_varint(value);
        let (decoded, len) = decode_varint(&bytes).unwrap();
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(len, bytes.len());
    }

    /// Generalizes `integer_widths_and_edge_values`'s hand-picked i8
    /// cases to the full range.
    #[test]
    fn prop_integer_i8_roundtrip(v: i8) {
        let payload = record_bytes(&[(1, &v.to_be_bytes())]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v as i64)])
        );
    }

    #[test]
    fn prop_integer_i16_roundtrip(v: i16) {
        let payload = record_bytes(&[(2, &v.to_be_bytes())]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v as i64)])
        );
    }

    #[test]
    fn prop_integer_i24_roundtrip(v in -8_388_608i32..=8_388_607i32) {
        let four = v.to_be_bytes();
        let payload = record_bytes(&[(3, &four[1..4])]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v as i64)])
        );
    }

    #[test]
    fn prop_integer_i32_roundtrip(v: i32) {
        let payload = record_bytes(&[(4, &v.to_be_bytes())]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v as i64)])
        );
    }

    #[test]
    fn prop_integer_i48_roundtrip(v in -140_737_488_355_328i64..=140_737_488_355_327i64) {
        let eight = v.to_be_bytes();
        let payload = record_bytes(&[(5, &eight[2..8])]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v)])
        );
    }

    #[test]
    fn prop_integer_i64_roundtrip(v: i64) {
        let payload = record_bytes(&[(6, &v.to_be_bytes())]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Integer(v)])
        );
    }

    /// Generalizes `real_edge_values_bit_identical` and
    /// `real_nan_decodes_as_null` to every `f64` bit pattern, not just a
    /// hand-picked list — including the many non-canonical NaN payloads
    /// `f64::NAN` alone doesn't cover.
    #[test]
    fn prop_real_roundtrip_bit_exact_or_nan_to_null(bits: u64) {
        let v = f64::from_bits(bits);
        let payload = record_bytes(&[(7, &v.to_be_bytes())]);
        let decoded = decode_record(&payload, TextEncoding::Utf8).unwrap();
        if v.is_nan() {
            prop_assert_eq!(decoded, vec![Value::Null]);
        } else {
            match &decoded[..] {
                [Value::Real(r)] => prop_assert_eq!(r.to_bits(), v.to_bits()),
                other => prop_assert!(false, "expected one Real, got {other:?}"),
            }
        }
    }

    /// Generalizes `blob_including_zero_length` to arbitrary byte
    /// strings.
    #[test]
    fn prop_blob_roundtrip(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        let serial_type = 12 + 2 * bytes.len() as u64;
        let payload = record_bytes(&[(serial_type, &bytes)]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Blob(bytes.clone())])
        );
    }

    /// Generalizes `text_utf8_including_empty` to arbitrary strings.
    #[test]
    fn prop_text_utf8_roundtrip(s in ".{0,100}") {
        let bytes = s.as_bytes();
        let serial_type = 13 + 2 * bytes.len() as u64;
        let payload = record_bytes(&[(serial_type, bytes)]);
        prop_assert_eq!(
            decode_record(&payload, TextEncoding::Utf8),
            Ok(vec![Value::Text(s.clone())])
        );
    }
}
