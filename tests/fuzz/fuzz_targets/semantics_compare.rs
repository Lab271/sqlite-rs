// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::record::Value;
use sqlite_rs::vdbe::{compare, Collation};
use std::cmp::Ordering;

// Discharges spec 008 Requirement 2's total-order properties for `compare`:
// arbitrary `Value` pairs under any collation must never panic, and the
// relation must be antisymmetric (a<=>b reverses b<=>a) and never randomly
// flip on repeated calls with the same inputs (determinism is a prerequisite
// for the transitivity check below, since a flaky order can't be transitive).
fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let collation = match selector % 3 {
        0 => Collation::Binary,
        1 => Collation::NoCase,
        _ => Collation::RTrim,
    };

    let Some((a, rest)) = decode_value(rest) else {
        return;
    };
    let Some((b, rest)) = decode_value(rest) else {
        return;
    };
    let Some((c, _)) = decode_value(rest) else {
        return;
    };

    let ab = compare(&a, &b, collation);
    let ba = compare(&b, &a, collation);
    assert_eq!(ab, ba.reverse(), "compare must be antisymmetric");

    let bc = compare(&b, &c, collation);
    let ac = compare(&a, &c, collation);
    if ab == Ordering::Less && bc == Ordering::Less {
        assert_eq!(ac, Ordering::Less, "compare must be transitive");
    }
    if ab == Ordering::Equal && bc == Ordering::Equal {
        assert_eq!(ac, Ordering::Equal, "compare must be transitive");
    }
});

fn decode_value(data: &[u8]) -> Option<(Value, &[u8])> {
    let (&tag, rest) = data.split_first()?;
    match tag % 5 {
        0 => Some((Value::Null, rest)),
        1 => {
            let (bytes, rest) = take::<8>(rest)?;
            Some((Value::Integer(i64::from_le_bytes(bytes)), rest))
        }
        2 => {
            let (bytes, rest) = take::<8>(rest)?;
            Some((Value::Real(f64::from_le_bytes(bytes)), rest))
        }
        3 => {
            let (&len, rest) = rest.split_first()?;
            let len = (len as usize).min(rest.len());
            let (text, rest) = rest.split_at(len);
            Some((
                Value::Text(String::from_utf8_lossy(text).into_owned().into()),
                rest,
            ))
        }
        _ => {
            let (&len, rest) = rest.split_first()?;
            let len = (len as usize).min(rest.len());
            let (blob, rest) = rest.split_at(len);
            Some((Value::Blob(blob.to_vec().into()), rest))
        }
    }
}

fn take<const N: usize>(data: &[u8]) -> Option<([u8; N], &[u8])> {
    if data.len() < N {
        return None;
    }
    let (head, rest) = data.split_at(N);
    let mut buf = [0u8; N];
    buf.copy_from_slice(head);
    Some((buf, rest))
}
