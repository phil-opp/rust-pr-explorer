//! The syntax tree of an expression.

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A number literal.
    Num(f64),
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
}

/// Something that can be reduced to a value.
pub trait Reduce {
    /// The value it reduces to.
    type Output;

    /// Reduces `self`.
    fn reduce(&self) -> Self::Output;
}
