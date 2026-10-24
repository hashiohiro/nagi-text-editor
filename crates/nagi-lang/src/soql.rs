//! SOQL（Salesforce のコンソール）。言語は接続先の種類で決まり、ライブラリにあたるのはオブジェクト・項目・リレーション（sql::Catalog）。
//!
//! 字句と場所の決め方は SQL のもの（sql::tokenize・sql::context）を借り、SOQL の違いだけをここに書く:
//! - JOIN の代わりにリレーション: `Owner.Name`・`Account.Owner.Email`（参照項目の rel と参照先をたどる）
//! - 副問い合わせ `(SELECT … FROM Contacts)` の FROM は、外側のオブジェクトの子のリレーション
//! - 日付の値（`TODAY`・`LAST_N_DAYS:n`）、`FIELDS(ALL)`、`WITH SECURITY_ENFORCED`・`FOR VIEW` など
//! - 名前は引用符で囲まない

use crate::lang::{Cx, Item, Language};
use crate::sql::{Catalog, Ctx, Tok, Want, context, tokenize};
use crate::text::Lexis;

pub struct Soql;

pub static SOQL: Soql = Soql;

impl Language for Soql {
    fn name(&self) -> &'static str {
        "soql"
    }

    fn lexis(&self) -> &'static Lexis {
        &crate::sql::LX
    }

    fn complete_all(&self, cx: &Cx) -> Option<Vec<Item>> {
        let c = cx.console.as_ref()?;
        Some(complete(c.stmt, c.cur, cx.prefix, c.catalog))
    }
}

const AFTER_OBJECT: &[&str] = &["WHERE", "WITH SECURITY_ENFORCED", "USING SCOPE", "GROUP BY", "ORDER BY", "LIMIT", "OFFSET", "FOR VIEW", "FOR REFERENCE", "FOR UPDATE"];
const AFTER_EXPR: &[&str] = &[
    "FROM", "AND", "OR", "NOT", "IN", "NOT IN", "LIKE", "INCLUDES", "EXCLUDES", "WHERE", "GROUP BY", "ORDER BY", "LIMIT", "OFFSET", "ASC", "DESC", "NULLS FIRST", "NULLS LAST", "HAVING",
];
const FUNCS: &[&str] = &["COUNT", "COUNT_DISTINCT", "SUM", "AVG", "MIN", "MAX", "FIELDS", "toLabel", "convertCurrency", "FORMAT", "CALENDAR_YEAR", "CALENDAR_MONTH", "DAY_ONLY", "GROUPING"];
const DATES: &[&str] = &[
    "TODAY", "YESTERDAY", "TOMORROW", "THIS_WEEK", "LAST_WEEK", "NEXT_WEEK", "THIS_MONTH", "LAST_MONTH", "NEXT_MONTH", "THIS_QUARTER", "LAST_QUARTER", "THIS_YEAR", "LAST_YEAR",
    "LAST_90_DAYS", "NEXT_90_DAYS", "LAST_N_DAYS:", "NEXT_N_DAYS:", "LAST_N_MONTHS:", "THIS_FISCAL_YEAR", "NULL", "TRUE", "FALSE",
];

/// 文の段（SELECT の括弧）: 字句の範囲（`all` の中）と、その段の FROM の名前
struct Level {
    start: usize,
    from: Option<String>,
    alias: Option<String>,
}

/// カーソルのある段と、外側の段（外から順）
fn levels(all: &[Tok], cur: usize) -> Vec<Level> {
    // カーソルの前で閉じていない ( SELECT の位置
    let mut starts = vec![0];
    let mut stack: Vec<Option<usize>> = Vec::new();
    for (k, t) in all.iter().enumerate().take(cur) {
        match t {
            Tok::Punct('(') => stack.push(all.get(k + 1).and_then(Tok::upper).filter(|u| u == "SELECT").map(|_| k + 1)),
            Tok::Punct(')') => {
                stack.pop();
            }
            _ => {}
        }
    }
    starts.extend(stack.into_iter().flatten());
    starts
        .into_iter()
        .map(|start| {
            // その段の FROM（括弧の深さ 0 で）
            let mut depth = 0;
            let mut from = None;
            let mut alias = None;
            for (k, t) in all.iter().enumerate().skip(start) {
                match t {
                    Tok::Punct('(') => depth += 1,
                    Tok::Punct(')') if depth == 0 => break,
                    Tok::Punct(')') => depth -= 1,
                    Tok::Word(w) if depth == 0 && w.eq_ignore_ascii_case("FROM") => {
                        from = all.get(k + 1).and_then(|x| x.name()).map(String::from);
                        alias = all.get(k + 2).and_then(|x| match x {
                            Tok::Word(a) if !AFTER_OBJECT.iter().any(|kw| kw.split(' ').next() == Some(&a.to_ascii_uppercase())) => Some(a.clone()),
                            _ => None,
                        });
                        break;
                    }
                    _ => {}
                }
            }
            Level { start, from, alias }
        })
        .collect()
}

/// 補完の候補（`stmt` はカーソルのある文、`cur` はその中のカーソルの位置、`prefix` は打った語）
pub fn complete(stmt: &str, cur: usize, prefix: &str, c: &dyn Catalog) -> Vec<Item> {
    let (before, open) = tokenize(&stmt[..cur]);
    if open {
        return Vec::new();
    }
    let all = tokenize(stmt).0;
    let lv = levels(&all, before.len());
    let here = lv.last().map_or(0, |l| l.start);
    let ctx = context(&before[here.min(before.len())..]);
    let schema = c.schema().to_string();
    // オブジェクトの (スキーマ, 名前)（届いていなければ読みたいと伝える）
    let object = |name: &str| {
        let (s, t) = c.resolve(None, name);
        c.want(Want::Columns(s.clone(), t.clone()));
        (s, t)
    };
    // 段ごとのオブジェクト（副問い合わせは外側の子のリレーション）
    let mut objs: Vec<Option<(String, String)>> = Vec::new();
    for (k, l) in lv.iter().enumerate() {
        let o = match (&l.from, k) {
            (Some(f), 0) => Some(object(f)),
            (Some(rel), _) => objs.last().cloned().flatten().and_then(|(s, t)| {
                c.want(Want::Relations(s.clone(), t.clone()));
                c.relations(&s, &t)?.iter().find(|(r, _)| r.eq_ignore_ascii_case(rel)).map(|(_, child)| object(child))
            }),
            (None, _) => None,
        };
        objs.push(o);
    }
    let obj = objs.last().cloned().flatten();
    // キーワードは打っている語に合わせて小文字に
    let lower = !prefix.is_empty() && prefix.chars().any(|ch| ch.is_ascii_lowercase()) && !prefix.chars().any(|ch| ch.is_ascii_uppercase());
    let kc = |k: &str| if lower { k.to_lowercase() } else { k.to_string() };
    let mut out: Vec<Item> = Vec::new();
    let mut push = |out: &mut Vec<Item>, it: Item| {
        if !out.iter().any(|x| x.name == it.name && x.mark == it.mark) {
            out.push(it);
        }
    };
    // オブジェクトの項目と、参照項目のリレーションの名前（`Owner.` と続ける）
    let fields = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, Item), o: &(String, String), rank: u8| {
        for col in c.columns(&o.0, &o.1).into_iter().flatten() {
            push(out, Item::new(col.name.clone(), 'f', format!("{}  {}", col.ty, o.1), rank));
            if !col.rel.is_empty() {
                push(out, Item::new(col.rel.clone(), 'v', format!("→ {}", col.fk), rank));
            }
        }
    };
    let words = |out: &mut Vec<Item>, push: &mut dyn FnMut(&mut Vec<Item>, Item), list: &[&str], mark: char, rank: u8| {
        for k in list {
            push(out, Item::new(kc(k), mark, "", rank));
        }
    };
    // 今の段の最後の句（WHERE の後なら日付の値も）
    let clause = before[here.min(before.len())..].iter().rev().find_map(|t| t.upper().filter(|u| matches!(u.as_str(), "SELECT" | "WHERE" | "HAVING" | "BY" | "FROM")));
    match ctx {
        Ctx::Start => words(&mut out, &mut push, &["SELECT"], 'K', 0),
        Ctx::Tables => {
            if lv.len() > 1 {
                // 副問い合わせ: 外側のオブジェクトの子のリレーション
                if let Some((s, t)) = objs.get(objs.len() - 2).cloned().flatten() {
                    c.want(Want::Relations(s.clone(), t.clone()));
                    for (r, child) in c.relations(&s, &t).into_iter().flatten() {
                        push(&mut out, Item::new(r.clone(), 'T', format!("子のリレーション → {child}"), 0));
                    }
                }
            } else {
                c.want(Want::Objects(schema.clone()));
                for (t, ty) in c.objects(&schema).map(|o| o.tables.as_slice()).unwrap_or_default() {
                    push(&mut out, Item::new(t.clone(), if ty == "VIEW" { 'V' } else { 'T' }, if ty == "VIEW" { "カスタムオブジェクト" } else { "オブジェクト" }, 0));
                }
            }
        }
        Ctx::AfterTable => words(&mut out, &mut push, AFTER_OBJECT, 'K', 0),
        Ctx::Qualified(parts) => {
            // 別名・オブジェクトの名前から始まる連なり、または今のオブジェクトのリレーションの連なり
            let mut cur = obj.clone();
            let skip = usize::from(parts.first().is_some_and(|p| lv.last().is_some_and(|l| l.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(p)) || l.from.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(p)))));
            for p in &parts[skip..] {
                let next = cur.as_ref().and_then(|o| c.columns(&o.0, &o.1)).and_then(|cols| cols.iter().find(|col| col.rel.eq_ignore_ascii_case(p))).map(|col| col.fk.split('/').next().unwrap_or("").to_string());
                cur = next.filter(|n| !n.is_empty()).map(|n| object(&n));
            }
            if let Some(o) = cur {
                fields(&mut out, &mut push, &o, 0);
            }
        }
        Ctx::Columns | Ctx::AfterExpr => {
            // FIELDS( の中
            if before.len() >= 2 && before[before.len() - 1] == Tok::Punct('(') && before[before.len() - 2].upper().as_deref() == Some("FIELDS") {
                words(&mut out, &mut push, &["ALL", "STANDARD", "CUSTOM"], 'K', 0);
                return out;
            }
            if ctx == Ctx::AfterExpr {
                words(&mut out, &mut push, AFTER_EXPR, 'K', 0);
            }
            if let Some(o) = &obj {
                fields(&mut out, &mut push, o, 1);
            }
            if matches!(clause.as_deref(), Some("WHERE" | "HAVING")) {
                words(&mut out, &mut push, DATES, 'K', 2);
            }
            for f in FUNCS {
                push(&mut out, Item { name: f.to_string(), mark: 'm', note: "関数".into(), call: Some(true), rank: 3, insert: None, import: None });
            }
            // SELECT の並びには副問い合わせも
            if clause.as_deref() == Some("SELECT") && lv.len() == 1 {
                words(&mut out, &mut push, &["(SELECT"], 'K', 4);
            }
        }
        Ctx::Types | Ctx::Pragma => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_workers::db::{Column, Objects};
    use std::cell::RefCell;

    /// 読んだことにした Account・Contact・User
    struct Fake {
        objects: Objects,
        cols: Vec<(String, Vec<Column>)>,
        rels: Vec<(String, (String, String))>,
        wants: RefCell<Vec<Want>>,
    }

    impl Catalog for Fake {
        fn schema(&self) -> &str {
            ""
        }
        fn schemas(&self) -> Option<&[String]> {
            None
        }
        fn objects(&self, _: &str) -> Option<&Objects> {
            Some(&self.objects)
        }
        fn columns(&self, _: &str, t: &str) -> Option<&[Column]> {
            self.cols.iter().find(|x| x.0 == t).map(|x| x.1.as_slice())
        }
        fn relations(&self, _: &str, t: &str) -> Option<&[(String, String)]> {
            self.rels.iter().find(|x| x.0 == t).map(|x| std::slice::from_ref(&x.1))
        }
        fn is_schema(&self, _: &str) -> bool {
            false
        }
        fn resolve(&self, _: Option<&str>, name: &str) -> (String, String) {
            let t = self.objects.tables.iter().find(|t| t.0.eq_ignore_ascii_case(name)).map_or(name.to_string(), |t| t.0.clone());
            (String::new(), t)
        }
        fn want(&self, w: Want) {
            self.wants.borrow_mut().push(w);
        }
    }

    fn fake() -> Fake {
        let col = |n: &str, fk: &str, rel: &str| Column { name: n.into(), ty: "string".into(), fk: fk.into(), rel: rel.into(), ..Default::default() };
        Fake {
            objects: Objects { tables: vec![("Account".into(), "TABLE".into()), ("Contact".into(), "TABLE".into()), ("Invoice__c".into(), "VIEW".into())], ..Default::default() },
            cols: vec![
                ("Account".into(), vec![col("Id", "", ""), col("Name", "", ""), col("OwnerId", "User", "Owner")]),
                ("Contact".into(), vec![col("Id", "", ""), col("LastName", "", ""), col("AccountId", "Account", "Account")]),
                ("User".into(), vec![col("Id", "", ""), col("Email", "", "")]),
            ],
            rels: vec![("Account".into(), ("Contacts".into(), "Contact".into()))],
            wants: RefCell::new(Vec::new()),
        }
    }

    fn names(stmt: &str) -> Vec<String> {
        complete(stmt, stmt.len(), "", &fake()).into_iter().map(|i| i.name).collect()
    }

    #[test]
    fn soql_places() {
        let has = |v: &[String], n: &[&str]| n.iter().all(|x| v.iter().any(|y| y == x));
        assert!(has(&names(""), &["SELECT"]));
        assert!(has(&names("SELECT Id FROM "), &["Account", "Invoice__c"]), "オブジェクト");
        let v = complete("SELECT  FROM Account", 7, "", &fake()).into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert!(has(&v, &["Name", "OwnerId", "Owner", "COUNT", "(SELECT"]), "項目・リレーション・関数・副問い合わせ: {v:?}");
        let s = "SELECT Id, Owner. FROM Account";
        let v = complete(s, s.find("Owner.").unwrap() + 6, "", &fake()).into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert!(has(&v, &["Email"]), "リレーションの先の項目: {v:?}");
        let s = "SELECT Id FROM Contact WHERE Account.Owner.";
        assert!(has(&names(s), &["Email"]), "2 段のリレーション");
        let s = "SELECT Id, (SELECT  FROM Contacts) FROM Account";
        let v = complete(s, s.find("(SELECT ").unwrap() + 8, "", &fake()).into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert!(has(&v, &["LastName"]) && !v.iter().any(|x| x == "Name"), "副問い合わせは子のオブジェクトの項目: {v:?}");
        let s = "SELECT Id, (SELECT Id FROM ) FROM Account";
        let v = complete(s, s.find("FROM )").unwrap() + 5, "", &fake()).into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert!(has(&v, &["Contacts"]), "副問い合わせの FROM は子のリレーション: {v:?}");
        assert!(has(&names("SELECT Id FROM Account WHERE CreatedDate = "), &["TODAY", "LAST_N_DAYS:"]), "日付の値");
        assert!(has(&names("SELECT Id FROM Account "), &["WHERE", "WITH SECURITY_ENFORCED"]));
        assert!(has(&names("SELECT FIELDS("), &["ALL", "STANDARD"]));
        assert!(has(&names("SELECT Id FROM Account a WHERE a."), &["Name"]), "別名");
    }
}
