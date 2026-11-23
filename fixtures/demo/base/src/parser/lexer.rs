use crate::ast::Op;
use crate::Error;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Token {
    Num(f64),
    Op(Op),
    LParen,
    RParen,
}

/// Splits the input into tokens.
pub(super) fn tokenize(input: &str) -> Result<Vec<Token>, Error> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' => i += 1,
            '0'..='9' | '.' => {
                let (n, len) = lex_number(&chars[i..]);
                tokens.push(Token::Num(n));
                i += len;
            }
            '+' => { tokens.push(Token::Op(Op::Add)); i += 1 }
            '-' => { tokens.push(Token::Op(Op::Sub)); i += 1 }
            '*' => { tokens.push(Token::Op(Op::Mul)); i += 1 }
            '/' => { tokens.push(Token::Op(Op::Div)); i += 1 }
            '(' => { tokens.push(Token::LParen); i += 1 }
            ')' => { tokens.push(Token::RParen); i += 1 }
            other => return Err(Error::UnexpectedChar(other)),
        }
    }
    Ok(tokens)
}

fn lex_number(chars: &[char]) -> (f64, usize) {
    let len = chars.iter().take_while(|c| c.is_ascii_digit() || **c == '.').count();
    let text: String = chars[..len].iter().collect();
    (text.parse().unwrap_or(0.0), len)
}
