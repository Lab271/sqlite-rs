//! The parser stack the lemon-rs driver template (`lempar.rs`) expects.
//!
//! The template hard-codes `use crate::parser::stack::Stack;` and drives it with
//! *relative* indices: `stack[0]` is the top of the stack, `stack[-2]` is two
//! entries below it, and `yyidx_shift()` pops/pushes by an offset. None of this
//! ships with lemon-rs's template, so it has to be written by hand — this module
//! is the glue that makes the generated parser compile.
//!
//! Invariant: `vec.len() == yyidx + 1` between operations, i.e. `yyidx` is always
//! the index of the top entry.

use std::ops::{Index, IndexMut};

pub struct Stack<T> {
    pub vec: Vec<T>,
    pub yyidx: usize,
}

impl<T> Stack<T> {
    pub fn with_capacity(n: usize) -> Self {
        Stack {
            vec: Vec::with_capacity(n),
            yyidx: 0,
        }
    }

    pub fn push(&mut self, entry: T) {
        self.vec.push(entry);
    }

    pub fn pop(&mut self) -> T {
        let top = self.vec.pop().expect("parser stack underflow");
        self.yyidx = self.yyidx.saturating_sub(1);
        top
    }

    /// Move the top index by `delta` (negative = pop). A shrink truncates the
    /// backing vector so that the top entry is always the last element.
    pub fn yyidx_shift(&mut self, delta: i8) {
        let idx = self.yyidx as isize + delta as isize;
        debug_assert!(idx >= 0, "parser stack underflow");
        self.yyidx = idx as usize;
        if self.vec.len() > self.yyidx + 1 {
            self.vec.truncate(self.yyidx + 1);
        }
    }

    fn offset(&self, i: i8) -> usize {
        let idx = self.yyidx as isize + i as isize;
        debug_assert!(idx >= 0 && (idx as usize) < self.vec.len(), "stack index out of range");
        idx as usize
    }
}

impl<T: Default> Stack<T> {
    /// Make room for one more entry (used for rules with an empty RHS, whose
    /// semantic action writes one slot *above* the current top).
    pub fn grow(&mut self) {
        self.vec.push(T::default());
    }

    /// Take the semantic value out of a stack slot, leaving a default behind.
    /// Generated reduce actions call this for every labelled RHS symbol.
    pub fn yy_move(&mut self, i: i8) -> T {
        std::mem::take(&mut self[i])
    }
}

impl<T> Index<i8> for Stack<T> {
    type Output = T;
    fn index(&self, i: i8) -> &T {
        let idx = self.offset(i);
        &self.vec[idx]
    }
}

impl<T> IndexMut<i8> for Stack<T> {
    fn index_mut(&mut self, i: i8) -> &mut T {
        let idx = self.offset(i);
        &mut self.vec[idx]
    }
}
