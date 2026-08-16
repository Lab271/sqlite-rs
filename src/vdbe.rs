//! The value-semantics kernel beneath the future VDBE opcodes: type
//! affinity, cross-type comparison order, collations, NULL/three-valued
//! logic, and numeric coercion. Pure functions on `Value` — no expression
//! evaluation, no parser coupling. See spec 008.

mod affinity;
mod arithmetic;
mod coerce;
mod collation;
mod compare;
mod control;
mod exec;
mod functions;
mod program;
mod result;
mod value;

pub use affinity::{affinity_of, apply_affinity, Affinity};
pub use coerce::{
    cast_to_integer, checked_add, checked_div, checked_mul, checked_rem, checked_sub,
    coerce_text_to_numeric,
};
pub use collation::{compare_text, Collation};
pub use compare::compare;
pub use exec::{execute, ExecError, Step, Vm};
pub use functions::{call as call_function, FunctionError};
pub use program::{Instruction, Opcode, Program, P4};
pub use value::{and, is, is_not, not, or, sql_eq, sql_lt};
