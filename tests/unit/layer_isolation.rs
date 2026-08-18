//! Enforces spec 001-architecture Requirement 1 (Layer Isolation), both
//! scenarios:
//! - "B-tree does not know SQL": Tier 0 core modules (schema, btree,
//!   pager, record, header) must never depend on the SQL-execution
//!   layers (parser, codegen, vdbe).
//! - "VDBE does not know file format": vdbe/codegen must reach storage
//!   only through the B-tree API, never `crate::pager` directly.
//!
//! Without this, both boundaries hold only by convention — a stray
//! `use crate::parser` in `schema.rs`, or a stray `use crate::pager` in
//! `vdbe/exec.rs`, would compile silently.

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

const TIER1_2_ROOTS: &[&str] = &["src/vdbe.rs", "src/codegen.rs"];

const STORAGE_BYPASS: &[&str] = &["use crate::pager"];

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

/// Collects every `.rs` file under each root, plus each root's sibling
/// submodule directory of the same name (e.g. `src/schema.rs` +
/// `src/schema/`), and returns the list.
fn collect_module_trees(roots: &[&str]) -> Vec<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let mut files = Vec::new();
    for root in roots {
        let full_root = Path::new(manifest_dir).join(root);
        collect_rs_files(&full_root, &mut files);
        if let Some(stem) = full_root.file_stem() {
            let sibling_dir = full_root.with_file_name(stem);
            if sibling_dir.is_dir() {
                collect_rs_files(&sibling_dir, &mut files);
            }
        }
    }
    files
}

fn find_violations(files: &[PathBuf], forbidden: &[&str]) -> Vec<String> {
    let mut violations = Vec::new();
    for file in files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        for pat in forbidden {
            if src.contains(pat) {
                violations.push(format!("{}: contains `{pat}`", file.display()));
            }
        }
    }
    violations
}

#[test]
fn tier0_modules_do_not_import_sql_execution_layers() {
    let files = collect_module_trees(TIER0_ROOTS);
    assert!(
        !files.is_empty(),
        "no Tier 0 source files found — check TIER0_ROOTS paths"
    );

    let violations = find_violations(&files, FORBIDDEN);
    assert!(
        violations.is_empty(),
        "Tier 0 layer isolation violated (spec 001-architecture Requirement 1, \
         \"B-tree does not know SQL\"):\n{}",
        violations.join("\n")
    );
}

#[test]
fn vdbe_and_codegen_do_not_bypass_btree_for_storage_access() {
    let files = collect_module_trees(TIER1_2_ROOTS);
    assert!(
        !files.is_empty(),
        "no vdbe/codegen source files found — check TIER1_2_ROOTS paths"
    );

    let violations = find_violations(&files, STORAGE_BYPASS);
    assert!(
        violations.is_empty(),
        "VDBE/codegen layer isolation violated (spec 001-architecture Requirement 1, \
         \"VDBE does not know file format\") — storage access must go through \
         the B-tree API, not crate::pager directly:\n{}",
        violations.join("\n")
    );
}
