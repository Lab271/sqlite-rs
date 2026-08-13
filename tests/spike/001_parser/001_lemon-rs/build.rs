//! Build script: compile the vendored Lemon parser generator, then run it over
//! `src/parse.y` with the Rust code-generation template
//! `third_party/lemon/lempar.rs`, emitting `$OUT_DIR/parse.rs`.
//!
//! Deliberately dependency-free: we invoke the system C compiler through
//! `std::process::Command` instead of pulling in the `cc` crate, so the spike
//! builds offline with zero crates.io dependencies.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let lemon_c = manifest.join("third_party/lemon/lemon.c");
    let lempar = manifest.join("third_party/lemon/lempar.rs");
    let grammar = manifest.join("src/parse.y");

    println!("cargo:rerun-if-changed={}", lemon_c.display());
    println!("cargo:rerun-if-changed={}", lempar.display());
    println!("cargo:rerun-if-changed={}", grammar.display());
    println!("cargo:rerun-if-env-changed=CC");

    // 1. Build the lemon tool itself (host binary).
    let lemon_bin = out_dir.join("lemon");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    run(
        Command::new(&cc)
            .arg("-O2")
            .arg("-w") // lemon.c is vendored verbatim; its warnings are not ours
            .arg("-o")
            .arg(&lemon_bin)
            .arg(&lemon_c),
        "compile lemon.c",
    );

    // 2. Run lemon over the grammar.
    //    -m  emit the `TokenType` enum inline (instead of a separate header)
    //    -T  use our Rust driver template
    //    -d  write generated files to OUT_DIR
    run(
        Command::new(&lemon_bin)
            .arg("-m")
            .arg(format!("-T{}", lempar.display()))
            .arg(format!("-d{}", out_dir.display()))
            .arg(&grammar),
        "run lemon over src/parse.y",
    );

    let generated = out_dir.join("parse.rs");
    assert!(
        generated.exists(),
        "lemon did not produce {}",
        generated.display()
    );
    // The report file (parse.out) also lands in OUT_DIR; handy when debugging
    // LALR conflicts.
    let _ = Path::new(&out_dir).join("parse.out");
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn ({what}): {e}"));
    assert!(status.success(), "{what} failed with {status}");
}
