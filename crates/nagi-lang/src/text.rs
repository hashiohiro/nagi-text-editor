//! 文字を読む部品（構文木を作らずに、カーソルの前の式・宣言の文字を読む）。
//!
//! 言語ごとに違う字句（コメント・引用符・語の文字・範囲の決まり方）は `Lexis` で受け取る。

use nagi_syntax::defs::Receiver;

/// 範囲（型・関数の本体）の決まり方
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `{ }`（Java・TypeScript・Kotlin・C#・Rust・Go）
    Braces,
    /// 字下げ（Python・YAML）
    Indent,
}

/// 言語の字句の規則
#[derive(Debug)]
pub struct Lexis {
    /// 英数字のほかに語に使える文字
    pub word_extra: &'static [char],
    /// 行のコメントの始まり
    pub line_comment: Option<&'static str>,
    /// 塊のコメントの (始まり, 終わり)
    pub block_comment: Option<(&'static str, &'static str)>,
    /// 文字列の引用符（中では `\` で逃がす）
    pub quotes: &'static [char],
    /// 作る式の予約語（`new X()`。無い言語は None）
    pub new_keyword: Option<&'static str>,
    /// 範囲の決まり方
    pub scope: Scope,
    /// 名前の大文字小文字を区別するか（SQL・PowerShell は区別しない）
    pub case_sensitive: bool,
}

impl Lexis {
    /// 語の文字か
    pub fn is_word(&self, c: char) -> bool {
        c.is_alphanumeric() || self.word_extra.contains(&c)
    }

    /// `s` の `i` にある引用符から始まる文字列の、閉じる引用符の次の位置（閉じなければ終わり）
    fn skip_quoted(&self, s: &str, i: usize) -> usize {
        let b = s.as_bytes();
        let q = b[i];
        let mut k = i + 1;
        while k < b.len() && b[k] != q {
            k += 1 + usize::from(b[k] == b'\\');
        }
        (k + 1).min(b.len())
    }

    fn is_quote(&self, b: u8) -> bool {
        b.is_ascii() && self.quotes.contains(&(b as char))
    }
}

/// Java・C# など（`"` と `'`）
pub const C_LIKE: Lexis = Lexis {
    word_extra: &['_', '$'],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    new_keyword: Some("new"),
    scope: Scope::Braces,
    case_sensitive: true,
};

/// TypeScript・JavaScript（テンプレートの文字列の ` も）
pub const JS_LIKE: Lexis = Lexis { quotes: &['"', '\'', '`'], ..C_LIKE };

/// 外側の括弧（`( [ {`、`angle` なら `< >` も。`=>` の > は数えない）と文字列の外で、`sep` ごとに分ける
pub fn split_top<'a>(lx: &Lexis, s: &'a str, sep: u8, angle: bool) -> Vec<&'a str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0i32, 0, 0);
    while i < b.len() {
        match b[i] {
            c if lx.is_quote(c) => {
                i = lx.skip_quoted(s, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b'<' if angle => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if angle && i > 0 && b[i - 1] != b'=' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// 呼び出しの引数の並び（`a, f(b, c), "x,y"`）を外側の , で分け、前後の空白を外す（空なら空の並び）
pub fn split_args(lx: &Lexis, a: &str) -> Vec<String> {
    if a.trim().is_empty() {
        return Vec::new();
    }
    split_top(lx, a, b',', false).into_iter().map(|x| x.trim().to_string()).collect()
}

/// 行の行コメントを外す（文字列の中は見ない）
pub fn strip_line_comment<'a>(lx: &Lexis, l: &'a str) -> &'a str {
    let Some(lc) = lx.line_comment else { return l };
    let b = l.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if lx.is_quote(b[i]) {
            i = lx.skip_quoted(l, i);
            continue;
        }
        if b[i..].starts_with(lc.as_bytes()) {
            return &l[..i];
        }
        i += 1;
    }
    l
}

/// コメント（行・塊）を外す（文字列の中は見ない）
pub fn strip_comments(lx: &Lexis, s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if lx.is_quote(b[i]) {
            let e = lx.skip_quoted(s, i);
            out.push_str(&s[i..e]);
            i = e;
        } else if lx.line_comment.is_some_and(|lc| b[i..].starts_with(lc.as_bytes())) {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if let Some((open, close)) = lx.block_comment.filter(|(o, _)| b[i..].starts_with(o.as_bytes())) {
            i += open.len();
            i = s[i..].find(close).map_or(b.len(), |k| i + k + close.len());
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// `...Foo<A<B>>` の終わりの `>` に対応する `<` の位置（文の切れ目 `; { } =` を越えない）
pub fn generic_open(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (k, c) in s.char_indices().rev() {
        match c {
            '>' => depth += 1,
            '<' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            ';' | '{' | '}' | '=' => return None,
            _ => {}
        }
    }
    None
}

/// `s[..end]` の終わりの `)` に対応する `(` の位置
fn paren_open(s: &str, end: usize) -> Option<usize> {
    let mut depth = 0;
    for (k, c) in s[..end].char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// `s` の終わりの語の始まり
fn word_start(lx: &Lexis, s: &str) -> Option<usize> {
    s.char_indices().rev().take_while(|(_, c)| lx.is_word(*c)).last().map(|x| x.0)
}

/// `s` が `new` の予約語で終わるか（語の途中でなく）
fn ends_with_new(lx: &Lexis, s: &str) -> bool {
    lx.new_keyword.is_some_and(|k| s.ends_with(k) && !s[..s.len() - k.len()].ends_with(|c| lx.is_word(c)))
}

/// `.` の前の式（`s`）の連なりの頭の位置（`a.b(x).c` の a、`new X<..>(..).c` の new）
pub fn chain_start(lx: &Lexis, s: &str) -> Option<usize> {
    let mut end = s.len();
    loop {
        let mut i = end;
        if s[..i].ends_with(')') {
            i = paren_open(s, i)?;
            if s[..i].ends_with('>') {
                i = generic_open(&s[..i])?;
            }
        }
        let ws = word_start(lx, &s[..i])?;
        if ws > 0 && s[..ws].ends_with('.') {
            end = ws - 1;
        } else {
            let t = s[..ws].trim_end();
            if ends_with_new(lx, t) {
                return Some(t.len() - lx.new_keyword.map_or(0, str::len));
            }
            return Some(ws);
        }
    }
}

/// 連なり（`.` の前の式）の各部分の呼び出しの引数の文字（呼び出しでなければ None）。頭から順に
pub fn chain_args(lx: &Lexis, s: &str) -> Vec<Option<Vec<String>>> {
    let mut out = Vec::new();
    let mut end = s.len();
    loop {
        let mut i = end;
        let mut args = None;
        if s[..i].ends_with(')') {
            let Some(o) = paren_open(s, i) else { break };
            args = Some(split_args(lx, &s[o + 1..i - 1]));
            i = o;
            if s[..i].ends_with('>') {
                let Some(g) = generic_open(&s[..i]) else { break };
                i = g;
            }
        }
        let Some(ws) = word_start(lx, &s[..i]) else { break };
        out.push(args);
        if ws > 0 && s[..ws].ends_with('.') {
            end = ws - 1;
        } else {
            break;
        }
    }
    out.reverse();
    out
}

/// `.` の前の、名前を `.` でつないだだけの連なり（`java.util`・`com.x.Foo`。呼び出しや `x().a` の途中は None）
pub fn dotted_chain<'a>(lx: &Lexis, s: &'a str) -> Option<&'a str> {
    let i = s.char_indices().rev().take_while(|(_, c)| lx.is_word(*c) || *c == '.').last()?.0;
    let i = i + s[i..].find(|c| lx.is_word(c))?;
    if s[..i].ends_with('.') || s.ends_with('.') || s[i..].contains("..") {
        return None;
    }
    Some(&s[i..])
}

/// 行の中の `.` の前 `before` が `.` で始まる（または空の）とき、前の行からの連なりを足す
/// （`list.stream()\n    .filter(..)\n    .` → `list.stream().filter(..)`）。`line_start` は行の始まりの位置
pub fn with_previous_lines(lx: &Lexis, src: &str, line_start: usize, before: &str) -> String {
    let t = before.trim_start();
    if !(t.is_empty() || t.starts_with('.')) {
        return before.to_string();
    }
    // 前の 30 行まで（コメントを外して字下げを詰める）
    let mut lines: Vec<&str> = Vec::new();
    let mut end = line_start.min(src.len());
    while end > 0 && lines.len() < 30 {
        let ls = src[..end - 1].rfind('\n').map_or(0, |i| i + 1);
        lines.push(strip_line_comment(lx, &src[ls..end - 1]).trim());
        end = ls;
    }
    lines.reverse();
    let joined = lines.concat();
    // 文の切れ目（外側の ; { } = , と、閉じていない ( [）の後から
    let mut depth = 0i32;
    let mut cut = 0;
    for (i, c) in joined.char_indices().rev() {
        match c {
            ')' | ']' | '}' if depth > 0 || c != '}' => depth += 1,
            '(' | '[' | '{' if depth > 0 => depth -= 1,
            '(' | '[' | '{' | '}' | ';' | '=' | ',' if depth == 0 => {
                cut = i + 1;
                break;
            }
            _ => {}
        }
    }
    format!("{}{t}", joined[cut..].trim())
}

/// 語として出てくる最初の位置
pub fn word_offset(lx: &Lexis, text: &str, name: &str) -> Option<usize> {
    text.match_indices(name).map(|(i, _)| i).find(|&i| !text[..i].ends_with(|c| lx.is_word(c)) && !text[i + name.len()..].starts_with(|c| lx.is_word(c)))
}

/// `.` の前の式を受け手にする（`a.b().c` → Call・Member の連なり）。`dot` は `.` の位置（行の中の byte）。
/// `var` は変数の型。`new X<..>(..)` の頭は `var("new X<..>")`（無ければ X）
pub fn receiver_before(lx: &Lexis, line: &str, dot: usize, var: &dyn Fn(&str) -> Option<String>) -> Option<Receiver> {
    let s = &line[..dot];
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut end = s.len();
    let mut newed = None;
    loop {
        let mut i = end;
        let mut call = false;
        // 呼び出しの括弧を飛ばす
        if s[..i].ends_with(')') {
            i = paren_open(s, i)?;
            call = true;
        }
        let paren = i;
        // new ArrayList<String>() の型引数
        if call && s[..i].ends_with('>') {
            i = generic_open(&s[..i])?;
        }
        let word_start = word_start(lx, &s[..i])?;
        parts.push((s[word_start..i].to_string(), call));
        if word_start > 0 && s[..word_start].ends_with('.') {
            end = word_start - 1;
        } else {
            let t = s[..word_start].trim_end();
            if call && ends_with_new(lx, t) {
                newed = Some(s[word_start..paren].to_string());
            }
            break;
        }
    }
    parts.reverse();
    let mut it = parts.into_iter();
    let (base, call) = it.next()?;
    let mut r = if let Some(n) = newed {
        Receiver::Type(var(&format!("new {n}")).unwrap_or(base))
    } else if call {
        Receiver::Call(Box::new(Receiver::This), base)
    } else if base == "this" || base == "super" || base == "self" {
        Receiver::This
    } else if let Some(t) = var(&base) {
        Receiver::Type(t)
    } else if base.chars().next().is_some_and(char::is_uppercase) {
        Receiver::Type(base)
    } else {
        Receiver::Member(Box::new(Receiver::This), base)
    };
    for (name, call) in it {
        r = if call { Receiver::Call(Box::new(r), name) } else { Receiver::Member(Box::new(r), name) };
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads() {
        assert_eq!(chain_start(&C_LIKE, "  x = a.b(c(d)).e"), Some(6));
        assert_eq!(chain_start(&C_LIKE, "  y = new ArrayList<String>(n).stream()"), Some(6));
    }

    #[test]
    fn previous_lines() {
        let src = "  var n = list.stream() // c\n      .filter(x -> {\n        return true;\n      })\n      .map(f)\n      .";
        let ls = src.rfind('\n').unwrap() + 1;
        assert_eq!(with_previous_lines(&C_LIKE, src, ls, "      "), "list.stream().filter(x -> {return true;}).map(f)");
        assert_eq!(with_previous_lines(&C_LIKE, src, ls, "  a.b"), "  a.b", "行の中で足りていればそのまま");
        let src = "  get(`http://x`)\n  .";
        assert_eq!(with_previous_lines(&JS_LIKE, src, src.rfind('\n').unwrap() + 1, "  "), "get(`http://x`)", "文字列の中の // はコメントでない");
    }

    #[test]
    fn chains() {
        assert_eq!(dotted_chain(&C_LIKE, "  @io.swagger.v3"), Some("io.swagger.v3"));
        assert_eq!(dotted_chain(&C_LIKE, "List<com.x"), Some("com.x"));
        assert_eq!(dotted_chain(&C_LIKE, "x = java.awt.Color"), Some("java.awt.Color"));
        assert_eq!(dotted_chain(&C_LIKE, "foo().bar"), None);
        assert_eq!(chain_args(&C_LIKE, "a.b(x, f(y, z)).c()"), vec![None, Some(vec!["x".to_string(), "f(y, z)".to_string()]), Some(vec![])]);
    }

    #[test]
    fn splits_and_comments() {
        assert_eq!(split_args(&C_LIKE, r#"a, "x,\"y", f(b, c)"#), vec!["a", r#""x,\"y""#, "f(b, c)"]);
        assert_eq!(split_top(&JS_LIKE, "Map<K, V>, (a, b) => c, `x,y`", b',', true), vec!["Map<K, V>", " (a, b) => c", " `x,y`"]);
        assert_eq!(strip_comments(&JS_LIKE, "a /* b */ c // d\ne 'f//g'"), "a  c \ne 'f//g'");
        assert_eq!(strip_line_comment(&C_LIKE, r#"x("a//b") // c"#), r#"x("a//b") "#);
        assert_eq!(strip_line_comment(&C_LIKE, "x(\"例\\あ\") // 例"), "x(\"例\\あ\") ", "文字の途中で切らない");
        assert_eq!(strip_comments(&C_LIKE, "a // 例\n/* 例 */b"), "a \nb");
    }
}
