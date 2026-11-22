use crate::ast::{Expr, Op};
use crate::env::Env;
use crate::Error;

/// Evaluates an expression, resolving variables in `env`.
///
/// Fails with [`Error::UnknownVariable`] or [`Error::DivisionByZero`].
pub fn evaluate(expr: &Expr, env: &Env) -> Result<f64, Error> {
    Ok(match expr {
        Expr::Num(n) => *n,
        Expr::Var(name) => lookup(env, name)?,
        Expr::Neg(e) => -evaluate(e, env)?,
        Expr::Binary { op, lhs, rhs } => apply(*op, evaluate(lhs, env)?, evaluate(rhs, env)?)?,
    })
}

fn lookup(env: &Env, name: &str) -> Result<f64, Error> {
    match env.get(name) {
        Some(v) => Ok(v),
        None if env.lenient => Ok(0.0),
        None => Err(Error::UnknownVariable(name.to_string())),
    }
}

fn apply(op: Op, a: f64, b: f64) -> Result<f64, Error> {
    Ok(match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div if b == 0.0 => return Err(Error::DivisionByZero),
        Op::Div => a / b,
    })
}
