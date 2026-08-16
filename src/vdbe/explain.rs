//! `EXPLAIN` output rendering (spec 009, Requirement 10): one row per
//! instruction — `addr`, `opcode`, `p1`, `p2`, `p3`, `p4` (rendered in
//! its display form, not its raw bytes), `p5`, `comment`. Stable enough
//! to feed parity #72's planned VM-diff dimension (instruction-by-
//! instruction comparison against the pinned oracle's own `EXPLAIN`).

use crate::vdbe::program::{Opcode, Program, SortKeyColumn, P4};

/// One rendered `EXPLAIN` row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainRow {
    pub addr: usize,
    pub opcode: &'static str,
    pub p1: i32,
    pub p2: i32,
    pub p3: i32,
    pub p4: String,
    pub p5: u16,
    pub comment: String,
}

/// Renders every instruction in `program` as one [`ExplainRow`], in
/// program order.
pub fn explain(program: &Program) -> Vec<ExplainRow> {
    (0..program.len())
        .filter_map(|addr| {
            let instr = program.get(addr)?;
            Some(ExplainRow {
                addr,
                opcode: opcode_name(instr.opcode),
                p1: instr.p1,
                p2: instr.p2,
                p3: instr.p3,
                p4: render_p4(&instr.p4),
                p5: instr.p5,
                comment: comment_for(instr.opcode, instr.p1, instr.p2, instr.p3),
            })
        })
        .collect()
}

fn render_p4(p4: &P4) -> String {
    match p4 {
        P4::None => String::new(),
        P4::Int(i) => i.to_string(),
        P4::Str(s) => s.clone(),
        P4::CollSeq {
            collation,
            affinity,
        } => {
            format!("{}-{affinity}", collation_name(*collation))
        }
        P4::SortKey(cols) => render_sort_key(cols),
    }
}

fn collation_name(collation: crate::vdbe::Collation) -> &'static str {
    match collation {
        crate::vdbe::Collation::Binary => "BINARY",
        crate::vdbe::Collation::NoCase => "NOCASE",
        crate::vdbe::Collation::RTrim => "RTRIM",
    }
}

/// Renders a sort-key descriptor as `k(N,dir,dir,...)`, mirroring the
/// harvested `"k(2,-B,B)"` shape (`program.rs`'s own doc example): `N`
/// key columns, each a direction sign (`-` for descending, none for
/// ascending) followed by the collation's first letter.
fn render_sort_key(cols: &[SortKeyColumn]) -> String {
    let mut parts = Vec::with_capacity(cols.len());
    for col in cols {
        let sign = if col.descending { "-" } else { "" };
        let coll = match col.collation {
            crate::vdbe::Collation::Binary => "B",
            crate::vdbe::Collation::NoCase => "N",
            crate::vdbe::Collation::RTrim => "R",
        };
        parts.push(format!("{sign}{coll}"));
    }
    format!("k({},{})", cols.len(), parts.join(","))
}

fn opcode_name(opcode: Opcode) -> &'static str {
    match opcode {
        Opcode::Init => "Init",
        Opcode::Goto => "Goto",
        Opcode::Once => "Once",
        Opcode::BeginSubrtn => "BeginSubrtn",
        Opcode::Return => "Return",
        Opcode::Halt => "Halt",
        Opcode::Transaction => "Transaction",
        Opcode::IfNot => "IfNot",
        Opcode::IfNotZero => "IfNotZero",
        Opcode::IfPos => "IfPos",
        Opcode::DecrJumpZero => "DecrJumpZero",
        Opcode::IsNull => "IsNull",
        Opcode::NotNull => "NotNull",
        Opcode::MustBeInt => "MustBeInt",
        Opcode::OffsetLimit => "OffsetLimit",
        Opcode::OpenRead => "OpenRead",
        Opcode::OpenEphemeral => "OpenEphemeral",
        Opcode::OpenPseudo => "OpenPseudo",
        Opcode::Rewind => "Rewind",
        Opcode::Last => "Last",
        Opcode::Next => "Next",
        Opcode::Column => "Column",
        Opcode::Rowid => "Rowid",
        Opcode::SeekRowid => "SeekRowid",
        Opcode::NullRow => "NullRow",
        Opcode::Sequence => "Sequence",
        Opcode::Found => "Found",
        Opcode::IdxInsert => "IdxInsert",
        Opcode::IdxLE => "IdxLE",
        Opcode::Delete => "Delete",
        Opcode::Eq => "Eq",
        Opcode::Ge => "Ge",
        Opcode::Gt => "Gt",
        Opcode::Le => "Le",
        Opcode::Lt => "Lt",
        Opcode::RealAffinity => "RealAffinity",
        Opcode::Add => "Add",
        Opcode::Subtract => "Subtract",
        Opcode::Multiply => "Multiply",
        Opcode::Divide => "Divide",
        Opcode::Remainder => "Remainder",
        Opcode::Not => "Not",
        Opcode::BitAnd => "BitAnd",
        Opcode::BitOr => "BitOr",
        Opcode::ShiftLeft => "ShiftLeft",
        Opcode::ShiftRight => "ShiftRight",
        Opcode::BitNot => "BitNot",
        Opcode::Concat => "Concat",
        Opcode::Function => "Function",
        Opcode::Integer => "Integer",
        Opcode::Null => "Null",
        Opcode::String8 => "String8",
        Opcode::MakeRecord => "MakeRecord",
        Opcode::ResultRow => "ResultRow",
        Opcode::SorterOpen => "SorterOpen",
        Opcode::SorterInsert => "SorterInsert",
        Opcode::SorterSort => "SorterSort",
        Opcode::SorterNext => "SorterNext",
        Opcode::SorterData => "SorterData",
        Opcode::Sort => "Sort",
    }
}

/// A short, human-readable annotation of the instruction's effect —
/// not required to be byte-identical to the oracle's own `EXPLAIN`
/// comments (Requirement 10 only mandates the addr/opcode/p1-p5/p4
/// columns support VM-diff), just clear and consistent.
fn comment_for(opcode: Opcode, p1: i32, p2: i32, p3: i32) -> String {
    match opcode {
        Opcode::Init => format!("start at {p2}"),
        Opcode::Goto => format!("goto {p2}"),
        Opcode::OpenRead => format!("cursor {p1} on root page {p2}"),
        Opcode::OpenEphemeral => format!("cursor {p1} ephemeral"),
        Opcode::OpenPseudo => format!("cursor {p1} pseudo, reads r[{p2}]"),
        Opcode::Rewind => format!("cursor {p1} rewind, jump {p2} if empty"),
        Opcode::Last => format!("cursor {p1} to last row, jump {p2} if empty"),
        Opcode::Next => format!("cursor {p1} next, jump {p2} if row found"),
        Opcode::Column => format!("r[{p3}] = cursor {p1} column {p2}"),
        Opcode::Rowid => format!("r[{p2}] = cursor {p1} rowid"),
        Opcode::ResultRow => format!("output r[{p1}..{p1}+{p2}]"),
        Opcode::Eq => format!("if r[{p1}]=r[{p3}] goto {p2}"),
        Opcode::Ge => format!("if r[{p1}]>=r[{p3}] goto {p2}"),
        Opcode::Gt => format!("if r[{p1}]>r[{p3}] goto {p2}"),
        Opcode::Le => format!("if r[{p1}]<=r[{p3}] goto {p2}"),
        Opcode::Lt => format!("if r[{p1}]<r[{p3}] goto {p2}"),
        Opcode::Add => format!("r[{p3}] = r[{p1}] + r[{p2}]"),
        Opcode::Subtract => format!("r[{p3}] = r[{p2}] - r[{p1}]"),
        Opcode::Multiply => format!("r[{p3}] = r[{p1}] * r[{p2}]"),
        Opcode::Divide => format!("r[{p3}] = r[{p2}] / r[{p1}]"),
        Opcode::Remainder => format!("r[{p3}] = r[{p2}] % r[{p1}]"),
        Opcode::Function => format!("r[{p3}] = func(r[{p2}..])"),
        Opcode::Integer => format!("r[{p2}] = {p1}"),
        Opcode::String8 => format!("r[{p2}] = <string>"),
        Opcode::Halt => "halt".to_string(),
        Opcode::SorterOpen => format!("cursor {p1} sorter open"),
        Opcode::SorterInsert => format!("cursor {p1} sorter insert r[{p2}]"),
        Opcode::SorterSort | Opcode::Sort => format!("cursor {p1} sort, jump {p2} if empty"),
        Opcode::SorterNext => format!("cursor {p1} sorter next, jump {p2} if row found"),
        Opcode::SorterData => format!("r[{p2}] = cursor {p1} sorted row"),
        Opcode::Found => format!("cursor {p1} found key at r[{p3}..], jump {p2}"),
        Opcode::IdxInsert => format!("cursor {p1} insert key r[{p2}..]"),
        _ => String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vdbe::program::Instruction;

    #[test]
    fn explain_renders_one_row_per_instruction() {
        let program = Program::new(vec![
            Instruction::new(Opcode::Init, 0, 1, 0),
            Instruction::new(Opcode::Halt, 0, 0, 0),
        ]);
        let rows = explain(&program);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].addr, 0);
        assert_eq!(rows[0].opcode, "Init");
        assert_eq!(rows[1].addr, 1);
        assert_eq!(rows[1].opcode, "Halt");
    }

    #[test]
    fn p4_renders_display_form_not_debug_bytes() {
        let program = Program::new(vec![Instruction::with_p4(
            Opcode::String8,
            0,
            1,
            0,
            P4::Str("g%".to_string()),
        )]);
        let rows = explain(&program);
        assert_eq!(rows[0].p4, "g%");
    }
}
