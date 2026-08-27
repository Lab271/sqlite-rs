// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #464 (spec 011): an in-memory Bloom filter, addressed by `Vm`'s
//! `filters` slot table (a disjoint address space from `cursors`,
//! same shape as `agg_contexts` — see `Vm::filter`/`Vm::filter_add`)
//! and driven by [`Opcode::FilterAdd`](crate::vdbe::program::Opcode::FilterAdd)/
//! [`Opcode::Filter`](crate::vdbe::program::Opcode::Filter).
//!
//! **No-false-negative contract:** [`BloomFilterState::insert`]/
//! [`BloomFilterState::might_contain`] only ever hash a
//! [`Value::Integer`] key — any other `Value` inserted instead poisons
//! the whole slot (`has_non_integer` latches `true` forever), and once
//! poisoned (or when the *probe* itself isn't an integer)
//! `might_contain` unconditionally reports `true` ("maybe present").
//! This is deliberately conservative: a real `Value` equality (spec
//! 008) can hold between an `Integer` and a numerically-equal `Real`,
//! or coerce a `Text`/`Blob` — reproducing every one of those coercion
//! rules inside the hash would risk a false *negative* (silently
//! dropping a matching join row), so instead the filter simply never
//! claims "definitely absent" for anything but a pure integer-keyed
//! set. That keeps it a pure optimization: `Filter` skipping a scan
//! can only ever be *correct*, never load-bearing for correctness.
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::record::Value;

/// Fixed number of bit positions probed per key (double hashing over
/// two independent 64-bit hashes) — not tuned per capacity, since this
/// filter only ever needs to bound its false-positive *rate*, not
/// hold any correctness weight (see the module doc's contract).
const HASH_COUNT: u32 = 4;

/// Bits per expected item, chosen for a single-digit-percent false
/// positive rate at [`HASH_COUNT`] hash probes (the classic `m/n ≈ 10`
/// rule of thumb for `k ≈ 7`; `k = 4` here trades a slightly higher
/// false-positive rate for a simpler fixed hash count, still a pure
/// throughput knob per the module doc's contract).
const BITS_PER_ITEM: u64 = 10;

#[derive(Debug)]
pub(crate) struct BloomFilterState {
    bits: Vec<bool>,
    /// Latched permanently by [`Self::insert`] the first time a
    /// non-integer `Value` is inserted — see the module doc's
    /// no-false-negative contract.
    has_non_integer: bool,
}

impl BloomFilterState {
    /// `expected_items` sizes the bit array (at least 64 bits, so a
    /// tiny/zero hint still gets a usable filter) — never a
    /// correctness bound, only a false-positive-rate one.
    pub(crate) fn new(expected_items: u64) -> Self {
        let num_bits = expected_items
            .saturating_mul(BITS_PER_ITEM)
            .clamp(64, 1 << 24);
        BloomFilterState {
            bits: vec![false; usize::try_from(num_bits).unwrap_or(64)],
            has_non_integer: false,
        }
    }

    fn positions(&self, key: i64) -> impl Iterator<Item = usize> + '_ {
        let h1 = hash64(&(key, 1u8));
        let h2 = hash64(&(key, 2u8)).wrapping_or_odd();
        let len = self.bits.len().max(1) as u64;
        (0..HASH_COUNT).map(move |i| {
            let combined = h1.wrapping_add(u64::from(i).wrapping_mul(h2));
            usize::try_from(combined.checked_rem(len).unwrap_or(0)).unwrap_or(0)
        })
    }

    pub(crate) fn insert(&mut self, value: &Value) {
        let Value::Integer(key) = value else {
            self.has_non_integer = true;
            return;
        };
        let positions: Vec<usize> = self.positions(*key).collect();
        for pos in positions {
            if let Some(bit) = self.bits.get_mut(pos) {
                *bit = true;
            }
        }
    }

    /// `true` means "maybe present" (the safe default whenever this
    /// filter can't rule the value out); `false` is the only claim
    /// that authorizes a caller to skip a scan, and only holds when
    /// every bit this key would have set is already set.
    pub(crate) fn might_contain(&self, value: &Value) -> bool {
        if self.has_non_integer {
            return true;
        }
        let Value::Integer(key) = value else {
            return true;
        };
        self.positions(*key)
            .all(|pos| self.bits.get(pos).copied().unwrap_or(true))
    }
}

trait WrapOdd {
    fn wrapping_or_odd(self) -> Self;
}
impl WrapOdd for u64 {
    /// Forces the second double-hashing multiplier odd (never 0, never
    /// even) so it can't degenerate into probing the same bit
    /// [`HASH_COUNT`] times over — a correctness-neutral distribution
    /// quality fix, not a safety requirement (see the module doc).
    fn wrapping_or_odd(self) -> Self {
        self | 1
    }
}

fn hash64<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_integer_is_always_reported_present() {
        let mut filter = BloomFilterState::new(100);
        for v in [1_i64, 2, 3, 1000, -5] {
            filter.insert(&Value::Integer(v));
        }
        for v in [1_i64, 2, 3, 1000, -5] {
            assert!(filter.might_contain(&Value::Integer(v)));
        }
    }

    #[test]
    fn a_value_far_outside_the_inserted_set_is_usually_absent() {
        let mut filter = BloomFilterState::new(1000);
        for v in 0..1000_i64 {
            filter.insert(&Value::Integer(v));
        }
        let absent = (2_000_000..2_001_000_i64)
            .filter(|v| !filter.might_contain(&Value::Integer(*v)))
            .count();
        assert!(
            absent > 900,
            "expected most probes to miss, got {absent}/1000"
        );
    }

    #[test]
    fn non_integer_insert_poisons_the_slot_to_always_maybe() {
        let mut filter = BloomFilterState::new(10);
        filter.insert(&Value::Text("x".to_string().into()));
        assert!(filter.might_contain(&Value::Integer(42)));
        assert!(filter.might_contain(&Value::Text("anything".to_string().into())));
    }

    #[test]
    fn non_integer_probe_is_always_maybe_present() {
        let mut filter = BloomFilterState::new(10);
        filter.insert(&Value::Integer(1));
        assert!(filter.might_contain(&Value::Real(1.0)));
        assert!(filter.might_contain(&Value::Null));
    }
}
