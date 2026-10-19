//! SQL コンソールの補完（DataGrip のように、文の中の場所に合わせて候補を出す）。
//!
//! 一覧は IntelliJ のような補完（smart_complete.rs）の枠で出す: 打つたびに絞り込み（大文字小文字は区別しない・飛ばし打ちも当てる）、
//! Enter・Tab で決める。`.` の後は打った時に、FROM・JOIN の後と語を打っている間は設定の「自動で出す」で出る。
//!
//! 場所（カーソルの前の語を、文字列・注釈を飛ばして見る）:
//! - FROM・JOIN・UPDATE・INTO の後: 今のスキーマのテーブル・ビュー、WITH の名前、スキーマ
//! - その後（テーブルを書いた後）: WHERE・JOIN などのキーワードを先に
//! - SELECT・WHERE・ON・SET・BY などの後・括弧の中: 文に出てくる表の列、別名、関数、キーワード
//! - `別名.`・`テーブル.`: その表の列。`スキーマ.`: そのスキーマのテーブル・ビュー。`スキーマ.テーブル.`: その列
//! - 文の頭: SELECT・INSERT INTO などの文の始まり
//!
//! 列・ほかのスキーマのテーブルがまだ読めていなければ裏で読み、届いたら出している一覧を作り直す（一覧が出ていなければ出す）。
//! 文は空行で区切ったまとまり（Ctrl+Enter と同じ）の中の、`;` で区切った部分。

use nagi_workers::db::Kind;

use crate::db_panel::ident;
use crate::editor::Editor;
use crate::smart_complete::Cand;

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
    fn name(&self) -> Option<&str> {
        match self {
            Tok::Word(w) | Tok::Quoted(w) => Some(w),
            _ => None,
        }
    }

    /// 語なら大文字にしたもの
    fn upper(&self) -> Option<String> {
        match self {
            Tok::Word(w) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    }
}

/// 字句に分ける。2 つ目は、文字列・"名前"・注釈の途中で終わったか（その中では補完しない）
pub fn tokenize(s: &str) -> (Vec<Tok>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let c = rest.chars().next().unwrap_or(' ');
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
}

const TABLE_KW: &[&str] = &["FROM", "JOIN", "UPDATE", "INTO", "TABLE", "DESC", "DESCRIBE"];
const EXPR_KW: &[&str] = &[
    "SELECT", "WHERE", "ON", "AND", "OR", "BY", "HAVING", "SET", "WHEN", "THEN", "ELSE", "CASE", "NOT", "BETWEEN", "LIKE", "ILIKE", "IN", "IS", "RETURNING", "DISTINCT", "VALUES",
    "USING", "PRIOR",
];

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

/// 接続先の種類ごとのキーワード・関数・引数の無い値
fn dialect(kind: Option<Kind>) -> (&'static [&'static str], &'static [&'static str], &'static [&'static str]) {
    match kind {
        Some(Kind::Oracle) => (
            &["FETCH FIRST", "ROWS ONLY", "OFFSET", "CONNECT BY", "START WITH", "PRIOR", "MINUS", "NULLS FIRST", "NULLS LAST", "MERGE INTO", "DUAL", "ROWNUM"],
            &["NVL", "NVL2", "DECODE", "TO_CHAR", "TO_DATE", "TO_NUMBER", "TO_TIMESTAMP", "TRUNC", "ADD_MONTHS", "MONTHS_BETWEEN", "LAST_DAY", "INSTR", "LISTAGG", "REGEXP_LIKE", "REGEXP_REPLACE", "REGEXP_SUBSTR"],
            &["SYSDATE", "SYSTIMESTAMP", "CURRENT_DATE", "CURRENT_TIMESTAMP"],
        ),
        Some(Kind::Postgres) => (
            &["LIMIT", "OFFSET", "ILIKE", "RETURNING", "ON CONFLICT", "NULLS FIRST", "NULLS LAST", "TRUE", "FALSE"],
            &["NOW", "DATE_TRUNC", "TO_CHAR", "TO_DATE", "TO_TIMESTAMP", "STRING_AGG", "ARRAY_AGG", "GENERATE_SERIES", "EXTRACT", "AGE", "JSONB_BUILD_OBJECT", "JSON_AGG", "SPLIT_PART"],
            &["CURRENT_DATE", "CURRENT_TIMESTAMP"],
        ),
        Some(Kind::Sqlite) => (&["LIMIT", "OFFSET", "PRAGMA", "RETURNING", "ON CONFLICT"], &["IFNULL", "DATE", "DATETIME", "STRFTIME", "GROUP_CONCAT", "INSTR", "PRINTF"], &["CURRENT_DATE", "CURRENT_TIMESTAMP"]),
        _ => (&["LIMIT"], &[], &[]),
    }
}

impl Editor {
    /// カーソルのある文（空行で区切ったまとまりの中の、`;` で区切った部分）と、その中での `at`（絶対 byte）の位置
    fn sql_statement_around(&self, at: usize) -> (String, usize) {
        let (l0, l1) = self.console_statement().unwrap_or((self.cursor.line, self.cursor.line + 1));
        let start = self.buf.line_start(l0);
        let end = if l1 < self.buf.line_count() { self.buf.line_start(l1) } else { self.buf.len_bytes() };
        let text = self.buf.slice_string(start, end.max(start));
        let cur = at.saturating_sub(start).min(text.len());
        // `;` で区切る（文字列の中の ; は数えない）
        let (mut a, mut b) = (0, text.len());
        let mut in_str = false;
        for (k, ch) in text.char_indices() {
            match ch {
                '\'' => in_str = !in_str,
                ';' if !in_str && k < cur => a = k + 1,
                ';' if !in_str && k >= cur => {
                    b = k;
                    break;
                }
                _ => {}
            }
        }
        (text[a..b].to_string(), cur - a)
    }

    /// 補完の候補（smart_complete の枠で出す）。`start` は打っている語の始まり（絶対 byte）、`prefix` は打った語
    pub(crate) fn sql_smart_candidates(&mut self, start: usize, prefix: &str) -> Vec<Cand> {
        let Some(i) = self.console_source() else { return Vec::new() };
        let (stmt, cur) = self.sql_statement_around(start);
        let (before, open) = tokenize(&stmt[..cur]);
        if open {
            return Vec::new();
        }
        let ctx = context(&before);
        let refs = table_refs(&tokenize(&stmt).0);
        let Some(p) = self.db.as_mut() else { return Vec::new() };
        let kind = p.kind(i);
        let c = &mut p.conns[i];
        // 書かれた (スキーマ, 名前) を、覚えている (スキーマ, テーブル) にする（大文字小文字は区別しない）
        let resolve = |c: &crate::db_panel::Conn, schema: Option<&str>, name: &str| -> (String, String) {
            let s = schema.unwrap_or(&c.schema);
            let s = c.objects.keys().find(|k| k.eq_ignore_ascii_case(s)).cloned().or_else(|| c.schemas.as_ref().and_then(|v| v.iter().find(|k| k.eq_ignore_ascii_case(s)).cloned())).unwrap_or_else(|| s.to_string());
            let t = c.objects.get(&s).and_then(|o| o.tables.iter().find(|t| t.0.eq_ignore_ascii_case(name)).map(|t| t.0.clone())).unwrap_or_else(|| name.to_string());
            (s, t)
        };
        let is_schema = |c: &crate::db_panel::Conn, n: &str| c.schemas.as_ref().is_some_and(|v| v.iter().any(|s| s.eq_ignore_ascii_case(n))) || c.objects.keys().any(|s| s.eq_ignore_ascii_case(n));
        // 別名・テーブル名で、文の表を引く
        let ref_of = |q: &str| refs.iter().find(|r| !r.cte && (r.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || (r.alias.is_none() && r.name.eq_ignore_ascii_case(q))));
        // 列を出す表（届いていなければ読み始める）
        let mut col_tables: Vec<(String, String)> = Vec::new();
        let mut list_schema: Option<String> = None;
        match &ctx {
            Ctx::Qualified(q) if q.len() == 1 => {
                if let Some(r) = ref_of(&q[0]) {
                    col_tables.push(resolve(c, r.schema.as_deref(), &r.name));
                } else if is_schema(c, &q[0]) {
                    list_schema = Some(resolve(c, Some(&q[0]), "").0);
                } else {
                    col_tables.push(resolve(c, None, &q[0]));
                }
            }
            Ctx::Qualified(q) => col_tables.push(resolve(c, Some(&q[q.len() - 2]), &q[q.len() - 1])),
            Ctx::Columns | Ctx::AfterExpr | Ctx::AfterTable => col_tables.extend(refs.iter().filter(|r| !r.cte).map(|r| resolve(c, r.schema.as_deref(), &r.name))),
            _ => {}
        }
        for (s, t) in &col_tables {
            c.ensure_objects(s);
            c.ensure_columns(s, t);
        }
        if let Some(s) = &list_schema {
            c.ensure_objects(s);
        }
        // 届いていない分は、届いたら作り直す
        self.sql_wait = Some((i, start));
        let Some(c) = self.db.as_ref().map(|p| &p.conns[i]) else { return Vec::new() };

        // キーワードは打っている語（打っていなければ文の最初の語）に合わせて小文字に
        let first = before.iter().find_map(|t| if let Tok::Word(w) = t { Some(w.as_str()) } else { None }).unwrap_or("");
        let lower = if prefix.is_empty() { first.chars().any(|c| c.is_ascii_lowercase()) } else { prefix.chars().any(|c| c.is_ascii_lowercase()) && !prefix.chars().any(|c| c.is_ascii_uppercase()) };
        let kc = |k: &str| if lower { k.to_lowercase() } else { k.to_string() };
        let mut out: Vec<Cand> = Vec::new();
        let mut push = |out: &mut Vec<Cand>, name: String, mark: char, note: String, call: Option<bool>, rank: u8, quote: bool| {
            if out.iter().any(|x| x.name == name && x.mark == mark) {
                return;
            }
            let insert = (quote && ident(&name) != name).then(|| ident(&name));
            out.push(Cand { name, mark, note, call, rank, insert });
        };
        let tables = |out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, String, char, String, Option<bool>, u8, bool), s: &str, rank: u8, more: bool| {
            if let Some(o) = c.objects.get(s) {
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
        let columns = |out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, String, char, String, Option<bool>, u8, bool), rank: u8| {
            for (s, t) in &col_tables {
                for col in c.columns.get(&(s.clone(), t.clone())).into_iter().flatten() {
                    let pk = if col.pk { "  PK" } else { "" };
                    push(out, col.name.clone(), 'f', format!("{}  {t}{pk}", col.ty), None, rank, true);
                }
            }
        };
        let (d_kw, d_funcs, d_consts) = dialect(kind);
        let keywords = |out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, String, char, String, Option<bool>, u8, bool), list: &[&str], rank: u8| {
            for k in list {
                push(out, kc(k), 'K', String::new(), None, rank, false);
            }
        };
        let funcs = |out: &mut Vec<Cand>, push: &mut dyn FnMut(&mut Vec<Cand>, String, char, String, Option<bool>, u8, bool), rank: u8| {
            for f in FUNCS.iter().chain(d_funcs) {
                push(out, kc(f), 'm', "関数".into(), Some(true), rank, false);
            }
            for k in d_consts {
                push(out, kc(k), 'K', String::new(), None, rank, false);
            }
        };
        let schema = c.schema.clone();
        match &ctx {
            Ctx::Qualified(_) => {
                columns(&mut out, &mut push, 0);
                if let Some(s) = &list_schema {
                    tables(&mut out, &mut push, s, 0, true);
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
                for s in c.schemas.iter().flatten().filter(|s| !s.eq_ignore_ascii_case(&schema)) {
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

    /// Ctrl-n の補完（打った語の前方一致だけ。入れる文字, 印, 補足）
    pub(crate) fn sql_candidates(&mut self, prefix: &str, _qualifier: Option<&str>) -> Vec<(String, char, String)> {
        let start = self.abs().saturating_sub(prefix.len());
        let low = prefix.to_lowercase();
        let mut v: Vec<Cand> = self.sql_smart_candidates(start, prefix).into_iter().filter(|c| c.name.to_lowercase().starts_with(&low) && (!prefix.is_empty() || c.mark != 'K')).collect();
        v.sort_by_key(|c| c.rank);
        v.into_iter().map(|c| (c.insert.unwrap_or(c.name), c.mark, c.note)).collect()
    }

    /// 列・テーブルが届いた: 出している SQL の補完の一覧を作り直す。一覧が出ていなければ（候補が無くて出せなかったなら）出す
    pub(crate) fn sql_metadata_arrived(&mut self, i: usize) {
        let Some((src, start)) = self.sql_wait else { return };
        if src != i || self.state() != crate::editor::State::Insert || self.console_source() != Some(i) {
            return;
        }
        let abs = self.abs();
        // カーソルが同じ語の中にいるときだけ
        let same_line = self.buf.line_start(self.cursor.line) <= start && start <= abs;
        if !same_line || self.buf.slice_string(start, abs).chars().any(|c| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '#'))) {
            self.sql_wait = None;
            return;
        }
        let sel = self.completion.as_ref().and_then(|c| c.sel.and_then(|s| c.items.get(s).cloned()));
        if self.completion.as_ref().is_some_and(|c| c.smart.is_some()) || self.completion.is_none() {
            let msg = self.notify.text.clone();
            self.completion = None;
            self.start_smart_completion();
            // 出せなければ「候補はありません」は出さない
            if self.completion.is_none() {
                self.notify.info(msg);
            }
            // 選んでいた候補を選び直す
            if let (Some(c), Some(name)) = (&mut self.completion, sel) {
                if let Some(k) = c.items.iter().position(|x| *x == name) {
                    c.sel = Some(k);
                }
            }
        }
    }
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

    /// SQLite で: 表の名前・別名の後の列（届いたら一覧を作り直す）・FROM の後はテーブル、SELECT の後は列とキーワード
    #[test]
    fn complete_in_console() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == crate::db_panel::DbRow::Source(i)).unwrap();
        let keys = |e: &mut Editor, k: &str| {
            for key in nagi_vim::parse_keys(k, " ") {
                e.key(key);
            }
        };
        keys(&mut e, "<CR>");
        crate::db_panel::tests::wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        e.db.as_mut().unwrap().conns[i].manual = true;
        e.db_execute(i, "create table orders(order_id integer primary key, customer_id integer, status text)".into());
        crate::db_panel::tests::wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        e.open_console(i, None);
        // Ctrl-n（前方一致で入れる）は今までどおり
        keys(&mut e, "Goselect * from ord<C-n>");
        assert!(e.buf.line(e.cursor.line).ends_with("from orders"), "{}", e.buf.line(e.cursor.line));
        // . の後は一覧を出す（列はまだ読んでいないので読みに行き、届いたら一覧を出す）
        keys(&mut e, "<C-y> o where o.");
        crate::db_panel::tests::wait(&mut e, &|e| e.completion.as_ref().is_some_and(|c| c.smart.is_some() && !c.items.is_empty()));
        assert_eq!(e.completion.as_ref().unwrap().items, vec!["order_id", "customer_id", "status"]);
        assert_eq!(e.completion.as_ref().unwrap().notes[0].1, "INTEGER  orders  PK");
        // 打って絞り込み（飛ばし打ちも）、Enter で決める
        keys(&mut e, "cid");
        assert_eq!(e.completion.as_ref().unwrap().items[0], "customer_id");
        keys(&mut e, "<CR>");
        assert!(e.buf.line(e.cursor.line).ends_with("o.customer_id"), "{}", e.buf.line(e.cursor.line));
        // 次の文（空行で区切る）: SELECT の後は FROM の表の列が先（関数の strftime より）
        keys(&mut e, "<Esc>o<CR>select st from orders<Esc>02fta");
        e.start_smart_completion();
        let c = e.completion.as_ref().unwrap();
        assert_eq!(c.items[0], "status", "{:?}", c.items);
        // FROM の後はテーブル
        keys(&mut e, "<Esc><Esc>o<CR>select * from o");
        e.start_smart_completion();
        assert_eq!(e.completion.as_ref().unwrap().items[0], "orders");
        keys(&mut e, "<CR>");
        assert!(e.buf.line(e.cursor.line).ends_with("select * from orders"), "{}", e.buf.line(e.cursor.line));
    }
}
