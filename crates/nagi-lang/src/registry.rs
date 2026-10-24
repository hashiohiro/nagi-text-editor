//! 言語を選ぶ

use nagi_syntax::Lang;

use crate::lang::Language;

/// ファイルの言語（構文の言語。分からなければパスから）の差し込み口
pub fn for_file(lang: Option<Lang>) -> &'static dyn Language {
    match lang {
        Some(Lang::Java) => &crate::java::lang::JAVA,
        Some(Lang::TypeScript | Lang::Tsx) => &crate::ts::lang::TS,
        Some(Lang::JavaScript) => &crate::ts::lang::JS,
        Some(Lang::Kotlin) => &crate::kotlin::KOTLIN,
        Some(Lang::Rust) => &crate::rust::RUST,
        Some(Lang::Python) => &crate::plain::PYTHON,
        Some(Lang::Html) => &crate::angular::TEMPLATE,
        _ => &crate::plain::OTHER,
    }
}

/// 言語を選ぶ: コンソール（`console` は接続先の種類）なら接続先の言語（パスで決まらない）、ほかはファイルの言語
pub fn pick(file: Option<Lang>, console: Option<Option<nagi_workers::db::Kind>>) -> &'static dyn Language {
    match console {
        Some(kind) => for_console(kind),
        None => for_file(file),
    }
}

/// 接続先の種類の言語（Salesforce は SOQL、ほかは方言ごとの SQL）
pub fn for_console(kind: Option<nagi_workers::db::Kind>) -> &'static dyn Language {
    use nagi_workers::db::Kind;
    match kind {
        Some(Kind::Salesforce) => &crate::soql::SOQL,
        Some(Kind::Oracle) => &crate::sql::SQL_ORACLE,
        Some(Kind::Postgres) => &crate::sql::SQL_POSTGRES,
        Some(Kind::Sqlite) => &crate::sql::SQL_SQLITE,
        None => &crate::sql::SQL,
    }
}

/// パス（根からの相対パスでもよい）の言語の差し込み口
pub fn for_path(path: &str) -> &'static dyn Language {
    for_file(Lang::detect(std::path::Path::new(path)))
}
