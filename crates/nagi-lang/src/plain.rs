//! 型を扱わない言語（予約語と字句だけ。補完はプロジェクトの索引とファイルの中の語から）

use crate::lang::Language;
use crate::text::{C_LIKE, Lexis, Scope};

pub struct Plain {
    pub name: &'static str,
    pub lexis: &'static Lexis,
    pub keywords: &'static [&'static str],
    /// 使う生態系（Kotlin は JVM）
    pub ecosystem: Option<&'static str>,
    /// 同じ名前の定義がオーバーロードか
    pub overloads: bool,
    /// 親の型の読み方（名前と、型引数ごとの文字）
    pub supertypes: fn(&str, Option<&str>) -> Vec<String>,
    pub supertype_texts: fn(&str, &str) -> Vec<String>,
}

impl Language for Plain {
    fn name(&self) -> &'static str {
        self.name
    }

    fn lexis(&self) -> &'static Lexis {
        self.lexis
    }

    fn keywords(&self) -> &'static [&'static str] {
        self.keywords
    }

    fn ecosystem(&self) -> Option<&'static str> {
        self.ecosystem
    }

    fn overloads(&self) -> bool {
        self.overloads
    }

    fn supertypes(&self, src: &str, of: Option<&str>) -> Vec<String> {
        (self.supertypes)(src, of)
    }

    fn supertype_texts(&self, src: &str, of: &str) -> Vec<String> {
        (self.supertype_texts)(src, of)
    }
}

/// Kotlin: `class Name<T>(引数) : Parent(..), Iface<X>, Other by x {` の親の型の名前（`of` なら、その名前の宣言だけ）
pub fn kotlin_supertypes(src: &str, of: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = kotlin_parents(src, of).into_iter().map(|t| t.split('<').next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string()).filter(|t| !t.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// Kotlin: 型 `of` の親の型を、型引数ごとの文字で（`Iface<X>`。呼び出しの括弧と by の後は外す）
pub fn kotlin_supertype_texts(src: &str, of: &str) -> Vec<String> {
    kotlin_parents(src, Some(of))
}

/// Kotlin の親の型の文字（書いた順）
fn kotlin_parents(src: &str, of: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for kw in ["class ", "interface ", "object "] {
        let mut from = 0;
        while let Some(i) = src[from..].find(kw).map(|i| i + from) {
            from = i + kw.len();
            if i > 0 && src[..i].ends_with(word) {
                continue;
            }
            let rest = src[from..].trim_start();
            let name: String = rest.chars().take_while(|c| word(*c)).collect();
            if name.is_empty() || of.is_some_and(|o| o != name) {
                continue;
            }
            // 型引数・主コンストラクタを飛ばして : を探す（{ か行の終わりまで）
            let mut k = name.len();
            let b = rest.as_bytes();
            let mut depth = 0i32;
            while k < b.len() {
                match b[k] {
                    b'<' | b'(' => depth += 1,
                    b'>' | b')' => depth -= 1,
                    b':' if depth == 0 => break,
                    b'{' | b'\n' if depth == 0 => {
                        k = b.len();
                        break;
                    }
                    _ => {}
                }
                k += 1;
            }
            if k >= b.len() {
                continue;
            }
            let head = &rest[k + 1..];
            let head = &head[..head.find(['{', '\n']).unwrap_or(head.len())];
            for part in crate::text::split_top(&crate::text::C_LIKE, head, b',', true) {
                let part = part.split(" by ").next().unwrap_or(part).trim();
                // 呼び出しの括弧（Parent(x)・Repo<A>()）を外す。型引数（<..>）は残す
                let gen_end = part.rfind('>').map_or(0, |g| g + 1);
                let t = match part[gen_end..].find('(') {
                    Some(p) => &part[..gen_end + p],
                    None => part,
                };
                let t = t.trim();
                if t.starts_with(|c: char| c.is_alphabetic()) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// Kotlin（`new` が無い）
const KT: Lexis = Lexis { new_keyword: None, ..C_LIKE };
/// Rust（`'a` は引用符でない）
const RS: Lexis = Lexis { quotes: &['"'], new_keyword: None, ..C_LIKE };
/// Python（`#` のコメント、字下げの範囲）
const PY: Lexis = Lexis { word_extra: &['_'], line_comment: Some("#"), block_comment: None, new_keyword: None, scope: Scope::Indent, ..C_LIKE };

pub static KOTLIN: Plain = Plain {
    name: "kotlin",
    lexis: &KT,
    ecosystem: Some(crate::eco::JVM),
    overloads: true,
    supertypes: kotlin_supertypes,
    supertype_texts: kotlin_supertype_texts,
    keywords: &[
        "as", "break", "class", "companion", "const", "continue", "data", "do", "else", "enum", "false", "for", "fun", "if", "import", "in", "interface", "is", "lateinit",
        "null", "object", "open", "override", "package", "private", "protected", "public", "return", "sealed", "super", "this", "throw", "true", "try", "val", "var", "when",
        "while",
    ],
};

pub static RUST: Plain = Plain {
    name: "rust",
    lexis: &RS,
    ecosystem: None,
    overloads: false,
    supertypes: crate::project::supertypes,
    supertype_texts: crate::project::supertype_texts,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "Self", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
    ],
};

pub static PYTHON: Plain = Plain {
    name: "python",
    lexis: &PY,
    ecosystem: None,
    overloads: false,
    supertypes: crate::project::supertypes,
    supertype_texts: crate::project::supertype_texts,
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
        "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with", "yield",
    ],
};

/// ほかの言語（予約語なし）
pub static OTHER: Plain = Plain { name: "other", lexis: &C_LIKE, keywords: &[], ecosystem: None, overloads: false, supertypes: crate::project::supertypes, supertype_texts: crate::project::supertype_texts };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_parents() {
        let src = "open class Base\nclass Repo<T>(val x: Int) : Base(), Saver<T>, java.io.Serializable {\n}\nobject Single : Other by impl\ninterface I : J\n";
        assert_eq!(kotlin_supertypes(src, Some("Repo")), vec!["Base", "Saver", "Serializable"]);
        assert_eq!(kotlin_supertypes(src, Some("Single")), vec!["Other"]);
        assert_eq!(kotlin_supertypes(src, None), vec!["Base", "J", "Other", "Saver", "Serializable"]);
        assert!(kotlin_supertypes(src, Some("Base")).is_empty());
        assert_eq!(kotlin_supertype_texts("class C : Repo<User, Long>(), java.io.Closeable {\n", "C"), vec!["Repo<User, Long>", "java.io.Closeable"]);
    }

    /// Rust の `'a` は引用符でない（前の行からの連なりで、行のコメントを外せる）
    #[test]
    fn rust_lifetime_is_not_a_quote() {
        let src = "fn f<'a>(v: &'a [u8]) -> usize { // x\n    v.iter() // y\n        .";
        let ls = src.rfind('\n').unwrap() + 1;
        assert_eq!(crate::text::with_previous_lines(RUST.lexis, src, ls, "        "), "v.iter()");
        assert_eq!(crate::text::strip_line_comment(RUST.lexis, "fn f<'a>(v: &'a str) { // c"), "fn f<'a>(v: &'a str) { ");
        assert_eq!(crate::text::strip_line_comment(RUST.lexis, "impl<'a> X<'a> { // c"), "impl<'a> X<'a> { ");
        assert_eq!(crate::text::strip_line_comment(&C_LIKE, "fn f(v: &'a str) { // c"), "fn f(v: &'a str) { // c", "引用符として読むと外せない");
        assert_eq!(crate::text::strip_line_comment(RUST.lexis, "fn f(v: &'a str) { // c"), "fn f(v: &'a str) { ");
    }
}
