//! SQL コンソールの補完の画面の側（候補を集めるのは nagi_lang::sql。一覧は smart_complete の枠で出す）。
//!
//! 打つたびに絞り込み（大文字小文字は区別しない・飛ばし打ちも当てる）、Enter・Tab で決める。`.` の後は打った時に、
//! FROM・JOIN の後と語を打っている間は設定の「自動で出す」で出る。
//! 列・ほかのスキーマのテーブルがまだ読めていなければ裏で読み、届いたら出している一覧を作り直す（一覧が出ていなければ出す）。
//! 文は空行で区切ったまとまり（Ctrl+Enter と同じ）の中の、`;` で区切った部分。

use std::cell::RefCell;

use crate::db_panel::Conn;
use crate::editor::Editor;
use crate::smart_complete::Cand;

pub use nagi_lang::sql::edit_target;

/// 接続（読んだスキーマ・テーブル・列）を nagi_lang の Catalog に。読みたいものは貯めて、後で読み始める
struct ConnCatalog<'a> {
    conn: &'a Conn,
    wants: RefCell<Vec<nagi_lang::sql::Want>>,
}

impl nagi_lang::sql::Catalog for ConnCatalog<'_> {
    fn schema(&self) -> &str {
        &self.conn.schema
    }

    fn schemas(&self) -> Option<&[String]> {
        self.conn.schemas.as_deref()
    }

    fn objects(&self, schema: &str) -> Option<&nagi_workers::db::Objects> {
        self.conn.objects.get(schema)
    }

    fn columns(&self, schema: &str, table: &str) -> Option<&[nagi_workers::db::Column]> {
        self.conn.columns.get(&(schema.to_string(), table.to_string())).map(Vec::as_slice)
    }

    fn relations(&self, schema: &str, table: &str) -> Option<&[(String, String)]> {
        self.conn.relations.get(&(schema.to_string(), table.to_string())).map(Vec::as_slice)
    }

    fn is_schema(&self, n: &str) -> bool {
        self.conn.schemas.as_ref().is_some_and(|v| v.iter().any(|s| s.eq_ignore_ascii_case(n))) || self.conn.objects.keys().any(|s| s.eq_ignore_ascii_case(n))
    }

    fn resolve(&self, schema: Option<&str>, name: &str) -> (String, String) {
        self.conn.resolve(schema, name)
    }

    fn want(&self, w: nagi_lang::sql::Want) {
        self.wants.borrow_mut().push(w);
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
        let Some(p) = self.db.as_ref() else { return Vec::new() };
        let cat = ConnCatalog { conn: &p.conns[i], wants: Default::default() };
        let console = nagi_lang::sql::Console { catalog: &cat, stmt: &stmt, cur };
        let src = self.buf.text();
        let line = self.buf.line(self.cursor.line);
        let mut cx = nagi_lang::lang::Cx::new(self.language(), &src, None, &line, 0, start, prefix, None, &self.nav.libs);
        cx.console = Some(console);
        let out = nagi_lang::template::complete::complete(&cx);
        // 届いていない分は読み始め、届いたら作り直す
        let wants = cat.wants.take();
        if let Some(p) = self.db.as_mut() {
            let c = &mut p.conns[i];
            for w in wants {
                match w {
                    nagi_lang::sql::Want::Objects(s) => c.ensure_objects(&s),
                    nagi_lang::sql::Want::Columns(s, t) => c.ensure_columns(&s, &t),
                    nagi_lang::sql::Want::Relations(s, t) => c.ensure_relations(&s, &t),
                }
            }
        }
        self.data_wait = Some((crate::smart_complete::DataWait::Console(i), start));
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

    /// 列・テーブルが届いた: 出している SQL の補完の一覧を作り直す（smart_complete::data_arrived）
    pub(crate) fn sql_metadata_arrived(&mut self, i: usize) {
        self.data_arrived(crate::smart_complete::DataWait::Console(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Oracle はスキーマ・テーブル・列の名前を "" で囲んで入れる（つながずに、読んだことにした名前で）
    #[test]
    fn oracle_names_are_quoted() {
        use nagi_workers::db::{Column, Objects};
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("ora jdbc:oracle:thin:@//db:1521/X");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        let c = &mut e.db.as_mut().unwrap().conns[i];
        c.schema = "HR".into();
        c.schemas = Some(vec!["HR".into(), "SCOTT".into()]);
        c.objects.insert("HR".into(), Objects { tables: vec![("EMP".into(), "TABLE".into())], ..Default::default() });
        c.columns.insert(("HR".into(), "EMP".into()), vec![Column { name: "EMP_ID".into(), ty: "NUMBER".into(), pk: true, ..Default::default() }]);
        e.open_console(i, None);
        let keys = |e: &mut Editor, k: &str| {
            for key in nagi_vim::parse_keys(k, " ") {
                e.key(key);
            }
        };
        keys(&mut e, "Goselect * from em");
        e.start_smart_completion();
        keys(&mut e, "<CR>");
        assert!(e.buf.line(e.cursor.line).ends_with("from \"EMP\""), "{}", e.buf.line(e.cursor.line));
        keys(&mut e, " e where e.");
        e.start_smart_completion();
        assert_eq!(e.completion.as_ref().unwrap().items, vec!["EMP_ID"]);
        keys(&mut e, "<CR>");
        assert!(e.buf.line(e.cursor.line).ends_with("where e.\"EMP_ID\""), "{}", e.buf.line(e.cursor.line));
        keys(&mut e, "<Esc>o<CR>select * from sco");
        e.start_smart_completion();
        assert_eq!(e.completion.as_ref().unwrap().items[0], "SCOTT");
        keys(&mut e, "<CR>");
        assert!(e.buf.line(e.cursor.line).ends_with("from \"SCOTT\""), "{}", e.buf.line(e.cursor.line));
    }

    /// Salesforce のコンソールは SOQL: リレーションの先の項目・副問い合わせの子のリレーション（つながずに、読んだことにした名前で）。
    /// まだ読んでいない子のリレーションは読みに行く
    #[test]
    fn soql_in_salesforce_console() {
        use nagi_workers::db::{Column, Objects};
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("sf salesforce:https://login.salesforce.com me@example.com");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        let c = &mut e.db.as_mut().unwrap().conns[i];
        c.objects.insert(String::new(), Objects { tables: vec![("Account".into(), "TABLE".into()), ("Contact".into(), "TABLE".into())], ..Default::default() });
        let col = |n: &str, fk: &str, rel: &str| Column { name: n.into(), ty: "string".into(), fk: fk.into(), rel: rel.into(), ..Default::default() };
        c.columns.insert((String::new(), "Account".into()), vec![col("Id", "", ""), col("Name", "", ""), col("OwnerId", "User", "Owner")]);
        c.columns.insert((String::new(), "User".into()), vec![col("Id", "", ""), col("Email", "", "")]);
        c.columns.insert((String::new(), "Contact".into()), vec![col("Id", "", ""), col("LastName", "", "")]);
        e.open_console(i, None);
        assert_eq!(e.language().name(), "soql");
        let keys = |e: &mut Editor, k: &str| {
            for key in nagi_vim::parse_keys(k, " ") {
                e.key(key);
            }
        };
        // `|` の所にカーソルを置いて補完する
        let items = |e: &mut Editor, line: &str| -> Vec<String> {
            keys(e, &format!("<Esc>Go{line}<Esc>0f|x"));
            if e.cursor.byte >= e.buf.line(e.cursor.line).len() {
                keys(e, "a");
            } else {
                keys(e, "i");
            }
            e.start_smart_completion();
            let v = e.completion.as_ref().map(|c| c.items.clone()).unwrap_or_default();
            keys(e, "<Esc><Esc>");
            v
        };
        let v = items(&mut e, "select Owner.| from Account");
        assert!(v.contains(&"Email".to_string()), "リレーションの先の項目: {v:?}");
        // 子のリレーションはまだ読んでいない: 読みに行くと頼む（つないでいないので届かない）
        let v = items(&mut e, "select Id, (select Id from |) from Account");
        assert!(v.is_empty(), "{v:?}");
        let c = &mut e.db.as_mut().unwrap().conns[i];
        c.relations.insert((String::new(), "Account".into()), vec![("Contacts".into(), "Contact".into())]);
        let v = items(&mut e, "select Id, (select Id from |) from Account");
        assert!(v.contains(&"Contacts".to_string()), "子のリレーション: {v:?}");
        let v = items(&mut e, "select Id, (select | from Contacts) from Account");
        assert!(v.contains(&"LastName".to_string()) && !v.contains(&"Name".to_string()), "副問い合わせは子のオブジェクトの項目: {v:?}");
    }
}
