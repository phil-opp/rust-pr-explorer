//! Turning text into an [`Expr`](crate::ast::Expr).

mod lexer;

use crate::ast::*;
use lexer::{Token, tokenize};

/// A recursive-descent parser over a token stream.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Tokenizes `input` and creates a parser for it.
    pub fn new(input: &str) -> Result<Self, crate::Error> {
        Ok(Parser {
            tokens: tokenize(input)?,
            pos: 0,
        })
    }

    /// Parses a full expression.
    pub fn parse_expr(&mut self) -> Result<Expr, crate::Error> {
        self.parse_binary(0)
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, crate::Error> {
        let mut lhs = self.parse_factor()?;
        while let Some(op) = self.next_op(min_prec) {
            let rhs = self.parse_binary(op.precedence() + 1)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// Takes the next token if it is an operator that binds at least as
    /// tightly as `min_prec`.
    fn next_op(&mut self, min_prec: u8) -> Option<Op> {
        let Some(Token::Op(op)) = self.peek() else {
            return None;
        };
        let op = *op;
        if op.precedence() < min_prec {
            return None;
        }
        self.next();
        Some(op)
    }

    fn parse_factor(&mut self) -> Result<Expr, crate::Error> {
        match self.next() {
            Some(Token::Num(n)) => Ok(Expr::Num(n)),
            Some(Token::Ident(name)) => Ok(Expr::Var(name)),
            Some(Token::Op(Op::Sub)) => Ok(Expr::Neg(Box::new(self.parse_factor()?))),
            Some(Token::LParen) => {
                let e = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(e)
            }
            _ => Err(crate::Error::UnexpectedEnd),
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn expect(&mut self, expected: Token) -> Result<(), crate::Error> {
        match self.next() {
            Some(t) if t == expected => Ok(()),
            _ => Err(crate::Error::UnexpectedEnd),
        }
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }
}

/// Parses `input` into an expression, or returns the first error.
pub fn parse(input: &str) -> Result<Expr, crate::Error> {
    Parser::new(input)?.parse_expr()
}
