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

/// Python（`#` のコメント、字下げの範囲）
const PY: Lexis = Lexis { word_extra: &['_'], line_comment: Some("#"), block_comment: None, new_keyword: None, scope: Scope::Indent, ..C_LIKE };

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

    /// Rust の `'a` は引用符でない（前の行からの連なりで、行のコメントを外せる）
    #[test]
    fn rust_lifetime_is_not_a_quote() {
        let src = "fn f<'a>(v: &'a [u8]) -> usize { // x\n    v.iter() // y\n        .";
        let ls = src.rfind('\n').unwrap() + 1;
        assert_eq!(crate::text::with_previous_lines(&crate::rust::LX, src, ls, "        "), "v.iter()");
        assert_eq!(crate::text::strip_line_comment(&crate::rust::LX, "fn f<'a>(v: &'a str) { // c"), "fn f<'a>(v: &'a str) { ");
        assert_eq!(crate::text::strip_line_comment(&crate::rust::LX, "impl<'a> X<'a> { // c"), "impl<'a> X<'a> { ");
        assert_eq!(crate::text::strip_line_comment(&C_LIKE, "fn f(v: &'a str) { // c"), "fn f(v: &'a str) { // c", "引用符として読むと外せない");
        assert_eq!(crate::text::strip_line_comment(&crate::rust::LX, "fn f(v: &'a str) { // c"), "fn f(v: &'a str) { ");
    }
}
