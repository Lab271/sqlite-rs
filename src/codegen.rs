//! SQL codegen: compiles a parsed [`crate::parser::ast::Select`] into a
//! [`crate::vdbe::Program`] (spec 009, Requirements 7, 10, 11 — the
//! convergence ticket #91, needing #89's VDBE core and #90's cursor/
//! sorter/ephemeral opcodes). Expressions compile to jump-based control
//! flow, never an intermediate boolean register (Requirement 11).

pub mod expr;
pub mod select;

pub use select::{compile_select, CodegenError};

use std::collections::HashMap;

use crate::vdbe::{Instruction, Opcode, Program, P4};

/// A not-yet-resolved jump target, placed later via [`Emitter::place`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Label(usize);

/// Where a boolean condition's true/false outcome continues: either an
/// explicit jump target, or "fall through to the next emitted
/// instruction" — the classic jumping-code-generation technique (Aho
/// et al.), used throughout `expr.rs` so AND/OR/CASE compose without
/// materializing an intermediate boolean register (Requirement 11).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Target {
    Jump(Label),
    Fallthrough,
}

/// Where a condition's *unknown* (SQL NULL) outcome continues — SQLite's
/// own `jumpIfNull` flag (`sqlite3ExprIfTrue`/`sqlite3ExprIfFalse`),
/// carried as the third field of [`CondTargets`].
///
/// It names one of the other two targets rather than being a third
/// [`Target`] of its own, on purpose. NULL is never an independent
/// continuation in practice: `WHERE` folds it into false (a NULL
/// predicate excludes the row), and `NOT` must leave it pinned to the
/// same address while swapping which of the two targets that address
/// is — which [`CondTargets::negate`] does in one line. An absolute
/// third label would have to be rewritten every time `AND`/`OR`
/// synthesize a fresh false/true label, and, worse, would be
/// unrepresentable for `AND`/`OR` at all: `NULL AND false` is *false*,
/// so a genuinely independent unknown continuation could not be taken
/// until the second operand had been evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NullTarget {
    /// NULL continues where [`CondTargets::on_true`] does.
    True,
    /// NULL continues where [`CondTargets::on_false`] does — what
    /// `WHERE`, `CASE WHEN`, and every other boolean consumer in V2
    /// wants.
    False,
}

/// The full jump-mode contract: where a condition's true, false, and
/// unknown outcomes each continue. Bundled rather than passed as three
/// parameters because [`negate`](CondTargets::negate) has to move all
/// three together — swapping true and false without flipping
/// `on_null` is precisely the #134 bug.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CondTargets {
    pub(crate) on_true: Target,
    pub(crate) on_false: Target,
    pub(crate) on_null: NullTarget,
}

impl CondTargets {
    /// The setting every boolean consumer in V2 wants: unknown joins
    /// false.
    pub(crate) fn null_is_false(on_true: Target, on_false: Target) -> Self {
        CondTargets {
            on_true,
            on_false,
            on_null: NullTarget::False,
        }
    }

    /// Unknown joins true — used only to separate "definitely false"
    /// from "unknown" when materializing a condition into a register.
    pub(crate) fn null_is_true(on_true: Target, on_false: Target) -> Self {
        CondTargets {
            on_true,
            on_false,
            on_null: NullTarget::True,
        }
    }

    /// The contract for the operand of a `NOT`: true and false trade
    /// places, and `on_null` flips so the unknown outcome still names
    /// the address it named before the swap.
    pub(crate) fn negate(self) -> Self {
        CondTargets {
            on_true: self.on_false,
            on_false: self.on_true,
            on_null: match self.on_null {
                NullTarget::True => NullTarget::False,
                NullTarget::False => NullTarget::True,
            },
        }
    }

    pub(crate) fn with_true(self, on_true: Target) -> Self {
        CondTargets { on_true, ..self }
    }

    pub(crate) fn with_false(self, on_false: Target) -> Self {
        CondTargets { on_false, ..self }
    }
}

/// Builds a [`Program`] with forward-referenceable jump targets:
/// `new_label`/`place` mark an address, `patch_p2` records a pending
/// fixup (every jump-carrying opcode this ticket emits targets `P2`),
/// and `finish` resolves every pending fixup in one pass.
#[derive(Debug, Default)]
pub(crate) struct Emitter {
    instructions: Vec<Instruction>,
    labels: HashMap<Label, usize>,
    patches: Vec<(usize, Label)>,
    next_label: usize,
}

impl Emitter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn emit(&mut self, instr: Instruction) -> usize {
        self.instructions.push(instr);
        self.instructions.len().saturating_sub(1)
    }

    pub(crate) fn here(&self) -> usize {
        self.instructions.len()
    }

    pub(crate) fn new_label(&mut self) -> Label {
        let label = Label(self.next_label);
        self.next_label = self.next_label.saturating_add(1);
        label
    }

    /// Binds `label` to the current (next-to-be-emitted) address.
    pub(crate) fn place(&mut self, label: Label) {
        self.labels.insert(label, self.here());
    }

    pub(crate) fn patch_p2(&mut self, addr: usize, label: Label) {
        self.patches.push((addr, label));
    }

    /// Overwrites an already-emitted instruction's `P4`, for cases
    /// where the value (e.g. a sort-key descriptor) isn't known until
    /// after later instructions — computing it requires registers that
    /// only get allocated once the code between the placeholder and
    /// the fixup has been emitted — have already been generated.
    pub(crate) fn patch_p4(&mut self, addr: usize, p4: P4) {
        if let Some(instr) = self.instructions.get_mut(addr) {
            instr.p4 = p4;
        }
    }

    /// Resolves every pending patch against its placed label's address,
    /// consuming the emitter into a finished [`Program`].
    pub(crate) fn finish(mut self) -> Program {
        for (addr, label) in &self.patches {
            let Some(&resolved) = self.labels.get(label) else {
                continue; // Every patched label is always placed by construction; skip defensively rather than panic.
            };
            #[allow(clippy::cast_possible_wrap)]
            let target = resolved as i32;
            if let Some(instr) = self.instructions.get_mut(*addr) {
                instr.p2 = target;
            }
        }
        Program::new(self.instructions)
    }

    /// Emits an unconditional jump to `label`, patched once placed.
    pub(crate) fn goto(&mut self, label: Label) {
        let addr = self.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
        self.patch_p2(addr, label);
    }
}

/// A monotonically-increasing register bump allocator — the simplest
/// correct scheme for V2's scope; SQLite's real register allocator
/// reuses freed slots, which this deliberately does not (known
/// simplification, not a TODO to chase further).
#[derive(Debug, Default)]
pub(crate) struct RegAlloc {
    next: i32,
}

impl RegAlloc {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn alloc(&mut self) -> i32 {
        let r = self.next;
        self.next = self.next.saturating_add(1);
        r
    }

    /// The register the next `alloc()` call would hand out, without
    /// allocating it — used to find the highest register a just-compiled
    /// expression touched (its last-allocated register isn't always its
    /// own return value, e.g. `CASE` allocates its destination first).
    pub(crate) fn peek(&self) -> i32 {
        self.next
    }
}

pub(crate) fn p4_coll_seq(
    collation: crate::vdbe::Collation,
    affinity: crate::vdbe::Affinity,
) -> P4 {
    P4::CollSeq {
        collation,
        affinity: affinity.to_p4_byte(),
    }
}
