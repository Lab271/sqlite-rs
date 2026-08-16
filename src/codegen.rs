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

    /// Allocates `count` contiguous registers, returning the first.
    pub(crate) fn alloc_range(&mut self, count: usize) -> i32 {
        let first = self.next;
        self.next = self
            .next
            .saturating_add(i32::try_from(count).unwrap_or(i32::MAX));
        first
    }
}

pub(crate) fn p4_coll_seq(collation: crate::vdbe::Collation) -> P4 {
    let affinity: u8 = 8; // Matches the harvested "BINARY-8" P4 rendering (program.rs's own doc example).
    P4::CollSeq {
        collation,
        affinity,
    }
}
