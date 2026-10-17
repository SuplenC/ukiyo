//! The signal-expression language used in `scene.toml` bindings.
//!
//! ```text
//! flicker = "typing.rate * 0.8 + cpu.load * 0.3"
//! glow    = "mix(0.4, 1.0, smoothstep(17.0, 19.0, clock.hour))"
//! ```
//!
//! Numbers, names (`a.b_c`, resolved to signals or earlier bindings), the
//! operators `+ - * / % ^`, comparisons `< <= > >= == !=` (1.0 or 0.0),
//! `&&` / `||` (on truthiness, non-zero), unary `-` and `!`, the ternary
//! `c ? a : b`, parentheses, and a fixed set of functions (see [`Func`]).
//! An expression is parsed once into a [`Expr`] tree whose names are
//! resolved to slots ([`Expr::compile`]), then evaluated every frame
//! without allocating.

use std::fmt;

/// A parse error, with the byte offset it happened at.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message} at column {}", .offset + 1)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

/// Built-in functions. Arity is checked at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Min,
    Max,
    Clamp,
    Saturate,
    Mix,
    Step,
    Smoothstep,
    Abs,
    Sign,
    Floor,
    Ceil,
    Fract,
    Sqrt,
    Pow,
    Exp,
    Log,
    Sin,
    Cos,
    /// `wave(x)`: 0..1 cosine, one period per unit of `x`, 0 at whole numbers.
    Wave,
    /// `remap(x, a, b, c, d)`: x from [a, b] to [c, d], clamped.
    Remap,
}

impl Func {
    fn lookup(name: &str) -> Option<(Func, usize)> {
        use Func::*;
        Some(match name {
            "min" => (Min, 2),
            "max" => (Max, 2),
            "clamp" => (Clamp, 3),
            "saturate" => (Saturate, 1),
            "mix" | "lerp" => (Mix, 3),
            "step" => (Step, 2),
            "smoothstep" => (Smoothstep, 3),
            "abs" => (Abs, 1),
            "sign" => (Sign, 1),
            "floor" => (Floor, 1),
            "ceil" => (Ceil, 1),
            "fract" => (Fract, 1),
            "sqrt" => (Sqrt, 1),
            "pow" => (Pow, 2),
            "exp" => (Exp, 1),
            "log" => (Log, 1),
            "sin" => (Sin, 1),
            "cos" => (Cos, 1),
            "wave" => (Wave, 1),
            "remap" => (Remap, 5),
            _ => return None,
        })
    }

    fn apply(self, a: &[f32]) -> f32 {
        use Func::*;
        match self {
            Min => a[0].min(a[1]),
            Max => a[0].max(a[1]),
            Clamp => a[0].max(a[1]).min(a[2]),
            Saturate => a[0].clamp(0.0, 1.0),
            Mix => a[0] + (a[1] - a[0]) * a[2],
            Step => {
                if a[1] < a[0] {
                    0.0
                } else {
                    1.0
                }
            }
            Smoothstep => {
                let (e0, e1, x) = (a[0], a[1], a[2]);
                if e0 == e1 {
                    return if x < e0 { 0.0 } else { 1.0 };
                }
                let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            }
            Abs => a[0].abs(),
            Sign => {
                if a[0] > 0.0 {
                    1.0
                } else if a[0] < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Floor => a[0].floor(),
            Ceil => a[0].ceil(),
            Fract => a[0] - a[0].floor(),
            Sqrt => a[0].max(0.0).sqrt(),
            Pow => a[0].abs().powf(a[1]),
            Exp => a[0].exp(),
            Log => a[0].max(1e-12).ln(),
            Sin => a[0].sin(),
            Cos => a[0].cos(),
            Wave => 0.5 - 0.5 * (a[0] * std::f32::consts::TAU).cos(),
            Remap => {
                let (x, a0, b0, c, d) = (a[0], a[1], a[2], a[3], a[4]);
                if a0 == b0 {
                    return c;
                }
                let t = ((x - a0) / (b0 - a0)).clamp(0.0, 1.0);
                c + (d - c) * t
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl BinOp {
    fn apply(self, a: f32, b: f32) -> f32 {
        let bool = |v: bool| if v { 1.0 } else { 0.0 };
        match self {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => {
                if b == 0.0 {
                    0.0
                } else {
                    a / b
                }
            }
            BinOp::Rem => {
                if b == 0.0 {
                    0.0
                } else {
                    a.rem_euclid(b)
                }
            }
            BinOp::Pow => a.abs().powf(b),
            BinOp::Lt => bool(a < b),
            BinOp::Le => bool(a <= b),
            BinOp::Gt => bool(a > b),
            BinOp::Ge => bool(a >= b),
            BinOp::Eq => bool(a == b),
            BinOp::Ne => bool(a != b),
            BinOp::And => bool(a != 0.0 && b != 0.0),
            BinOp::Or => bool(a != 0.0 || b != 0.0),
        }
    }
}

/// A parsed expression. `Var` holds the name until [`Expr::compile`] turns
/// it into a `Slot`.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f32),
    Var(String),
    Slot(usize),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, ParseError> {
        let tokens = lex(src)?;
        let mut p = Parser {
            tokens,
            pos: 0,
            end: src.len(),
        };
        let e = p.ternary()?;
        match p.peek() {
            None => Ok(e),
            Some((_, at)) => Err(ParseError {
                message: "unexpected token".into(),
                offset: at,
            }),
        }
    }

    /// Every name the expression reads, in order of appearance, deduplicated.
    pub fn names(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.visit_names(&mut |n| {
            if !out.iter().any(|o: &String| o == n) {
                out.push(n.to_string());
            }
        });
        out
    }

    fn visit_names(&self, f: &mut impl FnMut(&str)) {
        match self {
            Expr::Var(n) => f(n),
            Expr::Num(_) | Expr::Slot(_) => {}
            Expr::Neg(e) | Expr::Not(e) => e.visit_names(f),
            Expr::Bin(_, a, b) => {
                a.visit_names(f);
                b.visit_names(f);
            }
            Expr::Cond(c, a, b) => {
                c.visit_names(f);
                a.visit_names(f);
                b.visit_names(f);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.visit_names(f)),
        }
    }

    /// Replace names by slots. `resolve` returns the slot of a name; names it
    /// doesn't know become the constant 0 (signals are optional), and are
    /// returned so the caller can warn about them.
    pub fn compile(self, resolve: &mut impl FnMut(&str) -> Option<usize>) -> (Expr, Vec<String>) {
        let mut unknown = Vec::new();
        let e = self.compile_inner(resolve, &mut unknown);
        (e.fold(), unknown)
    }

    fn compile_inner(
        self,
        resolve: &mut impl FnMut(&str) -> Option<usize>,
        unknown: &mut Vec<String>,
    ) -> Expr {
        let mut go = |e: Expr| Box::new(e.compile_inner(resolve, unknown));
        match self {
            Expr::Var(n) => match resolve(&n) {
                Some(slot) => Expr::Slot(slot),
                None => {
                    unknown.push(n);
                    Expr::Num(0.0)
                }
            },
            Expr::Neg(e) => Expr::Neg(go(*e)),
            Expr::Not(e) => Expr::Not(go(*e)),
            Expr::Bin(op, a, b) => {
                let a = go(*a);
                Expr::Bin(op, a, go(*b))
            }
            Expr::Cond(c, a, b) => {
                let c = go(*c);
                let a = go(*a);
                Expr::Cond(c, a, go(*b))
            }
            Expr::Call(f, args) => Expr::Call(f, args.into_iter().map(|a| *go(a)).collect()),
            e => e,
        }
    }

    /// Constant folding: subtrees without names or slots become numbers.
    fn fold(self) -> Expr {
        match self {
            Expr::Neg(e) => match e.fold() {
                Expr::Num(v) => Expr::Num(-v),
                e => Expr::Neg(Box::new(e)),
            },
            Expr::Not(e) => match e.fold() {
                Expr::Num(v) => Expr::Num(if v == 0.0 { 1.0 } else { 0.0 }),
                e => Expr::Not(Box::new(e)),
            },
            Expr::Bin(op, a, b) => match (a.fold(), b.fold()) {
                (Expr::Num(x), Expr::Num(y)) => Expr::Num(op.apply(x, y)),
                (a, b) => Expr::Bin(op, Box::new(a), Box::new(b)),
            },
            Expr::Cond(c, a, b) => match c.fold() {
                Expr::Num(v) => {
                    if v != 0.0 {
                        a.fold()
                    } else {
                        b.fold()
                    }
                }
                c => Expr::Cond(Box::new(c), Box::new(a.fold()), Box::new(b.fold())),
            },
            Expr::Call(f, args) => {
                let args: Vec<Expr> = args.into_iter().map(Expr::fold).collect();
                if args.iter().all(|a| matches!(a, Expr::Num(_))) {
                    let vals: Vec<f32> = args
                        .iter()
                        .map(|a| if let Expr::Num(v) = a { *v } else { 0.0 })
                        .collect();
                    Expr::Num(f.apply(&vals))
                } else {
                    Expr::Call(f, args)
                }
            }
            e => e,
        }
    }

    /// Evaluate against slot values. A `Var` (not compiled) reads as 0.
    /// Non-finite results are returned as 0.
    pub fn eval(&self, slots: &[f32]) -> f32 {
        let v = self.eval_raw(slots);
        if v.is_finite() { v } else { 0.0 }
    }

    fn eval_raw(&self, slots: &[f32]) -> f32 {
        match self {
            Expr::Num(v) => *v,
            Expr::Var(_) => 0.0,
            Expr::Slot(i) => slots.get(*i).copied().unwrap_or(0.0),
            Expr::Neg(e) => -e.eval_raw(slots),
            Expr::Not(e) => {
                if e.eval_raw(slots) == 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Expr::Bin(op, a, b) => op.apply(a.eval_raw(slots), b.eval_raw(slots)),
            Expr::Cond(c, a, b) => {
                if c.eval_raw(slots) != 0.0 {
                    a.eval_raw(slots)
                } else {
                    b.eval_raw(slots)
                }
            }
            Expr::Call(f, args) => {
                // Arity is at most 5; no allocation per frame.
                let mut buf = [0.0f32; 5];
                for (b, a) in buf.iter_mut().zip(args) {
                    *b = a.eval_raw(slots);
                }
                f.apply(&buf[..args.len()])
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f32),
    Name(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Question,
    Colon,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Num(v) => write!(f, "{v}"),
            Tok::Name(n) => f.write_str(n),
            Tok::Op(o) => f.write_str(o),
            Tok::LParen => f.write_str("("),
            Tok::RParen => f.write_str(")"),
            Tok::Comma => f.write_str(","),
            Tok::Question => f.write_str("?"),
            Tok::Colon => f.write_str(":"),
        }
    }
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    const OPS: [&str; 15] = [
        "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "^", "<", ">", "!",
    ];
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text = &src[start..i];
            let v: f32 = text.parse().map_err(|_| ParseError {
                message: format!("bad number `{text}`"),
                offset: start,
            })?;
            out.push((Tok::Num(v), start));
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                i += 1;
            }
            let name = &src[start..i];
            if name.ends_with('.') || name.contains("..") {
                return Err(ParseError {
                    message: format!("bad name `{name}`"),
                    offset: start,
                });
            }
            out.push((Tok::Name(name.to_string()), start));
            continue;
        }
        let tok = match c {
            b'(' => Some(Tok::LParen),
            b')' => Some(Tok::RParen),
            b',' => Some(Tok::Comma),
            b'?' => Some(Tok::Question),
            b':' => Some(Tok::Colon),
            _ => None,
        };
        if let Some(t) = tok {
            out.push((t, start));
            i += 1;
            continue;
        }
        match OPS.iter().find(|op| src[i..].starts_with(**op)) {
            Some(op) => {
                out.push((Tok::Op(op), start));
                i += op.len();
            }
            None => {
                let ch = src[i..].chars().next().unwrap_or('?');
                return Err(ParseError {
                    message: format!("unexpected character `{ch}`"),
                    offset: start,
                });
            }
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<(Tok, usize)>,
    pos: usize,
    end: usize,
}

impl Parser {
    fn peek(&self) -> Option<(&Tok, usize)> {
        self.tokens.get(self.pos).map(|(t, o)| (t, *o))
    }

    fn here(&self) -> usize {
        self.peek().map_or(self.end, |(_, o)| o)
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            message: message.into(),
            offset: self.here(),
        })
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek().is_some_and(|(p, _)| p == t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Tok) -> Result<(), ParseError> {
        if self.eat(&t) {
            Ok(())
        } else {
            match self.peek() {
                Some((found, _)) => self.err(format!("expected `{t}`, found `{found}`")),
                None => self.err(format!("expected `{t}`, found the end")),
            }
        }
    }

    fn ternary(&mut self) -> Result<Expr, ParseError> {
        let c = self.binary(0)?;
        if self.eat(&Tok::Question) {
            let a = self.ternary()?;
            self.expect(Tok::Colon)?;
            let b = self.ternary()?;
            return Ok(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binop(&self) -> Option<(BinOp, u8, bool)> {
        // (op, precedence, right associative)
        let Some((Tok::Op(o), _)) = self.peek() else {
            return None;
        };
        Some(match *o {
            "||" => (BinOp::Or, 1, false),
            "&&" => (BinOp::And, 2, false),
            "==" => (BinOp::Eq, 3, false),
            "!=" => (BinOp::Ne, 3, false),
            "<" => (BinOp::Lt, 4, false),
            "<=" => (BinOp::Le, 4, false),
            ">" => (BinOp::Gt, 4, false),
            ">=" => (BinOp::Ge, 4, false),
            "+" => (BinOp::Add, 5, false),
            "-" => (BinOp::Sub, 5, false),
            "*" => (BinOp::Mul, 6, false),
            "/" => (BinOp::Div, 6, false),
            "%" => (BinOp::Rem, 6, false),
            "^" => (BinOp::Pow, 8, true),
            _ => return None,
        })
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.unary()?;
        while let Some((op, prec, right)) = self.binop() {
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(if right { prec } else { prec + 1 })?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&Tok::Op("-")) {
            // Binds looser than ^ : -x^2 == -(x^2).
            return Ok(Expr::Neg(Box::new(self.binary(7)?)));
        }
        if self.eat(&Tok::Op("+")) {
            return self.binary(7);
        }
        if self.eat(&Tok::Op("!")) {
            return Ok(Expr::Not(Box::new(self.binary(7)?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let Some((tok, at)) = self.peek().map(|(t, o)| (t.clone(), o)) else {
            return self.err("unexpected end of expression");
        };
        self.pos += 1;
        match tok {
            Tok::Num(v) => Ok(Expr::Num(v)),
            Tok::LParen => {
                let e = self.ternary()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Name(name) => {
                if self.eat(&Tok::LParen) {
                    let Some((func, arity)) = Func::lookup(&name) else {
                        return Err(ParseError {
                            message: format!("unknown function `{name}`"),
                            offset: at,
                        });
                    };
                    let mut args = Vec::new();
                    if !self.eat(&Tok::RParen) {
                        loop {
                            args.push(self.ternary()?);
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            self.expect(Tok::RParen)?;
                            break;
                        }
                    }
                    if args.len() != arity {
                        return Err(ParseError {
                            message: format!(
                                "`{name}` takes {arity} argument(s), got {}",
                                args.len()
                            ),
                            offset: at,
                        });
                    }
                    Ok(Expr::Call(func, args))
                } else if name == "pi" {
                    Ok(Expr::Num(std::f32::consts::PI))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            t => Err(ParseError {
                message: format!("unexpected `{t}`"),
                offset: at,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> f32 {
        Expr::parse(src).unwrap().eval(&[])
    }

    fn eval_with(src: &str, vars: &[(&str, f32)]) -> f32 {
        let (e, unknown) = Expr::parse(src)
            .unwrap()
            .compile(&mut |n| vars.iter().position(|(k, _)| *k == n));
        assert!(unknown.is_empty(), "unknown {unknown:?}");
        let slots: Vec<f32> = vars.iter().map(|(_, v)| *v).collect();
        e.eval(&slots)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("1 + 2 * 3"), 7.0);
        assert_eq!(eval("(1 + 2) * 3"), 9.0);
        assert_eq!(eval("2 ^ 3 ^ 2"), 512.0);
        assert_eq!(eval("-2 ^ 2"), -4.0);
        assert_eq!(eval("10 - 4 - 3"), 3.0);
        assert_eq!(eval("7 % 4"), 3.0);
        assert_eq!(eval("1.5e1 / 3"), 5.0);
        assert_eq!(eval(".5 + .25"), 0.75);
    }

    #[test]
    fn comparisons_logic_and_ternary() {
        assert_eq!(eval("1 < 2"), 1.0);
        assert_eq!(eval("2 <= 1"), 0.0);
        assert_eq!(eval("1 < 2 && 3 > 4"), 0.0);
        assert_eq!(eval("1 < 2 || 3 > 4"), 1.0);
        assert_eq!(eval("!0"), 1.0);
        assert_eq!(eval("1 > 0 ? 5 : 6"), 5.0);
        assert_eq!(eval("0 ? 5 : 1 ? 7 : 8"), 7.0);
    }

    #[test]
    fn functions() {
        assert_eq!(eval("min(3, 4) + max(3, 4)"), 7.0);
        assert_eq!(eval("clamp(5, 0, 1)"), 1.0);
        assert_eq!(eval("saturate(-1)"), 0.0);
        assert_eq!(eval("mix(10, 20, 0.25)"), 12.5);
        assert_eq!(eval("smoothstep(0, 1, 0.5)"), 0.5);
        assert_eq!(eval("step(0.5, 0.7)"), 1.0);
        assert_eq!(eval("remap(15, 10, 20, 0, 1)"), 0.5);
        assert_eq!(eval("remap(25, 10, 20, 0, 1)"), 1.0);
        assert_eq!(eval("fract(2.25)"), 0.25);
        assert!((eval("sin(pi / 2)") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn variables_and_unknowns() {
        let src = "typing.rate * 0.8 + cpu.load * 0.3";
        assert!((eval_with(src, &[("typing.rate", 1.0), ("cpu.load", 0.5)]) - 0.95).abs() < 1e-6);
        let (e, unknown) = Expr::parse("a + missing.signal")
            .unwrap()
            .compile(&mut |n| (n == "a").then_some(0));
        assert_eq!(unknown, vec!["missing.signal".to_string()]);
        assert_eq!(e.eval(&[2.0]), 2.0);
        assert_eq!(
            Expr::parse("a.b + c * a.b").unwrap().names(),
            vec!["a.b", "c"]
        );
    }

    #[test]
    fn folding_and_safety() {
        let (e, _) = Expr::parse("1 + 2 * 3").unwrap().compile(&mut |_| None);
        assert_eq!(e, Expr::Num(7.0));
        assert_eq!(eval("1 / 0"), 0.0);
        assert_eq!(eval("sqrt(-4)"), 0.0);
        assert!(eval("log(0)").is_finite());
        assert_eq!(eval("exp(1000)"), 0.0, "non-finite results read as 0");
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = Expr::parse("1 + * 2").unwrap_err();
        assert_eq!(e.offset, 4);
        assert!(
            Expr::parse("foo(1)")
                .unwrap_err()
                .message
                .contains("unknown function")
        );
        assert!(
            Expr::parse("min(1)")
                .unwrap_err()
                .message
                .contains("takes 2")
        );
        assert!(Expr::parse("(1 + 2").is_err());
        assert!(Expr::parse("1 2").is_err());
        assert!(Expr::parse("a. + 1").is_err());
        assert!(Expr::parse("1 $ 2").is_err());
        assert!(Expr::parse("").is_err());
        assert_eq!(
            Expr::parse("1 +").unwrap_err().to_string(),
            "unexpected end of expression at column 4"
        );
    }
}
