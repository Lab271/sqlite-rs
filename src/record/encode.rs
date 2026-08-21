use super::value::{TextEncoding, Value};

/// Encodes a varint: the inverse of `decode_varint`. Mirrors the
/// decoder's bit layout — big-endian, 7 bits per byte with a high-bit
/// continuation flag, up to 9 bytes — always producing the minimal
/// encoding (no redundant continuation bytes).
#[allow(
    clippy::arithmetic_side_effects,
    reason = "groups/i/shift all range over the compile-time-constant 0..8, so these additions and the 7x multiply never overflow"
)]
pub(crate) fn encode_varint(value: u64) -> Vec<u8> {
    // The 9-byte form only kicks in once the value needs more than 56
    // bits (8 groups of 7): the decoder's own threshold (it reads 8
    // 7-bit groups, then an unconditional 9th full-byte group).
    if value < (1u64 << 56) {
        let mut groups = 1u32;
        while groups < 8 && value >= (1u64 << (7 * groups)) {
            groups += 1;
        }
        (0..groups)
            .map(|i| {
                let shift = 7 * (groups - 1 - i);
                #[allow(clippy::cast_possible_truncation)]
                let mut byte = ((value >> shift) & 0x7f) as u8;
                if i != groups - 1 {
                    byte |= 0x80;
                }
                byte
            })
            .collect()
    } else {
        let top56 = value >> 8;
        let mut out: Vec<u8> = (0..8)
            .map(|i| {
                let shift = 7 * (7 - i);
                #[allow(clippy::cast_possible_truncation)]
                let byte = (((top56 >> shift) & 0x7f) as u8) | 0x80;
                byte
            })
            .collect();
        #[allow(clippy::cast_possible_truncation)]
        out.push((value & 0xff) as u8);
        out
    }
}

// 24-bit and 48-bit signed integer ranges, per sqlite3VdbeSerialType's
// integer-width selection (smallest serial type that losslessly holds
// the value).
const I24_MIN: i64 = -(1 << 23);
const I24_MAX: i64 = (1 << 23) - 1;
const I48_MIN: i64 = -(1 << 47);
const I48_MAX: i64 = (1 << 47) - 1;

fn integer_serial_type(i: i64) -> u64 {
    if i == 0 {
        8
    } else if i == 1 {
        9
    } else if i8::try_from(i).is_ok() {
        1
    } else if i16::try_from(i).is_ok() {
        2
    } else if (I24_MIN..=I24_MAX).contains(&i) {
        3
    } else if i32::try_from(i).is_ok() {
        4
    } else if (I48_MIN..=I48_MAX).contains(&i) {
        5
    } else {
        6
    }
}

fn integer_body(i: i64, serial_type: u64) -> Vec<u8> {
    match serial_type {
        1 => vec![i as u8],
        2 => (i as i16).to_be_bytes().to_vec(),
        3 => {
            let b = i.to_be_bytes();
            b[5..8].to_vec()
        }
        4 => (i as i32).to_be_bytes().to_vec(),
        5 => {
            let b = i.to_be_bytes();
            b[2..8].to_vec()
        }
        6 => i.to_be_bytes().to_vec(),
        _ => Vec::new(), // 8/9: zero-byte constants
    }
}

fn encode_text(s: &str, encoding: TextEncoding) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => s.as_bytes().to_vec(),
        TextEncoding::Utf16Le => s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
        TextEncoding::Utf16Be => s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
    }
}

fn blob_serial_type(len: usize) -> u64 {
    12u64.saturating_add(2u64.saturating_mul(len as u64))
}

fn text_serial_type(len: usize) -> u64 {
    13u64.saturating_add(2u64.saturating_mul(len as u64))
}

/// Returns a value's serial type and encoded body, per the record-format
/// doc: the smallest integer width that losslessly holds an INTEGER, the
/// 8-byte IEEE-754 form for REAL, and the `12+2*len`/`13+2*len` scheme for
/// BLOB/TEXT.
fn serial_type_and_body(value: &Value, encoding: TextEncoding) -> (u64, Vec<u8>) {
    match value {
        Value::Null => (0, Vec::new()),
        Value::Integer(i) => {
            let st = integer_serial_type(*i);
            (st, integer_body(*i, st))
        }
        Value::Real(r) => (7, r.to_be_bytes().to_vec()),
        Value::Blob(b) => (blob_serial_type(b.len()), b.to_vec()),
        Value::Text(s) => {
            let body = encode_text(s, encoding);
            (text_serial_type(body.len()), body)
        }
    }
}

/// Encodes column values into a record payload, per the record-format
/// doc: a varint header length, one varint serial type per column, then
/// the column bodies back-to-back. The inverse of
/// [`super::decode::decode_record`] — round-tripping through both
/// functions reproduces the original values, and the byte layout matches
/// spec 003 exactly (reused as-is for `MakeRecord`'s in-memory rows).
pub fn encode_record(values: &[Value], encoding: TextEncoding) -> Vec<u8> {
    let parts: Vec<(u64, Vec<u8>)> = values
        .iter()
        .map(|v| serial_type_and_body(v, encoding))
        .collect();

    let mut serial_type_bytes = Vec::new();
    for (st, _) in &parts {
        serial_type_bytes.extend(encode_varint(*st));
    }

    // header_len includes its own varint's length; grow until the
    // varint's own encoded size is consistent with the declared length.
    let mut header_len = serial_type_bytes.len().saturating_add(1);
    let header_len_varint = loop {
        #[allow(clippy::cast_possible_truncation)]
        let hl_bytes = encode_varint(header_len as u64);
        if hl_bytes.len().saturating_add(serial_type_bytes.len()) == header_len {
            break hl_bytes;
        }
        header_len = header_len.saturating_add(1);
    };

    let mut out = header_len_varint;
    out.extend(&serial_type_bytes);
    for (_, body) in &parts {
        out.extend(body);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::super::decode::decode_record;
    use super::*;

    #[test]
    fn round_trips_through_decode_record() {
        let values = vec![
            Value::Null,
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(-1),
            Value::Integer(i64::MAX),
            Value::Integer(i64::MIN),
            Value::Real(1.5),
            Value::Text("hello".to_string().into()),
            Value::Text(String::new().into()),
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef].into()),
            Value::Blob(Vec::new().into()),
        ];
        let payload = encode_record(&values, TextEncoding::Utf8);
        assert_eq!(decode_record(&payload, TextEncoding::Utf8), Ok(values));
    }

    #[test]
    fn integer_widths_pick_smallest_serial_type() {
        let cases: &[(i64, u64)] = &[
            (0, 8),
            (1, 9),
            (2, 1),
            (i8::MIN as i64, 1),
            (i8::MAX as i64 + 1, 2),
            (i16::MIN as i64, 2),
            (i16::MAX as i64 + 1, 3),
            (I24_MIN, 3),
            (I24_MAX + 1, 4),
            (i32::MIN as i64, 4),
            (i32::MAX as i64 + 1, 5),
            (I48_MIN, 5),
            (I48_MAX + 1, 6),
            (i64::MAX, 6),
            (i64::MIN, 6),
        ];
        for (v, expected_st) in cases {
            let (st, _) = serial_type_and_body(&Value::Integer(*v), TextEncoding::Utf8);
            assert_eq!(
                st, *expected_st,
                "value {v} expected serial type {expected_st}"
            );
        }
    }

    #[test]
    fn matches_spec_003_header_shape_for_a_multi_column_row() {
        // Mirrors the decoder's own fixture-construction convention.
        let values = vec![Value::Integer(42), Value::Text("abc".to_string().into())];
        let payload = encode_record(&values, TextEncoding::Utf8);
        // header_len(1) + serial_type(42 -> type 1, 1 byte) + serial_type(abc -> 13+2*3=19, 1 byte) = 3
        assert_eq!(payload[0], 3);
        assert_eq!(payload[1], 1); // type 1: i8
        assert_eq!(payload[2], 19); // type 13+2*3
        assert_eq!(payload[3], 42);
        assert_eq!(&payload[4..7], b"abc");
    }
}
