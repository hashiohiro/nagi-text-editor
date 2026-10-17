//! 右の欄の「データベース」タブ（IntelliJ の Database の画面と同じ形）。
//!
//! 接続先は、プロジェクトの Spring の設定（application*.properties）と、設定ファイル
//! `~/.config/nagi/databases.txt`（1 行に `名前 | jdbc:URL | 利用者`）から集める。パスワードはつなぐときに
//! 伏せ字で打ってもらい、Nagi を閉じるまでメモリにだけ置く。
//!
//! 木: 接続先 ▸ テーブル ▸ 列。テーブルの上で o（ダブルクリック）で先頭の 500 行を下の表に出す。
//! c で接続先の SQL コンソール（.sql のファイル）を開き、Ctrl+Enter でカーソルのある文（空行で区切る。
//! Visual なら選んだ範囲）を実行する。SELECT など読むだけの文はすぐ、データを変える文は y で確かめてから。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use nagi_vim::Key;
use nagi_workers::db::{Client, Column, DataSource, Reply};

use crate::editor::{Editor, Effect, Mode, Prompt};
use crate::result_grid::ResultGrid;
use crate::right_panel::RightTab;

/// 表に出す行の上限
pub const MAX_ROWS: usize = 500;

/// 届くはずの応答（頼んだ順に届く）
enum Expect {
    Connect,
    Tables,
    Columns,
    Query,
}

#[derive(Default)]
pub struct Conn {
    pub client: Option<Client>,
    pub product: String,
    pub schema: String,
    pub connected: bool,
    pub tables: Option<Vec<(String, String)>>,
    pub columns: HashMap<String, Vec<Column>>,
    pub error: Option<String>,
    expect: VecDeque<Expect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbRow {
    Source(usize),
    Table(usize, String),
    Column(usize, String, usize),
    Info(usize, String),
}

pub struct DbPanel {
    pub sources: Vec<DataSource>,
    pub conns: Vec<Conn>,
    pub expanded: HashSet<String>,
    pub rows: Vec<DbRow>,
    pub selected: usize,
    pub scroll: usize,
    /// パスワードの入力（接続先, 入力欄, つないだら流す SQL）
    pub password: Option<(usize, Prompt, Option<String>)>,
    /// コンソールのファイル → 接続先
    pub consoles: HashMap<PathBuf, usize>,
    /// データを変える文の確認（接続先, SQL）
    pub confirm: Option<(usize, String)>,
    /// つながったら流す SQL（接続先 → SQL）
    pub password_then: HashMap<usize, String>,
}

fn key_source(i: usize) -> String {
    format!("s{i}")
}

fn key_table(i: usize, t: &str) -> String {
    format!("t{i}:{t}")
}

impl DbPanel {
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for (i, c) in self.conns.iter().enumerate() {
            rows.push(DbRow::Source(i));
            if !self.expanded.contains(&key_source(i)) {
                continue;
            }
            match (&c.tables, &c.error, c.connected) {
                (_, Some(e), _) => rows.push(DbRow::Info(i, format!("誤り: {e}"))),
                (Some(ts), _, _) => {
                    if ts.is_empty() {
                        rows.push(DbRow::Info(i, "テーブルはありません".into()));
                    }
                    for (t, _) in ts {
                        rows.push(DbRow::Table(i, t.clone()));
                        if self.expanded.contains(&key_table(i, t)) {
                            match c.columns.get(t) {
                                Some(cols) => rows.extend((0..cols.len()).map(|k| DbRow::Column(i, t.clone(), k))),
                                None => rows.push(DbRow::Info(i, "…".into())),
                            }
                        }
                    }
                }
                (None, None, true) => rows.push(DbRow::Info(i, "テーブルを読んでいます…".into())),
                (None, None, false) if c.client.is_some() => rows.push(DbRow::Info(i, "つないでいます…".into())),
                _ => {}
            }
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    /// 行の見出し（字下げ, 本文, 補足）
    pub fn row_label(&self, r: &DbRow) -> (usize, String, String) {
        match r {
            DbRow::Source(i) => {
                let s = &self.sources[*i];
                let c = &self.conns[*i];
                let mark = if c.connected { "●" } else if c.client.is_some() { "◌" } else { "○" };
                let host = s.url.rsplit('@').next().unwrap_or(&s.url).trim_start_matches("//");
                (0, format!("{mark} {}", s.name), format!("{}{}", if s.user.is_empty() { String::new() } else { format!("{}@", s.user) }, host))
            }
            DbRow::Table(i, t) => {
                let ty = self.conns[*i].tables.as_ref().and_then(|ts| ts.iter().find(|x| &x.0 == t)).map_or("", |x| x.1.as_str());
                (1, t.clone(), if ty == "VIEW" { "ビュー".into() } else { String::new() })
            }
            DbRow::Column(i, t, k) => {
                let c = &self.conns[*i].columns[t][*k];
                let size = if c.size.is_empty() || c.ty.contains('(') { String::new() } else { format!("({})", c.size) };
                (2, c.name.clone(), format!("{}{size}{}", c.ty, if c.nullable { "" } else { " NOT NULL" }))
            }
            DbRow::Info(_, s) => (1, s.clone(), String::new()),
        }
    }
}

/// 読むだけの文か（確かめずに流してよいか）
pub fn is_read_only(sql: &str) -> bool {
    let first = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("--"))
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(first.as_str(), "SELECT" | "WITH" | "SHOW" | "DESC" | "DESCRIBE" | "EXPLAIN" | "VALUES")
}

/// カーソルの行を含む文（空行で区切ったまとまり。コメントだけの行は除く）
pub fn statement_at(text: &str, line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let line = line.min(lines.len() - 1);
    let blank = |l: &str| l.trim().is_empty();
    if blank(lines[line]) {
        return String::new();
    }
    let start = (0..=line).rev().find(|&i| blank(lines[i])).map_or(0, |i| i + 1);
    let end = (line..lines.len()).find(|&i| blank(lines[i])).unwrap_or(lines.len());
    lines[start..end].iter().filter(|l| !l.trim_start().starts_with("--")).copied().collect::<Vec<_>>().join("\n").trim().to_string()
}

fn config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    home.join(".config").join("nagi").join("databases.txt")
}

fn console_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_DB_DIR") {
        return PathBuf::from(d).join("consoles");
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("consoles")
}

impl Editor {
    /// `:Db`（ActivateDatabaseToolWindow）: データベースのタブを出して移る
    pub(crate) fn open_db_panel(&mut self) {
        if self.db.is_none() {
            let mut sources = nagi_workers::db::detect_sources(&self.nav_root());
            let cfg = config_path();
            if let Ok(t) = std::fs::read_to_string(&cfg) {
                sources.extend(nagi_workers::db::config_sources(&t, &cfg.display().to_string()));
            }
            let conns = sources.iter().map(|_| Conn::default()).collect();
            let mut p = DbPanel { sources, conns, expanded: HashSet::new(), rows: Vec::new(), selected: 0, scroll: 0, password: None, consoles: HashMap::new(), confirm: None, password_then: HashMap::new() };
            p.rebuild();
            if p.sources.is_empty() {
                self.message = format!("接続先がありません（application.properties の spring.datasource.url か {} に書いてください）", cfg.display());
            }
            self.db = Some(p);
        }
        self.focus_right(RightTab::Database);
    }

    /// `:DbAdd 名前 jdbc:URL [利用者]`
    pub(crate) fn db_add(&mut self, arg: &str) {
        let f: Vec<&str> = arg.split_whitespace().collect();
        let (Some(name), Some(url)) = (f.first(), f.get(1).filter(|u| u.starts_with("jdbc:"))) else {
            self.message = ":DbAdd 名前 jdbc:URL [利用者]".into();
            return;
        };
        let ds = DataSource { name: name.to_string(), url: url.to_string(), user: f.get(2).unwrap_or(&"").to_string(), origin: ":DbAdd".into() };
        self.open_db_panel();
        if let Some(p) = &mut self.db {
            p.sources.push(ds);
            p.conns.push(Conn::default());
            p.rebuild();
            p.selected = p.rows.iter().rposition(|r| matches!(r, DbRow::Source(_))).unwrap_or(0);
        }
        self.message = format!("接続先 {name} を足しました（Nagi を閉じるまで。ずっと使うなら {} に書く）", config_path().display());
    }

    /// つなぐ（パスワードを打ち終えたとき）
    fn db_connect(&mut self, i: usize, pass: String) {
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        let c = &mut p.conns[i];
        c.error = None;
        c.connected = false;
        c.tables = None;
        c.columns.clear();
        match Client::connect(&s.url, &s.user, &pass, waker) {
            Ok(cl) => {
                c.client = Some(cl);
                c.expect = VecDeque::from([Expect::Connect]);
                p.expanded.insert(key_source(i));
                self.message = format!("{} につないでいます…", s.name);
            }
            Err(e) => {
                c.client = None;
                c.error = Some(e.clone());
                p.expanded.insert(key_source(i));
                self.message = e;
            }
        }
        p.rebuild();
    }

    /// 裏の応答を受け取る。変わったら true
    pub fn poll_db(&mut self) -> bool {
        let Some(p) = &mut self.db else { return false };
        let mut changed = false;
        let mut results: Vec<(usize, Reply)> = Vec::new();
        for (i, c) in p.conns.iter_mut().enumerate() {
            let Some(cl) = &c.client else { continue };
            while let Ok(r) = cl.rx.try_recv() {
                results.push((i, r));
            }
        }
        for (i, r) in results {
            changed = true;
            let Some(p) = &mut self.db else { break };
            let name = p.sources[i].name.clone();
            let c = &mut p.conns[i];
            let exp = c.expect.pop_front();
            match (exp, r) {
                (Some(Expect::Connect), Reply::Connected { product, schema }) => {
                    c.connected = true;
                    c.product = product.clone();
                    c.schema = schema;
                    if let Some(cl) = &c.client {
                        cl.tables();
                        c.expect.push_back(Expect::Tables);
                    }
                    self.message = format!("{name} につなぎました（{product}）");
                    // つないだら流す SQL
                    if let Some(sql) = p.password_then.remove(&i) {
                        self.db_execute(i, sql);
                    }
                }
                (Some(Expect::Connect), Reply::Error(e)) => {
                    c.client = None;
                    c.error = Some(e.clone());
                    self.message = format!("{name} につなげません: {e}");
                }
                (Some(Expect::Tables), Reply::Tables(t)) => c.tables = Some(t),
                (Some(Expect::Columns), Reply::Columns(t, cols)) => {
                    c.columns.insert(t, cols);
                }
                (Some(Expect::Query), Reply::Rows { cols, rows, more, ms }) => {
                    if let Some(g) = &mut self.db_result {
                        g.set_rows(cols, rows, more, ms);
                    }
                }
                (Some(Expect::Query), Reply::Updated { count, ms }) => {
                    if let Some(g) = &mut self.db_result {
                        g.set_info(format!("{} · {ms} ms", if count >= 0 { format!("{count} 行を更新しました") } else { "実行しました".into() }), false);
                    }
                    // テーブルを作ったり消したりしたかもしれないので読み直す
                    if let Some(cl) = &c.client {
                        cl.tables();
                        c.expect.push_back(Expect::Tables);
                    }
                }
                (Some(Expect::Query), Reply::Error(e)) => {
                    if let Some(g) = &mut self.db_result {
                        g.set_info(format!("誤り: {e}"), true);
                    }
                    self.message = format!("SQL の誤り: {}", e.lines().next().unwrap_or(""));
                }
                (_, Reply::Error(e)) => {
                    self.message = format!("{name}: {}", e.lines().next().unwrap_or(""));
                    if e.contains("接続が切れました") {
                        c.client = None;
                        c.connected = false;
                    }
                }
                _ => {}
            }
        }
        if changed {
            if let Some(p) = &mut self.db {
                p.rebuild();
            }
        }
        changed
    }

    /// SQL を流す（つないでいなければパスワードを聞いてから。データを変える文は確かめてから）
    pub(crate) fn db_execute(&mut self, i: usize, sql: String) {
        if sql.trim().is_empty() {
            self.message = "流す文がありません（カーソルを文の上に置いてください）".into();
            return;
        }
        let Some(p) = &mut self.db else { return };
        if !p.conns[i].connected {
            if p.conns[i].client.is_some() {
                // つないでいる途中: つながったら流す
                p.password_then.insert(i, sql);
            } else {
                p.password = Some((i, Prompt::default(), Some(sql)));
                self.focus_right(RightTab::Database);
                self.message = format!("{} のパスワード（Enter でつないで実行・Esc でやめる）", self.db.as_ref().unwrap().sources[i].name);
            }
            return;
        }
        if !is_read_only(&sql) && p.confirm.is_none() {
            let head: String = sql.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
            self.message = format!("データを変える文です（{head} …）。{} で実行しますか（y で実行）", p.sources[i].name);
            p.confirm = Some((i, sql));
            return;
        }
        let c = &mut p.conns[i];
        let Some(cl) = &c.client else { return };
        cl.query(&sql, MAX_ROWS);
        c.expect.push_back(Expect::Query);
        let first = sql.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        let title = format!("{} · {first}", p.sources[i].name);
        self.show_result(ResultGrid::running(&title));
    }

    /// データを変える文の確認の答え（key で呼ぶ）。扱ったら true
    pub(crate) fn db_confirm_key(&mut self, key: Key) -> bool {
        let Some(p) = &mut self.db else { return false };
        let Some((i, sql)) = p.confirm.take() else { return false };
        if key == Key::Char('y') {
            // 確認済みとして流す
            let c = &mut p.conns[i];
            if let Some(cl) = &c.client {
                cl.query(&sql, MAX_ROWS);
                c.expect.push_back(Expect::Query);
                let first = sql.lines().next().unwrap_or("").chars().take(60).collect::<String>();
                let title = format!("{} · {first}", p.sources[i].name);
                self.show_result(ResultGrid::running(&title));
            }
        } else {
            self.message = "実行をやめました".into();
        }
        true
    }

    /// 今のバッファが SQL コンソールなら、その接続先
    pub fn console_source(&self) -> Option<usize> {
        let path = self.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())?;
        self.db.as_ref()?.consoles.get(&path).copied()
    }

    /// Ctrl+Enter（コンソールで）: カーソルのある文、Visual なら選んだ範囲を流す
    pub(crate) fn db_run_at_cursor(&mut self) {
        let Some(i) = self.console_source() else { return };
        let sql = if let Mode::Visual { .. } = self.mode {
            let (a, b, _) = self.visual_range();
            let s = self.buf.slice_string(a, b);
            self.mode = Mode::Normal;
            s
        } else {
            statement_at(&self.buf.text(), self.cursor.line)
        };
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        self.db_execute(i, sql);
    }

    /// 接続先の SQL コンソールを開く
    fn open_console(&mut self, i: usize, initial: Option<String>) {
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        let dir = console_dir();
        let _ = std::fs::create_dir_all(&dir);
        let safe: String = s.name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
        let path = dir.join(format!("{safe}.sql"));
        if !path.exists() {
            let _ = std::fs::write(&path, format!("-- {}  {}\n-- Ctrl+Enter でカーソルのある文（空行で区切る）を実行。Visual なら選んだ範囲\n\n", s.name, s.url));
        }
        let abs = std::path::absolute(&path).unwrap_or(path.clone());
        p.consoles.insert(abs, i);
        self.edit_file(path);
        self.mode = Mode::Normal;
        if let Some(sql) = initial {
            let end = self.buf.len_bytes();
            let nl = if self.buf.text().ends_with('\n') || end == 0 { "" } else { "\n" };
            self.hist.begin();
            self.edit(end, end, &format!("{nl}{sql}\n"));
            self.hist.close();
            self.cursor.line = self.buf.line_count().saturating_sub(2);
            self.cursor.byte = 0;
            self.clamp_normal();
        }
    }

    fn db_selected(&self) -> Option<DbRow> {
        let p = self.db.as_ref()?;
        p.rows.get(p.selected).cloned()
    }

    fn db_toggle(&mut self, key: String) {
        if let Some(p) = &mut self.db {
            if !p.expanded.remove(&key) {
                p.expanded.insert(key);
            }
            p.rebuild();
        }
    }

    /// Enter: 接続先は開け閉め（つないでいなければパスワードを聞く）、テーブルは列を出す
    fn db_activate(&mut self) {
        match self.db_selected() {
            Some(DbRow::Source(i)) => {
                let Some(p) = &mut self.db else { return };
                if p.conns[i].client.is_none() {
                    p.password = Some((i, Prompt::default(), None));
                    self.message = format!("{} のパスワード（{}。Enter でつなぐ・Esc でやめる）", p.sources[i].name, p.sources[i].url);
                } else {
                    self.db_toggle(key_source(i));
                }
            }
            Some(DbRow::Table(i, t)) => {
                let Some(p) = &mut self.db else { return };
                let c = &mut p.conns[i];
                if !c.columns.contains_key(&t) {
                    if let Some(cl) = &c.client {
                        cl.columns(&t);
                        c.expect.push_back(Expect::Columns);
                    }
                }
                self.db_toggle(key_table(i, &t));
            }
            _ => {}
        }
    }

    /// o: テーブルの先頭の行を出す
    fn db_open_table(&mut self) {
        let Some(DbRow::Table(i, t)) = self.db_selected() else { return };
        let Some(p) = &self.db else { return };
        let schema = &p.conns[i].schema;
        let q = |s: &str| if s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') { s.to_string() } else { format!("\"{s}\"") };
        let name = if schema.is_empty() { q(&t) } else { format!("{}.{}", q(schema), q(&t)) };
        self.db_execute(i, format!("SELECT * FROM {name}"));
        self.mode = Mode::List;
        self.list_side = crate::editor::Side::Right;
    }

    pub(crate) fn db_key(&mut self, key: Key) -> Effect {
        let half = (self.windows_rows() / 2).max(1);
        let Some(p) = &mut self.db else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        // パスワードの入力中
        if let Some((i, prompt, then)) = &mut p.password {
            match key {
                Key::Esc => {
                    p.password = None;
                    self.message = "やめました".into();
                }
                Key::Enter => {
                    let (i, pass, then) = (*i, std::mem::take(&mut prompt.text), then.take());
                    p.password = None;
                    if let Some(sql) = then {
                        p.password_then.insert(i, sql);
                    }
                    self.db_connect(i, pass);
                }
                k => {
                    prompt.key(k);
                }
            }
            return Effect::None;
        }
        let last = p.rows.len().saturating_sub(1);
        match key {
            Key::Char('j') | Key::Down => p.selected = (p.selected + 1).min(last),
            Key::Char('k') | Key::Up => p.selected = p.selected.saturating_sub(1),
            Key::Ctrl('d') => p.selected = (p.selected + half).min(last),
            Key::Ctrl('u') => p.selected = p.selected.saturating_sub(half),
            Key::Char('G') => p.selected = last,
            Key::Char('g') => p.selected = 0,
            Key::Enter | Key::Char('l') | Key::Right => self.db_activate(),
            Key::Char('h') | Key::Left => {
                let cur = p.rows.get(p.selected).cloned();
                let parent = match &cur {
                    Some(DbRow::Table(i, t)) if p.expanded.contains(&key_table(*i, t)) => Some(key_table(*i, t)),
                    Some(DbRow::Source(i)) if p.expanded.contains(&key_source(*i)) => Some(key_source(*i)),
                    _ => None,
                };
                match parent {
                    Some(k) => self.db_toggle(k),
                    None => {
                        let depth = cur.as_ref().map_or(0, |r| p.row_label(r).0);
                        if let Some(j) = p.rows[..p.selected].iter().rposition(|r| p.row_label(r).0 < depth) {
                            p.selected = j;
                        }
                    }
                }
            }
            Key::Char('o') => self.db_open_table(),
            // コンソール（テーブルの上なら SELECT を書き足して）
            Key::Char('c') => {
                let init = match self.db_selected() {
                    Some(DbRow::Table(_, t)) => Some(format!("SELECT * FROM {t};")),
                    _ => None,
                };
                if let Some(DbRow::Source(i) | DbRow::Table(i, _) | DbRow::Column(i, _, _) | DbRow::Info(i, _)) = self.db_selected() {
                    self.open_console(i, init);
                }
            }
            // 切る
            Key::Char('x') => {
                if let Some(DbRow::Source(i) | DbRow::Table(i, _) | DbRow::Column(i, _, _) | DbRow::Info(i, _)) = self.db_selected() {
                    let p = self.db.as_mut().unwrap();
                    p.conns[i] = Conn::default();
                    p.expanded.remove(&key_source(i));
                    p.rebuild();
                    p.selected = p.rows.iter().position(|r| *r == DbRow::Source(i)).unwrap_or(0);
                    self.message = format!("{} を切りました", p.sources[i].name);
                }
            }
            // テーブルを読み直す
            Key::Char('r') => {
                if let Some(DbRow::Source(i) | DbRow::Table(i, _) | DbRow::Column(i, _, _) | DbRow::Info(i, _)) = self.db_selected() {
                    let c = &mut self.db.as_mut().unwrap().conns[i];
                    if let Some(cl) = &c.client {
                        c.tables = None;
                        c.columns.clear();
                        cl.tables();
                        c.expect.push_back(Expect::Tables);
                    }
                    self.db.as_mut().unwrap().rebuild();
                }
            }
            Key::Char('q') => {
                self.db = None;
                self.mode = Mode::Normal;
                self.rows = self.text_rows(self.active_rect());
            }
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.mode = Mode::Command;
                self.cmdline = Default::default();
            }
            _ => {}
        }
        Effect::None
    }

    pub(crate) fn db_click(&mut self, row: usize, clicks: u32) {
        let Some(p) = &mut self.db else { return };
        let i = p.scroll + row - 1;
        if i >= p.rows.len() {
            return;
        }
        p.selected = i;
        self.focus_right(RightTab::Database);
        match self.db_selected() {
            Some(DbRow::Table(..)) if clicks >= 2 => self.db_open_table(),
            Some(DbRow::Table(..)) | Some(DbRow::Source(_)) if clicks == 1 => {
                // 1 回目は選ぶだけ（つなぐ・開くは 2 回目かキーで）
            }
            Some(_) if clicks >= 2 => self.db_activate(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements() {
        let text = "-- head\n\nselect 1\nfrom dual;\n\n-- note\nupdate t set a = 1\n";
        assert_eq!(statement_at(text, 2), "select 1\nfrom dual;");
        assert_eq!(statement_at(text, 3), "select 1\nfrom dual;");
        assert_eq!(statement_at(text, 6), "update t set a = 1");
        assert_eq!(statement_at(text, 1), "");
        assert!(is_read_only("-- x\n  select * from t"));
        assert!(is_read_only("WITH a AS (select 1) select * from a"));
        assert!(!is_read_only("update t set a=1"));
        assert!(!is_read_only("DELETE FROM t"));
        assert!(!is_read_only("drop table t"));
    }

    /// H2 のメモリ DB で、つなぐ・表を作る（確認あり）・コンソールから流す・テーブルの木
    #[test]
    fn console_round_trip() {
        if nagi_workers::db::find_java().is_none() || nagi_workers::db::find_driver("jdbc:h2:mem:x").is_none() {
            eprintln!("Java か H2 が無いので飛ばす");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nagi-dbui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: テストの中だけで使う
        unsafe { std::env::set_var("NAGI_DB_DIR", &dir) };
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(140, 40);
        e.open_db_panel();
        e.db_add("mem jdbc:h2:mem:ui sa");
        let keys = |e: &mut Editor, k: &str| {
            for key in nagi_vim::parse_keys(k, " ") {
                e.key(key);
            }
        };
        let wait = |e: &mut Editor, what: &dyn Fn(&Editor) -> bool| {
            for _ in 0..3000 {
                e.poll_db();
                if what(e) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("待ちきれない: {}", e.message);
        };
        // 接続先を選んで Enter → パスワード（空）→ Enter
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        keys(&mut e, "<CR>");
        assert!(e.db.as_ref().unwrap().password.is_some());
        keys(&mut e, "<CR>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].tables.is_some());
        // コンソールを開いて表を作る（データを変える文なので y で確かめる）
        keys(&mut e, "c");
        assert_eq!(e.console_source(), Some(i));
        // Ctrl+Enter は Ctrl-m として届く
        keys(&mut e, "Gocreate table fruit(id int, name varchar(10))<Esc><C-m>");
        assert!(e.message.contains("データを変える文"), "{}", e.message);
        keys(&mut e, "y");
        wait(&mut e, &|e| e.db_result.as_ref().is_some_and(|g| !g.running));
        assert!(e.db_result.as_ref().unwrap().info.contains("実行しました") || e.db_result.as_ref().unwrap().info.contains("行を更新"), "{}", e.db_result.as_ref().unwrap().info);
        keys(&mut e, "o<CR>insert into fruit values (1, 'りんご'), (2, null)<Esc><C-m>y");
        wait(&mut e, &|e| e.db_result.as_ref().is_some_and(|g| !g.running));
        assert!(e.db_result.as_ref().unwrap().info.starts_with("2 行を更新"), "{}", e.db_result.as_ref().unwrap().info);
        // SELECT はすぐ流れる
        keys(&mut e, "o<CR>select * from fruit order by id<Esc><C-m>");
        wait(&mut e, &|e| e.db_result.as_ref().is_some_and(|g| !g.running));
        let g = e.db_result.as_ref().unwrap();
        assert_eq!(g.cols, vec!["ID", "NAME"]);
        assert_eq!(g.rows, vec![vec![Some("1".to_string()), Some("りんご".into())], vec![Some("2".into()), None]]);
        // テーブルの木に FRUIT が出る（作った後に読み直している）
        wait(&mut e, &|e| e.db.as_ref().unwrap().rows.contains(&DbRow::Table(i, "FRUIT".into())));
    }
}
