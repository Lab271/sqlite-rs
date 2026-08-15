#![no_main]

use libfuzzer_sys::fuzz_target;
use sqlite_rs::parser::parse_select;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // `parse_select` must never panic on any input — accept, reject as
    // unsupported, or reject as invalid are the only allowed outcomes.
    let _ = parse_select(src);
});
