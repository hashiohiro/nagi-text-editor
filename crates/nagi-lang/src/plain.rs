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
}

/// Kotlin（`new` が無い）
const KT: Lexis = Lexis { new_keyword: None, member: &[".", "?."], ..C_LIKE };
/// Rust（`'a` は引用符でない）
const RS: Lexis = Lexis { quotes: &['"'], member: &[".", "::"], new_keyword: None, ..C_LIKE };
/// Python（`#` のコメント、字下げの範囲）
const PY: Lexis = Lexis { word_extra: &['_'], line_comment: Some("#"), block_comment: None, new_keyword: None, scope: Scope::Indent, ..C_LIKE };

pub static KOTLIN: Plain = Plain {
    name: "kotlin",
    lexis: &KT,
    ecosystem: Some(crate::eco::JVM),
    overloads: true,
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
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
        "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with", "yield",
    ],
};

/// ほかの言語（予約語なし）
pub static OTHER: Plain = Plain { name: "other", lexis: &C_LIKE, keywords: &[], ecosystem: None, overloads: false };
