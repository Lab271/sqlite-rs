// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Throwaway tree-walking expression evaluator (spike #59, spec
//! `002-parser` Req 3 / `008-value-semantics`): parser AST -> kernel
//! calls -> `Value`. First real consumer of both the phase-1 parser's
//! `Expr` AST and the phase-2 value-semantics kernel; end-to-end
//! `parse("expr") -> Value`, oracle-diffed by
//! `tests/oracle_diff.rs` against the vectors under
//! `tests/corpus/expr_vectors/`.
//!
//! Disposed at spike close (per epic #56): only `findings.md` and the
//! committed oracle vectors survive; this module does not.

use sqlite_rs::parser::ast::{
    BinaryOp, Expr, ExprKind, FunctionArgs, Literal, ResultColumn, UnaryOp,
};
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::vdbe::{
    affinity_of, and, apply_affinity, call_function, cast_to_integer, checked_add, checked_mul,
    checked_sub, coerce_text_to_numeric, is, is_not, not, or, sql_eq, sql_lt, Affinity, Collation,
    FunctionError,
};

#[derive(Debug)]
pub enum WalkError {
    Parse(String),
    Unsupported(String),
    Function(FunctionError),
}

/// Parses `expr_sql` as a scalar expression (via `SELECT <expr>`, since
/// the parser's own `expr()` entry point is crate-private) and
/// evaluates it through the value-semantics kernel.
pub fn eval_sql_expr(expr_sql: &str) -> Result<Value, WalkError> {
    let sql = format!("SELECT {expr_sql}");
    match parse_select(&sql) {
        ParseOutcome::Accepted(select) => {
            let column =
                select.columns.into_iter().next().ok_or_else(|| {
                    WalkError::Unsupported("empty SELECT column list".to_string())
                })?;
            match column {
                ResultColumn::Expr { expr, .. } => eval(&expr),
                _ => Err(WalkError::Unsupported(
                    "expected a scalar expression column".to_string(),
                )),
            }
        }
        ParseOutcome::Unsupported { message, .. } | ParseOutcome::Invalid { message, .. } => {
            Err(WalkError::Parse(message))
        }
    }
}

/// SQL truthiness in a boolean context (`WHERE`/`CASE`/`AND`/`OR`
/// operands): NULL is unknown, TEXT/REAL are falsy only at exactly
/// zero (checking both the integer and real coercion outcomes — a
/// lesson from #92's `iif()` review, where only the integer outcome
/// was checked and `'0.0'` was wrongly truthy), BLOB is always truthy.
fn truthy(v: &Value) -> Option<bool> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(*i != 0),
        Value::Real(r) => Some(*r != 0.0),
        Value::Text(s) => Some(match coerce_text_to_numeric(s) {
            Value::Integer(i) => i != 0,
            Value::Real(r) => r != 0.0,
            _ => false,
        }),
        Value::Blob(_) => Some(true),
    }
}

fn bool_to_value(b: Option<bool>) -> Value {
    match b {
        Some(true) => Value::Integer(1),
        Some(false) => Value::Integer(0),
        None => Value::Null,
    }
}

fn literal_to_value(lit: &Literal) -> Value {
    match lit {
        Literal::Integer(i) => Value::Integer(*i),
        Literal::Float(f) => Value::Real(*f),
        Literal::Str(s) => Value::Text(s.clone()),
        Literal::Blob(b) => Value::Blob(b.clone()),
        Literal::Null => Value::Null,
        Literal::True => Value::Integer(1),
        Literal::False => Value::Integer(0),
    }
}

fn eval(expr: &Expr) -> Result<Value, WalkError> {
    match &expr.kind {
        ExprKind::Literal(lit) => Ok(literal_to_value(lit)),
        ExprKind::FunctionCall { name, args, .. } => eval_call(name, args),
        ExprKind::Unary { op, expr } => eval_unary(*op, expr),
        ExprKind::Binary { op, lhs, rhs } => eval_binary(*op, lhs, rhs),
        ExprKind::Is { lhs, rhs, negated } => {
            let (a, b) = (eval(lhs)?, eval(rhs)?);
            let result = if *negated {
                is_not(&a, &b, Collation::Binary)
            } else {
                is(&a, &b, Collation::Binary)
            };
            Ok(bool_to_value(Some(result)))
        }
        ExprKind::IsNull { expr, negated } => {
            let is_null = matches!(eval(expr)?, Value::Null);
            Ok(bool_to_value(Some(if *negated {
                !is_null
            } else {
                is_null
            })))
        }
        ExprKind::Between {
            expr,
            lo,
            hi,
            negated,
        } => eval_between(expr, lo, hi, *negated),
        ExprKind::In {
            expr,
            list,
            negated,
        } => eval_in(expr, list, *negated),
        ExprKind::Like {
            expr,
            pattern,
            glob,
            negated,
            escape,
        } => eval_like(expr, pattern, *glob, *negated, escape.as_deref()),
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => eval_case(operand.as_deref(), whens, else_.as_deref()),
        ExprKind::Cast { expr, type_name } => eval_cast(expr, type_name),
        // Collation affects downstream comparisons, not the value
        // itself; the walker doesn't track collation propagation
        // through arbitrary sub-expressions (a phase-3 VDBE concern).
        ExprKind::Collate { expr, .. } => eval(expr),
        ExprKind::Paren(inner) => eval(inner),
        ExprKind::Column { .. } | ExprKind::Param(_) => Err(WalkError::Unsupported(
            "walker only evaluates constant scalar expressions".to_string(),
        )),
    }
}

fn eval_call(name: &str, args: &FunctionArgs) -> Result<Value, WalkError> {
    let list = match args {
        FunctionArgs::Star => {
            return Err(WalkError::Unsupported(format!("{name}(*) not supported")))
        }
        FunctionArgs::List(list) => list,
    };
    let values = list.iter().map(eval).collect::<Result<Vec<_>, _>>()?;
    call_function(name, &values).map_err(WalkError::Function)
}

fn eval_unary(op: UnaryOp, expr: &Expr) -> Result<Value, WalkError> {
    let v = eval(expr)?;
    Ok(match op {
        UnaryOp::Not => bool_to_value(not(truthy(&v))),
        UnaryOp::Plus => v,
        UnaryOp::Minus => match v {
            Value::Null => Value::Null,
            Value::Integer(i) => match i.checked_neg() {
                Some(n) => Value::Integer(n),
                // i64::MIN negates by promoting to REAL, matching
                // abs()'s overflow handling in src/vdbe/functions.rs.
                #[allow(clippy::cast_precision_loss)]
                None => Value::Real(-(i as f64)),
            },
            Value::Real(r) => Value::Real(-r),
            other => Value::Real(-value_f64(&other)),
        },
        UnaryOp::BitNot => match v {
            Value::Null => Value::Null,
            other => Value::Integer(!value_int(&other)),
        },
    })
}

fn value_f64(v: &Value) -> f64 {
    match v {
        Value::Integer(i) => *i as f64,
        Value::Real(r) => *r,
        Value::Text(s) => match coerce_text_to_numeric(s) {
            Value::Integer(i) => i as f64,
            Value::Real(r) => r,
            _ => 0.0,
        },
        Value::Null | Value::Blob(_) => 0.0,
    }
}

fn value_int(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        Value::Real(r) => *r as i64,
        Value::Text(s) => cast_to_integer(&Value::Text(s.clone())),
        Value::Null | Value::Blob(_) => 0,
    }
}

fn is_real_typed(v: &Value) -> bool {
    match v {
        Value::Real(_) => true,
        Value::Text(s) => matches!(coerce_text_to_numeric(s), Value::Real(_)),
        _ => false,
    }
}

fn any_null(vs: &[&Value]) -> bool {
    vs.iter().any(|v| matches!(v, Value::Null))
}

/// Evaluates `expr`, peeling off an explicit `COLLATE` if it's the
/// immediate node (COLLATE binds tighter than comparison operators, so
/// it's typically the direct child of the comparison it modifies —
/// good enough for this spike's scalar-expression corpus, though a
/// real VDBE would track collation propagation through the whole
/// expression per SQLite's "nearest COLLATE wins, rightmost breaks
/// ties" rule; see findings.md).
fn eval_with_collation(expr: &Expr) -> Result<(Value, Option<Collation>), WalkError> {
    if let ExprKind::Collate {
        expr: inner,
        collation,
    } = &expr.kind
    {
        return Ok((eval(inner)?, Some(parse_collation(collation))));
    }
    Ok((eval(expr)?, None))
}

fn parse_collation(name: &str) -> Collation {
    match name.to_ascii_uppercase().as_str() {
        "NOCASE" => Collation::NoCase,
        "RTRIM" => Collation::RTrim,
        _ => Collation::Binary,
    }
}

fn eval_binary(op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Result<Value, WalkError> {
    match op {
        // Short-circuit: the emission-order finding this spike exists to
        // produce (findings.md) — a real evaluator never evaluates the
        // right operand once the left one already determines the
        // three-valued result.
        BinaryOp::And => {
            let a = truthy(&eval(lhs)?);
            if a == Some(false) {
                return Ok(bool_to_value(Some(false)));
            }
            let b = truthy(&eval(rhs)?);
            Ok(bool_to_value(and(a, b)))
        }
        BinaryOp::Or => {
            let a = truthy(&eval(lhs)?);
            if a == Some(true) {
                return Ok(bool_to_value(Some(true)));
            }
            let b = truthy(&eval(rhs)?);
            Ok(bool_to_value(or(a, b)))
        }
        BinaryOp::Eq => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(sql_eq(
                &a,
                &b,
                ca.or(cb).unwrap_or(Collation::Binary),
            )))
        }
        BinaryOp::Ne => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(not(sql_eq(
                &a,
                &b,
                ca.or(cb).unwrap_or(Collation::Binary),
            ))))
        }
        BinaryOp::Lt => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(sql_lt(
                &a,
                &b,
                ca.or(cb).unwrap_or(Collation::Binary),
            )))
        }
        BinaryOp::Gt => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(sql_lt(
                &b,
                &a,
                ca.or(cb).unwrap_or(Collation::Binary),
            )))
        }
        BinaryOp::Le => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(not(sql_lt(
                &b,
                &a,
                ca.or(cb).unwrap_or(Collation::Binary),
            ))))
        }
        BinaryOp::Ge => {
            let ((a, ca), (b, cb)) = (eval_with_collation(lhs)?, eval_with_collation(rhs)?);
            Ok(bool_to_value(not(sql_lt(
                &a,
                &b,
                ca.or(cb).unwrap_or(Collation::Binary),
            ))))
        }
        // `checked_add`/`sub`/`mul` are low-level arithmetic primitives
        // with no NULL awareness by design (same layering as
        // `round_fn`/`value_f64` in src/vdbe/functions.rs) — NULL
        // propagation is the caller's job.
        BinaryOp::Add => {
            let (a, b) = (eval(lhs)?, eval(rhs)?);
            Ok(if any_null(&[&a, &b]) {
                Value::Null
            } else {
                checked_add(&a, &b)
            })
        }
        BinaryOp::Sub => {
            let (a, b) = (eval(lhs)?, eval(rhs)?);
            Ok(if any_null(&[&a, &b]) {
                Value::Null
            } else {
                checked_sub(&a, &b)
            })
        }
        BinaryOp::Mul => {
            let (a, b) = (eval(lhs)?, eval(rhs)?);
            Ok(if any_null(&[&a, &b]) {
                Value::Null
            } else {
                checked_mul(&a, &b)
            })
        }
        BinaryOp::Div => eval_div(&eval(lhs)?, &eval(rhs)?),
        BinaryOp::Mod => eval_mod(&eval(lhs)?, &eval(rhs)?),
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::Shl | BinaryOp::Shr => {
            eval_bitwise(op, &eval(lhs)?, &eval(rhs)?)
        }
        BinaryOp::Concat => eval_concat(&eval(lhs)?, &eval(rhs)?),
    }
}

/// `/`: NULL propagates, divide-by-zero yields NULL, integer/integer
/// truncates toward zero, either operand REAL-typed yields REAL.
fn eval_div(a: &Value, b: &Value) -> Result<Value, WalkError> {
    if any_null(&[a, b]) {
        return Ok(Value::Null);
    }
    if is_real_typed(a) || is_real_typed(b) {
        let (x, y) = (value_f64(a), value_f64(b));
        return Ok(if y == 0.0 {
            Value::Null
        } else {
            Value::Real(x / y)
        });
    }
    let (x, y) = (value_int(a), value_int(b));
    Ok(if y == 0 {
        Value::Null
    } else if x == i64::MIN && y == -1 {
        // Matches SQLite's overflow guard in vdbe.c's OP_Divide.
        Value::Real(-(x as f64))
    } else {
        Value::Integer(x / y)
    })
}

/// `%`: operands are cast to INTEGER first (matching SQLite's
/// `vdbe.c` `OP_Remainder`), but the result renders as REAL if either
/// operand was REAL-typed.
fn eval_mod(a: &Value, b: &Value) -> Result<Value, WalkError> {
    if any_null(&[a, b]) {
        return Ok(Value::Null);
    }
    let real_result = is_real_typed(a) || is_real_typed(b);
    let (x, y) = (value_int(a), value_int(b));
    if y == 0 {
        return Ok(Value::Null);
    }
    let r = if x == i64::MIN && y == -1 { 0 } else { x % y };
    Ok(if real_result {
        #[allow(clippy::cast_precision_loss)]
        Value::Real(r as f64)
    } else {
        Value::Integer(r)
    })
}

fn eval_bitwise(op: BinaryOp, a: &Value, b: &Value) -> Result<Value, WalkError> {
    if any_null(&[a, b]) {
        return Ok(Value::Null);
    }
    let (x, y) = (value_int(a), value_int(b));
    Ok(Value::Integer(match op {
        BinaryOp::BitAnd => x & y,
        BinaryOp::BitOr => x | y,
        BinaryOp::Shl => {
            if !(0..64).contains(&y) {
                0
            } else {
                x.wrapping_shl(y as u32)
            }
        }
        BinaryOp::Shr => {
            if !(0..64).contains(&y) {
                if x < 0 {
                    -1
                } else {
                    0
                }
            } else {
                x.wrapping_shr(y as u32)
            }
        }
        _ => unreachable!("eval_bitwise only called for bitwise ops"),
    }))
}

/// `||`: NULL propagates, both operands render through their TEXT
/// representation (numeric -> text, matching `quote()`'s rendering).
fn eval_concat(a: &Value, b: &Value) -> Result<Value, WalkError> {
    if any_null(&[a, b]) {
        return Ok(Value::Null);
    }
    Ok(Value::Text(format!(
        "{}{}",
        value_as_text(a),
        value_as_text(b)
    )))
}

fn value_as_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => {
            // Reuses the same rendering path as CAST/quote() via the
            // registry so concat's numeric-to-text stays consistent
            // with the rest of the kernel's known REAL-precision gap
            // (see #92's `quote()` known-gap note).
            match call_function("quote", std::slice::from_ref(&Value::Real(*r))) {
                Ok(Value::Text(q)) => q,
                _ => r.to_string(),
            }
        }
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn eval_between(expr: &Expr, lo: &Expr, hi: &Expr, negated: bool) -> Result<Value, WalkError> {
    let (v, l, h) = (eval(expr)?, eval(lo)?, eval(hi)?);
    let ge_lo = not(sql_lt(&v, &l, Collation::Binary));
    let le_hi = not(sql_lt(&h, &v, Collation::Binary));
    let result = and(ge_lo, le_hi);
    Ok(bool_to_value(if negated { not(result) } else { result }))
}

fn eval_in(expr: &Expr, list: &[Expr], negated: bool) -> Result<Value, WalkError> {
    let v = eval(expr)?;
    let mut saw_null = matches!(v, Value::Null);
    let mut found = false;
    for item in list {
        let item_v = eval(item)?;
        match sql_eq(&v, &item_v, Collation::Binary) {
            Some(true) => {
                found = true;
                break;
            }
            Some(false) => {}
            None => saw_null = true,
        }
    }
    let result = if found {
        Some(true)
    } else if saw_null {
        None
    } else {
        Some(false)
    };
    Ok(bool_to_value(if negated { not(result) } else { result }))
}

/// `LIKE`/`GLOB`: no matcher exists anywhere else in the crate (spec 008
/// only covers affinity/comparison/collation/coercion/functions) — this
/// is genuinely net-new for the spike, per the research findings.
fn eval_like(
    expr: &Expr,
    pattern: &Expr,
    glob: bool,
    negated: bool,
    escape: Option<&Expr>,
) -> Result<Value, WalkError> {
    let (v, p) = (eval(expr)?, eval(pattern)?);
    if any_null(&[&v, &p]) {
        return Ok(Value::Null);
    }
    let escape_char = match escape {
        Some(e) => match eval(e)? {
            Value::Null => return Ok(Value::Null),
            other => value_as_text(&other).chars().next(),
        },
        None => None,
    };
    let text = value_as_text(&v);
    let pat = value_as_text(&p);
    let result = if glob {
        glob_match(&text, &pat)
    } else {
        like_match(&text, &pat, escape_char)
    };
    Ok(bool_to_value(Some(if negated { !result } else { result })))
}

/// SQLite `LIKE`: ASCII case-insensitive, `%` = any run, `_` = any one
/// char, optional `ESCAPE` char makes the following wildcard literal.
fn like_match(text: &str, pattern: &str, escape: Option<char>) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_rec(&t, &p, escape, 0, 0)
}

fn like_rec(t: &[char], p: &[char], escape: Option<char>, mut ti: usize, mut pi: usize) -> bool {
    loop {
        if pi == p.len() {
            return ti == t.len();
        }
        let pc = p[pi];
        if Some(pc) == escape && pi + 1 < p.len() {
            let literal = p[pi + 1];
            if ti >= t.len() || !ascii_eq(t[ti], literal) {
                return false;
            }
            ti += 1;
            pi += 2;
            continue;
        }
        match pc {
            '%' => {
                // Collapse consecutive '%' (a run behaves as one).
                while pi < p.len() && p[pi] == '%' {
                    pi += 1;
                }
                if pi == p.len() {
                    return true;
                }
                for start in ti..=t.len() {
                    if like_rec(t, p, escape, start, pi) {
                        return true;
                    }
                }
                return false;
            }
            '_' => {
                if ti >= t.len() {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
            _ => {
                if ti >= t.len() || !ascii_eq(t[ti], pc) {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
        }
    }
}

fn ascii_eq(a: char, b: char) -> bool {
    a.eq_ignore_ascii_case(&b)
}

/// SQLite `GLOB`: case-sensitive, `*` = any run, `?` = any one char,
/// `[...]`/`[^...]` character classes (with `-` ranges).
fn glob_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    glob_rec(&t, &p, 0, 0)
}

fn glob_rec(t: &[char], p: &[char], mut ti: usize, mut pi: usize) -> bool {
    loop {
        if pi == p.len() {
            return ti == t.len();
        }
        match p[pi] {
            '*' => {
                while pi < p.len() && p[pi] == '*' {
                    pi += 1;
                }
                if pi == p.len() {
                    return true;
                }
                for start in ti..=t.len() {
                    if glob_rec(t, p, start, pi) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ti >= t.len() {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
            '[' => {
                let Some((matches, next_pi)) = glob_class(p, pi, t.get(ti).copied()) else {
                    return false;
                };
                if ti >= t.len() || !matches {
                    return false;
                }
                ti += 1;
                pi = next_pi;
            }
            c => {
                if ti >= t.len() || t[ti] != c {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
        }
    }
}

/// Parses a `[...]`/`[^...]` class starting at `p[start]` (`p[start] ==
/// '['`); returns whether `c` matched and the index just past the `]`.
fn glob_class(p: &[char], start: usize, c: Option<char>) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = p.get(i) == Some(&'^');
    if negate {
        i += 1;
    }
    let class_start = i;
    let mut matched = false;
    loop {
        if i >= p.len() {
            return None; // unterminated class: treat as no match
        }
        if p[i] == ']' && i > class_start {
            i += 1;
            break;
        }
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            let (lo, hi) = (p[i], p[i + 2]);
            if let Some(c) = c {
                if c >= lo && c <= hi {
                    matched = true;
                }
            }
            i += 3;
        } else {
            if Some(p[i]) == c {
                matched = true;
            }
            i += 1;
        }
    }
    Some((matched != negate && c.is_some(), i))
}

fn eval_case(
    operand: Option<&Expr>,
    whens: &[(Expr, Expr)],
    else_: Option<&Expr>,
) -> Result<Value, WalkError> {
    let base = operand.map(eval).transpose()?;
    for (when, then) in whens {
        let matched = match &base {
            Some(b) => sql_eq(b, &eval(when)?, Collation::Binary) == Some(true),
            None => truthy(&eval(when)?) == Some(true),
        };
        if matched {
            return eval(then);
        }
    }
    match else_ {
        Some(e) => eval(e),
        None => Ok(Value::Null),
    }
}

fn eval_cast(expr: &Expr, type_name: &str) -> Result<Value, WalkError> {
    let v = eval(expr)?;
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    Ok(match affinity_of(type_name) {
        Affinity::Integer => Value::Integer(cast_to_integer(&v)),
        Affinity::Real => Value::Real(value_f64(&v)),
        Affinity::Text => Value::Text(value_as_text(&v)),
        Affinity::Blob => match v {
            Value::Blob(b) => Value::Blob(b),
            other => Value::Blob(value_as_text(&other).into_bytes()),
        },
        Affinity::Numeric => {
            let mut v = v;
            apply_affinity(&mut v, Affinity::Numeric);
            v
        }
    })
}
