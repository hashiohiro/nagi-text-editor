//! テーブルのデータの編集（DataGrip のテーブルを開いた画面。窓の領域全体に出す）。
//!
//! 木のテーブルの上で o（ダブルクリック）で開く。WHERE / ORDER BY（f / F）、並べ替え（s）、ページ送り（< >）。
//! i（Enter）で値を直し（c なら空にしてから）、x で NULL、o で行を足し、dd で行を消す（もう一度で取り消し）、u で 1 つ戻す。
//! 直した値は青、足した行は緑、消した行は赤。Ctrl+Enter で作った SQL（INSERT・UPDATE・DELETE）を見せ、
//! 確認の窓で「はい」なら 1 つのトランザクションで送る（どの文も 1 行だけを変えなければ全部取り消す）。行は主キーで見分けるので、
//! 主キーの無いテーブルは読むだけ。

use std::collections::{BTreeSet, HashMap};

use nagi_vim::Key;
use nagi_workers::db::{Column, Reply};

use crate::db_panel::{MAX_ROWS, ident, literal, qualified};
use crate::editor::{Editor, State};
use crate::vim_field::{FieldOut, VimField};
use crate::result_grid::{Field, ResultTab, widths};

enum Change {
    /// (行, 列, 前の直した値。None は直していなかった)
    Edit(usize, usize, Option<Option<String>>),
    /// 足した行の値を直した (足した行の番号, 列, 前の値)
    AddedEdit(usize, usize, Option<String>),
    Add,
    Delete(usize),
}

/// 行の状態
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Normal,
    Added,
    Deleted,
}

pub struct TableEditor {
    pub src: usize,
    pub schema: String,
    pub table: String,
    pub cols: Vec<String>,
    pub types: Vec<String>,
    pub colinfo: Vec<Column>,
    pub rows: Vec<Vec<Option<String>>>,
    pub edits: HashMap<(usize, usize), Option<String>>,
    pub added: Vec<Vec<Option<String>>>,
    pub deleted: BTreeSet<usize>,
    undo: Vec<Change>,
    pub widths: Vec<usize>,
    pub row: usize,
    pub col: usize,
    pub scroll: usize,
    pub hscroll: usize,
    pub offset: usize,
    pub more: bool,
    pub total: Option<i64>,
    pub where_: String,
    pub order: String,
    pub running: bool,
    pub busy: bool,
    pub info: String,
    /// 値を直している欄（Vim 風）
    pub editing: Option<VimField>,
    pub filter_edit: Option<(Field, VimField)>,
    /// 送る前に見せる SQL
    pub preview: Option<Vec<String>>,
    pending: Option<char>,
    quit_armed: bool,
}

impl TableEditor {
    pub fn changes(&self) -> usize {
        let rows: BTreeSet<usize> = self.edits.keys().map(|k| k.0).filter(|r| !self.deleted.contains(r)).collect();
        rows.len() + self.added.len() + self.deleted.len()
    }

    pub fn len(&self) -> usize {
        self.added.len() + self.rows.len()
    }

    /// 表示の行 → 状態と、元の行の番号（足した行なら足した行の番号）
    pub fn at(&self, d: usize) -> (RowState, usize) {
        if d < self.added.len() {
            (RowState::Added, d)
        } else {
            let r = d - self.added.len();
            (if self.deleted.contains(&r) { RowState::Deleted } else { RowState::Normal }, r)
        }
    }

    pub fn value(&self, d: usize, c: usize) -> Option<String> {
        match self.at(d) {
            (RowState::Added, a) => self.added[a][c].clone(),
            (_, r) => self.edits.get(&(r, c)).cloned().unwrap_or_else(|| self.rows[r][c].clone()),
        }
    }

    pub fn modified(&self, d: usize, c: usize) -> bool {
        matches!(self.at(d), (RowState::Normal, r) if self.edits.contains_key(&(r, c)))
    }

    pub fn pk(&self) -> Vec<usize> {
        self.cols.iter().enumerate().filter(|(_, n)| self.colinfo.iter().any(|c| c.pk && &c.name == *n)).map(|(i, _)| i).collect()
    }

    fn name(&self) -> String {
        qualified(&self.schema, &self.table)
    }

    pub fn select_sql(&self) -> String {
        let mut s = format!("SELECT * FROM {}", self.name());
        if !self.where_.trim().is_empty() {
            s.push_str(&format!(" WHERE {}", self.where_.trim()));
        }
        if !self.order.trim().is_empty() {
            s.push_str(&format!(" ORDER BY {}", self.order.trim()));
        }
        s
    }

    pub fn count_sql(&self) -> String {
        let w = if self.where_.trim().is_empty() { String::new() } else { format!(" WHERE {}", self.where_.trim()) };
        format!("SELECT COUNT(*) FROM {}{w}", self.name())
    }

    /// 送る SQL（足す → 直す → 消すの順）
    pub fn statements(&self) -> nagi_core::Result<Vec<String>> {
        let pk = self.pk();
        if pk.is_empty() && (!self.edits.is_empty() || !self.deleted.is_empty()) {
            return Err("主キーが無いテーブルは直せません（読むだけ）".into());
        }
        let name = self.name();
        let mut out = Vec::new();
        for a in &self.added {
            let set: Vec<usize> = (0..self.cols.len()).filter(|&c| a[c].is_some()).collect();
            if set.is_empty() {
                continue;
            }
            out.push(format!(
                "INSERT INTO {name} ({}) VALUES ({})",
                set.iter().map(|&c| ident(&self.cols[c])).collect::<Vec<_>>().join(", "),
                set.iter().map(|&c| literal(&a[c])).collect::<Vec<_>>().join(", ")
            ));
        }
        let where_pk = |r: usize| pk.iter().map(|&c| format!("{} = {}", ident(&self.cols[c]), literal(&self.rows[r][c]))).collect::<Vec<_>>().join(" AND ");
        let rows: BTreeSet<usize> = self.edits.keys().map(|k| k.0).filter(|r| !self.deleted.contains(r)).collect();
        for r in rows {
            let mut set: Vec<(usize, &Option<String>)> = self.edits.iter().filter(|(k, _)| k.0 == r).map(|(k, v)| (k.1, v)).collect();
            set.sort_by_key(|x| x.0);
            out.push(format!("UPDATE {name} SET {} WHERE {}", set.iter().map(|(c, v)| format!("{} = {}", ident(&self.cols[*c]), literal(v))).collect::<Vec<_>>().join(", "), where_pk(r)));
        }
        for &r in &self.deleted {
            out.push(format!("DELETE FROM {name} WHERE {}", where_pk(r)));
        }
        Ok(out)
    }

    pub fn set_columns(&mut self, cols: Vec<Column>) {
        self.colinfo = cols;
    }

    pub fn reply(&mut self, r: Reply) {
        self.running = false;
        match r {
            Reply::Rows { cols, types, rows, more, ms } => {
                self.widths = widths(&cols, &types, &rows);
                self.info = format!("{ms} ms");
                self.cols = cols;
                self.types = types;
                self.rows = rows;
                self.more = more;
                self.edits.clear();
                self.added.clear();
                self.deleted.clear();
                self.undo.clear();
                self.busy = false;
                self.row = self.row.min(self.len().saturating_sub(1));
                self.col = self.col.min(self.cols.len().saturating_sub(1));
            }
            Reply::Error(e) => self.info = format!("誤り: {}", e.lines().next().unwrap_or("")),
            _ => {}
        }
    }

    /// 書き出し用に、今の値の表にする
    pub fn as_tab(&self) -> ResultTab {
        ResultTab {
            id: 0,
            src: self.src,
            title: self.table.clone(),
            base: self.select_sql(),
            filterable: false,
            cols: self.cols.clone(),
            types: self.types.clone(),
            rows: (0..self.len()).filter(|&d| self.at(d).0 != RowState::Deleted).map(|d| (0..self.cols.len()).map(|c| self.value(d, c)).collect()).collect(),
            widths: self.widths.clone(),
            info: String::new(),
            error: false,
            running: false,
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            offset: 0,
            more: false,
            total: None,
            where_: String::new(),
            order: String::new(),
        }
    }

    /// 「1–500 / 12,480」
    pub fn range_label(&self) -> String {
        if self.rows.is_empty() {
            return "0 行".into();
        }
        let (a, b) = (self.offset + 1, self.offset + self.rows.len());
        let total = match self.total {
            Some(t) => crate::result_grid::group(t),
            None if self.more => format!("{}+", crate::result_grid::group(b as i64)),
            None => crate::result_grid::group(b as i64),
        };
        format!("{a}–{b} / {total}")
    }

    fn set_value(&mut self, d: usize, c: usize, v: Option<String>) {
        match self.at(d) {
            (RowState::Added, a) => {
                let prev = std::mem::replace(&mut self.added[a][c], v);
                self.undo.push(Change::AddedEdit(a, c, prev));
            }
            (RowState::Deleted, _) => {}
            (RowState::Normal, r) => {
                let prev = if self.rows[r][c] == v { self.edits.remove(&(r, c)) } else { self.edits.insert((r, c), v) };
                self.undo.push(Change::Edit(r, c, prev));
            }
        }
    }

    fn undo_one(&mut self) -> bool {
        let Some(ch) = self.undo.pop() else { return false };
        match ch {
            Change::Edit(r, c, prev) => {
                match prev {
                    Some(v) => self.edits.insert((r, c), v),
                    None => self.edits.remove(&(r, c)),
                };
            }
            Change::AddedEdit(a, c, prev) => {
                if a < self.added.len() {
                    self.added[a][c] = prev;
                }
            }
            Change::Add => {
                if !self.added.is_empty() {
                    self.added.remove(0);
                }
            }
            Change::Delete(r) => {
                if !self.deleted.remove(&r) {
                    self.deleted.insert(r);
                }
            }
        }
        true
    }
}

impl Editor {
    /// テーブル（ビュー）のデータを開く
    pub(crate) fn db_open_table(&mut self, i: usize, schema: &str, table: &str) {
        let mut te = TableEditor {
            src: i,
            schema: schema.into(),
            table: table.into(),
            cols: Vec::new(),
            types: Vec::new(),
            colinfo: Vec::new(),
            rows: Vec::new(),
            edits: HashMap::new(),
            added: Vec::new(),
            deleted: BTreeSet::new(),
            undo: Vec::new(),
            widths: Vec::new(),
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            offset: 0,
            more: false,
            total: None,
            where_: String::new(),
            order: String::new(),
            running: true,
            busy: false,
            info: "読んでいます…".into(),
            editing: None,
            filter_edit: None,
            preview: None,
            pending: None,
            quit_armed: false,
        };
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if let Some(cols) = c.columns.get(&(schema.to_string(), table.to_string())) {
            te.colinfo = cols.clone();
        } else {
            c.send_columns(schema, table);
        }
        c.send_table_query(te.select_sql(), 0, Some(te.count_sql()));
        self.db_table = Some(te);
        self.set_state(State::DbTable);
    }

    /// 読み直す（変更は捨てる）
    pub(crate) fn db_table_reload(&mut self) {
        let Some(te) = &mut self.db_table else { return };
        te.running = true;
        te.preview = None;
        let (i, sql, count, offset) = (te.src, te.select_sql(), te.count_sql(), te.offset);
        if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
            c.send_table_query(sql, offset, Some(count));
        }
    }

    pub(crate) fn close_db_table(&mut self) {
        self.db_table = None;
        if self.state() == State::DbTable {
            self.set_state(State::Normal);
        }
    }

    /// 入力欄に IME の確定を入れる
    pub(crate) fn db_table_insert(&mut self, text: &str) {
        let Some(te) = &mut self.db_table else { return };
        if let Some(f) = &mut te.editing {
            f.insert_text(text);
        } else if let Some((_, f)) = &mut te.filter_edit {
            f.insert_text(text);
        }
    }

    /// IME を使う入力欄にいるか
    pub fn db_table_typing(&self) -> bool {
        self.state() == State::DbTable && self.db_table.as_ref().is_some_and(|t| t.editing.as_ref().is_some_and(|f| f.is_insert()) || t.filter_edit.as_ref().is_some_and(|f| f.1.is_insert()))
    }

    /// 確かめた変更を送る
    fn db_table_send(&mut self) {
        let Some(te) = &mut self.db_table else { return };
        let stmts = te.preview.take().unwrap_or_default();
        te.busy = true;
        let i = te.src;
        if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
            c.send_batch(stmts);
        }
        self.notify.info("送信しています…");
    }

    pub(crate) fn db_table_key(&mut self, key: Key) {
        let half = (self.windows_rows() / 2).max(1);
        let Some(te) = &mut self.db_table else {
            self.set_state(State::Normal);
            return;
        };
        // 値の入力中（Vim 風。Enter か Tab で決める、ノーマルの Esc でやめる）
        if te.editing.is_some() {
            let out = if key == Key::Tab { FieldOut::Submit } else { self.feed_field(key, |e| e.db_table.as_mut().and_then(|t| t.editing.as_mut())) };
            let Some(te) = &mut self.db_table else { return };
            match out {
                FieldOut::Cancel => te.editing = None,
                FieldOut::Submit => {
                    let v = te.editing.take().map(|f| f.text()).unwrap_or_default();
                    let (d, c) = (te.row, te.col);
                    te.set_value(d, c, Some(v));
                    if key == Key::Tab {
                        te.col = (te.col + 1).min(te.cols.len().saturating_sub(1));
                    }
                }
                _ => {}
            }
            return;
        }
        // 絞り込みの欄の入力中
        if te.filter_edit.is_some() {
            let out = self.feed_field(key, |e| e.db_table.as_mut().and_then(|t| t.filter_edit.as_mut()).map(|x| &mut x.1));
            let Some(te) = &mut self.db_table else { return };
            match out {
                FieldOut::Cancel => te.filter_edit = None,
                FieldOut::Submit => {
                    let (field, f) = te.filter_edit.take().unwrap();
                    let text = f.text();
                    match field {
                        Field::Where => te.where_ = text,
                        Field::Order => te.order = text,
                    }
                    te.offset = 0;
                    te.total = None;
                    self.db_table_reload();
                }
                _ => {}
            }
            return;
        }
        let pending = te.pending.take();
        let last_row = te.len().saturating_sub(1);
        let last_col = te.cols.len().saturating_sub(1);
        if key != Key::Char('q') {
            te.quit_armed = false;
        }
        let dirty = te.changes() > 0;
        match (pending, key) {
            (Some('d'), Key::Char('d')) => match te.at(te.row) {
                (RowState::Added, a) => {
                    te.added.remove(a);
                    te.row = te.row.min(te.len().saturating_sub(1));
                }
                (_, r) => {
                    if !te.deleted.remove(&r) {
                        te.deleted.insert(r);
                    }
                    te.undo.push(Change::Delete(r));
                }
            },
            (Some('g'), Key::Char('g')) => te.row = 0,
            (_, Key::Char(c @ ('d' | 'g'))) => te.pending = Some(c),
            (_, Key::Char('j') | Key::Down) => te.row = (te.row + 1).min(last_row),
            (_, Key::Char('k') | Key::Up) => te.row = te.row.saturating_sub(1),
            (_, Key::Char('l') | Key::Right | Key::Tab) => te.col = (te.col + 1).min(last_col),
            (_, Key::Char('h') | Key::Left) => te.col = te.col.saturating_sub(1),
            (_, Key::Char('0') | Key::Home) => te.col = 0,
            (_, Key::Char('$') | Key::End) => te.col = last_col,
            (_, Key::Ctrl('d')) => te.row = (te.row + half).min(last_row),
            (_, Key::Ctrl('u')) => te.row = te.row.saturating_sub(half),
            (_, Key::Char('G')) => te.row = last_row,
            (_, Key::Char('i') | Key::Enter) if !te.cols.is_empty() && te.len() > 0 => {
                if te.at(te.row).0 == RowState::Deleted {
                    self.notify.warn("消す行は直せません（dd で取り消し）");
                } else {
                    te.editing = Some(VimField::new(&te.value(te.row, te.col).unwrap_or_default(), false));
                }
            }
            // c: 空にしてから直す（値を入れ替える）
            (_, Key::Char('c')) if !te.cols.is_empty() && te.len() > 0 => {
                if te.at(te.row).0 == RowState::Deleted {
                    self.notify.warn("消す行は直せません（dd で取り消し）");
                } else {
                    te.editing = Some(VimField::new("", false));
                }
            }
            (_, Key::Char('x')) if !te.cols.is_empty() && te.len() > 0 => {
                let (d, c) = (te.row, te.col);
                te.set_value(d, c, None);
            }
            (_, Key::Char('o')) if !te.cols.is_empty() => {
                te.added.insert(0, vec![None; te.cols.len()]);
                te.undo.push(Change::Add);
                te.row = 0;
            }
            (_, Key::Char('u')) => {
                if !te.undo_one() {
                    self.notify.info("戻す変更はありません");
                }
            }
            (_, Key::Ctrl('m')) => match te.statements() {
                Ok(s) if s.is_empty() => self.notify.info("送る変更はありません"),
                Ok(s) => {
                    let q = format!("{} 件の変更を送りますか（送る文は上に出ています）", s.len());
                    te.preview = Some(s);
                    self.ask_or(q, |e| e.db_table_send(), |e| {
                        if let Some(te) = &mut e.db_table {
                            te.preview = None;
                        }
                        e.notify.info("送信をやめました（変更は残っています）");
                    });
                }
                Err(e) => self.notify.error(e),
            },
            (_, Key::Char('f') | Key::Char('F')) if !dirty => {
                let field = if key == Key::Char('f') { Field::Where } else { Field::Order };
                te.filter_edit = Some((field, VimField::new(if field == Field::Where { &te.where_ } else { &te.order }, false)));
            }
            (_, Key::Char('s')) if !dirty && !te.cols.is_empty() => {
                let c = ident(&te.cols[te.col]);
                te.order = if te.order == format!("{c} ASC") {
                    format!("{c} DESC")
                } else if te.order == format!("{c} DESC") {
                    String::new()
                } else {
                    format!("{c} ASC")
                };
                te.offset = 0;
                self.db_table_reload();
            }
            (_, Key::Char('>')) if !dirty && te.more => {
                te.offset += MAX_ROWS;
                te.row = 0;
                self.db_table_reload();
            }
            (_, Key::Char('<')) if !dirty && te.offset > 0 => {
                te.offset = te.offset.saturating_sub(MAX_ROWS);
                te.row = 0;
                self.db_table_reload();
            }
            (_, Key::Char('R')) => {
                if dirty && !te.quit_armed {
                    te.quit_armed = true;
                    self.notify.info("未送信の変更があります。もう一度 R で捨てて読み直す");
                } else {
                    self.db_table_reload();
                }
            }
            (_, Key::Char('f' | 'F' | 's' | '>' | '<')) => self.notify.info("未送信の変更があります（Ctrl+Enter で送るか、u で戻す）"),
            (_, Key::Char('q') | Key::Esc) => {
                if dirty && !te.quit_armed {
                    te.quit_armed = true;
                    self.notify.info(format!("未送信の変更が {} 件あります。もう一度 q で捨てて閉じる", te.changes()));
                } else {
                    self.close_db_table();
                }
            }
            (_, Key::Char(':')) => {
                self.enter_command("");
            }
            _ => {}
        }
    }
}



/// この部品の入口（component.rs）
/// Vim を使わないときの状態行の説明
const PLAIN_HELP: &str = "矢印 移動  Enter 値を直す  右クリック 操作  Ctrl+Enter 送る ";

pub struct DbTableComponent;

impl crate::component::Component for DbTableComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let n = ed.db_table.as_ref().map_or(0, |t| t.changes());
        let help = "i 値  c 入れ替え  x NULL  o 行を足す  dd 消す  u 戻す  f/F 絞り込み  s 並べ替え  < > ページ  Ctrl+Enter 送信  q 閉じる ";
        if n > 0 {
            Some(crate::component::Status { name: format!("-- 編集 --  変更 {n} 件（未送信）"), warn: true, help: help.into(), plain: Some(PLAIN_HELP.into()) })
        } else {
            crate::component::Status::keys("-- テーブル --", help, PLAIN_HELP)
        }
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.db_table_key(key);
        crate::editor::Effect::None
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.db_table_insert(text);
    }
}


/// テーブルの編集（layout.rs の View）
pub struct DbTableView;

impl crate::layout::View for DbTableView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.db_table.as_ref()?;
        let (x, w) = ed.windows_x();
        Some(crate::panes::Rect { x, y: 0, w, h: ed.windows_rows() })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        if let Some(te) = &ed.db_table {
            draw::draw_db_table(p, ed, te, r);
        }
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        // 窓を覆って出ているテーブル: 押したらテーブルへ向き、押したセルを選ぶ（ダブルクリックで直す。Enter と同じ）。下の本文へは通さない
        ed.set_state(crate::editor::State::DbTable);
        let Some(te) = &mut ed.db_table else { return crate::layout::Click::Done };
        // 値を打っている間・送る前の確認の間は、選び直さない
        if te.editing.is_some() || te.preview.is_some() {
            return crate::layout::Click::Done;
        }
        // 描き方（draw_db_table）と同じ割り付け: 見出し 2 行の下に表（表の見出し 2 行）
        let len = te.len();
        let pick = crate::list::pick_row(&mut te.row, te.scroll, row - r.y, 4, len, clicks);
        if pick.is_some() {
            if let Some(c) = crate::result_grid::grid_col_at(te.hscroll, &te.widths, col - r.x) {
                te.col = c;
            }
        }
        if let Some(crate::list::Pick::Accept(_)) = pick {
            ed.db_table_key(nagi_vim::Key::Enter);
        }
        crate::layout::Click::Done
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (row, col);
        if let Some(te) = &mut ed.db_table {
            // 上の 4 行は見出し（view.rs の build と同じ高さ）
            let len = te.len();
            crate::list::wheel(&mut te.row, &mut te.scroll, len, r.h.saturating_sub(4), dy);
            crate::result_grid::grid_hwheel(&mut te.col, &mut te.hscroll, &te.widths, r.w, dx);
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        use nagi_vim::Key::*;
        let _ = (r, row, col);
        ed.set_state(crate::editor::State::DbTable);
        let m = crate::menu::Menu::new(row, col)
            .keys("値を直す", &[Char('i')])
            .keys("値を入れ替える", &[Char('c')])
            .keys("NULL にする", &[Char('x')])
            .keys("行を足す", &[Char('o')])
            .keys("行を消す", &[Char('d'), Char('d')])
            .keys("戻す", &[Char('u')])
            .sep()
            .keys("絞り込み（WHERE）", &[Char('f')])
            .keys("並べ替え（ORDER BY）", &[Char('F')])
            .keys("この列で並べ替え", &[Char('s')])
            .keys("次のページ", &[Char('>')])
            .keys("前のページ", &[Char('<')])
            .keys("読み直す", &[Char('R')])
            .sep()
            .keys("変更を送る", &[Ctrl('m')])
            .keys("閉じる", &[Char('q')]);
        ed.open_menu(m);
        true
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// テーブルのデータの編集（窓の領域全体）
    pub(crate) fn draw_db_table(p: &mut Painter, ed: &Editor, te: &crate::db_table::TableEditor, r: Rect) {
        use crate::db_table::RowState;
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
        let saved = p.cols;
        p.cols = r.x + r.w;
        // 1 行目: 名前と接続先、件数
        let src = ed.db.as_ref().map_or(String::new(), |d| d.sources[te.src].name.clone());
        let c = p.text(&format!(" {}", crate::db_panel::qualified(&te.schema, &te.table)), r.x, y0, FG);
        let c = p.text(&format!("（{src}）"), c, y0, STATUS_FG);
        if te.pk().is_empty() && !te.colinfo.is_empty() {
            p.text("  主キーが無いので読むだけ", c, y0, DIAG_WARN);
        }
        let right = format!("{}   {}{}", te.range_label(), if te.running { "読んでいます…" } else { &te.info }, if te.busy { "   送信しています…" } else { "" });
        let rw = p.width(&right);
        p.text(&right, (r.x + r.w).saturating_sub(rw + 1), y0, STATUS_FG);
        // 2 行目: 絞り込み
        let fy = y0 + cell.h;
        draw_filter_row(p, r, fy, &te.where_, &te.order, te.filter_edit.as_ref(), "", &ed.preedit);
        p.out.push(Instance::fill(x0, fy + cell.h - 1.0, wpx, 1.0, rgb(0x2a2a2a)));
        let marks: Vec<char> = te.cols.iter().map(|n| te.colinfo.iter().find(|c| &c.name == n).map_or(' ', |c| if c.pk { 'K' } else if !c.fk.is_empty() { 'F' } else { ' ' })).collect();
        let value = |d: usize, c: usize| te.value(d, c);
        let row_style = |d: usize| match te.at(d) {
            (RowState::Added, _) => ("＋".to_string(), Some(DIFF_ADD_BG)),
            (RowState::Deleted, r) => ((te.offset + r + 1).to_string(), Some(DIFF_DEL_BG)),
            (RowState::Normal, r) => ((te.offset + r + 1).to_string(), None),
        };
        let cell_bg = |d: usize, c: usize| te.modified(d, c).then_some(rgb(0x1f3550));
        let g = GridView {
            cols: &te.cols,
            types: &te.types,
            widths: &te.widths,
            marks,
            sort: sort_of(&te.order),
            rows: te.len(),
            value: &value,
            row_style: &row_style,
            cell_bg: &cell_bg,
            row: te.row,
            col: te.col,
            scroll: te.scroll,
            hscroll: te.hscroll,
            focused: te.editing.is_none(),
        };
        draw_grid(p, &g, Rect { x: r.x, y: r.y + 2, w: r.w, h: r.h.saturating_sub(2) });
        // 値の入力欄（選んだセルの上に）
        if let Some(f) = &te.editing {
            if te.row >= te.scroll {
                let mut x = r.x + 7;
                for c in te.hscroll..te.col {
                    x += te.widths[c] + 3;
                }
                let y = (r.y + 4 + te.row - te.scroll) as f32 * cell.h;
                let w = (te.widths.get(te.col).copied().unwrap_or(10) + 2).max(p.width(&f.text()) + p.width(&ed.preedit) + 3);
                p.out.push(Instance::fill((x - 1) as f32 * cell.w - 1.0, y - 1.0, w as f32 * cell.w + 2.0, cell.h + 2.0, FG));
                p.out.push(Instance::fill((x - 1) as f32 * cell.w, y, w as f32 * cell.w, cell.h, rgb(0x1f3550)));
                draw_vim_field(p, f, x, y, &ed.preedit);
            }
        }
        // 送る前の確認（右下に SQL）
        if let Some(stmts) = &te.preview {
            let lines: Vec<String> = std::iter::once(format!(" 送信する変更（{} 件） — {src}", stmts.len())).chain(stmts.iter().map(|s| format!(" {s}"))).collect();
            let w = lines.iter().map(|l| p.width(l)).max().unwrap_or(20).min(r.w.saturating_sub(4)) + 2;
            let h = lines.len().min(r.h.saturating_sub(4));
            let bx = (r.x + r.w).saturating_sub(w + 2);
            let by = (r.y + r.h).saturating_sub(h + 1);
            let (x, y) = (bx as f32 * cell.w, by as f32 * cell.h);
            p.out.push(Instance::fill(x - 1.0, y - 1.0, w as f32 * cell.w + 2.0, h as f32 * cell.h + 2.0, STATUS_FG));
            p.out.push(Instance::fill(x, y, w as f32 * cell.w, h as f32 * cell.h, rgb(0x141414)));
            let lim = p.cols;
            p.cols = bx + w;
            for (k, l) in lines.iter().take(h).enumerate() {
                let color = if k == 0 { FG } else if k + 1 == lines.len() { STATUS_FG } else { FINDER_MATCH };
                p.text(l, bx, y + k as f32 * cell.h, color);
            }
            p.cols = lim;
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_panel::tests::wait;

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    /// SQLite で: 開く → 値を直す・NULL・行を足す・消す → SQL を見て y で送る → 読み直した中身
    /// マウスで: セルを押すと選び、ダブルクリックで直し始める（Enter と同じ）
    #[test]
    fn click_cells() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == crate::db_panel::DbRow::Source(i)).unwrap();
        keys(&mut e, "<CR>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        for sql in ["create table item(id integer primary key, name text)", "insert into item values (1, 'a'), (2, 'b')"] {
            e.db.as_mut().unwrap().conns[i].manual = true;
            e.db_execute(i, sql.into());
            wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running));
        }
        e.db.as_mut().unwrap().conns[i].manual = false;
        e.db_open_table(i, "main", "item");
        wait(&mut e, &|e| e.db_table.as_ref().is_some_and(|t| !t.running && !t.rows.is_empty()));
        let s = crate::layout::Screen { rows: 50, cols: 160 };
        let (x, _) = e.windows_x();
        // 表は 2 行目から（見出し 2 行）なので、データの 2 行目は 5 行目。name は 7 列目 + id の幅 + 3 から
        let name_x = x + 7 + e.db_table.as_ref().unwrap().widths[0] + 3 + 1;
        crate::layout::press(&mut e, s, 5, name_x, |_| 1);
        let te = e.db_table.as_ref().unwrap();
        assert_eq!((te.row, te.col), (1, 1));
        assert!(te.editing.is_none(), "1 回では直さない");
        crate::layout::press(&mut e, s, 5, name_x, |_| 2);
        assert!(e.db_table.as_ref().unwrap().editing.is_some(), "ダブルクリックで直し始める");
    }

    #[test]
    fn edit_and_submit() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == crate::db_panel::DbRow::Source(i)).unwrap();
        keys(&mut e, "<CR>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        for sql in ["create table item(id integer primary key, name text, note text)", "insert into item values (1, 'a', 'x'), (2, 'b', 'y'), (3, 'c', 'z')"] {
            e.db.as_mut().unwrap().conns[i].manual = true;
            e.db_execute(i, sql.into());
            wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running));
        }
        e.db.as_mut().unwrap().conns[i].manual = false;
        e.db_open_table(i, "main", "item");
        assert_eq!(e.state(), State::DbTable);
        wait(&mut e, &|e| e.db_table.as_ref().is_some_and(|t| !t.running && !t.colinfo.is_empty() && t.total.is_some()));
        assert_eq!(e.db_table.as_ref().unwrap().range_label(), "1–3 / 3");
        // 1 行目の name を直す、2 行目の note を NULL に、3 行目を消す、行を足す
        // i は今の値の後ろから Insert。Esc でノーマルにして cc で入れ替える（Vim 風の欄）
        keys(&mut e, "li");
        keys(&mut e, "<Esc>ccりんご<CR>");
        keys(&mut e, "jlx");
        keys(&mut e, "jdd");
        keys(&mut e, "o0i9<Tab>inew<CR>");
        let te = e.db_table.as_ref().unwrap();
        assert_eq!(te.changes(), 4);
        assert_eq!(
            te.statements().unwrap(),
            vec![
                "INSERT INTO main.item (id, name) VALUES ('9', 'new')".to_string(),
                "UPDATE main.item SET name = 'りんご' WHERE id = '1'".into(),
                "UPDATE main.item SET note = NULL WHERE id = '2'".into(),
                "DELETE FROM main.item WHERE id = '3'".into(),
            ]
        );
        // u で 1 つ戻す（足した行の name）→ もう一度入れる
        keys(&mut e, "u");
        assert_eq!(e.db_table.as_ref().unwrap().added[0][1], None);
        keys(&mut e, "inew<CR>");
        // Ctrl+Enter で SQL を見せ、y で送る
        keys(&mut e, "<C-m>");
        assert!(e.db_table.as_ref().unwrap().preview.is_some());
        keys(&mut e, "y");
        wait(&mut e, &|e| e.db_table.as_ref().is_some_and(|t| !t.busy && !t.running && t.changes() == 0 && t.rows.len() == 3));
        let te = e.db_table.as_ref().unwrap();
        let names: Vec<Option<String>> = te.rows.iter().map(|r| r[1].clone()).collect();
        assert_eq!(names, vec![Some("りんご".into()), Some("b".into()), Some("new".into())]);
        assert_eq!(te.rows[1][2], None);
        // 未送信の変更があれば q は 2 回
        keys(&mut e, "x");
        // 本文に向こうとしても（木でファイルを開いたなど）、未送信の変更があれば閉じずに知らせる
        e.set_state(State::Normal);
        assert!(e.db_table.is_some());
        assert_eq!(e.state(), State::DbTable);
        assert!(e.notify.text.contains("送っていない変更が 1 件"), "{}", e.notify.text);
        keys(&mut e, "q");
        assert!(e.db_table.is_some());
        keys(&mut e, "q");
        assert!(e.db_table.is_none());
    }
}
