//! Re-exports Cargo.toml's `[package.metadata.oracle] version` pin as the
//! `ORACLE_VERSION` compile-time env var (`env!("ORACLE_VERSION")`), so
//! `src/` code that needs the pinned sqlite3 version (e.g. `sqlite_version()`,
//! #136) reads it from the single source of truth instead of carrying its
//! own copy of the literal — see `tools/version_pin.py`.

use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=Cargo.toml");

    let Ok(manifest) = fs::read_to_string("Cargo.toml") else {
        eprintln!("build.rs: could not read Cargo.toml");
        return ExitCode::FAILURE;
    };
    let Some(oracle_section) = manifest.split("[package.metadata.oracle]").nth(1) else {
        eprintln!("build.rs: Cargo.toml missing [package.metadata.oracle] section");
        return ExitCode::FAILURE;
    };
    let Some(version_line) = oracle_section
        .lines()
        .find(|line| line.trim_start().starts_with("version"))
    else {
        eprintln!("build.rs: [package.metadata.oracle] missing version key");
        return ExitCode::FAILURE;
    };
    let Some(version) = version_line.split('"').nth(1) else {
        eprintln!("build.rs: oracle version line missing quoted value");
        return ExitCode::FAILURE;
    };

    println!("cargo:rustc-env=ORACLE_VERSION={version}");
    ExitCode::SUCCESS
}
