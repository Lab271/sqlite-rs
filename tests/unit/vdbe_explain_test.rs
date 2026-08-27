// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! `EXPLAIN` output format acceptance (spec 009, Requirement 10):
//! named scenarios for the compiled-program printer
//! (`src/vdbe/explain.rs`).

use sqlite_rs::vdbe::{explain, Collation, Instruction, Opcode, Program, P4};

#[test]
fn explain_renders_one_row_per_instruction_all_columns() {
    let program = Program::new(vec![
        Instruction::new(Opcode::Init, 0, 1, 0),
        Instruction::new(Opcode::OpenRead, 0, 2, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = explain(&program);
    assert_eq!(rows.len(), 3);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.addr, i);
    }
    // p4 empty/blank when absent, rather than a placeholder value.
    assert_eq!(rows[0].p4, "");
    assert_eq!(rows[0].opcode, "Init");
    assert_eq!(rows[1].opcode, "OpenRead");
    assert_eq!(rows[2].opcode, "Halt");
}

#[test]
fn explain_p4_column_matches_oracle_display_form() {
    let program = Program::new(vec![
        Instruction::with_p4(
            Opcode::Ge,
            1,
            2,
            3,
            P4::CollSeq {
                collation: Collation::Binary,
                affinity: 8,
            },
        ),
        Instruction::with_p4(Opcode::String8, 0, 1, 0, P4::Str("g%".to_string())),
    ]);
    let rows = explain(&program);
    assert_eq!(rows[0].p4, "BINARY-8");
    assert_eq!(rows[1].p4, "g%");
}
