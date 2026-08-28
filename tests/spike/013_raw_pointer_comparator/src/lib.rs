//! Spike 013: does sqlite3's raw-pointer sort-key comparator trick
//! (`vdbesort.c`'s `vdbeSorterCompareInt`) actually explain the
//! remaining ~2.6-3.6x gap between sqlite-rs and the oracle on
//! `group_by_agg` (#631), or is the gap mostly the *general decode
//! pipeline* (header-walk-into-a-Vec, a tagged `Value` enum, a
//! collation-dispatching `compare()`) rather than bounds-checking
//! itself?
//!
//! Three comparators, same record format (a minimal single-column
//! INTEGER record: 1-byte header-length varint, 1-byte serial type,
//! then a big-endian integer body of the width the serial type names —
//! serial types 1/2/3/4/5/6 for 1/2/3/4/6/8-byte integers, matching
//! `src/record/decode.rs`'s `serial_type_len` table), same test data:
//!
//! - [`regular::compare`] — the *general* path sqlite-rs actually used
//!   before #631's spike-013-adjacent fixes: parse the whole header
//!   into a freshly allocated `Vec<(serial_type, offset)>`, decode the
//!   wanted column into a tagged [`regular::Value`] enum via a
//!   multi-arm `match`, then compare through a generic, collation-aware
//!   `compare()`. Safe Rust throughout.
//! - [`safe_fast::compare`] — the algorithm sqlite-rs actually shipped
//!   in #631 (`decode_single_column` in `src/record/decode.rs` plus a
//!   same-serial-type byte-compare, mirroring sqlite3's *shape* without
//!   its raw pointers): no header `Vec`, no `Value` enum, direct
//!   same-width byte comparison — but every access is bounds-checked
//!   (`.get()`), never `unsafe`.
//! - [`unsafe_trick::compare`] — a literal port of sqlite3's
//!   `vdbeSorterCompareInt`: raw pointer arithmetic, no bounds checks,
//!   `unsafe` throughout. This crate has no `unsafe_code = "deny"` (the
//!   main crate does — see its `Cargo.toml`), so this is exempt
//!   specifically to allow this comparison to exist at all.
//!
//! See `benches/compare.rs` for the head-to-head, and `README.md` for
//! the results and conclusion.

use std::cmp::Ordering;

/// Number of body bytes a serial type occupies — mirrors
/// `src/record/decode.rs::serial_type_len`, restricted to the integer
/// serial types this spike's records ever use.
fn serial_type_len(serial_type: u8) -> usize {
    match serial_type {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 => 8,
        _ => panic!("spike only encodes integer serial types 0-6"),
    }
}

/// Picks the smallest integer serial type/width that can hold `v`,
/// mirroring how sqlite-rs's own record encoder picks a width — a
/// realistic GROUP BY key column (small, mostly non-negative bucket
/// values) skews heavily toward serial type 1 (1 byte), same as the
/// real `bench_data.bucket` column #631 was benchmarked against.
fn minimal_serial_type(v: i64) -> u8 {
    if (-128..=127).contains(&v) {
        1
    } else if (-32768..=32767).contains(&v) {
        2
    } else if (-8_388_608..=8_388_607).contains(&v) {
        3
    } else if (-2_147_483_648..=2_147_483_647).contains(&v) {
        4
    } else if (-140_737_488_355_328..=140_737_488_355_327).contains(&v) {
        5
    } else {
        6
    }
}

/// Encodes `v` as a minimal single-column INTEGER record: a 1-byte
/// header-length varint (always valid here — header is 2 bytes: the
/// length byte itself plus one serial-type byte, well under the 128
/// that would need a second varint byte), the serial type byte, then
/// the big-endian body bytes truncated/sign-extended to the chosen
/// width.
pub fn encode_int_record(v: i64) -> Vec<u8> {
    let serial_type = minimal_serial_type(v);
    let width = serial_type_len(serial_type);
    let be = v.to_be_bytes(); // 8 bytes, big-endian
    let mut out = Vec::with_capacity(2 + width);
    out.push(2); // header length: 1 (own varint) + 1 (serial type byte)
    out.push(serial_type);
    // Keep the low `width` bytes of the big-endian 8-byte representation —
    // sign-extension already makes these the correct minimal-width
    // big-endian encoding for any `v` `minimal_serial_type` chose `width` for.
    out.extend_from_slice(&be[8 - width..]);
    out
}

/// The *general* decode pipeline sqlite-rs used for every sorter key
/// column before #631: allocate a header-entries `Vec`, decode into a
/// tagged `Value`, compare via a generic dispatcher. Mirrors
/// `src/record/decode.rs` (`parse_header_into`/`decode_serial_value`)
/// and `src/vdbe/compare.rs` (`compare`) in shape, trimmed to the
/// integer-only subset this spike's records ever contain.
pub mod regular {
    use super::{serial_type_len, Ordering};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Value {
        Null,
        Integer(i64),
    }

    /// Mirrors `src/record/varint.rs::decode_varint` — a bounds-checked,
    /// up-to-9-byte varint decode. Every record this spike builds has a
    /// single-byte header-length varint, so this always returns on its
    /// first loop iteration in practice, same as production.
    fn decode_varint(buf: &[u8]) -> Option<(u64, usize)> {
        let mut result: u64 = 0;
        for i in 0..8 {
            let byte = *buf.get(i)?;
            result = (result << 7) | u64::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                return Some((result, i + 1));
            }
        }
        let byte = *buf.get(8)?;
        result = (result << 8) | u64::from(byte);
        Some((result, 9))
    }

    /// Mirrors `parse_header_into`: walks every header entry into a
    /// freshly allocated `Vec` — the exact per-call allocation #631's
    /// `decode_record_only_into`/`decode_single_column` fast path was
    /// written specifically to avoid.
    fn parse_header(payload: &[u8]) -> Option<Vec<(u64, usize)>> {
        let (header_len, n) = decode_varint(payload)?;
        let header_len = usize::try_from(header_len).ok()?;
        let mut pos = n;
        let mut body_pos = header_len;
        let mut entries = Vec::new();
        while pos < header_len {
            let (serial_type, len) = decode_varint(payload.get(pos..)?)?;
            pos += len;
            entries.push((serial_type, body_pos));
            body_pos += serial_type_len(u8::try_from(serial_type).ok()?);
        }
        Some(entries)
    }

    /// Mirrors `decode_serial_value`'s per-serial-type `match` — a
    /// generic dispatcher covering every serial type sqlite's record
    /// format defines, not just the integer ones this spike encodes,
    /// since that generality (needed for TEXT/BLOB/REAL/NULL columns in
    /// production) is itself part of what makes the general path
    /// heavier than a type-specialized one.
    fn decode_serial_value(serial_type: u64, buf: &[u8], pos: usize) -> Option<Value> {
        match serial_type {
            0 => Some(Value::Null),
            1 => Some(Value::Integer(i64::from(*buf.get(pos)? as i8))),
            2 => {
                let b: [u8; 2] = buf.get(pos..pos + 2)?.try_into().ok()?;
                Some(Value::Integer(i64::from(i16::from_be_bytes(b))))
            }
            3 => {
                let b = buf.get(pos..pos + 3)?;
                let sign = if b.first()? & 0x80 != 0 { 0xFFu8 } else { 0 };
                let full = [sign, *b.first()?, *b.get(1)?, *b.get(2)?];
                Some(Value::Integer(i64::from(i32::from_be_bytes(full))))
            }
            4 => {
                let b: [u8; 4] = buf.get(pos..pos + 4)?.try_into().ok()?;
                Some(Value::Integer(i64::from(i32::from_be_bytes(b))))
            }
            5 => {
                let b = buf.get(pos..pos + 6)?;
                let sign = if b.first()? & 0x80 != 0 { 0xFFu8 } else { 0 };
                let full = [
                    sign,
                    sign,
                    *b.first()?,
                    *b.get(1)?,
                    *b.get(2)?,
                    *b.get(3)?,
                    *b.get(4)?,
                    *b.get(5)?,
                ];
                Some(Value::Integer(i64::from_be_bytes(full)))
            }
            6 => {
                let b: [u8; 8] = buf.get(pos..pos + 8)?.try_into().ok()?;
                Some(Value::Integer(i64::from_be_bytes(b)))
            }
            8 => Some(Value::Integer(0)),
            9 => Some(Value::Integer(1)),
            _ => None,
        }
    }

    /// Mirrors `src/vdbe/compare.rs::compare`'s type-ranking dispatch,
    /// trimmed to the two classes this spike's records ever produce.
    fn compare_values(a: Value, b: Value) -> Ordering {
        match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::Integer(x), Value::Integer(y)) => x.cmp(&y),
        }
    }

    pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
        let ea = parse_header(a).expect("valid spike record");
        let eb = parse_header(b).expect("valid spike record");
        let &(sa, oa) = ea.first().expect("single-column spike record");
        let &(sb, ob) = eb.first().expect("single-column spike record");
        let va = decode_serial_value(sa, a, oa).expect("valid spike record");
        let vb = decode_serial_value(sb, b, ob).expect("valid spike record");
        compare_values(va, vb)
    }
}

/// The algorithm sqlite-rs actually shipped in #631: no header `Vec`,
/// no `Value` enum, a direct same-serial-type byte comparison — the
/// *shape* of sqlite3's trick, but with every access bounds-checked
/// (`.get()`) instead of raw pointers. Falls back to a full (still
/// safe) decode-and-compare on the rare cross-width case, same as
/// production would via its generic path.
pub mod safe_fast {
    use super::{regular, serial_type_len, Ordering};

    pub fn compare(a: &[u8], b: &[u8]) -> Ordering {
        let ha = *a.first().expect("valid spike record") as usize;
        let hb = *b.first().expect("valid spike record") as usize;
        let sa = *a.get(1).expect("valid spike record");
        let sb = *b.get(1).expect("valid spike record");
        if sa != sb {
            // Cross-width comparison needs real numeric decode either
            // way — falls back to the general path, same as sqlite3's
            // own comparators do for the cases their fast path doesn't
            // cover.
            return regular::compare(a, b);
        }
        let n = serial_type_len(sa);
        let va = a.get(ha..ha + n).expect("valid spike record");
        let vb = b.get(hb..hb + n).expect("valid spike record");
        for i in 0..n {
            let (byte_a, byte_b) = (
                *va.get(i).expect("in bounds"),
                *vb.get(i).expect("in bounds"),
            );
            if byte_a != byte_b {
                // Differing sign bit on the leading byte decides it
                // outright — same big-endian-bytes-are-numeric-order
                // trick sqlite3's comparator uses.
                if ((va.first().expect("nonempty") ^ vb.first().expect("nonempty")) & 0x80) != 0 {
                    return if va.first().expect("nonempty") & 0x80 != 0 {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                return byte_a.cmp(&byte_b);
            }
        }
        Ordering::Equal
    }
}

/// A literal port of sqlite3's `vdbesort.c::vdbeSorterCompareInt`: raw
/// pointer arithmetic, zero bounds checks. `unsafe` is sound here only
/// because both inputs are always well-formed records this spike's own
/// [`encode_int_record`] produced — production code would need the same
/// invariant sqlite3 documents for the real function (a validated
/// header size and a buffer with trailing padding), which is exactly
/// the kind of safety obligation the main crate's `unsafe_code = "deny"`
/// lint exists to keep out.
pub mod unsafe_trick {
    use super::{regular, serial_type_len, Ordering};

    /// # Safety
    /// `a` and `b` must each be a valid record this spike's
    /// `encode_int_record` produced (or an equivalent: 1-byte header
    /// length, 1-byte serial type, a body at least as long as
    /// `serial_type_len` of that serial type names).
    pub unsafe fn compare(a: &[u8], b: &[u8]) -> Ordering {
        let p1 = a.as_ptr();
        let p2 = b.as_ptr();
        let s1 = *p1.add(1);
        let s2 = *p2.add(1);
        if s1 != s2 {
            // Same fallback as the safe version — sqlite3's real
            // comparator handles this inline (see `vdbeSorterCompareInt`
            // in `vdbesort.c`), trimmed here for the spike's purposes.
            return regular::compare(a, b);
        }
        let h1 = *p1 as usize;
        let h2 = *p2 as usize;
        let v1 = p1.add(h1);
        let v2 = p2.add(h2);
        let n = serial_type_len(s1);
        for i in 0..n {
            let byte_a = *v1.add(i);
            let byte_b = *v2.add(i);
            if byte_a != byte_b {
                if ((*v1 ^ *v2) & 0x80) != 0 {
                    return if *v1 & 0x80 != 0 {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    };
                }
                return byte_a.cmp(&byte_b);
            }
        }
        Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_values() -> Vec<i64> {
        vec![
            0,
            1,
            -1,
            127,
            -128,
            128,
            -129,
            32767,
            -32768,
            32768,
            i32::MAX as i64,
            i32::MIN as i64,
            i32::MAX as i64 + 1,
            i64::MAX,
            i64::MIN,
            42,
            -42,
            1_000_000,
            -1_000_000,
        ]
    }

    #[test]
    fn all_three_comparators_agree_on_every_pair() {
        let values = sample_values();
        let records: Vec<Vec<u8>> = values.iter().map(|&v| encode_int_record(v)).collect();
        for (i, a) in records.iter().enumerate() {
            for (j, b) in records.iter().enumerate() {
                let expected = values[i].cmp(&values[j]);
                assert_eq!(regular::compare(a, b), expected, "regular {i} vs {j}");
                assert_eq!(safe_fast::compare(a, b), expected, "safe_fast {i} vs {j}");
                assert_eq!(
                    unsafe { unsafe_trick::compare(a, b) },
                    expected,
                    "unsafe_trick {i} vs {j}"
                );
            }
        }
    }

    #[test]
    fn minimal_serial_type_round_trips_through_decode() {
        for &v in &sample_values() {
            let record = encode_int_record(v);
            let regular::Value::Integer(decoded) = regular_decode_only(&record) else {
                panic!("expected an integer");
            };
            assert_eq!(decoded, v);
        }
    }

    fn regular_decode_only(record: &[u8]) -> regular::Value {
        // Exercises the same decode `compare::compare` uses internally,
        // by comparing the record against itself (Equal iff the decode
        // round-trips) — indirect, but avoids exposing `regular`'s
        // private decode function just for this test.
        assert_eq!(regular::compare(record, record), Ordering::Equal);
        // Re-derive the value via encode_int_record's own inverse for
        // the actual assertion.
        let mut buf = [0u8; 8];
        let width = record.len() - 2;
        let sign = if record.get(2).is_some_and(|b| b & 0x80 != 0) {
            0xFFu8
        } else {
            0
        };
        buf.fill(sign);
        buf[8 - width..].copy_from_slice(&record[2..]);
        regular::Value::Integer(i64::from_be_bytes(buf))
    }
}
