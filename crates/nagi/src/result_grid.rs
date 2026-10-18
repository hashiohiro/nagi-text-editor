//! データベースの結果（下のパネル。全文検索の結果と入れ替えて出す。DataGrip の Services と同じ形）。
//!
//! タブ: 「出力」（流した文・件数・時間・エラーの記録）と「結果 1」「結果 2」…（流すたびに 1 つ。8 つまで）。
//! 表: 行番号、列名と型の 2 行の見出し、WHERE / ORDER BY の絞り込み（f / F）、列で並べ替え（s）、
//! ページ送り（< >。1 ページ 500 行、件数は数えてから出す）、コピー（y 値・Y 行・gy 表全体をタブ区切り）、
//! 書き出し（:DbExport csv|tsv|insert）。] [ でタブを替える。

use nagi_vim::Key;
use nagi_workers::db::Reply;

use crate::db_panel::MAX_ROWS;
use crate::editor::{Editor, State, Side};
use crate::vim_field::{FieldOut, VimField};

/// 下のパネルに出しているもの
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BottomKind {
    /// 全文検索の結果
    #[default]
    Grep,
    /// クエリの結果
    Result,
    /// 履歴（グラフ付きのログ）
    Git,
    /// 診断の一覧
    Problems,
    /// 実行したコマンドの出力
    Output,
    /// シェル
    Terminal,
}

/// 表に出す 1 つの値（改行とタブは記号に）
pub fn show(v: &Option<String>) -> String {
    match v {
        None => "<null>".into(),
        Some(s) => s.replace('\n', "↵").replace('\r', "").replace('\t', "→"),
    }
}

/// 列の幅（値の先頭 300 行と列名・型から。4〜40）
pub fn widths(cols: &[String], types: &[String], rows: &[Vec<Option<String>>]) -> Vec<usize> {
    let w = |s: &str| nagi_core::display_col(s, s.len(), false);
    (0..cols.len()).map(|c| rows.iter().take(300).map(|r| w(&show(&r[c]))).chain([w(&cols[c]) + 2, types.get(c).map_or(0, |t| w(t))]).max().unwrap_or(4).clamp(4, 40)).collect()
}

pub struct ResultTab {
    pub id: u64,
    /// 接続先の番号
    pub src: usize,
    pub title: String,
    /// 流した SQL（絞り込みはこれを包んで流す）
    pub base: String,
    /// 絞り込み・ページ送りができるか（読むだけの文）
    pub filterable: bool,
    pub cols: Vec<String>,
    pub types: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub widths: Vec<usize>,
    /// 件数などの補足（「120 行 · 35 ms」「3 行を変えました」「誤り: …」）
    pub info: String,
    pub error: bool,
    pub running: bool,
    pub row: usize,
    pub col: usize,
    pub scroll: usize,
    pub hscroll: usize,
    pub offset: usize,
    pub more: bool,
    pub total: Option<i64>,
    pub where_: String,
    pub order: String,
}

impl ResultTab {
    /// 絞り込みと並べ替えを入れた SQL
    pub fn effective_sql(&self) -> String {
        let base = nagi_workers::db::strip_semicolon(&self.base);
        if !self.filterable || (self.where_.trim().is_empty() && self.order.trim().is_empty()) {
            return base;
        }
        let mut s = format!("SELECT * FROM (\n{base}\n) nagi_q");
        if !self.where_.trim().is_empty() {
            s.push_str(&format!(" WHERE {}", self.where_.trim()));
        }
        if !self.order.trim().is_empty() {
            s.push_str(&format!(" ORDER BY {}", self.order.trim()));
        }
        s
    }

    /// 件数を数える SQL（読むだけの文のとき）
    pub fn count_sql(&self) -> Option<String> {
        if !self.filterable {
            return None;
        }
        let base = nagi_workers::db::strip_semicolon(&self.base);
        let w = if self.where_.trim().is_empty() { String::new() } else { format!(" WHERE {}", self.where_.trim()) };
        Some(format!("SELECT COUNT(*) FROM (\n{base}\n) nagi_q{w}"))
    }

    /// 「1–500 / 12,480」
    pub fn range_label(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let (a, b) = (self.offset + 1, self.offset + self.rows.len());
        let total = match self.total {
            Some(t) => group(t),
            None if self.more => format!("{}+", group(b as i64)),
            None => group(b as i64),
        };
        format!("{a}–{b} / {total}")
    }

    /// 表全体をタブ区切りに（1 行目は列名）
    pub fn to_tsv(&self) -> String {
        let mut s = self.cols.join("\t");
        for r in &self.rows {
            s.push('\n');
            s.push_str(&r.iter().map(|v| v.clone().unwrap_or_default()).collect::<Vec<_>>().join("\t"));
        }
        s
    }

    pub fn to_csv(&self) -> String {
        let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
        let mut s = self.cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(",");
        for r in &self.rows {
            s.push_str("\r\n");
            s.push_str(&r.iter().map(|v| v.as_deref().map(q).unwrap_or_default()).collect::<Vec<_>>().join(","));
        }
        s.push_str("\r\n");
        s
    }

    pub fn to_inserts(&self, table: &str) -> String {
        let cols = self.cols.iter().map(|c| crate::db_panel::ident(c)).collect::<Vec<_>>().join(", ");
        self.rows.iter().map(|r| format!("INSERT INTO {table} ({cols}) VALUES ({});\n", r.iter().map(crate::db_panel::literal).collect::<Vec<_>>().join(", "))).collect()
    }
}

/// 3 桁ごとの区切り
pub fn group(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

pub struct LogLine {
    pub time: String,
    pub src: String,
    pub text: String,
    pub outcome: String,
    pub error: bool,
}

/// 絞り込みの欄
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Where,
    Order,
}

#[derive(Default)]
pub struct DbResults {
    pub tabs: Vec<ResultTab>,
    /// 0 は「出力」、1 から結果のタブ
    pub active: usize,
    pub log: Vec<LogLine>,
    pub log_scroll: usize,
    next_id: u64,
    /// 絞り込みの欄の入力中（Vim 風）
    pub editing: Option<(Field, VimField)>,
    pending_g: bool,
}

fn now_hms() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
    }
    #[cfg(not(windows))]
    {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        format!("{:02}:{:02}:{:02}", s / 3600 % 24, s / 60 % 60, s % 60)
    }
}

impl DbResults {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty() && self.log.is_empty()
    }

    pub fn new_tab(&mut self, src: usize, title: &str, sql: &str, filterable: bool) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        if self.tabs.len() >= 8 {
            self.tabs.remove(0);
        }
        self.tabs.push(ResultTab {
            id,
            src,
            title: title.into(),
            base: sql.into(),
            filterable,
            cols: Vec::new(),
            types: Vec::new(),
            rows: Vec::new(),
            widths: Vec::new(),
            info: "実行しています…".into(),
            error: false,
            running: true,
            row: 0,
            col: 0,
            scroll: 0,
            hscroll: 0,
            offset: 0,
            more: false,
            total: None,
            where_: String::new(),
            order: String::new(),
        });
        self.active = self.tabs.len();
        id
    }

    pub fn tab(&self, id: u64) -> Option<&ResultTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut ResultTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active_tab(&self) -> Option<&ResultTab> {
        self.active.checked_sub(1).and_then(|i| self.tabs.get(i))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut ResultTab> {
        self.active.checked_sub(1).and_then(|i| self.tabs.get_mut(i))
    }

    pub fn reply(&mut self, id: u64, r: Reply) {
        let Some(t) = self.tab_mut(id) else { return };
        t.running = false;
        match r {
            Reply::Rows { cols, types, rows, more, ms } => {
                t.widths = widths(&cols, &types, &rows);
                t.info = format!("{} 行{} · {ms} ms", rows.len(), if more { "（続きあり）" } else { "" });
                t.cols = cols;
                t.types = types;
                t.rows = rows;
                t.more = more;
                t.error = false;
                t.row = t.row.min(t.rows.len().saturating_sub(1));
                t.col = t.col.min(t.cols.len().saturating_sub(1));
            }
            Reply::Updated { count, ms } => {
                t.info = format!("{} · {ms} ms", if count >= 0 { format!("{count} 行を変えました") } else { "実行しました".into() });
                t.error = false;
            }
            Reply::Error(e) => {
                t.info = format!("誤り: {}", e.lines().next().unwrap_or(""));
                t.error = true;
            }
            _ => {}
        }
    }

    pub fn set_total(&mut self, id: u64, n: i64) {
        if let Some(t) = self.tab_mut(id) {
            t.total = Some(n);
        }
    }

    pub fn log(&mut self, src: String, text: String, outcome: String, error: bool) {
        self.log.push(LogLine { time: now_hms(), src, text, outcome, error });
        if self.log.len() > 1000 {
            self.log.remove(0);
        }
    }
}

impl Editor {
    /// 結果のパネルを出す（全文検索の結果とは入れ替え）
    pub(crate) fn show_results(&mut self) {
        self.bottom_kind = BottomKind::Result;
        self.bottom_hidden = false;
        self.rows = self.text_rows(self.active_rect());
    }

    fn copy_text(&mut self, s: String, what: &str) {
        if let Some(cb) = &self.clipboard {
            (cb.set)(&s);
        }
        let n = s.lines().count();
        self.registers.insert('"', (s, false));
        self.notify.info(format!("{what}をコピーしました{}", if n > 1 { format!("（{n} 行）") } else { String::new() }));
    }

    /// `:DbExport csv|tsv|insert`: 今の結果（テーブルの編集の画面ならその表）を書き出して開く
    pub(crate) fn db_export(&mut self, fmt: &str) {
        let fmt = fmt.trim().to_ascii_lowercase();
        let (text, name) = if let Some(te) = &self.db_table {
            let t = te.as_tab();
            let table = crate::db_panel::qualified(&te.schema, &te.table);
            (match fmt.as_str() {
                "csv" => t.to_csv(),
                "insert" | "sql" => t.to_inserts(&table),
                _ => t.to_tsv(),
            }, te.table.clone())
        } else if let Some(t) = self.db_results.active_tab() {
            (match fmt.as_str() {
                "csv" => t.to_csv(),
                "insert" | "sql" => t.to_inserts("TABLE_NAME"),
                _ => t.to_tsv(),
            }, format!("result{}", t.id))
        } else {
            self.notify.info("書き出す結果がありません");
            return;
        };
        let ext = match fmt.as_str() {
            "csv" => "csv",
            "insert" | "sql" => "sql",
            _ => "tsv",
        };
        self.db_open_text(&format!("{}.{ext}", crate::db_panel::safe_name(&name)), &text);
    }

    /// 結果のパネルにいる間のキー
    pub(crate) fn result_key(&mut self, key: Key) -> crate::editor::Effect {
        let half = (self.panel_rect().map_or(10, |r| r.h) / 2).max(1);
        // 絞り込みの欄の入力中（Vim 風。Enter で決める、ノーマルの Esc でやめる）
        if self.db_results.editing.is_some() {
            let out = self.feed_field(key, |e| e.db_results.editing.as_mut().map(|x| &mut x.1));
            let r = &mut self.db_results;
            match out {
                FieldOut::Cancel => r.editing = None,
                FieldOut::Submit => {
                    let (field, f) = r.editing.take().unwrap();
                    let text = f.text();
                    if let Some(t) = r.active_tab_mut() {
                        match field {
                            Field::Where => t.where_ = text,
                            Field::Order => t.order = text,
                        }
                        t.offset = 0;
                        t.total = None;
                        t.running = true;
                        let id = t.id;
                        self.db_requery(id);
                    }
                }
                _ => {}
            }
            return crate::editor::Effect::None;
        }
        let r = &mut self.db_results;
        let pending = std::mem::take(&mut r.pending_g);
        match key {
            Key::Char(']') | Key::Ctrl('n') => r.active = (r.active + 1) % (r.tabs.len() + 1),
            Key::Char('[') | Key::Ctrl('p') => r.active = (r.active + r.tabs.len()) % (r.tabs.len() + 1),
            Key::Esc | Key::Ctrl('w') => self.set_state(State::Normal),
            Key::Char(':') => {
                self.enter_command("");
            }
            Key::Char('q') => {
                if r.active == 0 {
                    self.hide_bottom();
                } else {
                    r.tabs.remove(r.active - 1);
                    r.active = r.active.min(r.tabs.len());
                }
            }
            k if r.active == 0 => match k {
                Key::Char('j') | Key::Down => r.log_scroll = r.log_scroll.saturating_sub(1),
                Key::Char('k') | Key::Up => r.log_scroll = (r.log_scroll + 1).min(r.log.len().saturating_sub(1)),
                Key::Char('G') => r.log_scroll = 0,
                _ => {}
            },
            k => {
                let Some(t) = r.active_tab_mut() else { return crate::editor::Effect::None };
                let last_row = t.rows.len().saturating_sub(1);
                let last_col = t.cols.len().saturating_sub(1);
                let moved = crate::list::step(k, t.row, last_row + 1, half * 2);
                match k {
                    Key::Char('l') | Key::Right | Key::Tab => t.col = (t.col + 1).min(last_col),
                    Key::Char('h') | Key::Left => t.col = t.col.saturating_sub(1),
                    // 表では Home・End は列の端へ
                    Key::Char('0') | Key::Home => t.col = 0,
                    Key::Char('$') | Key::End => t.col = last_col,
                    _ if moved.is_some() => t.row = moved.unwrap_or(t.row),
                    Key::Char('g') if pending => t.row = 0,
                    Key::Char('y') if pending => {
                        let s = t.to_tsv();
                        self.copy_text(s, "表");
                    }
                    Key::Char('g') => r.pending_g = true,
                    Key::Char('y') => {
                        if let Some(v) = t.rows.get(t.row).and_then(|x| x.get(t.col)) {
                            let s = v.clone().unwrap_or_default();
                            self.copy_text(s, "値");
                        }
                    }
                    Key::Char('Y') => {
                        if let Some(x) = t.rows.get(t.row) {
                            let s = x.iter().map(|v| v.clone().unwrap_or_default()).collect::<Vec<_>>().join("\t");
                            self.copy_text(s, "行");
                        }
                    }
                    Key::Char('f') | Key::Char('F') if t.filterable => {
                        let field = if k == Key::Char('f') { Field::Where } else { Field::Order };
                        let f = VimField::new(if field == Field::Where { &t.where_ } else { &t.order }, false);
                        r.editing = Some((field, f));
                    }
                    Key::Char('s') if t.filterable && !t.cols.is_empty() => {
                        let c = crate::db_panel::ident(&t.cols[t.col]);
                        t.order = if t.order == format!("{c} ASC") {
                            format!("{c} DESC")
                        } else if t.order == format!("{c} DESC") {
                            String::new()
                        } else {
                            format!("{c} ASC")
                        };
                        t.offset = 0;
                        t.running = true;
                        let id = t.id;
                        self.db_requery(id);
                    }
                    Key::Char('>') if t.filterable && t.more => {
                        t.offset += MAX_ROWS;
                        t.row = 0;
                        t.running = true;
                        let id = t.id;
                        self.db_requery(id);
                    }
                    Key::Char('<') if t.filterable && t.offset > 0 => {
                        t.offset = t.offset.saturating_sub(MAX_ROWS);
                        t.row = 0;
                        t.running = true;
                        let id = t.id;
                        self.db_requery(id);
                    }
                    Key::Char('R') if t.filterable => {
                        t.running = true;
                        let id = t.id;
                        self.db_requery(id);
                    }
                    Key::Char('E') => {
                        self.enter_command("");
                        self.cmdline.set("DbExport ");
                        self.update_cmd_complete();
                    }
                    _ => {}
                }
            }
        }
        crate::editor::Effect::None
    }

    /// 結果のパネルのクリック（`row` はパネルの中の行、`col` はパネルの左端から）
    pub(crate) fn result_click(&mut self, row: usize, col: usize) {
        self.list_side = Side::Bottom;
        self.set_state(State::List);
        let r = &mut self.db_results;
        // 1 行目: タブ
        if row == 0 {
            let mut x = 1;
            let labels: Vec<String> = std::iter::once("出力".to_string()).chain(r.tabs.iter().enumerate().map(|(k, t)| tab_label(k, t))).collect();
            for (k, l) in labels.iter().enumerate() {
                let w = nagi_core::display_col(l, l.len(), false) + 2;
                if col >= x && col < x + w {
                    r.active = k;
                    return;
                }
                x += w + 1;
            }
            return;
        }
        let Some(t) = r.active_tab_mut() else { return };
        let head = if t.filterable { 4 } else { 3 };
        if row == 1 && t.filterable {
            let field = if col < 40 { Field::Where } else { Field::Order };
            let f = VimField::new(if field == Field::Where { &t.where_ } else { &t.order }, false);
            r.editing = Some((field, f));
            return;
        }
        if row < head {
            return;
        }
        if let Some(i) = crate::list::row_at(t.scroll, row, head, t.rows.len()) {
            t.row = i;
        }
        let mut x = 7;
        for c in t.hscroll..t.cols.len() {
            let w = t.widths[c] + 3;
            if col >= x && col < x + w {
                t.col = c;
                break;
            }
            x += w;
        }
    }
}

/// タブの見出し
pub fn tab_label(k: usize, t: &ResultTab) -> String {
    let short: String = t.title.split(" · ").nth(1).unwrap_or(&t.title).chars().take(18).collect();
    format!("結果 {} · {short}", k + 1)
}



/// この部品の入口（component.rs）
pub struct ResultComponent;

impl crate::component::Component for ResultComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- クエリの結果 --", "hjkl 移動  y 値  Y 行  gy 表全体（タブ区切り）  q 閉じる ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.result_key(key)
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.bottom_text(text);
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// データベースの結果（下のパネル）。1 行目はタブ、出力なら記録、結果なら絞り込みの行と表
    pub(crate) fn draw_db_results(p: &mut Painter, ed: &Editor, r: Rect) {
        let res = &ed.db_results;
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
        p.out.push(Instance::fill(x0, y0, wpx, 1.0, LINE_NO));
        let focused = ed.state() == State::List && ed.list_side == crate::editor::Side::Bottom;
        let saved = p.cols;
        p.cols = (r.x + r.w).saturating_sub(3);
        // タブ
        let mut x = r.x + 1;
        let labels: Vec<String> = std::iter::once("出力".to_string()).chain(res.tabs.iter().enumerate().map(|(k, t)| crate::result_grid::tab_label(k, t))).collect();
        for (k, l) in labels.iter().enumerate() {
            let on = k == res.active;
            let end = p.text(&format!(" {l} "), x, y0, if on && focused { FG } else if on { rgb(0xc8c8c8) } else { STATUS_FG });
            if on {
                p.out.push(Instance::fill(x as f32 * cell.w, y0 + cell.h - 2.0, (end - x) as f32 * cell.w, 2.0, if focused { GIT_MOD } else { STATUS_FG }));
            }
            x = end + 1;
        }
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, wpx, 1.0, rgb(0x2a2a2a)));
        p.cols = r.x + r.w;
        match res.active_tab() {
            None => {
                // 出力: 新しいものを下に
                let body = r.h.saturating_sub(1);
                let end = res.log.len().saturating_sub(res.log_scroll);
                let start = end.saturating_sub(body);
                for (k, l) in res.log[start..end].iter().enumerate() {
                    let y = y0 + (1 + k) as f32 * cell.h;
                    let mut c = p.text(&format!(" [{}] ", l.time), r.x, y, LINE_NO);
                    if !l.src.is_empty() {
                        c = p.text(&format!("{}  ", l.src), c, y, STATUS_FG);
                    }
                    c = p.text(&l.text, c, y, if l.error && l.outcome.is_empty() { DIAG_ERROR } else { FG });
                    if !l.outcome.is_empty() {
                        p.text(&format!("   {}", l.outcome), c, y, if l.error { DIAG_ERROR } else { STATUS_FG });
                    }
                }
                if res.log.is_empty() {
                    p.text(" まだ何も流していません", r.x, y0 + cell.h, LINE_NO);
                }
            }
            Some(t) => {
                let mut y = y0 + cell.h;
                let right = format!("{}   {}", t.range_label(), t.info);
                if t.filterable {
                    draw_filter_row(p, r, y, &t.where_, &t.order, res.editing.as_ref(), &right, &ed.preedit);
                    y += cell.h;
                } else {
                    let rw = p.width(&right);
                    p.text(&right, (r.x + r.w).saturating_sub(rw + 3), y, if t.error { DIAG_ERROR } else { STATUS_FG });
                    y += cell.h;
                }
                if t.cols.is_empty() {
                    p.text(&format!(" {}", t.info), r.x, y, if t.error { DIAG_ERROR } else if t.running { STATUS_FG } else { FG });
                } else {
                    let value = |k: usize, c: usize| t.rows[k][c].clone();
                    let row_style = |k: usize| ((t.offset + k + 1).to_string(), None);
                    let cell_bg = |_: usize, _: usize| None;
                    let g = GridView {
                        cols: &t.cols,
                        types: &t.types,
                        widths: &t.widths,
                        marks: Vec::new(),
                        sort: sort_of(&t.order),
                        rows: t.rows.len(),
                        value: &value,
                        row_style: &row_style,
                        cell_bg: &cell_bg,
                        row: t.row,
                        col: t.col,
                        scroll: t.scroll,
                        hscroll: t.hscroll,
                        focused,
                    };
                    let top = ((y - y0) / cell.h) as usize;
                    draw_grid(p, &g, Rect { x: r.x, y: r.y + top, w: r.w, h: r.h.saturating_sub(top) });
                }
            }
        }
        p.cols = saved;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_sql_and_export() {
        let mut r = DbResults::default();
        let id = r.new_tab(0, "t", "select * from item;", true);
        r.reply(id, Reply::Rows { cols: vec!["ID".into(), "NAME".into()], types: vec!["INTEGER".into(), "TEXT".into()], rows: vec![vec![Some("1".into()), None], vec![Some("2".into()), Some("a,\"b\"".into())]], more: true, ms: 12 });
        let t = r.tab(id).unwrap();
        assert_eq!(t.info, "2 行（続きあり） · 12 ms");
        assert_eq!(t.range_label(), "1–2 / 2+");
        assert_eq!(t.effective_sql(), "select * from item");
        assert_eq!(t.to_csv(), "ID,NAME\r\n1,\r\n2,\"a,\"\"b\"\"\"\r\n");
        assert_eq!(t.to_inserts("ITEM"), "INSERT INTO ITEM (ID, NAME) VALUES ('1', NULL);\nINSERT INTO ITEM (ID, NAME) VALUES ('2', 'a,\"b\"');\n");
        let t = r.tab_mut(id).unwrap();
        t.where_ = "id > 1".into();
        t.order = "name desc".into();
        assert_eq!(t.effective_sql(), "SELECT * FROM (\nselect * from item\n) nagi_q WHERE id > 1 ORDER BY name desc");
        assert_eq!(t.count_sql().unwrap(), "SELECT COUNT(*) FROM (\nselect * from item\n) nagi_q WHERE id > 1");
        assert_eq!(group(12480), "12,480");
    }
}
