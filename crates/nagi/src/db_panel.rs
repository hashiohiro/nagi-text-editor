//! 右の欄の「データベース」タブ（DataGrip と同じ使い勝手を、ターミナルの見た目で）。
//!
//! - 接続先: Spring の設定（application*.properties）、`~/.config/nagi/databases.txt`（`名前 | jdbc:URL | 利用者`）、
//!   `a`（＋）で足したもの。パスワードはつなぐときに伏せ字で打ち、メモリにだけ置く。
//! - ドライバー: Oracle は Instant Client が要る。無ければ、つなぐ前に「ダウンロードしますか」と聞いて取ってくる
//!   （nagi_workers::drivers）。SQLite は中に組み込んであるので準備は要らない。
//! - 木: 接続先 › スキーマ › テーブル・ビュー・シーケンス・ルーチン（数）› 列・キー・インデックス。
//!   K は主キー、F は外部キー（参照先も出す）。ほかのスキーマは「ほかのスキーマ」の下に、開いたときに読む。
//!   `/` で名前を絞り込む。
//! - コンソール: 接続先ごとの .sql ファイル。上の帯に ▶ 実行・Tx 自動／手動・未コミットの数・コミット／
//!   ロールバック・接続先 › スキーマ。Ctrl+Enter でカーソルのある文を流し、結果は下のパネルのタブに出す。
//!   自動のときはデータを変える文を y で確かめてから。手動のときはそのまま流し、未コミットの数を数える。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use nagi_vim::Key;
use nagi_workers::db::{Client, Column, DataSource, Index, Key as DbKey, Kind, Objects, Reply, Req};
use nagi_workers::drivers::{self, DlMsg, Download};

use crate::editor::{Editor, Effect, Mode, Prompt};
use crate::right_panel::RightTab;

/// 表に出す行の上限（1 ページ）
pub const MAX_ROWS: usize = 500;

/// 結果を返す先
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// 下のパネルの結果のタブ（番号）
    Result(u64),
    /// テーブルのデータの編集の画面
    Table,
}

/// 届くはずの応答（頼んだ順に届く）
enum Expect {
    Connect,
    Schemas,
    Objects(String),
    Columns(String, String),
    Indexes(String, String),
    /// (返す先, 出力に書く SQL, データを変えうる文か)
    Query(Target, String, bool),
    Count(Target),
    /// まとめて送った文の数
    Batch(usize),
    Tx,
    SetSchema(String),
    Ddl(String),
}

#[derive(Default)]
pub struct Conn {
    pub client: Option<Client>,
    pub connected: bool,
    pub product: String,
    pub schema: String,
    pub schemas: Option<Vec<String>>,
    pub objects: HashMap<String, Objects>,
    pub columns: HashMap<(String, String), Vec<Column>>,
    pub indexes: HashMap<(String, String), (Vec<Index>, Vec<DbKey>)>,
    pub error: Option<String>,
    expect: VecDeque<Expect>,
    requested: HashSet<String>,
    /// 手動のトランザクション
    pub manual: bool,
    /// 手動のときの、まだコミットしていない文の数
    pub pending: usize,
}

impl Conn {
    fn send(&mut self, req: Req, e: Expect) {
        if let Some(c) = &self.client {
            c.send(req);
            self.expect.push_back(e);
        }
    }

    pub(crate) fn send_batch(&mut self, stmts: Vec<String>) {
        let n = stmts.len();
        self.send(Req::Batch(stmts), Expect::Batch(n));
    }

    pub(crate) fn send_table_query(&mut self, sql: String, offset: usize, count: Option<String>) {
        self.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset }, Expect::Query(Target::Table, sql, false));
        if let Some(c) = count {
            self.send(Req::Count(c), Expect::Count(Target::Table));
        }
    }

    /// 列がまだ無ければ読みに行く
    pub(crate) fn ensure_columns(&mut self, s: &str, t: &str) {
        if self.connected && !self.columns.contains_key(&(s.to_string(), t.to_string())) && self.requested.insert(format!("c:{s}.{t}")) {
            self.send_columns(s, t);
        }
    }

    pub(crate) fn send_columns(&mut self, s: &str, t: &str) {
        self.send(Req::Columns(s.into(), t.into()), Expect::Columns(s.into(), t.into()));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Tables,
    Views,
    Sequences,
    Routines,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Tables => "テーブル",
            Group::Views => "ビュー",
            Group::Sequences => "シーケンス",
            Group::Routines => "プロシージャ・関数",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sub {
    Columns,
    Keys,
    Indexes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbRow {
    Source(usize),
    Schema(usize, String, usize),
    Others(usize),
    Group(usize, String, Group, usize),
    Object(usize, String, String, Group, usize),
    Sub(usize, String, String, Sub, usize),
    Column(usize, String, String, usize, usize),
    Key(usize, String, String, usize, usize),
    Index(usize, String, String, usize, usize),
    Info(usize, usize, String),
}

impl DbRow {
    pub fn source(&self) -> usize {
        match self {
            DbRow::Source(i) | DbRow::Others(i) | DbRow::Schema(i, ..) | DbRow::Group(i, ..) | DbRow::Object(i, ..) | DbRow::Sub(i, ..) | DbRow::Column(i, ..) | DbRow::Key(i, ..) | DbRow::Index(i, ..) | DbRow::Info(i, ..) => *i,
        }
    }

    /// 開け閉めのキー
    pub fn key(&self) -> Option<String> {
        Some(match self {
            DbRow::Source(i) => format!("s{i}"),
            DbRow::Others(i) => format!("o{i}"),
            DbRow::Schema(i, s, _) => format!("c{i}:{s}"),
            DbRow::Group(i, s, g, _) => format!("g{i}:{s}:{g:?}"),
            DbRow::Object(i, s, t, Group::Tables | Group::Views, _) => format!("t{i}:{s}:{t}"),
            DbRow::Sub(i, s, t, k, _) => format!("u{i}:{s}:{t}:{k:?}"),
            _ => return None,
        })
    }
}

/// 接続先の追加のフォーム
pub struct Form {
    pub kind: Kind,
    /// (見出し, 入力欄)。0 番は種類（← → で切り替え）
    pub fields: Vec<(&'static str, Prompt)>,
    pub at: usize,
}

impl Form {
    fn new(kind: Kind) -> Form {
        let mut f = Form { kind, fields: Vec::new(), at: 1 };
        f.set_kind(kind);
        f
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        let p = |s: &str| {
            let mut p = Prompt::default();
            p.set(s);
            p
        };
        let name = self.fields.get(1).map(|f| f.1.text.clone()).unwrap_or_default();
        self.fields = match kind {
            Kind::Oracle => vec![("種類", p("Oracle")), ("名前", p(&name)), ("ホスト", p("localhost")), ("ポート", p("1521")), ("サービス名", p("")), ("利用者", p(""))],
            Kind::Sqlite => vec![("種類", p("SQLite")), ("名前", p(&name)), ("ファイル", p(""))],
            Kind::Salesforce => vec![("種類", p("Salesforce")), ("名前", p(&name)), ("ログイン URL", p("https://login.salesforce.com")), ("利用者", p(""))],
        };
        self.at = self.at.min(self.fields.len() - 1);
    }

    fn get(&self, label: &str) -> String {
        self.fields.iter().find(|f| f.0 == label).map_or(String::new(), |f| f.1.text.trim().to_string())
    }

    /// 入力から接続先を作る
    pub fn source(&self) -> Result<DataSource, String> {
        let name = self.get("名前");
        if name.is_empty() {
            return Err("名前を入れてください".into());
        }
        let (url, user) = match self.kind {
            Kind::Oracle => {
                let (host, port, svc) = (self.get("ホスト"), self.get("ポート"), self.get("サービス名"));
                if host.is_empty() || svc.is_empty() {
                    return Err("ホストとサービス名を入れてください".into());
                }
                (format!("jdbc:oracle:thin:@//{host}:{}/{svc}", if port.is_empty() { "1521".into() } else { port }), self.get("利用者"))
            }
            Kind::Sqlite => {
                let f = self.get("ファイル");
                (format!("jdbc:sqlite:{}", if f.is_empty() { ":memory:".into() } else { f }), String::new())
            }
            Kind::Salesforce => {
                let u = self.get("ログイン URL");
                (format!("salesforce:{}", if u.is_empty() { "https://login.salesforce.com".into() } else { u }), self.get("利用者"))
            }
        };
        Ok(DataSource { name, url, user, origin: config_path().display().to_string() })
    }
}

pub struct DbPanel {
    pub sources: Vec<DataSource>,
    pub conns: Vec<Conn>,
    pub expanded: HashSet<String>,
    pub rows: Vec<DbRow>,
    pub selected: usize,
    pub scroll: usize,
    /// 名前の絞り込み（打っている間は filtering）
    pub filter: Prompt,
    pub filtering: bool,
    /// パスワードの入力（接続先, 入力欄）
    pub password: Option<(usize, Prompt)>,
    /// ドライバーのダウンロードの確認（接続先）
    pub setup_ask: Option<usize>,
    /// ダウンロード中（接続先, 進み具合の文）
    pub download: Option<(usize, Download, String)>,
    pub form: Option<Form>,
    /// コンソールのファイル → 接続先
    pub consoles: HashMap<PathBuf, usize>,
    /// データを変える文の確認（接続先, SQL）
    pub confirm: Option<(usize, String)>,
    /// つながったら流す SQL（接続先 → SQL）
    pub then: HashMap<usize, String>,
}

pub fn config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    home.join(".config").join("nagi").join("databases.txt")
}

pub fn work_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_DB_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi")
}

/// 名前をファイル名に使える形に
pub fn safe_name(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
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
    matches!(first.as_str(), "SELECT" | "WITH" | "SHOW" | "DESC" | "DESCRIBE" | "EXPLAIN" | "VALUES" | "PRAGMA")
}

/// カーソルの行を含む文の行の範囲（空行で区切ったまとまり。start..end）
pub fn statement_lines(lines: &[&str], line: usize) -> Option<(usize, usize)> {
    let blank = |l: &str| l.trim().is_empty();
    if lines.is_empty() || blank(lines[line.min(lines.len() - 1)]) {
        return None;
    }
    let line = line.min(lines.len() - 1);
    let start = (0..=line).rev().find(|&i| blank(lines[i])).map_or(0, |i| i + 1);
    let end = (line..lines.len()).find(|&i| blank(lines[i])).unwrap_or(lines.len());
    // 頭のコメントだけの行は除く
    let start = (start..end).find(|&i| !lines[i].trim_start().starts_with("--")).unwrap_or(end);
    (start < end).then_some((start, end))
}

/// カーソルの行を含む文（コメントだけの行は除く）
#[cfg(test)]
pub fn statement_at(text: &str, line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some((s, e)) = statement_lines(&lines, line) else { return String::new() };
    lines[s..e].iter().filter(|l| !l.trim_start().starts_with("--")).copied().collect::<Vec<_>>().join("\n").trim().to_string()
}

/// 識別子（必要なら "" で囲む）
pub fn ident(s: &str) -> String {
    let plain = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '#');
    let same_case = s.chars().all(|c| !c.is_ascii_lowercase()) || s.chars().all(|c| !c.is_ascii_uppercase());
    if plain && same_case { s.to_string() } else { format!("\"{}\"", s.replace('"', "\"\"")) }
}

/// 値の書き方（NULL か '...'）
pub fn literal(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

/// スキーマ付きの名前
pub fn qualified(schema: &str, name: &str) -> String {
    if schema.is_empty() { ident(name) } else { format!("{}.{}", ident(schema), ident(name)) }
}

impl DbPanel {
    pub fn kind(&self, i: usize) -> Option<Kind> {
        Kind::of(&self.sources.get(i)?.url)
    }

    pub fn is_open(&self, r: &DbRow) -> bool {
        r.key().is_some_and(|k| self.expanded.contains(&k)) || (!self.filter.text.is_empty() && matches!(r, DbRow::Group(..)))
    }

    fn matches(&self, name: &str) -> bool {
        self.filter.text.is_empty() || name.to_lowercase().contains(&self.filter.text.to_lowercase())
    }

    pub(crate) fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for i in 0..self.conns.len() {
            rows.push(DbRow::Source(i));
            if !self.expanded.contains(&format!("s{i}")) {
                continue;
            }
            let c = &self.conns[i];
            if let Some((d, _, text)) = &self.download {
                if *d == i {
                    rows.push(DbRow::Info(i, 1, text.clone()));
                    continue;
                }
            }
            if let Some(e) = &c.error {
                rows.push(DbRow::Info(i, 1, format!("誤り: {e}")));
                continue;
            }
            if !c.connected {
                if c.client.is_some() {
                    rows.push(DbRow::Info(i, 1, "つないでいます…".into()));
                }
                continue;
            }
            let cur = c.schema.clone();
            self.push_schema(i, &cur, 1, &mut rows);
            let others: Vec<String> = c.schemas.iter().flatten().filter(|s| **s != cur).cloned().collect();
            if !others.is_empty() {
                let r = DbRow::Others(i);
                let open = self.is_open(&r);
                rows.push(r);
                if open {
                    for s in others {
                        self.push_schema(i, &s, 2, &mut rows);
                    }
                }
            }
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    fn push_schema(&self, i: usize, s: &str, d: usize, rows: &mut Vec<DbRow>) {
        let r = DbRow::Schema(i, s.to_string(), d);
        let open = self.is_open(&r);
        rows.push(r);
        if !open {
            return;
        }
        let Some(o) = self.conns[i].objects.get(s) else {
            rows.push(DbRow::Info(i, d + 1, "読んでいます…".into()));
            return;
        };
        let groups: [(Group, Vec<String>); 4] = [
            (Group::Tables, o.tables.iter().filter(|t| t.1 != "VIEW").map(|t| t.0.clone()).collect()),
            (Group::Views, o.tables.iter().filter(|t| t.1 == "VIEW").map(|t| t.0.clone()).collect()),
            (Group::Sequences, o.sequences.clone()),
            (Group::Routines, o.routines.clone()),
        ];
        for (g, names) in groups {
            let names: Vec<String> = names.into_iter().filter(|n| self.matches(n) || self.column_matches(i, s, n)).collect();
            if names.is_empty() && (g != Group::Tables || !self.filter.text.is_empty()) {
                continue;
            }
            let r = DbRow::Group(i, s.to_string(), g, names.len());
            let open = self.is_open(&r);
            rows.push(r);
            if !open {
                continue;
            }
            for n in names {
                let r = DbRow::Object(i, s.to_string(), n.clone(), g, d + 2);
                let open = self.is_open(&r);
                rows.push(r);
                if open {
                    self.push_object(i, s, &n, d + 3, rows);
                }
            }
        }
    }

    fn column_matches(&self, i: usize, s: &str, t: &str) -> bool {
        !self.filter.text.is_empty() && self.conns[i].columns.get(&(s.to_string(), t.to_string())).is_some_and(|cs| cs.iter().any(|c| self.matches(&c.name)))
    }

    fn push_object(&self, i: usize, s: &str, t: &str, d: usize, rows: &mut Vec<DbRow>) {
        let c = &self.conns[i];
        let key = (s.to_string(), t.to_string());
        let cols = c.columns.get(&key);
        let idx = c.indexes.get(&key);
        for (sub, n) in [(Sub::Columns, cols.map_or(0, |v| v.len())), (Sub::Keys, idx.map_or(0, |v| v.1.len())), (Sub::Indexes, idx.map_or(0, |v| v.0.len()))] {
            let r = DbRow::Sub(i, s.to_string(), t.to_string(), sub, n);
            let open = self.is_open(&r);
            rows.push(r);
            if !open {
                continue;
            }
            let loaded = match sub {
                Sub::Columns => cols.is_some(),
                _ => idx.is_some(),
            };
            if !loaded {
                rows.push(DbRow::Info(i, d + 1, "読んでいます…".into()));
                continue;
            }
            for k in 0..n {
                rows.push(match sub {
                    Sub::Columns => DbRow::Column(i, s.to_string(), t.to_string(), k, d + 1),
                    Sub::Keys => DbRow::Key(i, s.to_string(), t.to_string(), k, d + 1),
                    Sub::Indexes => DbRow::Index(i, s.to_string(), t.to_string(), k, d + 1),
                });
            }
        }
    }

    /// 行の見出し（字下げ, 印, 本文, 補足）。印: T テーブル V ビュー S シーケンス R ルーチン K 主キー F 外部キー I インデックス
    pub fn row_label(&self, r: &DbRow) -> (usize, char, String, String) {
        match r {
            DbRow::Source(i) => {
                let s = &self.sources[*i];
                let c = &self.conns[*i];
                let mark = if c.connected { '●' } else if c.client.is_some() { '◌' } else { '○' };
                let kind = Kind::of(&s.url).map_or("?", |k| k.label());
                let note = if c.connected { c.product.clone() } else { format!("{kind}  {}", s.user) };
                (0, mark, s.name.clone(), note)
            }
            DbRow::Schema(_, s, d) => (*d, ' ', s.clone(), "スキーマ".into()),
            DbRow::Others(i) => (1, ' ', "ほかのスキーマ".into(), format!("{}", self.conns[*i].schemas.as_ref().map_or(0, |v| v.len().saturating_sub(1)))),
            DbRow::Group(i, _, g, n) => {
                let label = match (self.kind(*i), g) {
                    (Some(Kind::Salesforce), Group::Tables) => "オブジェクト",
                    (Some(Kind::Salesforce), Group::Views) => "カスタムオブジェクト",
                    _ => g.label(),
                };
                (0, ' ', label.into(), n.to_string())
            }
            DbRow::Object(_, _, t, g, d) => (
                *d,
                match g {
                    Group::Tables => 'T',
                    Group::Views => 'V',
                    Group::Sequences => 'S',
                    Group::Routines => 'R',
                },
                t.clone(),
                String::new(),
            ),
            DbRow::Sub(_, _, _, k, n) => (
                0,
                ' ',
                match k {
                    Sub::Columns => "列",
                    Sub::Keys => "キー",
                    Sub::Indexes => "インデックス",
                }
                .into(),
                if *n > 0 { n.to_string() } else { String::new() },
            ),
            DbRow::Column(i, s, t, k, d) => {
                let c = &self.conns[*i].columns[&(s.clone(), t.clone())][*k];
                let mark = if c.pk { 'K' } else if !c.fk.is_empty() { 'F' } else { ' ' };
                let note = format!("{}{}{}", c.ty, if c.nullable { "" } else { " NOT NULL" }, if c.fk.is_empty() { String::new() } else { format!(" → {}", c.fk) });
                (*d, mark, c.name.clone(), note)
            }
            DbRow::Key(i, s, t, k, d) => {
                let key = &self.conns[*i].indexes[&(s.clone(), t.clone())].1[*k];
                (*d, if key.refs.is_empty() { 'K' } else { 'F' }, key.name.clone(), if key.refs.is_empty() { format!("({})", key.cols) } else { format!("({}) → {}", key.cols, key.refs) })
            }
            DbRow::Index(i, s, t, k, d) => {
                let x = &self.conns[*i].indexes[&(s.clone(), t.clone())].0[*k];
                (*d, 'I', x.name.clone(), format!("({}){}", x.cols, if x.unique { " UNIQUE" } else { "" }))
            }
            DbRow::Info(_, d, s) => (*d, ' ', s.clone(), String::new()),
        }
    }

    /// 字下げ（Group と Sub は親から決める）
    pub fn depth(&self, k: usize) -> usize {
        match &self.rows[k] {
            DbRow::Group(..) => self.rows[..k].iter().rev().find_map(|r| if let DbRow::Schema(_, _, d) = r { Some(d + 1) } else { None }).unwrap_or(2),
            DbRow::Sub(..) => self.rows[..k].iter().rev().find_map(|r| if let DbRow::Object(_, _, _, _, d) = r { Some(d + 1) } else { None }).unwrap_or(4),
            r => self.row_label(r).0,
        }
    }
}

impl Editor {
    /// `:Db`（ActivateDatabaseToolWindow）: データベースのタブを出して移る
    pub(crate) fn open_db_panel(&mut self) {
        if self.db.is_none() {
            let mut sources = nagi_workers::db::detect_sources(&self.nav_root());
            let cfg = config_path();
            if let Ok(t) = std::fs::read_to_string(&cfg) {
                for s in nagi_workers::db::config_sources(&t, &cfg.display().to_string()) {
                    if !sources.iter().any(|x| x.url == s.url && x.user == s.user) {
                        sources.push(s);
                    }
                }
            }
            let conns = sources.iter().map(|_| Conn::default()).collect();
            let mut p = DbPanel {
                sources,
                conns,
                expanded: HashSet::new(),
                rows: Vec::new(),
                selected: 0,
                scroll: 0,
                filter: Prompt::default(),
                filtering: false,
                password: None,
                setup_ask: None,
                download: None,
                form: None,
                consoles: HashMap::new(),
                confirm: None,
                then: HashMap::new(),
            };
            p.rebuild();
            self.db = Some(p);
        }
        if self.db.as_ref().is_some_and(|p| p.sources.is_empty()) {
            self.message = "接続先がありません。a（＋）で足せます".into();
        }
        self.focus_right(RightTab::Database);
    }

    /// `:DbAdd`（引数なしならフォーム、`名前 jdbc:URL [利用者]` ならそのまま足す）
    pub(crate) fn db_add(&mut self, arg: &str) {
        self.open_db_panel();
        let f: Vec<&str> = arg.split_whitespace().collect();
        let Some(p) = &mut self.db else { return };
        let (Some(name), Some(url)) = (f.first(), f.get(1).filter(|u| u.starts_with("jdbc:"))) else {
            p.form = Some(Form::new(Kind::Oracle));
            self.message = "接続先を足す: Tab で次の欄、種類は ← →、最後の欄で Enter で保存してつなぐ".into();
            return;
        };
        let ds = DataSource { name: name.to_string(), url: url.to_string(), user: f.get(2).unwrap_or(&"").to_string(), origin: ":DbAdd".into() };
        self.db_push_source(ds, false);
    }

    fn db_push_source(&mut self, ds: DataSource, save: bool) {
        let Some(p) = &mut self.db else { return };
        if save {
            let cfg = config_path();
            if let Some(dir) = cfg.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!("{} | {} | {}\n", ds.name, ds.url, ds.user));
            match std::fs::write(&cfg, text) {
                Ok(()) => self.message = format!("{} を {} に保存しました", ds.name, cfg.display()),
                Err(e) => self.message = format!("保存できません: {e}"),
            }
        } else {
            self.message = format!("接続先 {} を足しました", ds.name);
        }
        p.sources.push(ds);
        p.conns.push(Conn::default());
        p.rebuild();
        p.selected = p.rows.iter().position(|r| *r == DbRow::Source(p.sources.len() - 1)).unwrap_or(0);
    }

    /// つなぐ（準備ができていなければダウンロードを聞く。パスワードが要ればそれを聞く）
    fn db_begin_connect(&mut self, i: usize) {
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        if Kind::of(&s.url) == Some(Kind::Oracle) && !drivers::oracle_ready() {
            p.setup_ask = Some(i);
            self.message = format!(
                "Oracle のドライバー（Instant Client Basic Light、約 {} MB）が要ります。{} に置きます（{}）。y でダウンロード",
                drivers::ORACLE_CLIENT_SIZE_MB,
                drivers::drivers_dir().join("oracle").display(),
                drivers::ORACLE_LICENSE
            );
            return;
        }
        if matches!(Kind::of(&s.url), Some(Kind::Sqlite | Kind::Salesforce)) {
            self.db_connect(i, String::new());
            return;
        }
        p.password = Some((i, Prompt::default()));
        self.message = format!("{} のパスワード（{}。Enter でつなぐ・Esc でやめる）", s.name, s.url);
    }

    fn db_connect(&mut self, i: usize, pass: String) {
        let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        p.conns[i] = Conn::default();
        p.expanded.insert(format!("s{i}"));
        match Client::connect(&s.url, &s.user, &pass, waker) {
            Ok(cl) => {
                p.conns[i].client = Some(cl);
                p.conns[i].expect = VecDeque::from([Expect::Connect]);
                self.message = format!("{} につないでいます…", s.name);
            }
            Err(e) => {
                p.conns[i].error = Some(e.clone());
                self.message = e;
            }
        }
        p.rebuild();
    }

    /// 開いたところの中身を読む
    fn db_ensure(&mut self, r: &DbRow) {
        let Some(p) = &mut self.db else { return };
        let i = r.source();
        let c = &mut p.conns[i];
        if !c.connected {
            return;
        }
        match r {
            DbRow::Schema(_, s, _) => {
                if !c.objects.contains_key(s) && c.requested.insert(format!("o:{s}")) {
                    c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
                }
            }
            DbRow::Object(_, s, t, Group::Tables | Group::Views, _) | DbRow::Sub(_, s, t, Sub::Columns, _) => {
                if !c.columns.contains_key(&(s.clone(), t.clone())) && c.requested.insert(format!("c:{s}.{t}")) {
                    c.send(Req::Columns(s.clone(), t.clone()), Expect::Columns(s.clone(), t.clone()));
                }
                if matches!(r, DbRow::Object(..)) && !c.indexes.contains_key(&(s.clone(), t.clone())) && c.requested.insert(format!("i:{s}.{t}")) {
                    c.send(Req::Indexes(s.clone(), t.clone()), Expect::Indexes(s.clone(), t.clone()));
                }
            }
            _ => {}
        }
    }

    fn db_toggle_row(&mut self, r: &DbRow) {
        let Some(k) = r.key() else { return };
        let opened = {
            let Some(p) = &mut self.db else { return };
            let open = !p.expanded.remove(&k);
            if open {
                p.expanded.insert(k);
            }
            open
        };
        if opened {
            self.db_ensure(r);
        }
        if let Some(p) = &mut self.db {
            p.rebuild();
        }
    }

    /// 裏の応答を受け取る。変わったら true
    pub fn poll_db(&mut self) -> bool {
        let mut changed = false;
        // ダウンロード
        let dl = self.db.as_mut().and_then(|p| {
            let (i, d, _) = p.download.as_ref()?;
            let mut last = None;
            while let Ok(m) = d.rx.try_recv() {
                let done = matches!(m, DlMsg::Done(_));
                last = Some(m);
                if done {
                    break;
                }
            }
            last.map(|m| (*i, m))
        });
        if let Some((i, m)) = dl {
            changed = true;
            let p = self.db.as_mut().unwrap();
            match m {
                DlMsg::Progress(got, total) => {
                    let text = if total > 0 { format!("ダウンロードしています… {} / {} MB", got >> 20, total >> 20) } else { format!("ダウンロードしています… {} MB", got >> 20) };
                    if let Some(d) = &mut p.download {
                        d.2 = text;
                    }
                }
                DlMsg::Extracting => {
                    if let Some(d) = &mut p.download {
                        d.2 = "展開しています…".into();
                    }
                }
                DlMsg::Done(r) => {
                    p.download = None;
                    match r {
                        Ok(dir) => {
                            self.message = format!("Oracle のドライバーを {} に置きました", dir.display());
                            self.db_log_line(&format!("ドライバー: Oracle Instant Client を {} に置きました", dir.display()), false);
                            self.db_begin_connect(i);
                        }
                        Err(e) => {
                            p.conns[i].error = Some(e.clone());
                            self.message = e;
                        }
                    }
                }
            }
            if let Some(p) = &mut self.db {
                p.rebuild();
            }
        }
        let Some(p) = &mut self.db else { return changed };
        let mut replies: Vec<(usize, Reply)> = Vec::new();
        for (i, c) in p.conns.iter().enumerate() {
            let Some(cl) = &c.client else { continue };
            while let Ok(r) = cl.rx.try_recv() {
                replies.push((i, r));
            }
        }
        for (i, r) in replies {
            changed = true;
            self.db_reply(i, r);
        }
        if changed {
            if let Some(p) = &mut self.db {
                p.rebuild();
            }
        }
        changed
    }

    fn db_reply(&mut self, i: usize, r: Reply) {
        // 途中の知らせは、頼みごとの答えではない
        if let Reply::Note(s) = r {
            self.message = s.clone();
            self.db_log_line(&s, false);
            return;
        }
        let Some(p) = &mut self.db else { return };
        let name = p.sources[i].name.clone();
        let c = &mut p.conns[i];
        let exp = c.expect.pop_front();
        match (exp, r) {
            (Some(Expect::Connect), Reply::Connected { product, schema }) => {
                c.connected = true;
                c.product = product.clone();
                c.schema = schema.clone();
                c.send(Req::Schemas, Expect::Schemas);
                c.send(Req::Objects(schema.clone()), Expect::Objects(schema.clone()));
                c.requested.insert(format!("o:{schema}"));
                p.expanded.insert(format!("c{i}:{schema}"));
                p.expanded.insert(format!("g{i}:{schema}:Tables"));
                self.message = format!("{name} につなぎました（{product}、{schema}）");
                self.db_log_line(&format!("{name} につなぎました（{product}、{schema}）"), false);
                if let Some(sql) = self.db.as_mut().and_then(|p| p.then.remove(&i)) {
                    self.db_execute(i, sql);
                }
            }
            (Some(Expect::Connect), Reply::Error(e)) => {
                c.client = None;
                c.error = Some(e.clone());
                self.message = format!("{name} につなげません: {e}");
                self.db_log_line(&format!("{name} につなげません: {e}"), true);
            }
            (Some(Expect::Schemas), Reply::Schemas(s)) => c.schemas = Some(s),
            (Some(Expect::Objects(s)), Reply::Objects(o)) => {
                c.objects.insert(s, o);
            }
            (Some(Expect::Columns(s, t)), Reply::Columns(cols)) => {
                if let Some(te) = &mut self.db_table {
                    if te.src == i && te.schema == s && te.table == t {
                        te.set_columns(cols.clone());
                    }
                }
                c.columns.insert((s, t), cols);
            }
            (Some(Expect::Indexes(s, t)), Reply::Indexes(idx, keys)) => {
                c.indexes.insert((s, t), (idx, keys));
            }
            (Some(Expect::Query(target, sql, changes)), reply) => {
                let ok = !matches!(reply, Reply::Error(_));
                if ok && changes && c.manual && matches!(reply, Reply::Updated { .. }) {
                    c.pending += 1;
                }
                let outcome = match &reply {
                    Reply::Rows { rows, more, ms, .. } => format!("{} 行{} · {ms} ms", rows.len(), if *more { "（続きあり）" } else { "" }),
                    Reply::Updated { count, ms } => format!("{} · {ms} ms", if *count >= 0 { format!("{count} 行") } else { "実行しました".into() }),
                    Reply::Error(e) => e.clone(),
                    _ => String::new(),
                };
                self.db_log(&name, &sql, &outcome, !ok);
                if let Reply::Error(e) = &reply {
                    self.message = format!("SQL の誤り: {}", e.lines().next().unwrap_or(""));
                }
                // テーブルを作ったり消したりしたかもしれないので、木を読み直す
                if ok && changes {
                    if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
                        let s = c.schema.clone();
                        c.send(Req::Objects(s.clone()), Expect::Objects(s));
                    }
                }
                match target {
                    Target::Result(id) => self.db_results.reply(id, reply),
                    Target::Table => {
                        if let Some(te) = &mut self.db_table {
                            te.reply(reply);
                        }
                    }
                }
            }
            (Some(Expect::Count(target)), Reply::Count(n)) => match target {
                Target::Result(id) => self.db_results.set_total(id, n),
                Target::Table => {
                    if let Some(te) = &mut self.db_table {
                        te.total = Some(n);
                    }
                }
            },
            (Some(Expect::Count(_)), Reply::Error(_)) => {}
            (Some(Expect::Batch(n)), reply) => match &reply {
                Reply::Updated { ms, .. } => {
                    if c.manual {
                        c.pending += n;
                    }
                    let manual = c.manual;
                    self.message = format!("{n} 件の変更を送信しました{}", if manual { "（まだコミットしていません）" } else { "" });
                    self.db_log(&name, &format!("{n} 件の変更（テーブルの編集）"), &format!("{ms} ms"), false);
                    self.db_table_reload();
                }
                Reply::Error(e) => {
                    self.message = format!("送信できません: {}", e.lines().next().unwrap_or(""));
                    self.db_log(&name, &format!("{n} 件の変更（テーブルの編集）"), e, true);
                    if let Some(te) = &mut self.db_table {
                        te.busy = false;
                    }
                }
                _ => {}
            },
            (Some(Expect::Tx), Reply::Error(e)) | (Some(Expect::SetSchema(_)), Reply::Error(e)) | (Some(Expect::Ddl(_)), Reply::Error(e)) => {
                self.message = format!("{name}: {}", e.lines().next().unwrap_or(""));
                self.db_log_line(&format!("{name}: {e}"), true);
            }
            (Some(Expect::Tx), _) => {}
            (Some(Expect::SetSchema(s)), _) => {
                c.schema = s.clone();
                if !c.objects.contains_key(&s) {
                    c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
                }
                p.expanded.insert(format!("c{i}:{s}"));
                p.expanded.insert(format!("g{i}:{s}:Tables"));
                self.message = format!("スキーマを {s} にしました");
            }
            (Some(Expect::Ddl(t)), Reply::Ddl(text)) => self.db_open_text(&format!("{}.ddl.sql", safe_name(&t)), &text),
            (_, Reply::Error(e)) => {
                self.message = format!("{name}: {}", e.lines().next().unwrap_or(""));
            }
            _ => {}
        }
    }

    /// 出力のタブに 1 行書く
    pub(crate) fn db_log_line(&mut self, text: &str, error: bool) {
        self.db_results.log(String::new(), text.to_string(), String::new(), error);
    }

    fn db_log(&mut self, src: &str, sql: &str, outcome: &str, error: bool) {
        let first = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        self.db_results.log(src.to_string(), first, outcome.to_string(), error);
    }

    /// 文字をファイルに書いて開く（DDL・書き出し）
    pub(crate) fn db_open_text(&mut self, file: &str, text: &str) {
        let dir = work_dir().join("consoles").join("out");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(file);
        if let Err(e) = std::fs::write(&path, text) {
            self.message = format!("書けません: {e}");
            return;
        }
        self.edit_file(path.clone());
        self.mode = Mode::Normal;
        self.message = format!("{} に書きました", path.display());
    }

    /// SQL を流す（つないでいなければつないでから。自動のときは、データを変える文を確かめてから）
    pub(crate) fn db_execute(&mut self, i: usize, sql: String) {
        if sql.trim().is_empty() {
            self.message = "流す文がありません（カーソルを文の上に置いてください）".into();
            return;
        }
        let Some(p) = &mut self.db else { return };
        if !p.conns[i].connected {
            p.then.insert(i, sql);
            if p.conns[i].client.is_none() {
                self.focus_right(RightTab::Database);
                self.db_begin_connect(i);
            }
            return;
        }
        let changes = !is_read_only(&sql);
        if changes && !p.conns[i].manual && p.confirm.is_none() {
            let head: String = sql.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
            self.message = format!("データを変える文です（{head} …）。{} で実行しますか（y で実行。Tx を手動にすると確かめません）", p.sources[i].name);
            p.confirm = Some((i, sql));
            return;
        }
        self.db_run(i, sql);
    }

    fn db_run(&mut self, i: usize, sql: String) {
        let Some(p) = &self.db else { return };
        let changes = !is_read_only(&sql);
        let first = sql.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        let title = format!("{} · {first}", p.sources[i].name);
        let sf = p.kind(i) == Some(Kind::Salesforce);
        let id = self.db_results.new_tab(i, &title, &sql, !changes && !sf);
        let c = &mut self.db.as_mut().unwrap().conns[i];
        c.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset: 0 }, Expect::Query(Target::Result(id), sql, changes));
        self.show_results();
    }

    /// 結果のタブを、絞り込み・並べ替え・ページを変えて読み直す
    pub(crate) fn db_requery(&mut self, id: u64) {
        let Some(t) = self.db_results.tab(id) else { return };
        let (i, sql, count, offset) = (t.src, t.effective_sql(), t.count_sql(), t.offset);
        let Some(c) = self.db.as_mut().and_then(|p| p.conns.get_mut(i)) else { return };
        if !c.connected {
            self.message = "まだつないでいません".into();
            return;
        }
        c.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset }, Expect::Query(Target::Result(id), sql, false));
        if let Some(cs) = count {
            c.send(Req::Count(cs), Expect::Count(Target::Result(id)));
        }
    }

    /// 確認（ドライバーのダウンロード、データを変える文）の答え。扱ったら true
    pub(crate) fn db_confirm_key(&mut self, key: Key) -> bool {
        if let Some(i) = self.db.as_mut().and_then(|p| p.setup_ask.take()) {
            if key == Key::Char('y') {
                let waker = self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}));
                let p = self.db.as_mut().unwrap();
                p.download = Some((i, drivers::download_oracle(waker), "ダウンロードを始めます…".into()));
                p.expanded.insert(format!("s{i}"));
                p.rebuild();
                self.message = "Oracle のドライバーをダウンロードしています…".into();
            } else {
                self.message = "やめました".into();
            }
            return true;
        }
        let Some(p) = &mut self.db else { return false };
        let Some((i, sql)) = p.confirm.take() else { return false };
        if key == Key::Char('y') {
            self.db_run(i, sql);
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

    /// コンソールでカーソルのある文の行の範囲
    pub fn console_statement(&self) -> Option<(usize, usize)> {
        self.console_source()?;
        let lo = self.cursor.line.saturating_sub(200);
        let hi = (self.cursor.line + 200).min(self.buf.line_count());
        let lines: Vec<String> = (lo..hi).map(|l| self.buf.line(l)).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        statement_lines(&refs, self.cursor.line - lo).map(|(s, e)| (s + lo, e + lo))
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
            match self.console_statement() {
                Some((s, e)) => (s..e).map(|l| self.buf.line(l)).filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n").trim().to_string(),
                None => String::new(),
            }
        };
        if self.mode == Mode::Insert {
            self.key(Key::Esc);
        }
        self.db_execute(i, sql);
    }

    /// Tx の自動・手動を切り替える
    pub(crate) fn db_toggle_tx(&mut self, i: usize) {
        if self.db.as_ref().and_then(|p| p.kind(i)) == Some(Kind::Salesforce) {
            self.message = "Salesforce にトランザクションはありません".into();
            return;
        }
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.message = "まだつないでいません".into();
            return;
        }
        if c.manual && c.pending > 0 {
            self.message = format!("まだコミットしていない変更が {} 件あります。先にコミットかロールバックを", c.pending);
            return;
        }
        c.manual = !c.manual;
        let m = c.manual;
        c.send(Req::AutoCommit(!m), Expect::Tx);
        self.message = if m { "Tx: 手動（コミットするまで確定しません）".into() } else { "Tx: 自動".into() };
        self.db_log_line(&format!("トランザクション: {}", if m { "手動" } else { "自動" }), false);
    }

    pub(crate) fn db_commit(&mut self, i: usize, commit: bool) {
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            return;
        }
        let n = c.pending;
        c.pending = 0;
        c.send(if commit { Req::Commit } else { Req::Rollback }, Expect::Tx);
        let what = if commit { "コミット" } else { "ロールバック" };
        self.message = format!("{what}しました（{n} 件）");
        self.db_log_line(&format!("{what}しました（{n} 件）"), false);
    }

    /// 今の接続先（コンソールの接続先、無ければ木で選んでいる接続先）
    fn db_current_source(&self) -> Option<usize> {
        self.console_source().or_else(|| self.db.as_ref().and_then(|p| p.rows.get(p.selected)).map(|r| r.source()))
    }

    /// `:DbLogout`: Salesforce の保存したログインを消して切る
    pub(crate) fn db_logout(&mut self) {
        let Some(i) = self.db_current_source() else {
            self.message = "接続先がありません".into();
            return;
        };
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        if p.kind(i) != Some(Kind::Salesforce) {
            self.message = "ログアウトは Salesforce の接続先だけです".into();
            return;
        }
        let had = nagi_workers::salesforce::forget(&s.url, &s.user);
        p.conns[i] = Conn::default();
        p.rebuild();
        self.message = if had { format!("{} の保存したログインを消しました", s.name) } else { "保存したログインはありません".into() };
    }

    /// `:DbSchema 名前`
    pub(crate) fn db_set_schema(&mut self, s: &str) {
        let Some(i) = self.db_current_source() else {
            self.message = "接続先がありません".into();
            return;
        };
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.message = "まだつないでいません".into();
            return;
        }
        let s = s.trim().to_string();
        c.send(Req::SetSchema(s.clone()), Expect::SetSchema(s));
    }

    /// スキーマの候補（:DbSchema の補完）
    pub fn db_schema_names(&self) -> Vec<String> {
        self.db_current_source().and_then(|i| self.db.as_ref()?.conns.get(i)?.schemas.clone()).unwrap_or_default()
    }

    /// `:DbCommit` / `:DbRollback` / `:DbTx`
    pub(crate) fn db_tx_command(&mut self, what: &str) {
        let Some(i) = self.db_current_source() else {
            self.message = "接続先がありません".into();
            return;
        };
        match what {
            "commit" => self.db_commit(i, true),
            "rollback" => self.db_commit(i, false),
            _ => self.db_toggle_tx(i),
        }
    }

    /// 接続先の SQL コンソールを開く
    pub(crate) fn open_console(&mut self, i: usize, initial: Option<String>) {
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        let dir = work_dir().join("consoles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.sql", safe_name(&s.name)));
        if !path.exists() {
            let _ = std::fs::write(&path, format!("-- {}  {}\n-- Ctrl+Enter でカーソルのある文（空行で区切る）を実行。Visual なら選んだ範囲\n\n", s.name, s.url));
        }
        let abs = std::path::absolute(&path).unwrap_or(path.clone());
        p.consoles.insert(abs, i);
        self.edit_file(path);
        self.mode = Mode::Normal;
        if let Some(sql) = initial {
            let end = self.buf.len_bytes();
            let text = self.buf.text();
            let nl = if text.is_empty() || text.ends_with("\n\n") { "" } else if text.ends_with('\n') { "\n" } else { "\n\n" };
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

    /// Enter: 接続先はつなぐか開け閉め、ほかは開け閉め
    fn db_activate(&mut self) {
        let Some(r) = self.db_selected() else { return };
        if let DbRow::Source(i) = r {
            let p = self.db.as_ref().unwrap();
            if p.conns[i].client.is_none() && p.download.is_none() {
                self.db_begin_connect(i);
                return;
            }
        }
        self.db_toggle_row(&r);
    }

    /// o: テーブル・ビューのデータを開く
    fn db_open_selected(&mut self) {
        match self.db_selected() {
            // Salesforce は表を直さないので、SOQL で先頭の 200 件を出す
            Some(DbRow::Object(i, _, t, Group::Tables | Group::Views, _)) if self.db.as_ref().and_then(|p| p.kind(i)) == Some(Kind::Salesforce) => {
                self.db_execute(i, format!("SELECT FIELDS(STANDARD) FROM {t} LIMIT 200"));
                self.mode = Mode::List;
                self.list_side = crate::editor::Side::Right;
            }
            Some(DbRow::Object(i, s, t, Group::Tables | Group::Views, _)) => self.db_open_table(i, &s, &t),
            Some(DbRow::Source(_)) | Some(DbRow::Schema(..)) => self.db_activate(),
            _ => {}
        }
    }

    /// D: DDL を開く
    fn db_ddl(&mut self) {
        if let Some(DbRow::Object(i, s, t, g, _)) = self.db_selected() {
            if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
                let kind = if g == Group::Views { "VIEW" } else { "TABLE" };
                c.send(Req::Ddl(s.clone(), t.clone(), kind.into()), Expect::Ddl(format!("{s}.{t}")));
            }
        }
    }

    /// 入力欄（絞り込み・フォーム）に IME の確定を入れる。入れたら true
    pub(crate) fn db_insert(&mut self, text: &str) -> bool {
        let Some(p) = &mut self.db else { return false };
        if let Some(f) = &mut p.form {
            if f.at > 0 {
                f.fields[f.at].1.insert(text);
            }
            return true;
        }
        if p.filtering {
            p.filter.insert(text);
            p.rebuild();
            return true;
        }
        false
    }

    /// IME を使う入力欄にいるか（パスワードでは使わない）
    pub fn db_typing(&self) -> bool {
        self.right_focused() && self.db.as_ref().is_some_and(|p| p.form.as_ref().is_some_and(|f| f.at > 0) || p.filtering)
    }

    pub(crate) fn db_key(&mut self, key: Key) -> Effect {
        let half = (self.windows_rows() / 2).max(1);
        let Some(p) = &mut self.db else {
            self.mode = Mode::Normal;
            return Effect::None;
        };
        // パスワードの入力中
        if let Some((i, prompt)) = &mut p.password {
            match key {
                Key::Esc => {
                    p.password = None;
                    self.message = "やめました".into();
                }
                Key::Enter => {
                    let (i, pass) = (*i, std::mem::take(&mut prompt.text));
                    p.password = None;
                    self.db_connect(i, pass);
                }
                k => {
                    prompt.key(k);
                }
            }
            return Effect::None;
        }
        // 接続先を足すフォーム
        if let Some(f) = &mut p.form {
            match key {
                Key::Esc => {
                    p.form = None;
                    self.message = "やめました".into();
                }
                Key::Tab | Key::Down => f.at = (f.at + 1).min(f.fields.len() - 1),
                Key::Up => f.at = f.at.saturating_sub(1),
                Key::Left | Key::Right | Key::Char(' ') if f.at == 0 => {
                    let order = [Kind::Oracle, Kind::Sqlite, Kind::Salesforce];
                    let k = order.iter().position(|k| *k == f.kind).unwrap_or(0);
                    let k = if key == Key::Left { (k + 2) % 3 } else { (k + 1) % 3 };
                    f.set_kind(order[k]);
                }
                Key::Enter if f.at + 1 < f.fields.len() => f.at += 1,
                Key::Enter => match f.source() {
                    Ok(ds) => {
                        p.form = None;
                        self.db_push_source(ds, true);
                        let i = self.db.as_ref().unwrap().sources.len() - 1;
                        self.db_begin_connect(i);
                    }
                    Err(e) => self.message = e,
                },
                k if f.at > 0 => {
                    f.fields[f.at].1.key(k);
                }
                _ => {}
            }
            return Effect::None;
        }
        // 絞り込みの入力中
        if p.filtering {
            match key {
                Key::Esc => {
                    p.filtering = false;
                    p.filter = Prompt::default();
                    p.rebuild();
                }
                Key::Enter => p.filtering = false,
                k => {
                    if p.filter.key(k) {
                        p.rebuild();
                        p.selected = p.rows.iter().position(|r| matches!(r, DbRow::Object(..))).unwrap_or(p.selected);
                    }
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
            Key::Char('/') => {
                p.filtering = true;
                self.message = "絞り込み: 名前の一部を打つ（Enter で決める・Esc で消す）".into();
            }
            Key::Enter | Key::Char('l') | Key::Right => self.db_activate(),
            Key::Char('h') | Key::Left => {
                let k = p.selected;
                let r = p.rows.get(k).cloned();
                match r {
                    Some(r) if r.key().is_some() && p.is_open(&r) => self.db_toggle_row(&r),
                    Some(_) => {
                        let d = p.depth(k);
                        if let Some(j) = (0..k).rev().find(|&j| p.depth(j) < d) {
                            p.selected = j;
                        }
                    }
                    None => {}
                }
            }
            Key::Char('o') => self.db_open_selected(),
            Key::Char('a') => {
                p.form = Some(Form::new(Kind::Oracle));
                self.message = "接続先を足す: Tab で次の欄、種類は ← →、最後の欄で Enter で保存してつなぐ".into();
            }
            Key::Char('D') => self.db_ddl(),
            // コンソール（テーブルの上なら SELECT を書き足して）
            Key::Char('c') => {
                if let Some(r) = self.db_selected() {
                    let sf = self.db.as_ref().and_then(|p| p.kind(r.source())) == Some(Kind::Salesforce);
                    let init = match &r {
                        DbRow::Object(_, _, t, Group::Tables | Group::Views, _) if sf => Some(format!("SELECT Id, Name FROM {t} LIMIT 100")),
                        DbRow::Object(_, s, t, Group::Tables | Group::Views, _) => Some(format!("SELECT * FROM {};", qualified(s, t))),
                        _ => None,
                    };
                    self.open_console(r.source(), init);
                }
            }
            // 切る
            Key::Char('x') => {
                if let Some(r) = self.db_selected() {
                    let i = r.source();
                    let p = self.db.as_mut().unwrap();
                    if p.conns[i].pending > 0 {
                        self.message = format!("まだコミットしていない変更が {} 件あります。先にコミットかロールバックを", p.conns[i].pending);
                    } else {
                        p.conns[i] = Conn::default();
                        let pre = format!("s{i}");
                        p.expanded.remove(&pre);
                        p.rebuild();
                        p.selected = p.rows.iter().position(|r| *r == DbRow::Source(i)).unwrap_or(0);
                        self.message = format!("{} を切りました", p.sources[i].name);
                    }
                }
            }
            // 読み直す
            Key::Char('r') => {
                if let Some(r) = self.db_selected() {
                    let i = r.source();
                    let c = &mut self.db.as_mut().unwrap().conns[i];
                    if c.connected {
                        c.objects.clear();
                        c.columns.clear();
                        c.indexes.clear();
                        c.requested.clear();
                        let s = c.schema.clone();
                        c.send(Req::Schemas, Expect::Schemas);
                        c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
                        c.requested.insert(format!("o:{s}"));
                        self.message = "読み直しています…".into();
                    }
                    self.db.as_mut().unwrap().rebuild();
                }
            }
            Key::Char('q') => {
                if self.db.as_ref().is_some_and(|p| p.conns.iter().any(|c| c.pending > 0)) {
                    self.message = "まだコミットしていない変更があります。先にコミットかロールバックを".into();
                } else {
                    self.db = None;
                    self.mode = Mode::Normal;
                    self.rows = self.text_rows(self.active_rect());
                }
            }
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => {
                self.enter_command("");
            }
            _ => {}
        }
        Effect::None
    }

    pub(crate) fn db_click(&mut self, row: usize, clicks: u32) {
        let Some(p) = &mut self.db else { return };
        // 1 行目はタブ、2 行目は絞り込みの欄
        if row == 1 {
            p.filtering = true;
            self.focus_right(RightTab::Database);
            return;
        }
        let i = p.scroll + row.saturating_sub(2);
        if i >= p.rows.len() {
            return;
        }
        p.selected = i;
        self.focus_right(RightTab::Database);
        match self.db_selected() {
            Some(DbRow::Object(_, _, _, Group::Tables | Group::Views, _)) if clicks >= 2 => self.db_open_selected(),
            Some(r) if clicks >= 2 || (r.key().is_some() && !matches!(r, DbRow::Source(_) | DbRow::Object(..))) => self.db_activate(),
            _ => {}
        }
    }

    /// コンソールの上の帯のボタン（見出し, 横の位置, 幅, 何をするか）。帯の左端から数える
    pub fn console_bar(&self, w: usize) -> Vec<(String, usize, usize, BarItem)> {
        let Some(i) = self.console_source() else { return Vec::new() };
        let Some(p) = &self.db else { return Vec::new() };
        let c = &p.conns[i];
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        let mut v = Vec::new();
        // 左端の「出す」ボタン（»）と重ならないように 2 列あける
        let mut x = 2;
        let mut push = |v: &mut Vec<(String, usize, usize, BarItem)>, s: String, item: BarItem| {
            let n = width(&s);
            v.push((s, x, n, item));
            x += n + 2;
        };
        push(&mut v, "▶ 実行".into(), BarItem::Run);
        push(&mut v, "Ctrl+Enter".into(), BarItem::Label);
        if p.kind(i) != Some(Kind::Salesforce) {
            push(&mut v, format!("Tx: {} ▾", if c.manual { "手動" } else { "自動" }), BarItem::Tx);
        }
        if c.manual {
            push(&mut v, format!("未コミット {} 件", c.pending), BarItem::Pending);
            push(&mut v, "✓ コミット".into(), BarItem::Commit);
            push(&mut v, "↺ ロールバック".into(), BarItem::Rollback);
        }
        let right = format!("{} › {} ▾", p.sources[i].name, if c.schema.is_empty() { "…" } else { &c.schema });
        let n = width(&right);
        v.push((right, w.saturating_sub(n + 1), n, BarItem::Schema));
        v
    }

    /// 帯のボタンを押す
    pub(crate) fn console_bar_press(&mut self, item: BarItem) {
        let Some(i) = self.console_source() else { return };
        match item {
            BarItem::Run => self.db_run_at_cursor(),
            BarItem::Tx => self.db_toggle_tx(i),
            BarItem::Commit => self.db_commit(i, true),
            BarItem::Rollback => self.db_commit(i, false),
            BarItem::Schema => {
                self.enter_command("");
                self.cmdline.set("DbSchema ");
                self.update_cmd_complete();
            }
            BarItem::Label | BarItem::Pending => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarItem {
    Run,
    Label,
    Tx,
    Pending,
    Commit,
    Rollback,
    Schema,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn statements() {
        let text = "-- head\n\nselect 1\nfrom dual;\n\n-- note\nupdate t set a = 1\n";
        assert_eq!(statement_at(text, 2), "select 1\nfrom dual;");
        assert_eq!(statement_at(text, 3), "select 1\nfrom dual;");
        assert_eq!(statement_at(text, 6), "update t set a = 1");
        assert_eq!(statement_at(text, 5), "update t set a = 1");
        assert_eq!(statement_at(text, 1), "");
        assert!(is_read_only("-- x\n  select * from t"));
        assert!(!is_read_only("update t set a=1"));
        assert_eq!(ident("ORDERS"), "ORDERS");
        assert_eq!(ident("order items"), "\"order items\"");
        assert_eq!(ident("Mixed"), "\"Mixed\"");
        assert_eq!(literal(&Some("O'Neil".into())), "'O''Neil'");
        assert_eq!(literal(&None), "NULL");
        let f = Form::new(Kind::Oracle);
        assert!(f.source().is_err());
    }

    fn keys(e: &mut Editor, k: &str) {
        for key in nagi_vim::parse_keys(k, " ") {
            e.key(key);
        }
    }

    pub(crate) fn wait(e: &mut Editor, what: &dyn Fn(&Editor) -> bool) {
        for _ in 0..2000 {
            e.poll_db();
            if what(e) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("待ちきれない: {}", e.message);
    }

    /// SQLite で: フォームで足す → つなぐ → コンソールで表を作る（確認あり）→ 手動 Tx → 木 → 絞り込み
    #[test]
    fn sqlite_flow() {
        let dir = std::env::temp_dir().join(format!("nagi-dbui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: テストの中だけで使う
        unsafe {
            std::env::set_var("NAGI_DB_DIR", &dir);
            std::env::set_var("USERPROFILE", &dir);
        }
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        // a → 種類を SQLite に → 名前 → ファイル → Enter で保存してつなぐ
        keys(&mut e, "a");
        assert!(e.db.as_ref().unwrap().form.is_some());
        keys(&mut e, "<Up><Right><Down>shop<Tab>");
        let file = dir.join("shop.db");
        for c in file.display().to_string().chars() {
            e.key(Key::Char(c));
        }
        keys(&mut e, "<CR>");
        assert!(std::fs::read_to_string(dir.join(".config/nagi/databases.txt")).unwrap().contains("shop | jdbc:sqlite:"));
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.contains_key("main"));
        // コンソール: CREATE は確かめてから
        keys(&mut e, "c");
        assert_eq!(e.console_source(), Some(i));
        keys(&mut e, "Gocreate table item(id integer primary key, name text)<Esc><C-m>");
        assert!(e.message.contains("データを変える文"), "{}", e.message);
        keys(&mut e, "y");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        // 手動 Tx では確かめずに流し、未コミットを数える。ロールバックで消える
        e.db_toggle_tx(i);
        keys(&mut e, "o<CR>insert into item values (1, 'りんご')<Esc><C-m>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].pending == 1);
        let bar: Vec<String> = e.console_bar(120).into_iter().map(|b| b.0).collect();
        assert!(bar.contains(&"未コミット 1 件".to_string()), "{bar:?}");
        e.db_commit(i, false);
        keys(&mut e, "o<CR>select count(*) as n from item<Esc><C-m>");
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running && !t.rows.is_empty()));
        assert_eq!(e.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("0"), "ロールバックで消えた");
        // 出力のタブに記録が残る
        assert!(e.db_results.log.iter().any(|l| l.text.contains("insert into item")));
        // 木: テーブル（1）› item
        let p = e.db.as_ref().unwrap();
        assert!(p.rows.iter().any(|r| matches!(r, DbRow::Group(_, _, Group::Tables, 1))), "{:?}", p.rows);
        assert!(p.rows.iter().any(|r| matches!(r, DbRow::Object(_, _, t, _, _) if t == "item")));
        // 絞り込み
        e.focus_right(RightTab::Database);
        keys(&mut e, "/zzz<CR>");
        assert!(!e.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
        keys(&mut e, "/<Esc>");
        assert!(e.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
    }
}
