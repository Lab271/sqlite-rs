// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spike measurements: error-message samples + rough throughput.
//! Run with `cargo run --release --example bench`.

use std::time::Instant;

const VALID: &str = include_str!("../../fixtures/valid.sql");
const INVALID: &str = include_str!("../../fixtures/invalid.sql");

fn main() {
    let valid = pomelo_spike::split_statements(VALID);
    let invalid = pomelo_spike::split_statements(INVALID);

    println!("== error messages ({} invalid statements) ==", invalid.len());
    for sql in &invalid {
        match pomelo_spike::parse(sql) {
            Ok(_) => println!("!! WRONGLY ACCEPTED: {sql}"),
            Err(e) => println!("{sql}\n    {e}"),
        }
    }

    // warm-up
    for sql in &valid {
        let _ = pomelo_spike::parse(sql);
    }

    const ITERS: u32 = 1000;
    let t0 = Instant::now();
    for _ in 0..ITERS {
        for sql in &valid {
            pomelo_spike::parse(sql).expect("valid fixture must parse");
        }
    }
    let elapsed = t0.elapsed();

    let total_stmts = ITERS as u128 * valid.len() as u128;
    println!("\n== perf ==");
    println!(
        "{} iterations x {} statements ({} bytes/iter): {:?} total",
        ITERS,
        valid.len(),
        VALID.len(),
        elapsed
    );
    println!("  per full valid.sql pass: {:?}", elapsed / ITERS);
    println!(
        "  per statement:            {:.3} us",
        elapsed.as_nanos() as f64 / total_stmts as f64 / 1000.0
    );
}
