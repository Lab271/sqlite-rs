//! Enforces spec 001-architecture Requirement 1 (Layer Isolation): Tier 0
//! core modules (schema, btree, pager, record, header) must never depend
//! on the SQL-execution layers (parser, codegen, vdbe). Without this,
//! that boundary holds only by convention — a stray `use crate::parser`
//! in `schema.rs` would compile silently.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::path::{Path, PathBuf};

const TIER0_ROOTS: &[&str] = &[
    "src/schema.rs",
    "src/btree.rs",
    "src/pager.rs",
    "src/record.rs",
    "src/header.rs",
];

const FORBIDDEN: &[&str] = &["use crate::parser", "use crate::codegen", "use crate::vdbe"];

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        out.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn tier0_modules_do_not_import_sql_execution_layers() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut files = Vec::new();
    for root in TIER0_ROOTS {
        let full_root = Path::new(manifest_dir).join(root);
        collect_rs_files(&full_root, &mut files);
        // `src/schema.rs` also has a sibling `src/schema/` directory of
        // submodules; same for btree/pager/record — cover those too.
        if let Some(stem) = full_root.file_stem() {
            let sibling_dir = full_root.with_file_name(stem);
            if sibling_dir.is_dir() {
                collect_rs_files(&sibling_dir, &mut files);
            }
        }
    }

    assert!(
        !files.is_empty(),
        "no Tier 0 source files found — check TIER0_ROOTS paths"
    );

    let mut violations = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        for pat in FORBIDDEN {
            if src.contains(pat) {
                violations.push(format!("{}: contains `{pat}`", file.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Tier 0 layer isolation violated (spec 001-architecture Requirement 1):\n{}",
        violations.join("\n")
    );
}
