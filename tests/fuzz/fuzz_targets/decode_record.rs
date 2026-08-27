// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![no_main]

use libfuzzer_sys::fuzz_target;

use sqlite_rs::record::{decode_record, TextEncoding};

// Directly discharges spec 003 Requirement 6's "Fuzz safety" scenario:
// arbitrary bytes must decode to `Ok` or a structured `Err`, never panic.
// The first byte selects the text encoding so all three decode paths
// (UTF-8/UTF-16LE/UTF-16BE) get exercised; the rest is the record payload.
fuzz_target!(|data: &[u8]| {
    let Some((&selector, payload)) = data.split_first() else {
        return;
    };
    let encoding = match selector % 3 {
        0 => TextEncoding::Utf8,
        1 => TextEncoding::Utf16Le,
        _ => TextEncoding::Utf16Be,
    };
    let _ = decode_record(payload, encoding);
});
