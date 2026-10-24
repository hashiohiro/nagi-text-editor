//! SQL（DataGrip のように、文の中の場所に合わせて候補を出す）。言語はファイルのパスでなく SQL のコンソールで決まり、
//! ライブラリにあたるのは接続先のスキーマ・テーブル・列（`Catalog`。接続ごとに持ち、まだ読んでいなければ読みたいものを伝える）。
//!
//! 場所（カーソルの前の語を、文字列・注釈を飛ばして見る）:
//! - FROM・JOIN・UPDATE・INTO の後: 今のスキーマのテーブル・ビュー、WITH の名前、スキーマ
//! - その後（テーブルを書いた後）: WHERE・JOIN などのキーワードを先に
//! - SELECT・WHERE・ON・SET・BY などの後・括弧の中: 文に出てくる表の列、別名、関数、キーワード
//! - `別名.`・`テーブル.`: その表の列。`スキーマ.`: そのスキーマのテーブル・ビュー。`スキーマ.テーブル.`: その列
//! - 文の頭: SELECT・INSERT INTO などの文の始まり

use nagi_workers::db::{Column, Kind, Objects};

use crate::lang::{Cx, Item, Language};
use crate::text::{C_LIKE, Lexis};

/// SQL の字句（`--` の注釈、`'` の文字列、名前の大文字小文字は区別しない）
pub const LX: Lexis = Lexis { word_extra: &['_', '$', '#'], line_comment: Some("--"), quotes: &['\''], new_keyword: None, case_sensitive: false, ..C_LIKE };

/// 接続先のスキーマ・テーブル・列（読んだもの）
pub trait Catalog {
    /// 今のスキーマ
    fn schema(&self) -> &str;
    /// スキーマの一覧（読んでいれば）
    fn schemas(&self) -> Option<&[String]>;
    /// スキーマのテーブル・ビューなど（読んでいれば）
    fn objects(&self, schema: &str) -> Option<&Objects>;
    /// テーブルの列（読んでいれば）
    fn columns(&self, schema: &str, table: &str) -> Option<&[Column]>;
    /// 子のリレーション（名前, 子のテーブル）（読んでいれば。Salesforce）
    fn relations(&self, _schema: &str, _table: &str) -> Option<&[(String, String)]> {
        None
    }
    /// スキーマの名前か（大文字小文字は区別しない）
    fn is_schema(&self, name: &str) -> bool;
    /// 書かれた (スキーマ, 名前) を、読んだ (スキーマ, テーブル) に
    fn resolve(&self, schema: Option<&str>, name: &str) -> (String, String);
    /// 読みたいもの（まだ読んでいなければ、画面の側が読み始め、届いたら一覧を作り直す）
    fn want(&self, w: Want);
}

/// 読みたいもの
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// スキーマのテーブル・ビューなど
    Objects(String),
    /// (スキーマ, テーブル) の列
    Columns(String, String),
    /// (スキーマ, テーブル) の子のリレーション
    Relations(String, String),
}

/// SQL のコンソールで補完する場所（Cx の console）
pub struct Console<'a> {
    pub catalog: &'a dyn Catalog,
    /// カーソルのある文と、その中のカーソルの位置
    pub stmt: &'a str,
    pub cur: usize,
}

/// SQL の差し込み口（手順は丸ごと SQL のもの。方言は接続先の種類ごとの設定）
pub struct Sql {
    pub dialect: &'static Dialect,
}

pub static SQL: Sql = Sql { dialect: &GENERIC };
pub static SQL_ORACLE: Sql = Sql { dialect: &ORACLE };
pub static SQL_POSTGRES: Sql = Sql { dialect: &POSTGRES };
pub static SQL_SQLITE: Sql = Sql { dialect: &SQLITE };

impl Language for Sql {
    fn name(&self) -> &'static str {
        self.dialect.name
    }

    fn lexis(&self) -> &'static Lexis {
        &LX
    }

    fn complete_all(&self, cx: &Cx) -> Option<Vec<Item>> {
        let c = cx.console.as_ref()?;
        Some(complete(self.dialect, c.stmt, c.cur, cx.prefix, c.catalog))
    }
}

/// 識別子（必要なら "" で囲む）
pub fn ident(s: &str) -> String {
    let plain = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#');
    let same_case = s.chars().all(|c| !c.is_ascii_lowercase()) || s.chars().all(|c| !c.is_ascii_uppercase());
    if plain && same_case { s.to_string() } else { format!("\"{}\"", s.replace('"', "\"\"")) }
}

/// いつも "" で囲んだ名前
pub fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// SQL の字句（注釈・空白は捨てる）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    /// 語（キーワードか名前）
    Word(String),
    /// "名前"
    Quoted(String),
    /// '文字列'
    Str,
    Num,
    Punct(char),
}

impl Tok {
    /// 名前として読めるなら、その名前
    pub fn name(&self) -> Option<&str> {
        match self {
            Tok::Word(w) | Tok::Quoted(w) => Some(w),
            _ => None,
        }
    }

    /// 語なら大文字にしたもの
    pub fn upper(&self) -> Option<String> {
        match self {
            Tok::Word(w) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    }
}

/// 字句に分ける。2 つ目は、文字列・"名前"・注釈の途中で終わったか（その中では補完しない）
pub fn tokenize(s: &str) -> (Vec<Tok>, bool) {
    tokenize_with(s, &GENERIC)
}

/// `tokenize` を方言で（$$…$$・q'[…]' の文字列）
pub fn tokenize_with(s: &str, d: &Dialect) -> (Vec<Tok>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap_or(' ');
        // PostgreSQL: $$…$$・$tag$…$tag$
        if d.dollar_quote && c == '$' {
            let tag_end = rest[1..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_')).map(|k| k + 1);
            if let Some(te) = tag_end.filter(|&te| rest[te..].starts_with('$')) {
                let tag = &rest[..=te];
                match rest[tag.len()..].find(tag) {
                    Some(n) => {
                        out.push(Tok::Str);
                        i += tag.len() + n + tag.len();
                        continue;
                    }
                    None => return (out, true),
                }
            }
        }
        // Oracle: q'[…]'・q'{…}'・q'(…)'・q'<…>'・q'!…!'
        if d.q_quote && (rest.starts_with("q'") || rest.starts_with("Q'")) && rest.len() > 2 {
            let open = rest[2..].chars().next().unwrap_or(' ');
            let close = match open {
                '[' => ']',
                '{' => '}',
                '(' => ')',
                '<' => '>',
                c => c,
            };
            match rest[3..].find(&format!("{close}'")) {
                Some(n) => {
                    out.push(Tok::Str);
                    i += 3 + n + 2;
                    continue;
                }
                None => return (out, true),
            }
        }
        if c.is_whitespace() {
            i += c.len_utf8();
        } else if rest.starts_with("--") {
            match rest.find('\n') {
                Some(n) => i += n,
                None => return (out, true),
            }
        } else if rest.starts_with("/*") {
            match rest[2..].find("*/") {
                Some(n) => i += 2 + n + 2,
                None => return (out, true),
            }
        } else if c == '\'' {
            // '' は文字列の中の '
            let mut j = 1;
            loop {
                match rest[j..].find('\'') {
                    Some(n) if rest[j + n + 1..].starts_with('\'') => j += n + 2,
                    Some(n) => {
                        j += n + 1;
                        break;
                    }
                    None => return (out, true),
                }
            }
            out.push(Tok::Str);
            i += j;
        } else if c == '"' {
            match rest[1..].find('"') {
                Some(n) => {
                    out.push(Tok::Quoted(rest[1..1 + n].to_string()));
                    i += n + 2;
                }
                None => return (out, true),
            }
        } else if c.is_alphabetic() || c == '_' {
            let n = rest.char_indices().find(|(_, ch)| !(ch.is_alphanumeric() || matches!(ch, '_' | '$' | '#'))).map_or(rest.len(), |x| x.0);
            out.push(Tok::Word(rest[..n].to_string()));
            i += n;
        } else if c.is_ascii_digit() {
            let n = rest.char_indices().find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '.')).map_or(rest.len(), |x| x.0);
            out.push(Tok::Num);
            i += n;
        } else {
            out.push(Tok::Punct(c));
            i += c.len_utf8();
        }
    }
    (out, false)
}

/// 別名にならない語（テーブルの後に来るキーワード）
const RESERVED: &[&str] = &[
    "WHERE", "ON", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "NATURAL", "GROUP", "ORDER", "HAVING", "LIMIT", "OFFSET", "FETCH", "UNION", "INTERSECT", "EXCEPT",
    "MINUS", "SET", "VALUES", "USING", "WINDOW", "FOR", "CONNECT", "START", "RETURNING", "SELECT", "FROM", "AND", "OR", "AS", "WITH", "LATERAL", "PARTITION", "SAMPLE",
];

/// 文に出てくる表（FROM・JOIN・UPDATE・INTO の後）と WITH の名前
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    /// WITH で付けた名前（列は分からない）
    pub cte: bool,
}

/// `(` から対の `)` の次まで
fn skip_parens(t: &[Tok], mut i: usize) -> usize {
    let mut depth = 0;
    while i < t.len() {
        match t[i] {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => {
                depth -= 1;
                if depth <= 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

pub fn table_refs(t: &[Tok]) -> Vec<TableRef> {
    let mut out = Vec::new();
    let up = |i: usize| t.get(i).and_then(Tok::upper);
    let mut i = 0;
    while i < t.len() {
        match up(i).as_deref() {
            // WITH [RECURSIVE] 名前 [(列)] AS (...), 名前 AS (...)
            Some("WITH") => {
                i += 1;
                if up(i).as_deref() == Some("RECURSIVE") {
                    i += 1;
                }
                while let Some(n) = t.get(i).and_then(Tok::name) {
                    out.push(TableRef { schema: None, name: n.to_string(), alias: None, cte: true });
                    i += 1;
                    if t.get(i) == Some(&Tok::Punct('(')) {
                        i = skip_parens(t, i);
                    }
                    if up(i).as_deref() == Some("AS") {
                        i += 1;
                    }
                    if t.get(i) == Some(&Tok::Punct('(')) {
                        // 中の問い合わせの表も（その中で補完するとき）
                        let end = skip_parens(t, i);
                        out.extend(table_refs(&t[i + 1..end.saturating_sub(1).max(i + 1)]));
                        i = end;
                    }
                    if t.get(i) != Some(&Tok::Punct(',')) {
                        break;
                    }
                    i += 1;
                }
            }
            Some(k @ ("FROM" | "JOIN" | "UPDATE" | "INTO" | "TABLE")) => {
                let list = k == "FROM";
                i += 1;
                loop {
                    if t.get(i) == Some(&Tok::Punct('(')) {
                        // 副問い合わせ（中は先へ進んでから読む）。別名だけ飛ばす
                        let end = skip_parens(t, i);
                        out.extend(table_refs(&t[i + 1..end.saturating_sub(1).max(i + 1)]));
                        i = end;
                        if up(i).as_deref() == Some("AS") {
                            i += 1;
                        }
                        if t.get(i).and_then(Tok::name).is_some() && !up(i).is_some_and(|u| RESERVED.contains(&u.as_str())) {
                            i += 1;
                        }
                    } else if let Some(first) = t.get(i).and_then(Tok::name).filter(|_| !up(i).is_some_and(|u| RESERVED.contains(&u.as_str()))) {
                        let mut parts = vec![first.to_string()];
                        i += 1;
                        while t.get(i) == Some(&Tok::Punct('.')) {
                            match t.get(i + 1).and_then(Tok::name) {
                                Some(n) => {
                                    parts.push(n.to_string());
                                    i += 2;
                                }
                                None => {
                                    i += 1;
                                    break;
                                }
                            }
                        }
                        // テーブル関数・INSERT の列の並び
                        if t.get(i) == Some(&Tok::Punct('(')) {
                            i = skip_parens(t, i);
                        }
                        if up(i).as_deref() == Some("AS") {
                            i += 1;
                        }
                        let alias = t.get(i).and_then(Tok::name).filter(|_| !up(i).is_some_and(|u| RESERVED.contains(&u.as_str()))).map(str::to_string);
                        if alias.is_some() {
                            i += 1;
                        }
                        let name = parts.pop().unwrap_or_default();
                        let schema = parts.pop();
                        out.push(TableRef { schema, name, alias, cte: false });
                    } else {
                        break;
                    }
                    if list && t.get(i) == Some(&Tok::Punct(',')) {
                        i += 1;
                        continue;
                    }
                    break;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// 結果の表を直して書き戻せる文なら、その表 (スキーマ, テーブル)。
/// 1 つのテーブルからの SELECT で、選ぶのが `*` か列の名前だけ（式・列の別名・結合・集計・WITH・副問い合わせは書き戻せない）
pub fn edit_target(sql: &str) -> Option<(Option<String>, String)> {
    let (t, open) = tokenize(sql);
    let t: Vec<Tok> = t.into_iter().filter(|x| *x != Tok::Punct(';')).collect();
    if open || t.first()?.upper().as_deref() != Some("SELECT") {
        return None;
    }
    let deny = ["JOIN", "UNION", "INTERSECT", "EXCEPT", "MINUS", "GROUP", "HAVING", "DISTINCT", "CONNECT", "WITH", "PIVOT", "UNPIVOT"];
    if t.iter().any(|x| x.upper().is_some_and(|u| deny.contains(&u.as_str()))) {
        return None;
    }
    let from = t.iter().position(|x| x.upper().as_deref() == Some("FROM"))?;
    // 選ぶもの: `*`・`名前`・`a.名前`・`a.*` を , で区切ったもの
    for item in t[1..from].split(|x| *x == Tok::Punct(',')) {
        let ok = !item.is_empty()
            && item.iter().enumerate().all(|(k, x)| if k % 2 == 1 { *x == Tok::Punct('.') } else { x.name().is_some() || (*x == Tok::Punct('*') && k + 1 == item.len()) })
            && item.len() % 2 == 1;
        if !ok {
            return None;
        }
    }
    // FROM の後は 1 つのテーブル（副問い合わせ・, の並びは不可）
    let refs = table_refs(&t[from..]);
    if refs.len() != 1 || refs[0].cte || t.get(from + 1) == Some(&Tok::Punct('(')) {
        return None;
    }
    let r = refs.into_iter().next()?;
    Some((r.schema, r.name))
}

/// カーソルのある場所
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ctx {
    /// 文の頭
    Start,
    /// テーブルを書くところ（FROM・JOIN などの直後・FROM の , の後）
    Tables,
    /// テーブルを書いた後（WHERE・JOIN などが続く）
    AfterTable,
    /// 列・式を書くところ
    Columns,
    /// 式を書いた後（AS・FROM・AND などが続く）
    AfterExpr,
    /// `a.`・`s.t.` の後（`.` の前の名前）
    Qualified(Vec<String>),
    /// データ型を書くところ（CAST(x AS ・PostgreSQL の :: ・CREATE TABLE の列の名前の後）
    Types,
    /// PRAGMA の後（SQLite）
    Pragma,
}

const TABLE_KW: &[&str] = &["FROM", "JOIN", "UPDATE", "INTO", "TABLE", "DESC", "DESCRIBE"];
const EXPR_KW: &[&str] = &[
    "SELECT", "WHERE", "ON", "AND", "OR", "BY", "HAVING", "SET", "WHEN", "THEN", "ELSE", "CASE", "NOT", "BETWEEN", "LIKE", "ILIKE", "IN", "IS", "RETURNING", "DISTINCT", "VALUES",
    "USING", "PRIOR",
];

/// カーソルの前の字句から、場所を決める（方言の場所も）
pub fn context_with(before: &[Tok], d: &Dialect) -> Ctx {
    let n = before.len();
    // PostgreSQL の x::型
    if d.double_colon_cast && n >= 2 && before[n - 1] == Tok::Punct(':') && before[n - 2] == Tok::Punct(':') {
        return Ctx::Types;
    }
    let up = |t: &Tok| t.upper().unwrap_or_default();
    if !d.pragmas.is_empty() && before.last().is_some_and(|t| up(t) == "PRAGMA") {
        return Ctx::Pragma;
    }
    // 閉じていない ( の位置
    let mut depth = 0;
    let mut open = None;
    for (k, t) in before.iter().enumerate().rev() {
        match t {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') if depth == 0 => {
                open = Some(k);
                break;
            }
            Tok::Punct('(') => depth -= 1,
            _ => {}
        }
    }
    if let Some(o) = open {
        // CAST(x AS ・TRY_CAST(x AS
        if before.last().is_some_and(|t| up(t) == "AS") && o > 0 && matches!(up(&before[o - 1]).as_str(), "CAST" | "TRY_CAST") {
            return Ctx::Types;
        }
        // CREATE TABLE t ( 列 ・, 列
        let create = before.len() >= 2 && up(&before[0]) == "CREATE" && before[..o].iter().any(|t| up(t) == "TABLE");
        if create && n >= 2 && before[n - 1].name().is_some() && matches!(before[n - 2], Tok::Punct('(' | ',')) && n - 2 >= o {
            return Ctx::Types;
        }
    }
    context(before)
}

/// カーソルの前の字句から、場所を決める
pub fn context(before: &[Tok]) -> Ctx {
    if before.last() == Some(&Tok::Punct('.')) {
        let mut parts = Vec::new();
        let mut j = before.len();
        while j >= 2 && before[j - 1] == Tok::Punct('.') {
            match before[j - 2].name() {
                Some(n) => {
                    parts.insert(0, n.to_string());
                    j -= 2;
                }
                None => break,
            }
        }
        if !parts.is_empty() {
            return Ctx::Qualified(parts);
        }
    }
    // 直前が式の終わり（名前・数・文字列・閉じ括弧・*）か
    let after_expr = match before.last() {
        Some(Tok::Word(w)) => !EXPR_KW.contains(&w.to_ascii_uppercase().as_str()) && !TABLE_KW.contains(&w.to_ascii_uppercase().as_str()),
        Some(Tok::Quoted(_) | Tok::Num | Tok::Str | Tok::Punct(')' | '*')) => true,
        _ => false,
    };
    let mut depth = 0;
    for (k, t) in before.iter().enumerate().rev() {
        match t {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') => {
                if depth == 0 {
                    // 括弧の中（関数の引数・IN の並び・INSERT の列）
                    return if after_expr { Ctx::AfterExpr } else { Ctx::Columns };
                }
                depth -= 1;
            }
            Tok::Word(w) if depth == 0 => {
                let u = w.to_ascii_uppercase();
                if TABLE_KW.contains(&u.as_str()) {
                    let rest = &before[k + 1..];
                    return if rest.is_empty() || (u == "FROM" && rest.last() == Some(&Tok::Punct(','))) { Ctx::Tables } else { Ctx::AfterTable };
                }
                if EXPR_KW.contains(&u.as_str()) {
                    return if after_expr && k + 1 < before.len() { Ctx::AfterExpr } else { Ctx::Columns };
                }
            }
            _ => {}
        }
    }
    Ctx::Start
}

/// よく使うキーワード（大文字で持ち、打っている語に合わせて小文字にする）
const KEYWORDS: &[&str] = &[
    "SELECT", "DISTINCT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS NULL", "IS NOT NULL", "IN", "EXISTS", "BETWEEN", "LIKE", "AS", "ON", "JOIN", "INNER JOIN", "LEFT JOIN",
    "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "GROUP BY", "ORDER BY", "HAVING", "ASC", "DESC", "UNION", "UNION ALL", "CASE", "WHEN", "THEN", "ELSE", "END", "INSERT INTO", "VALUES",
    "UPDATE", "SET", "DELETE FROM", "WITH", "OVER", "PARTITION BY",
];
const START_KW: &[&str] = &["SELECT", "INSERT INTO", "UPDATE", "DELETE FROM", "WITH", "CREATE TABLE", "ALTER TABLE", "DROP TABLE", "TRUNCATE TABLE", "COMMIT", "ROLLBACK"];
const AFTER_TABLE_KW: &[&str] = &["WHERE", "JOIN", "INNER JOIN", "LEFT JOIN", "RIGHT JOIN", "CROSS JOIN", "ON", "AS", "GROUP BY", "ORDER BY", "HAVING", "UNION", "UNION ALL", "SET"];
const AFTER_EXPR_KW: &[&str] = &["FROM", "AS", "AND", "OR", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "ASC", "DESC", "IS NULL", "IS NOT NULL", "IN", "LIKE", "BETWEEN", "THEN", "ELSE", "END"];
const FUNCS: &[&str] = &[
    "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "NULLIF", "CAST", "UPPER", "LOWER", "TRIM", "LTRIM", "RTRIM", "LENGTH", "SUBSTR", "REPLACE", "ROUND", "ABS", "CONCAT", "ROW_NUMBER",
    "RANK", "DENSE_RANK", "LAG", "LEAD",
];

/// 引用符を付けない名前の大文字小文字
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// 大文字に寄せる（Oracle）
    Upper,
    /// 小文字に寄せる（PostgreSQL）
    Lower,
    /// 区別しない（SQLite）
    Any,
}

/// 方言（接続先の種類ごとの違い）
pub struct Dialect {
    pub name: &'static str,
    /// 方言のキーワード（テーブルの後・式の中に足す）
    pub keywords: &'static [&'static str],
    pub functions: &'static [&'static str],
    /// 引数の無い値（SYSDATE・CURRENT_DATE）
    pub constants: &'static [&'static str],
    /// データ型（CAST(x AS ・:: ・CREATE TABLE の列の後）
    pub types: &'static [&'static str],
    pub fold: Fold,
    /// スキーマ・テーブル・列の名前をいつも "" で囲む（Oracle）
    pub always_quote: bool,
    /// $$…$$・$tag$…$tag$ の文字列（PostgreSQL）
    pub dollar_quote: bool,
    /// q'[…]' の文字列（Oracle）
    pub q_quote: bool,
    /// `::` で型を書く（PostgreSQL）
    pub double_colon_cast: bool,
    /// PRAGMA の名前（SQLite）
    pub pragmas: &'static [&'static str],
}

pub static ORACLE: Dialect = Dialect {
    name: "Oracle",
    keywords: &["FETCH FIRST", "ROWS ONLY", "OFFSET", "CONNECT BY", "START WITH", "PRIOR", "MINUS", "NULLS FIRST", "NULLS LAST", "MERGE INTO", "DUAL", "ROWNUM", "ROWID", "LEVEL"],
    functions: &["NVL", "NVL2", "DECODE", "TO_CHAR", "TO_DATE", "TO_NUMBER", "TO_TIMESTAMP", "TRUNC", "ADD_MONTHS", "MONTHS_BETWEEN", "LAST_DAY", "INSTR", "LISTAGG", "REGEXP_LIKE", "REGEXP_REPLACE", "REGEXP_SUBSTR", "SYS_GUID"],
    constants: &["SYSDATE", "SYSTIMESTAMP", "CURRENT_DATE", "CURRENT_TIMESTAMP"],
    types: &["VARCHAR2", "NVARCHAR2", "CHAR", "NCHAR", "NUMBER", "INTEGER", "FLOAT", "BINARY_FLOAT", "BINARY_DOUBLE", "DATE", "TIMESTAMP", "TIMESTAMP WITH TIME ZONE", "CLOB", "NCLOB", "BLOB", "RAW"],
    fold: Fold::Upper,
    always_quote: true,
    dollar_quote: false,
    q_quote: true,
    double_colon_cast: false,
    pragmas: &[],
};

pub static POSTGRES: Dialect = Dialect {
    name: "PostgreSQL",
    keywords: &["LIMIT", "OFFSET", "ILIKE", "RETURNING", "ON CONFLICT", "DO NOTHING", "DO UPDATE SET", "NULLS FIRST", "NULLS LAST", "TRUE", "FALSE", "LATERAL", "DISTINCT ON"],
    functions: &["NOW", "DATE_TRUNC", "TO_CHAR", "TO_DATE", "TO_TIMESTAMP", "STRING_AGG", "ARRAY_AGG", "GENERATE_SERIES", "EXTRACT", "AGE", "JSONB_BUILD_OBJECT", "JSON_AGG", "SPLIT_PART", "GEN_RANDOM_UUID", "UNNEST"],
    constants: &["CURRENT_DATE", "CURRENT_TIMESTAMP"],
    types: &["integer", "bigint", "smallint", "serial", "bigserial", "numeric", "real", "double precision", "text", "varchar", "char", "boolean", "date", "time", "timestamp", "timestamptz", "interval", "uuid", "json", "jsonb", "bytea", "inet"],
    fold: Fold::Lower,
    always_quote: false,
    dollar_quote: true,
    q_quote: false,
    double_colon_cast: true,
    pragmas: &[],
};

pub static SQLITE: Dialect = Dialect {
    name: "SQLite",
    keywords: &["LIMIT", "OFFSET", "PRAGMA", "RETURNING", "ON CONFLICT", "AUTOINCREMENT", "WITHOUT ROWID"],
    functions: &["IFNULL", "DATE", "DATETIME", "STRFTIME", "GROUP_CONCAT", "INSTR", "PRINTF", "TYPEOF", "JSON_EXTRACT", "JSON_OBJECT"],
    constants: &["CURRENT_DATE", "CURRENT_TIMESTAMP"],
    types: &["INTEGER", "TEXT", "REAL", "BLOB", "NUMERIC"],
    fold: Fold::Any,
    always_quote: false,
    dollar_quote: false,
    q_quote: false,
    double_colon_cast: false,
    pragmas: &["table_info", "table_xinfo", "index_list", "index_info", "foreign_key_list", "foreign_keys", "journal_mode", "user_version", "integrity_check", "database_list", "compile_options"],
};

/// 種類の分からない接続先
pub static GENERIC: Dialect = Dialect {
    name: "SQL",
    keywords: &["LIMIT"],
    functions: &[],
    constants: &[],
    types: &[],
    fold: Fold::Any,
    always_quote: false,
    dollar_quote: false,
    q_quote: false,
    double_colon_cast: false,
    pragmas: &[],
};

/// 接続先の種類の方言
pub fn dialect(kind: Option<Kind>) -> &'static Dialect {
    match kind {
        Some(Kind::Oracle) => &ORACLE,
        Some(Kind::Postgres) => &POSTGRES,
        Some(Kind::Sqlite) => &SQLITE,
        _ => &GENERIC,
    }
}

/// 名前にしたら引用符が要る予約語
const RESERVED_NAMES: &[&str] = &[
    "ALL", "AND", "AS", "ASC", "BETWEEN", "BY", "CASE", "CHECK", "COLUMN", "CONSTRAINT", "CREATE", "DEFAULT", "DELETE", "DESC", "DISTINCT", "DROP", "ELSE", "END", "EXISTS", "FOR",
    "FOREIGN", "FROM", "GRANT", "GROUP", "HAVING", "IN", "INDEX", "INSERT", "INTO", "IS", "JOIN", "KEY", "LIKE", "LIMIT", "NOT", "NULL", "OF", "ON", "OR", "ORDER", "PRIMARY",
    "REFERENCES", "SELECT", "SET", "TABLE", "THEN", "TO", "UNION", "UNIQUE", "UPDATE", "USER", "USING", "VALUES", "WHEN", "WHERE", "WITH",
];

impl Dialect {
    /// 名前を、この方言で引用符が要れば囲む（大文字小文字の寄せ方に合わない・使えない文字・予約語）
    pub fn ident(&self, s: &str) -> String {
        let plain = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#');
        let fits = match self.fold {
            Fold::Upper => !s.chars().any(|c| c.is_ascii_lowercase()),
            Fold::Lower => !s.chars().any(|c| c.is_ascii_uppercase()),
            Fold::Any => true,
        };
        let reserved = RESERVED_NAMES.contains(&s.to_ascii_uppercase().as_str());
        if plain && fits && !reserved { s.to_string() } else { quoted(s) }
    }
}


/// `a.b.` の頭のスキーマ（`seq.` なら今のスキーマ）
fn schema_of(c: &dyn Catalog, q: &[String]) -> String {
    if q.len() >= 2 { c.resolve(Some(&q[q.len() - 2]), "").0 } else { c.schema().to_string() }
}

/// 補完の候補（`stmt` はカーソルのある文、`cur` はその中のカーソルの位置、`prefix` は打った語）
pub fn complete(d: &Dialect, stmt: &str, cur: usize, prefix: &str, c: &dyn Catalog) -> Vec<Item> {
    let (before, open) = tokenize_with(&stmt[..cur], d);
    if open {
        return Vec::new();
    }
    let ctx = context_with(&before, d);
    let refs = table_refs(&tokenize_with(stmt, d).0);
    // 別名・テーブル名で、文の表を引く
    let ref_of = |q: &str| refs.iter().find(|r| !r.cte && (r.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || (r.alias.is_none() && r.name.eq_ignore_ascii_case(q))));
    // 列を出す表（届いていなければ読みたいと伝える）
    let mut col_tables: Vec<(String, String)> = Vec::new();
    let mut list_schema: Option<String> = None;
    match &ctx {
        Ctx::Qualified(q) if q.len() == 1 => {
            if let Some(r) = ref_of(&q[0]) {
                col_tables.push(c.resolve(r.schema.as_deref(), &r.name));
            } else if c.is_schema(&q[0]) {
                list_schema = Some(c.resolve(Some(&q[0]), "").0);
            } else {
                col_tables.push(c.resolve(None, &q[0]));
            }
        }
        Ctx::Qualified(q) => col_tables.push(c.resolve(Some(&q[q.len() - 2]), &q[q.len() - 1])),
        Ctx::Columns | Ctx::AfterExpr | Ctx::AfterTable => col_tables.extend(refs.iter().filter(|r| !r.cte).map(|r| c.resolve(r.schema.as_deref(), &r.name))),
        _ => {}
    }
    for (s, t) in &col_tables {
        c.want(Want::Objects(s.clone()));
        c.want(Want::Columns(s.clone(), t.clone()));
    }
    if let Some(s) = &list_schema {
        c.want(Want::Objects(s.clone()));
    }
    // キーワードは打っている語（打っていなければ文の最初の語）に合わせて小文字に
    let first = before.iter().find_map(|t| if let Tok::Word(w) = t { Some(w.as_str()) } else { None }).unwrap_or("");
    let lower = if prefix.is_empty() { first.chars().any(|c| c.is_ascii_lowercase()) } else { prefix.chars().any(|c| c.is_ascii_lowercase()) && !prefix.chars().any(|c| c.is_ascii_uppercase()) };
    let kc = |k: &str| if lower { k.to_lowercase() } else { k.to_string() };
    let mut out: Vec<Item> = Vec::new();

    let mut push = |out: &mut Vec<Item>, name: String, mark: char, note: String, call: Option<bool>, rank: u8, quote: bool| {
        if out.iter().any(|x| x.name == name && x.mark == mark) {
            return;
        }
        // Oracle はスキーマ・テーブル・列の名前をいつも "" で囲む（WITH の名前は書いたとおり）
        let insert = if quote && d.always_quote && matches!(mark, 'T' | 'V' | 'f' | 'c') && note != "WITH" { Some(quoted(&name)) } else { (quote && d.ident(&name) != name).then(|| d.ident(&name)) };
        out.push(Item { name, mark, note, call, rank, insert, import: None });
    };
    let tables = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, String, char, String, Option<bool>, u8, bool), s: &str, rank: u8, more: bool| {
        if let Some(o) = c.objects(s) {
            for (t, ty) in &o.tables {
                let view = ty == "VIEW";
                push(out, t.clone(), if view { 'V' } else { 'T' }, if view { "ビュー".into() } else { "テーブル".into() }, None, rank, true);
            }
            if more {
                for q in &o.sequences {
                    push(out, q.clone(), 'v', "シーケンス".into(), None, rank + 1, true);
                }
                for r in &o.routines {
                    push(out, r.clone(), 'm', "ルーチン".into(), Some(true), rank + 1, true);
                }
            }
        }
    };
    let columns = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, String, char, String, Option<bool>, u8, bool), rank: u8| {
        for (s, t) in &col_tables {
            for col in c.columns(s, t).into_iter().flatten() {
                let pk = if col.pk { "  PK" } else { "" };
                push(out, col.name.clone(), 'f', format!("{}  {t}{pk}", col.ty), None, rank, true);
            }
        }
    };
    let (d_kw, d_funcs, d_consts) = (d.keywords, d.functions, d.constants);
    let keywords = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, String, char, String, Option<bool>, u8, bool), list: &[&str], rank: u8| {
        for k in list {
            push(out, kc(k), 'K', String::new(), None, rank, false);
        }
    };
    let funcs = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, String, char, String, Option<bool>, u8, bool), rank: u8| {
        for f in FUNCS.iter().chain(d_funcs) {
            push(out, kc(f), 'm', "関数".into(), Some(true), rank, false);
        }
        for k in d_consts {
            push(out, kc(k), 'K', String::new(), None, rank, false);
        }
    };
    let schema = c.schema().to_string();
    match &ctx {
        Ctx::Qualified(q) => {
            columns(&mut out, &mut push, 0);
            if let Some(s) = &list_schema {
                tables(&mut out, &mut push, s, 0, true);
            }
            // シーケンス（Oracle の seq.NEXTVAL・seq.CURRVAL）
            let seq = q.last().is_some_and(|n| c.objects(&schema_of(c, q)).is_some_and(|o| o.sequences.iter().any(|x| x.eq_ignore_ascii_case(n))));
            if seq && d.name == ORACLE.name {
                keywords(&mut out, &mut push, &["NEXTVAL", "CURRVAL"], 0);
            }
        }
        Ctx::Types => {
            for t in d.types {
                push(&mut out, t.to_string(), 'c', "データ型".into(), None, 0, false);
            }
        }
        Ctx::Pragma => {
            for p in d.pragmas {
                push(&mut out, p.to_string(), 'K', "PRAGMA".into(), None, 0, false);
            }
        }
        Ctx::Start => {
            keywords(&mut out, &mut push, START_KW, 0);
            keywords(&mut out, &mut push, KEYWORDS, 2);
        }
        Ctx::Tables => {
            for r in refs.iter().filter(|r| r.cte) {
                push(&mut out, r.name.clone(), 'T', "WITH".into(), None, 0, true);
            }
            tables(&mut out, &mut push, &schema, 0, false);
            for s in c.schemas().into_iter().flatten().filter(|s| !s.eq_ignore_ascii_case(&schema)) {
                push(&mut out, s.clone(), 'c', "スキーマ".into(), None, 2, true);
            }
        }
        Ctx::AfterTable => {
            keywords(&mut out, &mut push, AFTER_TABLE_KW, 0);
            keywords(&mut out, &mut push, d_kw, 1);
            columns(&mut out, &mut push, 2);
        }
        Ctx::Columns | Ctx::AfterExpr => {
            let after = ctx == Ctx::AfterExpr;
            if after {
                keywords(&mut out, &mut push, AFTER_EXPR_KW, 0);
            }
            columns(&mut out, &mut push, 1);
            for r in &refs {
                let (name, note) = match &r.alias {
                    Some(a) => (a.clone(), format!("{} の別名", r.name)),
                    None => (r.name.clone(), if r.cte { "WITH".into() } else { "テーブル".into() }),
                };
                push(&mut out, name, 'v', note, None, 2, true);
            }
            funcs(&mut out, &mut push, 3);
            keywords(&mut out, &mut push, KEYWORDS, 4);
            keywords(&mut out, &mut push, d_kw, 4);
            tables(&mut out, &mut push, &schema, 5, false);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(s: &str) -> Ctx {
        context(&tokenize(s).0)
    }

    #[test]
    fn refs_and_aliases() {
        let t = table_refs(&tokenize("SELECT o.id, c.name FROM orders o JOIN app.customers AS c ON c.id = o.cid, x WHERE o.x = 1").0);
        let got: Vec<(Option<&str>, &str, Option<&str>)> = t.iter().map(|r| (r.schema.as_deref(), r.name.as_str(), r.alias.as_deref())).collect();
        assert_eq!(got, vec![(None, "orders", Some("o")), (Some("app"), "customers", Some("c"))], "JOIN の後の , は FROM の並びではない");
        let t = table_refs(&tokenize("select * from a x, b, \"Mixed\" m where 1 = 1").0);
        assert_eq!(t.iter().map(|r| (r.name.as_str(), r.alias.as_deref())).collect::<Vec<_>>(), vec![("a", Some("x")), ("b", None), ("Mixed", Some("m"))], "FROM の , の並び");
        let t = table_refs(&tokenize("with recent as (select * from orders), top (id) as (select 1) select * from recent r join (select * from items) i on 1=1").0);
        assert_eq!(t.iter().map(|r| (r.name.as_str(), r.cte)).collect::<Vec<_>>(), vec![("recent", true), ("orders", false), ("top", true), ("recent", false), ("items", false)], "WITH の中の表も");
        let t = table_refs(&tokenize("insert into t (a, b) values (1, 2)").0);
        assert_eq!((t[0].name.as_str(), t[0].alias.as_deref()), ("t", None));
        // 注釈・文字列の中の FROM は見ない
        let t = table_refs(&tokenize("select 'from x' -- from y\n/* from z */ from w").0);
        assert_eq!(t.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["w"]);
    }

    #[test]
    fn contexts() {
        assert_eq!(ctx(""), Ctx::Start);
        assert_eq!(ctx("select * from "), Ctx::Tables);
        assert_eq!(ctx("select * from a, "), Ctx::Tables);
        assert_eq!(ctx("select * from a "), Ctx::AfterTable);
        assert_eq!(ctx("select * from a x join "), Ctx::Tables);
        assert_eq!(ctx("select * from a x join b y on "), Ctx::Columns);
        assert_eq!(ctx("select "), Ctx::Columns);
        assert_eq!(ctx("select a, "), Ctx::Columns);
        assert_eq!(ctx("select a "), Ctx::AfterExpr);
        assert_eq!(ctx("select count("), Ctx::Columns);
        assert_eq!(ctx("update t set a = 1 where "), Ctx::Columns);
        assert_eq!(ctx("insert into t ("), Ctx::Columns);
        assert_eq!(ctx("select * from a x where x."), Ctx::Qualified(vec!["x".into()]));
        assert_eq!(ctx("select * from hr."), Ctx::Qualified(vec!["hr".into()]));
        assert_eq!(ctx("select hr.emp."), Ctx::Qualified(vec!["hr".into(), "emp".into()]));
        assert_eq!(ctx("select * from a order by "), Ctx::Columns);
        assert!(tokenize("select 'abc").1 && tokenize("select -- x").1, "文字列・注釈の途中");
        assert!(!tokenize("select 'it''s' from x").1);
    }

    #[test]
    fn edit_targets() {
        let t = |s: &str| edit_target(s);
        assert_eq!(t("select * from emp"), Some((None, "emp".into())));
        assert_eq!(t("SELECT e.*, id FROM hr.emp e WHERE id > 1 ORDER BY id;"), Some((Some("hr".into()), "emp".into())));
        assert_eq!(t("select \"Id\", name from \"HR\".\"Emp\""), Some((Some("HR".into()), "Emp".into())));
        assert_eq!(t("select name as id from emp"), None, "列の別名");
        assert_eq!(t("select upper(name) from emp"), None, "式");
        assert_eq!(t("select count(*) from emp"), None);
        assert_eq!(t("select * from emp join dept on 1 = 1"), None);
        assert_eq!(t("select * from emp, dept"), None);
        assert_eq!(t("select distinct name from emp"), None);
        assert_eq!(t("select * from emp where id in (select id from x)"), None, "副問い合わせ");
        assert_eq!(t("with a as (select 1) select * from a"), None);
        assert_eq!(t("update emp set a = 1"), None);
    }

    #[test]
    fn dialects() {
        // 名前の囲み方: PostgreSQL は小文字に寄せる、Oracle は大文字、SQLite は区別しない。予約語は囲む
        assert_eq!(POSTGRES.ident("order_item"), "order_item");
        assert_eq!(POSTGRES.ident("EMP"), "\"EMP\"", "大文字は小文字に寄るので囲む");
        assert_eq!(ORACLE.ident("EMP"), "EMP");
        assert_eq!(ORACLE.ident("emp"), "\"emp\"");
        assert_eq!(SQLITE.ident("Emp"), "Emp");
        assert_eq!(SQLITE.ident("order"), "\"order\"", "予約語");
        // 方言の文字列
        assert!(!tokenize_with("select $$ it's ; $$ from t", &POSTGRES).1, "$$…$$ の中の ' は文字列の始まりでない");
        assert!(tokenize_with("select $$ abc", &POSTGRES).1, "閉じていない $$");
        assert!(!tokenize_with("select q'[it's]' from dual", &ORACLE).1);
        assert!(tokenize_with("select q'[it's]' from dual", &GENERIC).1, "ほかの方言では q'…' でない");
        // 方言の場所
        let ctx = |s: &str, d: &Dialect| context_with(&tokenize_with(s, d).0, d);
        assert_eq!(ctx("select x::", &POSTGRES), Ctx::Types);
        assert_ne!(ctx("select x::", &ORACLE), Ctx::Types);
        assert_eq!(ctx("select cast(x as ", &ORACLE), Ctx::Types);
        assert_eq!(ctx("create table t (id ", &SQLITE), Ctx::Types);
        assert_eq!(ctx("create table t (id integer, name ", &SQLITE), Ctx::Types);
        assert_eq!(ctx("pragma ", &SQLITE), Ctx::Pragma);
    }
}
