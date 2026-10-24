//! 言語を選ぶ

use nagi_syntax::Lang;

use crate::lang::Language;

/// ファイルの言語（構文の言語。分からなければパスから）の差し込み口
pub fn for_file(lang: Option<Lang>) -> &'static dyn Language {
    match lang {
        Some(Lang::Java) => &crate::java::lang::JAVA,
        Some(Lang::TypeScript | Lang::Tsx) => &crate::ts::lang::TS,
        Some(Lang::JavaScript) => &crate::ts::lang::JS,
        Some(Lang::Kotlin) => &crate::plain::KOTLIN,
        Some(Lang::Rust) => &crate::plain::RUST,
        Some(Lang::Python) => &crate::plain::PYTHON,
        _ => &crate::plain::OTHER,
    }
}
