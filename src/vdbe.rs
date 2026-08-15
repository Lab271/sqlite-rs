//! The value-semantics kernel beneath the future VDBE opcodes: type
//! affinity, cross-type comparison order, collations, NULL/three-valued
//! logic, and numeric coercion. Pure functions on `Value` — no expression
//! evaluation, no parser coupling. See spec 008.

mod affinity;
mod coerce;
mod collation;
mod compare;
mod value;

pub use affinity::{affinity_of, apply_affinity, Affinity};
pub use coerce::{cast_to_integer, checked_add, checked_mul, checked_sub, coerce_text_to_numeric};
pub use collation::{compare_text, Collation};
pub use compare::compare;
pub use value::{and, is, is_not, not, or, sql_eq, sql_lt};
