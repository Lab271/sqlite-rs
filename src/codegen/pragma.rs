//! `Pragma` AST -> `Program` compilation: `journal_mode` (#388) and
//! `integrity_check`/`quick_check` (#540, #541). Mirrors
//! `src/codegen/transaction.rs`'s shape: one control opcode per pragma,
//! operands carrying whatever the executor needs.

use crate::codegen::Emitter;
use crate::parser::ast::{Pragma, PragmaJournalMode};
use crate::vdbe::{Instruction, Opcode, Program, JOURNAL_MODE_DELETE, JOURNAL_MODE_WAL};

/// Compiles a `PRAGMA` statement into an `Init -> <op> -> Halt` program.
/// `journal_mode` emits `SetJournalMode` (`P1` carries the target mode,
/// no result rows); `integrity_check`/`quick_check` emit
/// `IntegrityCheck` (`P1` = 1 for the `quick_check` reduced pass, 0 for
/// the full `integrity_check`), which produces a result set of `TEXT`
/// rows.
pub fn compile_pragma(pragma: &Pragma) -> Program {
    let mut em = Emitter::new();
    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    match pragma {
        Pragma::JournalMode { journal_mode, .. } => {
            let mode = match journal_mode {
                PragmaJournalMode::Wal => JOURNAL_MODE_WAL,
                PragmaJournalMode::Delete => JOURNAL_MODE_DELETE,
            };
            em.emit(Instruction::new(Opcode::SetJournalMode, mode, 0, 0));
        }
        Pragma::IntegrityCheck { quick, .. } => {
            em.emit(Instruction::new(
                Opcode::IntegrityCheck,
                i32::from(*quick),
                0,
                0,
            ));
        }
    }
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    em.finish()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::parser::error::{parse_pragma, ParseOutcome};

    fn opcodes(program: &Program) -> Vec<Opcode> {
        program.instructions.iter().map(|i| i.opcode).collect()
    }

    #[test]
    fn journal_mode_wal_compiles_to_init_set_journal_mode_halt() {
        let pragma = match parse_pragma("PRAGMA journal_mode = WAL") {
            ParseOutcome::Accepted(p) => p,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_pragma(&pragma);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::SetJournalMode, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p1, JOURNAL_MODE_WAL);
    }

    #[test]
    fn journal_mode_delete_compiles_p1_to_delete() {
        let pragma = match parse_pragma("PRAGMA journal_mode = DELETE") {
            ParseOutcome::Accepted(p) => p,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_pragma(&pragma);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::SetJournalMode, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p1, JOURNAL_MODE_DELETE);
    }

    #[test]
    fn integrity_check_compiles_p1_zero() {
        let pragma = match parse_pragma("PRAGMA integrity_check") {
            ParseOutcome::Accepted(p) => p,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_pragma(&pragma);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::IntegrityCheck, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p1, 0);
    }

    #[test]
    fn quick_check_compiles_p1_one() {
        let pragma = match parse_pragma("PRAGMA quick_check") {
            ParseOutcome::Accepted(p) => p,
            other => panic!("expected Accepted, got {other:?}"),
        };
        let program = compile_pragma(&pragma);
        assert_eq!(
            opcodes(&program),
            vec![Opcode::Init, Opcode::IntegrityCheck, Opcode::Halt]
        );
        assert_eq!(program.instructions[1].p1, 1);
    }
}
