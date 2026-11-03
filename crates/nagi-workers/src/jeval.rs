//! Java の式の評価（IntelliJ の「式の評価」と条件付きブレークポイント）の式を読む。
//!
//! コンパイラは使わない（nagi の方針）。止まっているフレームで値を読む・メソッドを呼ぶ形で評価できるものだけを受ける:
//! 変数・フィールド（`a.b.c`・`this.x`）、配列の要素（`xs[2]`）と `.length`、メソッド呼び出し（`list.size()`・
//! `s.equals("x")`・`map.get(key)`）、リテラル、`! - * / % + - < <= > >= == != && ||` と括弧。
//! 評価そのものは debug.rs（JDWP で値を読む・呼ぶ）

/// 式
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Lit(Lit),
    /// 名前（ローカル変数・this のフィールド・static のフィールド）
    Name(String),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// メソッド呼び出し（受け手が無ければ this か、今のクラスの static）
    Call(Option<Box<Expr>>, String, Vec<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(Box<Expr>, Op, Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Int(i64),
    /// 末尾に L の付いた整数
    Long(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Lit(Lit),
    Sym(&'static str),
}

const SYMS: [&str; 22] = ["&&", "||", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")", "[", "]", ".", ",", "?", ":"];

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let st = i;
            while i < cs.len() && (cs[i].is_alphanumeric() || cs[i] == '_' || cs[i] == '$') {
                i += 1;
            }
            let w: String = cs[st..i].iter().collect();
            out.push(match w.as_str() {
                "true" => Tok::Lit(Lit::Bool(true)),
                "false" => Tok::Lit(Lit::Bool(false)),
                "null" => Tok::Lit(Lit::Null),
                _ => Tok::Ident(w),
            });
        } else if c.is_ascii_digit() {
            let st = i;
            while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '.' || cs[i] == '_') {
                i += 1;
            }
            let w: String = cs[st..i].iter().filter(|c| **c != '_').collect();
            let lower = w.to_ascii_lowercase();
            let lit = if let Some(n) = lower.strip_suffix('l') {
                Lit::Long(parse_int(n).ok_or_else(|| format!("数が読めません: {w}"))?)
            } else if lower.contains('.') || ((lower.ends_with('f') || lower.ends_with('d')) && !lower.starts_with("0x")) {
                Lit::Float(lower.trim_end_matches(['f', 'd']).parse().map_err(|_| format!("数が読めません: {w}"))?)
            } else {
                Lit::Int(parse_int(&lower).ok_or_else(|| format!("数が読めません: {w}"))?)
            };
            out.push(Tok::Lit(lit));
        } else if c == '"' || c == '\'' {
            let q = c;
            i += 1;
            let mut t = String::new();
            loop {
                let Some(&d) = cs.get(i) else { return Err("引用符が閉じていません".into()) };
                i += 1;
                if d == q {
                    break;
                }
                if d == '\\' {
                    let Some(&e) = cs.get(i) else { return Err("引用符が閉じていません".into()) };
                    i += 1;
                    t.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        x => x,
                    });
                } else {
                    t.push(d);
                }
            }
            if q == '"' {
                out.push(Tok::Lit(Lit::Str(t)));
            } else {
                let mut it = t.chars();
                match (it.next(), it.next()) {
                    (Some(ch), None) => out.push(Tok::Lit(Lit::Char(ch))),
                    _ => return Err(format!("文字のリテラルが読めません: '{t}'")),
                }
            }
        } else {
            let rest: String = cs[i..cs.len().min(i + 2)].iter().collect();
            let Some(sym) = SYMS.iter().find(|s| rest.starts_with(**s)) else { return Err(format!("読めない文字: {c}")) };
            i += sym.chars().count();
            out.push(Tok::Sym(sym));
        }
    }
    Ok(out)
}

fn parse_int(s: &str) -> Option<i64> {
    match s.strip_prefix("0x") {
        Some(h) => i64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek_sym(&self, s: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Sym(x)) if *x == s)
    }

    fn eat(&mut self, s: &str) -> bool {
        let hit = self.peek_sym(s);
        if hit {
            self.i += 1;
        }
        hit
    }

    fn expect(&mut self, s: &str) -> Result<(), String> {
        if self.eat(s) { Ok(()) } else { Err(format!("「{s}」がありません")) }
    }

    fn binary(&mut self, level: usize) -> Result<Expr, String> {
        // 弱いものから
        const LEVELS: [&[(&str, Op)]; 6] = [
            &[("||", Op::Or)],
            &[("&&", Op::And)],
            &[("==", Op::Eq), ("!=", Op::Ne)],
            &[("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)],
            &[("+", Op::Add), ("-", Op::Sub)],
            &[("*", Op::Mul), ("/", Op::Div), ("%", Op::Rem)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        'outer: loop {
            for (s, op) in LEVELS[level] {
                if self.eat(s) {
                    let right = self.binary(level + 1)?;
                    left = Expr::Bin(Box::new(left), *op, Box::new(right));
                    continue 'outer;
                }
            }
            return Ok(left);
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut v = Vec::new();
        if self.eat(")") {
            return Ok(v);
        }
        loop {
            v.push(self.binary(0)?);
            if self.eat(")") {
                return Ok(v);
            }
            self.expect(",")?;
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".") {
                let Some(Tok::Ident(name)) = self.t.get(self.i).cloned() else { return Err("「.」の後に名前がありません".into()) };
                self.i += 1;
                e = if self.eat("(") { Expr::Call(Some(Box::new(e)), name, self.args()?) } else { Expr::Field(Box::new(e), name) };
            } else if self.eat("[") {
                let ix = self.binary(0)?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(ix));
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Lit(l)) => {
                self.i += 1;
                Ok(Expr::Lit(l))
            }
            Some(Tok::Ident(n)) => {
                self.i += 1;
                if self.eat("(") { Ok(Expr::Call(None, n, self.args()?)) } else { Ok(Expr::Name(n)) }
            }
            Some(Tok::Sym("(")) => {
                self.i += 1;
                let e = self.binary(0)?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Sym(s)) => Err(format!("ここに「{s}」は書けません")),
            None => Err("式が途中で終わっています".into()),
        }
    }
}

/// 式を読む
pub fn parse(s: &str) -> Result<Expr, String> {
    let t = lex(s)?;
    if t.is_empty() {
        return Err("式が空です".into());
    }
    let mut p = P { t, i: 0 };
    let e = p.binary(0)?;
    if p.i < p.t.len() {
        return Err(match &p.t[p.i] {
            Tok::Sym("?" | ":") => "条件演算子（? :）は使えません".into(),
            Tok::Sym(s) => format!("「{s}」から後が読めません"),
            _ => "式の後に余分なものがあります".into(),
        });
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Box<Expr> {
        Box::new(Expr::Name(s.into()))
    }

    #[test]
    fn parses() {
        assert_eq!(parse("a.b[2].c").unwrap(), Expr::Field(Box::new(Expr::Index(Box::new(Expr::Field(n("a"), "b".into())), Box::new(Expr::Lit(Lit::Int(2))))), "c".into()));
        assert_eq!(parse("s.equals(\"x\\\"y\")").unwrap(), Expr::Call(Some(n("s")), "equals".into(), vec![Expr::Lit(Lit::Str("x\"y".into()))]));
        // 優先順位: && は || より強く、比べるのは足し算より弱い
        assert_eq!(
            parse("a || b && i + 1 > 2").unwrap(),
            Expr::Bin(n("a"), Op::Or, Box::new(Expr::Bin(n("b"), Op::And, Box::new(Expr::Bin(Box::new(Expr::Bin(n("i"), Op::Add, Box::new(Expr::Lit(Lit::Int(1))))), Op::Gt, Box::new(Expr::Lit(Lit::Int(2))))))))
        );
        assert_eq!(parse("!(x == null)").unwrap(), Expr::Not(Box::new(Expr::Bin(n("x"), Op::Eq, Box::new(Expr::Lit(Lit::Null))))));
        assert_eq!(parse("size()").unwrap(), Expr::Call(None, "size".into(), vec![]));
        assert_eq!(parse("10L").unwrap(), Expr::Lit(Lit::Long(10)));
        assert_eq!(parse("1.5").unwrap(), Expr::Lit(Lit::Float(1.5)));
        assert_eq!(parse("'a'").unwrap(), Expr::Lit(Lit::Char('a')));
        assert_eq!(parse("0x1F").unwrap(), Expr::Lit(Lit::Int(31)));
        assert!(parse("a ? b : c").unwrap_err().contains("条件演算子"));
        assert!(parse("a +").is_err());
        assert!(parse("\"abc").is_err());
        assert!(parse("").is_err());
    }
}
