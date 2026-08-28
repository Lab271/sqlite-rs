// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::record::Value;
use sqlite_rs::vdbe::call_function;

const NAMES: &[&str] = &[
    "length", "upper", "lower", "substr", "abs", "coalesce", "ifnull", "nullif", "typeof", "hex",
    "unhex", "quote", "min", "max", "round", "sign", "instr", "trim", "ltrim", "rtrim", "replace",
    "zeroblob", "iif",
];

fn decode_value(bytes: &[u8], pos: &mut usize) -> Value {
    let Some(&tag) = bytes.get(*pos) else {
        return Value::Null;
    };
    *pos = pos.saturating_add(1);
    match tag % 5 {
        0 => Value::Null,
        1 => {
            let n = bytes.get(*pos..pos.saturating_add(8)).unwrap_or(&[]);
            *pos = pos.saturating_add(8);
            let mut buf = [0u8; 8];
            buf[..n.len()].copy_from_slice(n);
            Value::Integer(i64::from_le_bytes(buf))
        }
        2 => {
            let n = bytes.get(*pos..pos.saturating_add(8)).unwrap_or(&[]);
            *pos = pos.saturating_add(8);
            let mut buf = [0u8; 8];
            buf[..n.len()].copy_from_slice(n);
            Value::Real(f64::from_le_bytes(buf))
        }
        3 => {
            let len = (*bytes.get(*pos).unwrap_or(&0) as usize) % 8;
            *pos = pos.saturating_add(1);
            let s = bytes
                .get(*pos..pos.saturating_add(len))
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            *pos = pos.saturating_add(len);
            Value::Text(s.into())
        }
        _ => {
            let len = (*bytes.get(*pos).unwrap_or(&0) as usize) % 8;
            *pos = pos.saturating_add(1);
            let b = bytes
                .get(*pos..pos.saturating_add(len))
                .unwrap_or(&[])
                .to_vec();
            *pos = pos.saturating_add(len);
            Value::Blob(b.into())
        }
    }
}

// Registry dispatch on arbitrary Values must never panic — accept
// (Ok) or a structured FunctionError are the only allowed outcomes.
fuzz_target!(|data: &[u8]| {
    let Some((&name_selector, rest)) = data.split_first() else {
        return;
    };
    let name = NAMES[name_selector as usize % NAMES.len()];
    let Some((&arity_byte, rest)) = rest.split_first() else {
        return;
    };
    let arity = (arity_byte as usize) % 5;
    let mut pos = 0;
    let args: Vec<Value> = (0..arity).map(|_| decode_value(rest, &mut pos)).collect();
    let _ = call_function(name, &args);
});
