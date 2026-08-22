//! `Begin`/`Commit`/`Rollback` AST -> `Program` compilation (#360). Each
//! compiles to a single control opcode, exactly like the DDL statements
//! in the sibling `ddl` module: `Transaction` for `BEGIN`, `AutoCommit`
//! for `COMMIT`/`ROLLBACK` (`P2` = 1/0 respectively, stock SQLite's
//! convention). `TransactionMode` (DEFERRED/IMMEDIATE/EXCLUSIVE) governs
//! lock acquisition, not the pager's rollback-journal write path, so it
//! carries no weight here yet — V5 Slim's lock-state work (#357) is
//! where it will matter.

use crate::codegen::Emitter;
use crate::parser::ast::{Begin, Commit, Rollback};
use crate::vdbe::{Instruction, Opcode, Program};

pub fn compile_begin(_begin: &Begin) -> Program {
    let mut em = Emitter::new();
    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(Opcode::Transaction, 0, 0, 0));
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    em.finish()
}

pub fn compile_commit(_commit: &Commit) -> Program {
    compile_auto_commit(1)
}

pub fn compile_rollback(_rollback: &Rollback) -> Program {
    compile_auto_commit(0)
}

fn compile_auto_commit(commit: i32) -> Program {
    let mut em = Emitter::new();
    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(Opcode::AutoCommit, 0, commit, 0));
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    em.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::parser::error::{parse_begin, parse_commit, parse_rollback, ParseOutcome};

    fn opcodes(program: &Program) -> Vec<Opcode> {
        program.instructions.iter().map(|i| i.opcode).collect()
    }

    #[test]
    fn begin_compiles_to_init_transaction_halt() {
        let begin = match parse_begin("BEGIN") {
            ParseOutcome::Accepted(b) => b,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_begin(&begin);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::Transaction, Opcode::Halt]
        );
    }

    #[test]
    fn commit_compiles_to_auto_commit_with_p2_one() {
        let commit = match parse_commit("COMMIT") {
            ParseOutcome::Accepted(c) => c,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_commit(&commit);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::AutoCommit, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p2, 1);
    }

    #[test]
    fn rollback_compiles_to_auto_commit_with_p2_zero() {
        let rollback = match parse_rollback("ROLLBACK") {
            ParseOutcome::Accepted(r) => r,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_rollback(&rollback);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::AutoCommit, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p2, 0);
    }
}
