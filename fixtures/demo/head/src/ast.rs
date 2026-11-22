//! The syntax tree of an expression.

use std::fmt;

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A number literal.
    Num(f64),
    /// A variable reference, resolved through an [`Env`](crate::Env).
    Var(String),
    /// Negation: `-expr`.
    Neg(Box<Expr>),
    /// A binary operation.
    Binary { op: Op, lhs: Box<Expr>, rhs: Box<Expr> },
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    /// Binding strength; higher binds tighter.
    pub fn precedence(self) -> u8 {
        match self {
            Op::Add | Op::Sub => 1,
            Op::Mul | Op::Div => 2,
        }
    }

    /// The operator as it is written in source.
    pub fn symbol(self) -> char {
        match self {
            Op::Add => '+',
            Op::Sub => '-',
            Op::Mul => '*',
            Op::Div => '/',
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Num(n) => write!(f, "{}", format_number(*n)),
            Expr::Var(name) => write!(f, "{name}"),
            Expr::Neg(e) => write!(f, "-{e}"),
            Expr::Binary { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.symbol()),
        }
    }
}

/// Formats a number without a trailing `.0`.
pub(crate) fn format_number(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}
