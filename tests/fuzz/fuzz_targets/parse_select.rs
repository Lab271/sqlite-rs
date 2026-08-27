// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
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

    // Deep expression nesting is the specific failure mode #118 fixed (a
    // stack overflow that pre-empted `MAX_EXPR_DEPTH`'s clean `Invalid`).
    // The corpus/dictionary directories are gitignored, so a committed
    // seed input isn't an option; deriving a nesting depth from the fuzz
    // input's own length instead guarantees every run also exercises this
    // path, rather than relying on random mutation to discover it.
    let depth = 1 + (data.len() % 300);
    let deep = format!("SELECT {}1{}", "abs(".repeat(depth), ")".repeat(depth));
    let _ = parse_select(&deep);
});
