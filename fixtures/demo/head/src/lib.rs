//! A tiny calculator for arithmetic expressions with variables.

pub mod ast;
pub mod env;
mod eval;
pub mod parser;

pub use ast::Op;
pub use env::Env;
pub use eval::evaluate;
pub use parser::parse;

/// Errors produced while parsing or evaluating an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The input contained a character that is not part of any token.
    UnexpectedChar(char),
    /// The input ended in the middle of an expression.
    UnexpectedEnd,
    /// A variable was used that is not defined in the [`Env`].
    UnknownVariable(String),
    /// The right-hand side of a division evaluated to zero.
    DivisionByZero,
}

/// Parses and evaluates `input` in one go, looking up variables in `env`.
pub fn calc(input: &str, env: &Env) -> Result<f64, Error> {
    let expr = parse(input)?;
    evaluate(&expr, env)
}
