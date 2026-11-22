//! A tiny calculator for arithmetic expressions.

pub mod ast;
mod eval;
pub mod parser;

pub use eval::evaluate;
pub use parser::parse;

/// Errors produced while parsing or evaluating an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The input contained a character that is not part of any token.
    UnexpectedChar(char),
    /// The input ended in the middle of an expression.
    UnexpectedEnd,
}

/// Parses and evaluates `input` in one go.
pub fn calc(input: &str) -> Result<f64, Error> {
    let expr = parse(input)?;
    Ok(evaluate(&expr))
}
