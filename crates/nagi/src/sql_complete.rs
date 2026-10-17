//! SQL コンソールの補完（DataGrip と同じく、テーブル名・列名を型と表の名前つきで出す）。
//!
//! - カーソルの前が `別名.` なら、今の文の FROM / JOIN からその別名の表を探し、その列を出す
//! - そうでなければ、今のスキーマのテーブル・ビュー、今の文に出てくる表の列、SQL のキーワード
//! 列がまだ読めていない表は、裏で読みに行く（届いたらもう一度 Ctrl+Space で出る）。

use std::sync::OnceLock;

use regex::Regex;

use crate::editor::Editor;

const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "EXISTS", "BETWEEN", "LIKE", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON", "GROUP BY", "ORDER BY", "HAVING", "DISTINCT", "COUNT", "SUM", "MAX", "MIN", "AVG", "AS",
    "INSERT INTO", "VALUES", "UPDATE", "SET", "DELETE FROM", "CASE", "WHEN", "THEN", "ELSE", "END", "UNION", "UNION ALL", "ASC", "DESC", "FETCH FIRST", "ROWS ONLY", "WITH", "COMMIT", "ROLLBACK",
];

/// 文の中の表と別名（表の書き方, 別名）
pub fn tables_in(stmt: &str) -> Vec<(String, Option<String>)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"(?i)\b(?:from|join|update|into)\s+([A-Za-z_"][\w$#".]*)(?:\s+(?:as\s+)?([A-Za-z_]\w*))?"#).unwrap());
    const NOT_ALIAS: &[&str] = &["where", "on", "join", "left", "right", "inner", "outer", "group", "order", "set", "values", "union", "having", "fetch", "cross", "full", "natural"];
    re.captures_iter(stmt)
        .map(|c| {
            let t = c[1].to_string();
            let a = c.get(2).map(|m| m.as_str().to_string()).filter(|a| !NOT_ALIAS.contains(&a.to_ascii_lowercase().as_str()));
            (t, a)
        })
        .collect()
}

impl Editor {
    /// 今の文の行（カーソルのある文。無ければカーソルの行）
    fn sql_statement_text(&self) -> String {
        match self.console_statement() {
            Some((s, e)) => (s..e).map(|l| self.buf.line(l)).collect::<Vec<_>>().join("\n"),
            None => self.buf.line(self.cursor.line),
        }
    }

    /// 表の書き方（schema.table・"x"）を、接続先のキャッシュの (スキーマ, 名前) にする
    fn sql_resolve(&self, i: usize, written: &str) -> Option<(String, String)> {
        let c = &self.db.as_ref()?.conns[i];
        let parts: Vec<String> = written.split('.').map(|p| p.trim_matches('"').to_string()).collect();
        let (schema, name) = match parts.as_slice() {
            [s, n] => (s.clone(), n.clone()),
            [n] => (c.schema.clone(), n.clone()),
            _ => return None,
        };
        let schema = c.objects.keys().find(|k| k.eq_ignore_ascii_case(&schema)).cloned().unwrap_or(schema);
        let found = c.objects.get(&schema).and_then(|o| o.tables.iter().find(|t| t.0.eq_ignore_ascii_case(&name)).map(|t| t.0.clone()));
        Some((schema, found.unwrap_or(name)))
    }

    /// 候補（入れる文字, 印, 補足）。`qualifier` は `別名.` の別名
    pub(crate) fn sql_candidates(&mut self, prefix: &str, qualifier: Option<&str>) -> Vec<(String, char, String)> {
        let Some(i) = self.console_source() else { return Vec::new() };
        let stmt = self.sql_statement_text();
        let refs: Vec<(String, String, Option<String>)> = tables_in(&stmt).into_iter().filter_map(|(w, a)| self.sql_resolve(i, &w).map(|(s, t)| (s, t, a))).collect();
        // 列がまだ無い表は読みに行く
        if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
            for (s, t, _) in &refs {
                c.ensure_columns(s, t);
            }
        }
        let Some(c) = self.db.as_ref().map(|p| &p.conns[i]) else { return Vec::new() };
        let low = prefix.to_lowercase();
        let ok = |s: &str| s.to_lowercase().starts_with(&low);
        let mut out: Vec<(String, char, String)> = Vec::new();
        let cols_of = |s: &str, t: &str, out: &mut Vec<(String, char, String)>| {
            if let Some(cols) = c.columns.get(&(s.to_string(), t.to_string())) {
                for col in cols {
                    if ok(&col.name) && !out.iter().any(|o| o.0 == col.name) {
                        let mark = if col.pk { 'K' } else if !col.fk.is_empty() { 'F' } else { ' ' };
                        out.push((col.name.clone(), mark, format!("{}  {t}", col.ty)));
                    }
                }
            }
        };
        if let Some(q) = qualifier {
            // 別名（または表の名前）の列だけ
            let hit = refs.iter().find(|(_, t, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || t.eq_ignore_ascii_case(q));
            if let Some((s, t, _)) = hit {
                cols_of(s, t, &mut out);
            }
            return out;
        }
        for (s, t, _) in &refs {
            cols_of(s, t, &mut out);
        }
        if let Some(o) = c.objects.get(&c.schema) {
            for (t, ty) in &o.tables {
                if ok(t) {
                    out.push((t.clone(), if ty == "VIEW" { 'V' } else { 'T' }, if ty == "VIEW" { "ビュー".into() } else { "テーブル".into() }));
                }
            }
        }
        for k in KEYWORDS {
            if ok(k) && !prefix.is_empty() {
                out.push((if prefix.chars().all(|c| c.is_ascii_lowercase()) { k.to_lowercase() } else { k.to_string() }, ' ', "キーワード".into()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        let t = tables_in("SELECT o.id, c.name FROM orders o JOIN app.customers AS c ON c.id = o.cid WHERE o.x = 1");
        assert_eq!(t, vec![("orders".to_string(), Some("o".to_string())), ("app.customers".into(), Some("c".into()))]);
        assert_eq!(tables_in("select * from item where id = 1"), vec![("item".to_string(), None)]);
    }

    /// SQLite で、表の名前・別名の後の列を補完する
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
        // 表の名前
        keys(&mut e, "Goselect * from ord<C-n>");
        assert!(e.buf.line(e.cursor.line).ends_with("from orders"), "{}", e.buf.line(e.cursor.line));
        keys(&mut e, " o where o.");
        // . の後は自動で候補（列はまだ読んでいなければ読みに行く）
        crate::db_panel::tests::wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].columns.contains_key(&("main".to_string(), "orders".to_string())));
        keys(&mut e, "cu<C-n>");
        assert!(e.buf.line(e.cursor.line).ends_with("o.customer_id"), "{}", e.buf.line(e.cursor.line));
        let c = e.completion.as_ref().unwrap();
        assert_eq!(c.notes[0].1, "INTEGER  orders");
    }
}
