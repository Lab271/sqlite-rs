//! Spike report helper: prints the per-statement outcome for both fixture files
//! and times a 1000x parse of the whole valid corpus.
//!
//!     cargo run --release --example report

use std::time::Instant;

use spike_lemon_rs::parse;

const VALID: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/valid.sql"));
const INVALID: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/invalid.sql"
));

fn statements(text: &str) -> Vec<&str> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn main() {
    let valid = statements(VALID);
    let invalid = statements(INVALID);

    let mut ok = 0;
    println!("== valid.sql ({} statements) ==", valid.len());
    for sql in &valid {
        match parse(sql) {
            Ok(_) => ok += 1,
            Err(e) => println!("  FAIL  {sql}\n        {e}"),
        }
    }
    println!("  {ok}/{} parsed", valid.len());

    let mut rejected = 0;
    println!("== invalid.sql ({} statements) ==", invalid.len());
    for sql in &invalid {
        match parse(sql) {
            Ok(ast) => println!("  ACCEPTED (should not be)  {sql}\n        {ast:?}"),
            Err(e) => {
                rejected += 1;
                println!("  ok  {sql}\n      -> {e}");
            }
        }
    }
    println!("  {rejected}/{} rejected", invalid.len());

    // Timing: 1000 passes over the whole valid corpus.
    const ITERS: u32 = 1000;
    let bytes: usize = valid.iter().map(|s| s.len()).sum();
    let start = Instant::now();
    let mut sink = 0usize;
    for _ in 0..ITERS {
        for sql in &valid {
            sink += parse(sql).map(|_| 1).unwrap_or(0);
        }
    }
    let elapsed = start.elapsed();
    println!("== timing ==");
    println!("  {sink} statements parsed");
    println!(
        "  {ITERS} passes over valid.sql ({} statements, {bytes} bytes): {:?} total, {:?} per pass, {:.0} ns per statement",
        valid.len(),
        elapsed,
        elapsed / ITERS,
        elapsed.as_nanos() as f64 / (ITERS as f64 * valid.len() as f64)
    );
}
