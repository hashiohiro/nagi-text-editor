//! データベースの結果（下のパネル。全文検索の結果と入れ替えて出す。DataGrip の Services と同じ形）。
//!
//! タブ: 「出力」（流した文・件数・時間・エラーの記録）と「結果 1」「結果 2」…（流すたびに 1 つ。8 つまで）。
//! 表: 行番号、列名と型の 2 行の見出し、WHERE / ORDER BY の絞り込み（f / F）、列で並べ替え（s）、
//! ページ送り（< >。1 ページ 500 行、件数は数えてから出す）、コピー（y 値・Y 行・gy 表全体をタブ区切り）、
//! 書き出し（:DbExport csv|tsv|insert）。] [ でタブを替える。
//! 1 つのテーブルからの SELECT（選ぶのが * か列の名前だけ。主キーの列を含む）なら値を直せる: F2・Enter・i・ダブルクリックで直し、
//! x で NULL、u・Ctrl+Z で戻す。直した値は青。Ctrl+Enter で UPDATE を確かめて 1 つのトランザクションで送る（主キーで行を見分ける）

use std::collections::HashMap;

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
    /// 接続先の方言（名前を囲む）
    pub dialect: &'static nagi_lang::sql::Dialect,
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
    /// 直して書き戻せる表 (スキーマ, テーブル)。1 つのテーブルからの SELECT のとき（sql_complete の edit_target）
    pub target: Option<(String, String)>,
    /// 直した値 ((行, 列) → 値。None は NULL)。送るまでは青で出す
    pub edits: HashMap<(usize, usize), Option<String>>,
    /// 戻す順の (行, 列, 前の直した値。None は直していなかった)
    pub(crate) undo: Vec<(usize, usize, Option<Option<String>>)>,
    /// 送った値の、送る前の値（手動の Tx でロールバックしたら戻す）
    pub(crate) sent: HashMap<(usize, usize), Option<String>>,
}

impl ResultTab {
    /// 表に出す値（直していれば直した値）
    pub fn value(&self, r: usize, c: usize) -> Option<String> {
        self.edits.get(&(r, c)).cloned().unwrap_or_else(|| self.rows[r][c].clone())
    }

    pub fn modified(&self, r: usize, c: usize) -> bool {
        self.edits.contains_key(&(r, c))
    }

    /// 直した行の数
    pub fn changes(&self) -> usize {
        self.edits.keys().map(|k| k.0).collect::<std::collections::BTreeSet<_>>().len()
    }

    pub fn set_value(&mut self, r: usize, c: usize, v: Option<String>) {
        let prev = if self.rows[r][c] == v { self.edits.remove(&(r, c)) } else { self.edits.insert((r, c), v) };
        self.undo.push((r, c, prev));
    }

    fn undo_one(&mut self) -> bool {
        let Some((r, c, prev)) = self.undo.pop() else { return false };
        match prev {
            Some(v) => self.edits.insert((r, c), v),
            None => self.edits.remove(&(r, c)),
        };
        true
    }

    /// 送った: 直した値を表に入れる
    pub fn commit_edits(&mut self) {
        for ((r, c), v) in self.edits.drain() {
            let old = std::mem::replace(&mut self.rows[r][c], v);
            self.sent.entry((r, c)).or_insert(old);
        }
        self.undo.clear();
    }

    /// 送る UPDATE（行は主キー `pk`（表の列の番号）で見分ける。`colname` は表の列の、テーブルでの名前）
    pub fn statements(&self, name: &str, pk: &[usize], colname: &[String]) -> Vec<String> {
        let rows: std::collections::BTreeSet<usize> = self.edits.keys().map(|k| k.0).collect();
        rows.into_iter()
            .map(|r| {
                let mut set: Vec<(usize, &Option<String>)> = self.edits.iter().filter(|(k, _)| k.0 == r).map(|(k, v)| (k.1, v)).collect();
                set.sort_by_key(|x| x.0);
                let set = set.iter().map(|(c, v)| format!("{} = {}", self.dialect.ident(&colname[*c]), crate::db_panel::literal(v))).collect::<Vec<_>>().join(", ");
                let wh = pk.iter().map(|&c| format!("{} = {}", self.dialect.ident(&colname[c]), crate::db_panel::literal(&self.rows[r][c]))).collect::<Vec<_>>().join(" AND ");
                format!("UPDATE {name} SET {set} WHERE {wh}")
            })
            .collect()
    }

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
        let cols = self.cols.iter().map(|c| self.dialect.ident(c)).collect::<Vec<_>>().join(", ");
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
    /// 値を直している欄（選んだセルの上に出す）
    pub value_edit: Option<VimField>,
    pending_g: bool,
}

/// 表の横のホイール: 左端の列 `hscroll` を 1 列ずつ送り、選んでいる列 `col` は見える範囲に留める
/// （見える範囲は view.rs の build の fit_grid と同じ数え方: 列の幅 + 3 の和 + 8 が `w` 以下）
pub fn grid_hwheel(col: &mut usize, hscroll: &mut usize, widths: &[usize], w: usize, n: isize) {
    if widths.is_empty() || n == 0 {
        return;
    }
    let len = widths.len();
    *hscroll = (*hscroll as isize + n.signum()).clamp(0, len as isize - 1) as usize;
    let mut last = *hscroll;
    while last + 1 < len && widths[*hscroll..=last + 1].iter().map(|w| w + 3).sum::<usize>() + 8 <= w {
        last += 1;
    }
    *col = (*col).clamp(*hscroll, last);
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
            dialect: &nagi_lang::sql::GENERIC,
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
            target: None,
            edits: HashMap::new(),
            undo: Vec::new(),
            sent: HashMap::new(),
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
                t.edits.clear();
                t.undo.clear();
                t.sent.clear();
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

    /// 直した値を送った: 表に入れる
    pub fn committed(&mut self, id: u64, info: String) {
        if let Some(t) = self.tab_mut(id) {
            t.commit_edits();
            t.info = info;
            t.error = false;
        }
    }

    /// 接続先 `src` をコミット・ロールバックした: ロールバックなら送った値を前に戻し、読み直せる結果の番号を返す
    pub fn tx_ended(&mut self, src: usize, commit: bool) -> Vec<u64> {
        let mut again = Vec::new();
        for t in self.tabs.iter_mut().filter(|t| t.src == src && !t.sent.is_empty()) {
            let sent = std::mem::take(&mut t.sent);
            if commit {
                continue;
            }
            for ((r, c), v) in sent {
                t.rows[r][c] = v;
            }
            if t.filterable {
                t.running = true;
                again.push(t.id);
            }
        }
        again
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

    pub(crate) fn copy_text(&mut self, s: String, what: &str) {
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
            let table = crate::db_panel::qualified(te.dialect, &te.schema, &te.table);
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

    /// 結果が届いた: 1 つのテーブルからの SELECT なら、書き戻す表を決め、主キーを知るために列を読んでおく
    pub(crate) fn result_find_target(&mut self, i: usize, id: u64) {
        let Some(t) = self.db_results.tab(id) else { return };
        let Some((schema, name)) = crate::sql_complete::edit_target(&t.base) else { return };
        if self.db.as_ref().is_some_and(|p| p.kind(i) == Some(nagi_workers::db::Kind::Salesforce)) {
            return;
        }
        let Some(c) = self.db.as_mut().and_then(|p| p.conns.get_mut(i)) else { return };
        let (s, n) = c.resolve(schema.as_deref(), &name);
        c.ensure_columns(&s, &n);
        if let Some(t) = self.db_results.tab_mut(id) {
            t.target = Some((s, n));
        }
    }

    /// 今の結果を書き戻す用意: (テーブルの名前, 主キーの列の番号, 結果の列ごとのテーブルでの名前)。直せなければその訳
    fn result_plan(&self) -> Result<(String, Vec<usize>, Vec<String>), String> {
        let t = self.db_results.active_tab().ok_or_else(String::new)?;
        let Some((s, n)) = &t.target else {
            return Err("この結果は直せません（1 つのテーブルからの SELECT で、選ぶのが * か列の名前だけのとき直せます）".into());
        };
        let Some(cols) = self.db.as_ref().and_then(|p| p.conns.get(t.src)).and_then(|c| c.columns.get(&(s.clone(), n.clone()))) else {
            return Err("テーブルの列を読んでいます。少し待ってからもう一度".into());
        };
        let mut pk = Vec::new();
        for c in cols.iter().filter(|c| c.pk) {
            match t.cols.iter().position(|rc| rc.eq_ignore_ascii_case(&c.name)) {
                Some(k) => pk.push(k),
                None => return Err(format!("主キーの列 {} が結果に無いので直せません", c.name)),
            }
        }
        if pk.is_empty() {
            return Err(format!("{n} は主キーが無いので直せません（読むだけ）"));
        }
        let names = t.cols.iter().map(|rc| cols.iter().find(|c| c.name.eq_ignore_ascii_case(rc)).map_or(rc.clone(), |c| c.name.clone())).collect();
        Ok((crate::db_panel::qualified(t.dialect, s, n), pk, names))
    }

    /// 選んでいるセルの値を直し始める（F2・Enter・ダブルクリック。`empty` なら空から）
    pub(crate) fn result_begin_edit(&mut self, empty: bool) {
        if let Err(e) = self.result_plan() {
            if !e.is_empty() {
                self.notify.warn(e);
            }
            return;
        }
        let r = &mut self.db_results;
        let Some(t) = r.active_tab() else { return };
        if t.rows.is_empty() || t.cols.is_empty() {
            return;
        }
        let v = if empty { String::new() } else { t.value(t.row, t.col).unwrap_or_default() };
        r.value_edit = Some(VimField::new(&v, false));
    }

    /// 打っている値を、選んでいるセルに入れる（欄を閉じる）
    fn result_commit_edit(&mut self) {
        let r = &mut self.db_results;
        let v = r.value_edit.take().map(|f| f.text()).unwrap_or_default();
        if let Some(t) = r.active_tab_mut() {
            let (row, col) = (t.row, t.col);
            t.set_value(row, col, Some(v));
        }
    }

    /// 表（クエリの結果・テーブルの編集）の Ctrl+C・Ctrl+V（Shift+Insert）。セルのコピー・貼り付けをしたら true。
    /// キーマップ（Vim を使わないときの Ctrl+C・V は本文のコピー・貼り付け）より先に見る
    pub(crate) fn grid_clipboard_key(&mut self, key: Key) -> bool {
        let table = self.state() == State::DbTable && self.db_table.is_some();
        let results = !table
            && self.focus == Some(crate::editor::Panel::List)
            && self.list_side == Side::Bottom
            && self.bottom_kind == BottomKind::Result
            && self.db_results.active > 0;
        if !table && !results {
            return false;
        }
        // 値・絞り込みを打っている間: Vim ならいつも通り（Ctrl+C は Insert を抜ける）。使わないなら、Ctrl+V は欄に貼り、Ctrl+C は選んだ所（選んでいなければ欄の値）をコピー
        let field = if table {
            self.db_table.as_ref().and_then(|t| t.editing.as_ref().or(t.filter_edit.as_ref().map(|x| &x.1))).map(|f| f.selected_text().unwrap_or_else(|| f.text()))
        } else {
            let r = &self.db_results;
            r.value_edit.as_ref().or(r.editing.as_ref().map(|x| &x.1)).map(|f| f.selected_text().unwrap_or_else(|| f.text()))
        };
        if let Some(text) = field {
            if self.vim_on {
                return false;
            }
            if key == Key::Ctrl('c') {
                self.copy_text(text, "値");
            } else if table {
                self.db_table_key(Key::Paste);
            } else {
                self.result_key(Key::Paste);
            }
            return true;
        }
        match (key, table) {
            (Key::Ctrl('c'), true) => self.db_table_copy(),
            (Key::Ctrl('c'), false) => self.result_copy(),
            (_, true) => self.db_table_paste(),
            (_, false) => self.result_paste(),
        }
        true
    }

    /// 選んでいるセルの値をコピー（NULL は空）
    fn result_copy(&mut self) {
        let Some(t) = self.db_results.active_tab().filter(|t| !t.rows.is_empty() && !t.cols.is_empty()) else { return };
        let s = t.value(t.row, t.col).unwrap_or_default();
        self.copy_text(s, "値");
    }

    /// クリップボードを選んでいるセルに貼る（タブ・改行で区切ってあれば、そこから右・下のセルへ）
    fn result_paste(&mut self) {
        if let Err(e) = self.result_plan() {
            if !e.is_empty() {
                self.notify.warn(e);
            }
            return;
        }
        let Some(cells) = self.clipboard_cells() else { return };
        let Some(t) = self.db_results.active_tab_mut().filter(|t| !t.rows.is_empty() && !t.cols.is_empty()) else { return };
        let (row, col) = (t.row, t.col);
        let (rows, cols) = (t.rows.len(), t.cols.len());
        let mut n = 0;
        for (dr, line) in cells.iter().enumerate().filter(|(dr, _)| row + dr < rows) {
            for (dc, v) in line.iter().enumerate().filter(|(dc, _)| col + dc < cols) {
                t.set_value(row + dr, col + dc, Some(v.clone()));
                n += 1;
            }
        }
        if n > 1 {
            self.notify.info(format!("{n} 個のセルに貼りました"));
        }
    }

    /// クリップボードの文字を、表のセルに分ける（行は改行、列はタブ。最後の改行は外す）
    pub(crate) fn clipboard_cells(&mut self) -> Option<Vec<Vec<String>>> {
        let text = match &self.clipboard {
            Some(cb) => (cb.get)(),
            None => crate::clipboard::get(),
        };
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            self.notify.info("クリップボードが空です");
            return None;
        };
        Some(cells_of(&text))
    }

    /// 選んでいるセルを NULL に
    pub(crate) fn result_set_null(&mut self) {
        if let Err(e) = self.result_plan() {
            self.notify.warn(e);
            return;
        }
        if let Some(t) = self.db_results.active_tab_mut().filter(|t| !t.rows.is_empty()) {
            let (r, c) = (t.row, t.col);
            t.set_value(r, c, None);
        }
    }

    pub(crate) fn result_undo(&mut self) {
        if !self.db_results.active_tab_mut().is_some_and(|t| t.undo_one()) {
            self.notify.info("戻す変更はありません");
        }
    }

    /// 直した値を UPDATE にして、確認の窓で「はい」なら 1 つのトランザクションで送る
    pub(crate) fn result_send(&mut self) {
        let (name, pk, names) = match self.result_plan() {
            Ok(x) => x,
            Err(e) => {
                self.notify.warn(e);
                return;
            }
        };
        let Some(t) = self.db_results.active_tab() else { return };
        let stmts = t.statements(&name, &pk, &names);
        if stmts.is_empty() {
            self.notify.info("送る変更はありません");
            return;
        }
        let (id, src) = (t.id, t.src);
        let first: String = stmts[0].chars().take(160).collect();
        let q = if stmts.len() == 1 { format!("送りますか: {first}") } else { format!("{} 件の変更を送りますか: {first} …", stmts.len()) };
        self.ask_or(
            q,
            move |e| {
                if let Some(c) = e.db.as_mut().and_then(|p| p.conns.get_mut(src)).filter(|c| c.connected) {
                    c.send_batch(crate::db_panel::Target::Result(id), stmts);
                    e.notify.info("送信しています…");
                } else {
                    e.notify.info("まだつないでいません");
                }
            },
            |e| e.notify.info("送信をやめました（変更は残っています）"),
        );
    }

    /// 結果のパネルにいる間のキー
    pub(crate) fn result_key(&mut self, key: Key) -> crate::editor::Effect {
        let half = (self.panel_rect().map_or(10, |r| r.h) / 2).max(1);
        // 値の入力中（Vim 風。Enter か Tab で決める、ノーマルの Esc でやめる）
        if self.db_results.value_edit.is_some() {
            let out = if key == Key::Tab { FieldOut::Submit } else { self.feed_field(key, |e| e.db_results.value_edit.as_mut()) };
            let r = &mut self.db_results;
            match out {
                FieldOut::Cancel => r.value_edit = None,
                FieldOut::Submit => {
                    self.result_commit_edit();
                    if let Some(t) = self.db_results.active_tab_mut().filter(|_| key == Key::Tab) {
                        t.col = (t.col + 1).min(t.cols.len().saturating_sub(1));
                    }
                }
                _ => {}
            }
            return crate::editor::Effect::None;
        }
        // 値を直す操作（結果のタブで）
        let dirty = self.db_results.active_tab().is_some_and(|t| !t.edits.is_empty());
        if self.db_results.active > 0 {
            match key {
                Key::F(2) | Key::Enter | Key::Char('i') => {
                    self.result_begin_edit(false);
                    return crate::editor::Effect::None;
                }
                Key::Char('c') => {
                    self.result_begin_edit(true);
                    return crate::editor::Effect::None;
                }
                Key::Char('x') => {
                    self.result_set_null();
                    return crate::editor::Effect::None;
                }
                Key::Char('u') | Key::Ctrl('z') => {
                    self.result_undo();
                    return crate::editor::Effect::None;
                }
                Key::Ctrl('m') => {
                    self.result_send();
                    return crate::editor::Effect::None;
                }
                // 読み直すと直した値が消えるので、送るか戻すまで止める
                Key::Char('f' | 'F' | 's' | '>' | '<' | 'R') if dirty => {
                    self.notify.info("未送信の変更があります（Ctrl+Enter で送るか、u・Ctrl+Z で戻す）");
                    return crate::editor::Effect::None;
                }
                Key::Char('q') if dirty => {
                    let n = self.db_results.active_tab().map_or(0, |t| t.changes());
                    let id = self.db_results.active_tab().map_or(0, |t| t.id);
                    self.ask_or(format!("未送信の変更が {n} 件あります。捨てて閉じますか"), move |e| {
                        let r = &mut e.db_results;
                        r.tabs.retain(|t| t.id != id);
                        r.active = r.active.min(r.tabs.len());
                    }, |_| {});
                    return crate::editor::Effect::None;
                }
                _ => {}
            }
        }
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
                        let c = t.dialect.ident(&t.cols[t.col]);
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
    pub(crate) fn result_click(&mut self, row: usize, col: usize, clicks: u32) {
        self.list_side = Side::Bottom;
        self.set_state(State::List);
        // 値を打っている間: 別のセルを押したら、打った値を決めてからそのセルへ（ダブルクリックならそのまま直し始める）。ほかは選び直さない
        if self.db_results.value_edit.is_some() {
            let other = self.db_results.active_tab().is_some_and(|t| {
                let head = if t.filterable { 4 } else { 3 };
                let hit = crate::list::row_at(t.scroll, row, head, t.rows.len()).zip(grid_col_at(t.hscroll, &t.widths, col));
                hit.is_some_and(|h| h != (t.row, t.col))
            });
            if !other {
                return;
            }
            self.result_commit_edit();
        }
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
            if !t.edits.is_empty() {
                self.notify.info("未送信の変更があります（Ctrl+Enter で送るか、u・Ctrl+Z で戻す）");
                return;
            }
            let field = if col < 40 { Field::Where } else { Field::Order };
            let f = VimField::new(if field == Field::Where { &t.where_ } else { &t.order }, false);
            r.editing = Some((field, f));
            return;
        }
        if row < head {
            return;
        }
        // 行と列（ダブルクリックで値を直す）
        let len = t.rows.len();
        let pick = crate::list::pick_row(&mut t.row, t.scroll, row, head, len, clicks);
        if let Some(c) = grid_col_at(t.hscroll, &t.widths, col) {
            t.col = c;
        }
        if let Some(crate::list::Pick::Accept(_)) = pick {
            self.result_begin_edit(false);
        }
    }
}

/// 貼る文字を、表のセルに分ける（行は改行、列はタブ。最後の改行は外す。Excel・結果の表の gy と同じ形）
pub fn cells_of(text: &str) -> Vec<Vec<String>> {
    let text = text.replace("\r\n", "\n");
    text.strip_suffix('\n').unwrap_or(&text).split('\n').map(|l| l.split('\t').map(str::to_string).collect()).collect()
}

/// 表（view.rs の draw_grid）の、表の左端から `col` 列目にある列（行番号の欄は 7 列、列は幅 + 3）
pub fn grid_col_at(hscroll: usize, widths: &[usize], col: usize) -> Option<usize> {
    let mut x = 7;
    for (c, w) in widths.iter().enumerate().skip(hscroll) {
        if col >= x && col < x + w + 3 {
            return Some(c);
        }
        x += w + 3;
    }
    None
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
        let help = "hjkl 移動  i・F2 値を直す  x NULL  Ctrl+V 貼る  u 戻す  Ctrl+Enter 送る  y 値  Y 行  gy 表全体  q 閉じる ";
        let plain = "矢印 移動  F2・ダブルクリック 値を直す  Ctrl+C・V コピー・貼り付け  Ctrl+Z 戻す  Ctrl+Enter 送る  右クリック 操作  Esc 窓へ ";
        match ed.db_results.active_tab().map_or(0, |t| t.changes()) {
            0 => crate::component::Status::keys("-- クエリの結果 --", help, plain),
            n => Some(crate::component::Status { name: format!("-- 編集 --  変更 {n} 行（未送信）"), warn: true, help: help.into(), plain: Some(plain.into()) }),
        }
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
                    let value = |k: usize, c: usize| t.value(k, c);
                    let row_style = |k: usize| ((t.offset + k + 1).to_string(), None);
                    let cell_bg = |k: usize, c: usize| t.modified(k, c).then_some(rgb(0x1f3550));
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
                        focused: focused && res.value_edit.is_none(),
                    };
                    let top = ((y - y0) / cell.h) as usize;
                    draw_grid(p, &g, Rect { x: r.x, y: r.y + top, w: r.w, h: r.h.saturating_sub(top) });
                    // 値の入力欄（選んだセルの上に。表の見出しは 2 行）
                    if let Some(f) = &res.value_edit {
                        if t.row >= t.scroll && r.y + top + 2 + t.row - t.scroll < r.y + r.h {
                            crate::db_table::draw::draw_cell_field(p, f, &ed.preedit, r.x, &t.widths, t.hscroll, t.col, r.y + top + 2 + t.row - t.scroll);
                        }
                    }
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

    #[test]
    fn cells_from_clipboard() {
        assert_eq!(cells_of("a"), [["a"]]);
        assert_eq!(cells_of("a\tb\r\nc\td\r\n"), [["a", "b"], ["c", "d"]]);
        assert_eq!(cells_of("a\n\nb"), [vec!["a"], vec![""], vec!["b"]]);
    }

    /// 結果の表: 打っている間に別の行・列をダブルクリックすると、打った値を決めてそのセルを直す。Ctrl+C・Ctrl+V でセルをコピー・貼り付け
    #[test]
    fn click_and_clipboard_cells() {
        use crate::db_panel::tests::wait;
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
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        e.db.as_mut().unwrap().conns[i].manual = true;
        e.db_execute(i, "create table item(id integer primary key, name text)".into());
        e.db_execute(i, "insert into item values (1, 'a'), (2, 'b')".into());
        e.db_execute(i, "select * from item order by id".into());
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| t.rows.len() == 2 && t.target.is_some()));
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].columns.contains_key(&("main".into(), "item".into())));
        // 表は 4 行目から（タブ・絞り込み・見出し 2 行）。id は 7 列目から、name は id の幅 + 3 の後
        let name_x = 7 + e.db_results.active_tab().unwrap().widths[0] + 3 + 1;
        e.result_click(4, name_x, 1);
        e.result_click(4, name_x, 2);
        assert!(e.db_results.value_edit.is_some());
        keys(&mut e, "<Esc>ccz");
        e.result_click(5, 8, 1);
        e.result_click(5, 8, 2);
        let t = e.db_results.active_tab().unwrap();
        assert_eq!(t.value(0, 1).as_deref(), Some("z"));
        assert_eq!((t.row, t.col), (1, 0));
        assert_eq!(e.db_results.value_edit.as_ref().map(|f| f.text()).as_deref(), Some("2"));
        keys(&mut e, "<Esc><Esc>");
        assert!(e.db_results.value_edit.is_none());
        thread_local! { static CLIP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) }; }
        fn set(t: &str) { CLIP.with(|c| *c.borrow_mut() = t.to_string()); }
        fn get() -> Option<String> { CLIP.with(|c| Some(c.borrow().clone())) }
        e.clipboard = Some(crate::editor::Clipboard { set, get });
        keys(&mut e, "kl");
        e.key(Key::Ctrl('c'));
        assert_eq!(get().as_deref(), Some("z"));
        keys(&mut e, "j");
        e.key(Key::Ctrl('v'));
        assert_eq!(e.db_results.active_tab().unwrap().value(1, 1).as_deref(), Some("z"));
        // 表の外へははみ出さない
        set("p\tq\nr\ts");
        e.key(Key::Ctrl('v'));
        let t = e.db_results.active_tab().unwrap();
        assert_eq!((t.value(1, 1).as_deref(), t.changes()), (Some("p"), 2));
    }

    /// 1 つのテーブルからの SELECT の結果を F2 で直し、Ctrl+Enter で送る（SQLite で）
    #[test]
    fn edit_result_and_send() {
        use crate::db_panel::tests::wait;
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
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        // データを変える文を確かめずに流す（DB は自動コミットのまま）
        e.db.as_mut().unwrap().conns[i].manual = true;
        e.db_execute(i, "create table item(id integer primary key, name text)".into());
        e.db_execute(i, "insert into item values (1, 'a'), (2, 'b')".into());
        // 式の列があると直せない
        e.db_execute(i, "select id, upper(name) from item".into());
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| t.rows.len() == 2));
        assert!(e.db_results.active_tab().unwrap().target.is_none());
        e.db_execute(i, "select * from item order by id".into());
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| t.rows.len() == 2 && t.target.is_some()));
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].columns.contains_key(&("main".into(), "item".into())));
        e.list_side = Side::Bottom;
        e.set_state(State::List);
        // 2 行目の name を F2 で直す（値を消して打つ）
        keys(&mut e, "jl");
        e.key(Key::F(2));
        assert!(e.db_results.value_edit.is_some());
        keys(&mut e, "<Esc>ccx'y<CR>");
        let t = e.db_results.active_tab().unwrap();
        assert_eq!(t.value(1, 1).as_deref(), Some("x'y"));
        assert!(t.modified(1, 1));
        assert_eq!(t.statements("item", &[0], &t.cols.clone()), vec!["UPDATE item SET name = 'x''y' WHERE id = '2'"]);
        // 送ると読み直さずに表へ入る。DB の値も変わっている（ここからは本当に手動の Tx に）
        e.db.as_mut().unwrap().conns[i].manual = false;
        e.db_toggle_tx(i);
        e.key(Key::Ctrl('m'));
        keys(&mut e, "y");
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| t.edits.is_empty() && t.info.contains("直しました")));
        assert_eq!(e.db_results.active_tab().unwrap().rows[1][1].as_deref(), Some("x'y"));
        e.db_execute(i, "select name from item where id = 2".into());
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| t.rows.len() == 1));
        assert_eq!(e.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("x'y"));
        // 手動の Tx をロールバックすると、直して送った結果の表も前の値に戻る
        let id = e.db_results.tabs[e.db_results.tabs.len() - 2].id;
        e.db_commit(i, false);
        wait(&mut e, &|e| e.db_results.tab(id).unwrap().rows[1][1].as_deref() == Some("b"));
        wait(&mut e, &|e| e.db_results.tab(id).is_some_and(|t| !t.running));
        assert_eq!(e.db_results.tab(id).unwrap().rows[1][1].as_deref(), Some("b"));
    }
}
