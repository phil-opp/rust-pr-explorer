//! Turning text into an [`Expr`](crate::ast::Expr).

mod lexer;

use self::lexer::{tokenize, Token};
use crate::ast::{Expr, Op};
use crate::Error;

/// A recursive-descent parser over a token stream.
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Tokenizes `input` and creates a parser for it.
    pub fn new(input: &str) -> Result<Self, Error> {
        Ok(Parser { tokens: tokenize(input)?, pos: 0 })
    }

    /// Parses a full expression.
    pub fn parse_expr(&mut self) -> Result<Expr, Error> {
        self.parse_binary(0)
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, Error> {
        let mut lhs = self.parse_factor()?;
        while let Some(Token::Op(op)) = self.peek() {
            let op = *op;
            if op.precedence() < min_prec {
                break;
            }
            self.next();
            let rhs = self.parse_binary(op.precedence() + 1)?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_factor(&mut self) -> Result<Expr, Error> {
        match self.next() {
            Some(Token::Num(n)) => Ok(Expr::Num(n)),
            Some(Token::Op(Op::Sub)) => Ok(Expr::Neg(Box::new(self.parse_factor()?))),
            Some(Token::LParen) => {
                let e = self.parse_expr()?;
                self.next();
                Ok(e)
            }
            _ => Err(Error::UnexpectedEnd),
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }
}

/// Parses `input` into an expression.
pub fn parse(input: &str) -> Result<Expr, Error> {
    Parser::new(input)?.parse_expr()
}
