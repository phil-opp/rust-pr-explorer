use crate::ast::{Expr, Op};

/// Evaluates an expression.
pub fn evaluate(expr: &Expr) -> f64 {
    match expr {
        Expr::Num(n) => *n,
        Expr::Neg(e) => -evaluate(e),
        Expr::Binary { op, lhs, rhs } => apply(*op, evaluate(lhs), evaluate(rhs)),
    }
}

fn apply(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => a / b,
    }
}

/// Formats a number without a trailing `.0`.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}
