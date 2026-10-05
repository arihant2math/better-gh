//! Recursive-descent parser producing [`Expr`].
//!
//! Precedence (loosest first): `||`, `&&`, `== !=`, `< <= > >=`, `!`,
//! postfix (`.prop`, `.*`, `[idx]`, `[*]`), primary.

use super::lexer::{Tok, Token, tokenize};
use super::{CompareOp, Expr, ExprError, Function};

/// Longest accepted expression source, in bytes.
pub const MAX_EXPRESSION_LENGTH: usize = 21_000;
/// Maximum nesting of parentheses / calls / indexes / `!` while parsing.
pub const MAX_NESTING: usize = 50;
/// Maximum depth of the resulting syntax tree (bounds evaluator recursion).
pub const MAX_AST_DEPTH: usize = 256;

pub fn parse(src: &str) -> Result<Expr, ExprError> {
    if src.len() > MAX_EXPRESSION_LENGTH {
        return Err(ExprError::TooLong {
            len: src.len(),
            max: MAX_EXPRESSION_LENGTH,
        });
    }
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Err(ExprError::EmptyExpression {
            expr: src.to_string(),
        });
    }
    let mut p = Parser {
        src,
        tokens,
        idx: 0,
        nesting: 0,
    };
    let expr = p.parse_or()?;
    if let Some(t) = p.peek() {
        return Err(p.unexpected(t));
    }
    if ast_depth(&expr) > MAX_AST_DEPTH {
        return Err(ExprError::TooDeep {
            max: MAX_AST_DEPTH,
            expr: src.to_string(),
        });
    }
    Ok(expr)
}

/// Depth of the tree, computed iteratively (so it is safe on any input).
fn ast_depth(root: &Expr) -> usize {
    let mut max = 0;
    let mut stack = vec![(root, 1usize)];
    while let Some((e, d)) = stack.pop() {
        max = max.max(d);
        match e {
            Expr::Null | Expr::Bool(_) | Expr::Number(_) | Expr::String(_) | Expr::Context(_) => {}
            Expr::Property(b, _) | Expr::Wildcard(b) | Expr::Not(b) => stack.push((b, d + 1)),
            Expr::Index(a, b) | Expr::And(a, b) | Expr::Or(a, b) | Expr::Compare(_, a, b) => {
                stack.push((a, d + 1));
                stack.push((b, d + 1));
            }
            Expr::Call(_, args) => stack.extend(args.iter().map(|a| (a, d + 1))),
        }
    }
    max
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    idx: usize,
    nesting: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.idx)
    }

    fn peek_tok(&self) -> Option<&Tok> {
        self.peek().map(|t| &t.tok)
    }

    fn next(&mut self) -> Result<Token, ExprError> {
        let t = self
            .tokens
            .get(self.idx)
            .cloned()
            .ok_or_else(|| ExprError::UnexpectedEnd {
                expr: self.src.to_string(),
            })?;
        self.idx += 1;
        Ok(t)
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek_tok() == Some(tok) {
            self.idx += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok) -> Result<(), ExprError> {
        let t = self.next()?;
        if &t.tok == tok {
            Ok(())
        } else {
            Err(self.unexpected(&t))
        }
    }

    fn unexpected(&self, t: &Token) -> ExprError {
        ExprError::UnexpectedToken {
            token: t.text.clone(),
            pos: t.pos,
            expr: self.src.to_string(),
        }
    }

    fn enter(&mut self) -> Result<(), ExprError> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(ExprError::TooDeep {
                max: MAX_NESTING,
                expr: self.src.to_string(),
            });
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.nesting -= 1;
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_and()?;
        while self.eat(&Tok::Or) {
            let rhs = self.parse_and()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_equality()?;
        while self.eat(&Tok::And) {
            let rhs = self.parse_equality()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek_tok() {
                Some(Tok::Eq) => CompareOp::Eq,
                Some(Tok::Ne) => CompareOp::Ne,
                _ => return Ok(lhs),
            };
            self.idx += 1;
            let rhs = self.parse_comparison()?;
            lhs = Expr::Compare(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek_tok() {
                Some(Tok::Lt) => CompareOp::Lt,
                Some(Tok::Le) => CompareOp::Le,
                Some(Tok::Gt) => CompareOp::Gt,
                Some(Tok::Ge) => CompareOp::Ge,
                _ => return Ok(lhs),
            };
            self.idx += 1;
            let rhs = self.parse_unary()?;
            lhs = Expr::Compare(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if self.eat(&Tok::Not) {
            self.enter()?;
            let inner = self.parse_unary()?;
            self.leave();
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, ExprError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.eat(&Tok::Dot) {
                let t = self.next()?;
                expr = match t.tok {
                    Tok::Star => Expr::Wildcard(Box::new(expr)),
                    Tok::Ident(name) => Expr::Property(Box::new(expr), name),
                    _ => return Err(self.unexpected(&t)),
                };
            } else if self.eat(&Tok::LBracket) {
                if self.peek_tok() == Some(&Tok::Star) {
                    self.idx += 1;
                    self.expect(&Tok::RBracket)?;
                    expr = Expr::Wildcard(Box::new(expr));
                } else {
                    self.enter()?;
                    let index = self.parse_or()?;
                    self.leave();
                    self.expect(&Tok::RBracket)?;
                    expr = Expr::Index(Box::new(expr), Box::new(index));
                }
            } else {
                return Ok(expr);
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        let t = self.next()?;
        match t.tok {
            Tok::Number(n) => Ok(Expr::Number(n)),
            Tok::Str(s) => Ok(Expr::String(s)),
            Tok::LParen => {
                self.enter()?;
                let inner = self.parse_or()?;
                self.leave();
                self.expect(&Tok::RParen)?;
                Ok(inner)
            }
            Tok::Ident(name) => {
                if self.peek_tok() == Some(&Tok::LParen) {
                    self.idx += 1;
                    return self.parse_call(name, t.pos);
                }
                Ok(match name.as_str() {
                    "true" => Expr::Bool(true),
                    "false" => Expr::Bool(false),
                    "null" => Expr::Null,
                    "NaN" => Expr::Number(f64::NAN),
                    "Infinity" => Expr::Number(f64::INFINITY),
                    _ => Expr::Context(name),
                })
            }
            _ => Err(self.unexpected(&t)),
        }
    }

    fn parse_call(&mut self, name: String, pos: usize) -> Result<Expr, ExprError> {
        let func = Function::from_name(&name).ok_or_else(|| ExprError::UnknownFunction {
            name: name.clone(),
            pos,
            expr: self.src.to_string(),
        })?;
        self.enter()?;
        let mut args = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                args.push(self.parse_or()?);
                if self.eat(&Tok::Comma) {
                    continue;
                }
                self.expect(&Tok::RParen)?;
                break;
            }
        }
        self.leave();
        let (min, max) = func.arity();
        if args.len() < min || max.is_some_and(|m| args.len() > m) {
            let expected = match max {
                Some(m) if m == min => format!("{min}"),
                Some(m) => format!("{min} to {m}"),
                None => format!("at least {min}"),
            };
            return Err(ExprError::ArgumentCount {
                name: func.name().to_string(),
                expected,
                got: args.len(),
                expr: self.src.to_string(),
            });
        }
        Ok(Expr::Call(func, args))
    }
}
