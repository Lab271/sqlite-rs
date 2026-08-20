//! Aggregate accumulator state for `AggStep`/`AggFinal` (spec 009,
//! Requirement 12, #241). Mirrors `functions.rs`'s name-keyed registry
//! shape, but an aggregate additionally threads a mutable [`AggState`]
//! accumulator across repeated `AggStep` calls before `AggFinal` reads
//! it once (SQLite's own `sqlite3_aggregate_context()` role, here
//! modeled as a plain enum rather than an opaque blob since every
//! accumulator shape is known up front).
//!
//! `count`/`sum` are implemented here to prove the opcode mechanism
//! end-to-end; `avg`/`min`/`max` are #242's scope, added to this same
//! registry rather than a new mechanism.

use crate::record::Value;
use crate::vdbe::functions::FunctionError;

/// One aggregate's running accumulator state, addressed by
/// `Vm::agg_context`/`Vm::set_agg_context` the same way a [`CursorSlot`](crate::vdbe::cursor::CursorSlot)
/// is addressed by `Vm::cursor` — a disjoint slot table keyed by an
/// opcode operand (`AggStep`/`AggFinal`'s `P1`).
#[derive(Debug, Clone, PartialEq)]
pub enum AggState {
    /// `count(x)` skips NULL args; `count(*)` (zero args) counts every
    /// row regardless of value.
    Count(i64),
    /// `sum(x)`: integer inputs accumulate exactly in `int_total` until
    /// a REAL input is seen, after which the whole running total moves
    /// to `real_total` — mirrors SQLite's own sum() promotion rule (an
    /// all-integer sum stays exact; one REAL input makes the result
    /// REAL).
    Sum {
        int_total: i128,
        real_total: f64,
        saw_real: bool,
        saw_any: bool,
    },
}

impl AggState {
    fn initial(name: &str) -> Result<Self, FunctionError> {
        match name.to_ascii_lowercase().as_str() {
            "count" => Ok(AggState::Count(0)),
            "sum" => Ok(AggState::Sum {
                int_total: 0,
                real_total: 0.0,
                saw_real: false,
                saw_any: false,
            }),
            other => Err(FunctionError::Unknown {
                name: other.to_string(),
                arity: 1,
            }),
        }
    }
}

/// `AggStep`: folds `args` into `state` (creating a fresh accumulator
/// via `name` on the first call for this context), returning the
/// updated state. `name` is only consulted to build the *initial*
/// state — `state.is_some()` calls ignore it, matching how a real
/// VDBE program always steps the same aggregate for a given context
/// slot.
pub fn step(
    name: &str,
    state: Option<AggState>,
    args: &[Value],
) -> Result<AggState, FunctionError> {
    let mut state = match state {
        Some(s) => s,
        None => AggState::initial(name)?,
    };
    match &mut state {
        AggState::Count(n) => {
            if args.first().is_none_or(|v| !matches!(v, Value::Null)) {
                *n = n.saturating_add(1);
            }
        }
        AggState::Sum {
            int_total,
            real_total,
            saw_real,
            saw_any,
        } => match args.first() {
            None | Some(Value::Null) => {}
            Some(Value::Integer(i)) => {
                *saw_any = true;
                *int_total = int_total.saturating_add(i128::from(*i));
            }
            Some(Value::Real(r)) => {
                *saw_any = true;
                *saw_real = true;
                *real_total += *r;
            }
            // Text/blob inputs to sum() are non-numeric here (no
            // numeric-text coercion) — out of scope for this ticket's
            // minimal count/sum proof; #242 can extend this match arm
            // if full sum() semantics are needed before then.
            Some(Value::Text(_) | Value::Blob(_)) => {}
        },
    }
    Ok(state)
}

/// `AggFinal`: produces the result for a context that has seen zero or
/// more `AggStep` calls. `state = None` means zero rows were
/// aggregated (an empty group, or a context slot never stepped) —
/// `count` finalizes to 0, `sum` to NULL, matching SQLite's own
/// zero-row aggregate results.
pub fn finalize(name: &str, state: Option<&AggState>) -> Result<Value, FunctionError> {
    match state {
        None => match name.to_ascii_lowercase().as_str() {
            "count" => Ok(Value::Integer(0)),
            "sum" => Ok(Value::Null),
            other => Err(FunctionError::Unknown {
                name: other.to_string(),
                arity: 1,
            }),
        },
        Some(AggState::Count(n)) => Ok(Value::Integer(*n)),
        Some(AggState::Sum {
            int_total,
            real_total,
            saw_real,
            saw_any,
        }) => {
            if !saw_any {
                return Ok(Value::Null);
            }
            #[allow(clippy::cast_precision_loss)]
            if *saw_real {
                Ok(Value::Real(*real_total + *int_total as f64))
            } else {
                i64::try_from(*int_total)
                    .map(Value::Integer)
                    .map_err(|_| FunctionError::IntegerOverflow)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn count_star_counts_every_row_regardless_of_args() {
        let mut state = None;
        for _ in 0..4 {
            state = Some(step("count", state, &[]).unwrap());
        }
        assert_eq!(
            finalize("count", state.as_ref()).unwrap(),
            Value::Integer(4)
        );
    }

    #[test]
    fn count_x_skips_null_args() {
        let mut state = None;
        for v in [Value::Integer(1), Value::Null, Value::Integer(2)] {
            state = Some(step("count", state, &[v]).unwrap());
        }
        assert_eq!(
            finalize("count", state.as_ref()).unwrap(),
            Value::Integer(2)
        );
    }

    #[test]
    fn count_with_zero_rows_finalizes_to_zero() {
        assert_eq!(finalize("count", None).unwrap(), Value::Integer(0));
    }

    #[test]
    fn sum_of_all_integers_stays_exact_integer() {
        let mut state = None;
        for v in [1i64, 2, 3] {
            state = Some(step("sum", state, &[Value::Integer(v)]).unwrap());
        }
        assert_eq!(finalize("sum", state.as_ref()).unwrap(), Value::Integer(6));
    }

    #[test]
    fn sum_promotes_to_real_once_any_real_input_seen() {
        let mut state = None;
        state = Some(step("sum", state, &[Value::Integer(1)]).unwrap());
        state = Some(step("sum", state, &[Value::Real(0.5)]).unwrap());
        assert_eq!(finalize("sum", state.as_ref()).unwrap(), Value::Real(1.5));
    }

    #[test]
    fn sum_skips_null_and_finalizes_null_on_zero_rows() {
        assert_eq!(finalize("sum", None).unwrap(), Value::Null);
        let state = step("sum", None, &[Value::Null]).unwrap();
        assert_eq!(finalize("sum", Some(&state)).unwrap(), Value::Null);
    }

    #[test]
    fn unknown_aggregate_name_errors() {
        assert!(matches!(
            step("median", None, &[Value::Integer(1)]),
            Err(FunctionError::Unknown { .. })
        ));
    }
}
