//! `Pragma` (`journal_mode` only, #388) AST -> `Program` compilation.
//! Mirrors `src/codegen/transaction.rs`'s shape exactly: one control
//! opcode (`SetJournalMode`), `P1` carrying which mode to switch to.
//! Stock SQLite's own `journal_mode` pragma is far broader (MEMORY/OFF/
//! TRUNCATE/PERSIST, plus every other pragma name), but this ticket's
//! grammar carve-out (`.openspec/grammar/sqlite.ebnf` V6) only accepts
//! `WAL`/`DELETE`, so `compile_pragma` never sees anything else.

use crate::codegen::Emitter;
use crate::parser::ast::{Pragma, PragmaJournalMode};
use crate::vdbe::{Instruction, Opcode, Program, JOURNAL_MODE_DELETE, JOURNAL_MODE_WAL};

/// Compiles `PRAGMA journal_mode = WAL|DELETE` into an
/// `Init -> SetJournalMode -> Halt` program, `P1` carrying the target mode.
pub fn compile_pragma(pragma: &Pragma) -> Program {
    let mode = match pragma.journal_mode {
        PragmaJournalMode::Wal => JOURNAL_MODE_WAL,
        PragmaJournalMode::Delete => JOURNAL_MODE_DELETE,
    };

    let mut em = Emitter::new();
    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(Opcode::SetJournalMode, mode, 0, 0));
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
}
