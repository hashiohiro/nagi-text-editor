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
use nagi_workers::drivers::{self, Download};

use crate::editor::{Editor, Effect, State, Prompt};
use crate::right_panel::RightTab;
use crate::vim_field::{FieldOut, VimField};

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
    Relations(String, String),
    Indexes(String, String),
    /// (返す先, 出力に書く SQL, データを変えうる文か)
    Query(Target, String, bool),
    Count(Target),
    /// まとめて送った (返す先, 文の数)
    Batch(Target, usize),
    Tx,
    /// (コミットか, 変更の数)
    Commit(bool, usize),
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
    /// 子のリレーション（Salesforce）
    pub relations: HashMap<(String, String), Vec<(String, String)>>,
    pub indexes: HashMap<(String, String), (Vec<Index>, Vec<DbKey>)>,
    pub error: Option<String>,
    expect: VecDeque<Expect>,
    requested: HashSet<String>,
    /// 手動のトランザクション
    pub manual: bool,
    /// 手動のときの、まだコミットしていない文の数
    pub pending: usize,
    /// つなぎ中のパスワードと、保存したものを使ったか（つながったら保存する。保存したもので失敗したら聞き直す）
    pass: Option<(String, bool)>,
}

impl Conn {
    fn send(&mut self, req: Req, e: Expect) {
        if let Some(c) = &self.client {
            c.send(req);
            self.expect.push_back(e);
        }
    }

    pub(crate) fn send_batch(&mut self, target: Target, stmts: Vec<String>) {
        let n = stmts.len();
        self.send(Req::Batch(stmts), Expect::Batch(target, n));
    }

    pub(crate) fn send_table_query(&mut self, sql: String, offset: usize, count: Option<String>) {
        self.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset }, Expect::Query(Target::Table, sql, false));
        if let Some(c) = count {
            self.send(Req::Count(c), Expect::Count(Target::Table));
        }
    }

    /// 子のリレーションがまだ無ければ読みに行く
    pub(crate) fn ensure_relations(&mut self, s: &str, t: &str) {
        if self.connected && !self.relations.contains_key(&(s.to_string(), t.to_string())) && self.requested.insert(format!("r:{s}.{t}")) {
            self.send(Req::Relations(s.into(), t.into()), Expect::Relations(s.into(), t.into()));
        }
    }

    /// 列がまだ無ければ読みに行く
    pub(crate) fn ensure_columns(&mut self, s: &str, t: &str) {
        if self.connected && !self.columns.contains_key(&(s.to_string(), t.to_string())) && self.requested.insert(format!("c:{s}.{t}")) {
            self.send_columns(s, t);
        }
    }

    /// スキーマのテーブル・ビューなどを読む（まだ読んでいなければ）
    pub(crate) fn ensure_objects(&mut self, s: &str) {
        if self.connected && !s.is_empty() && !self.objects.contains_key(s) && self.requested.insert(format!("o:{s}")) {
            self.send(Req::Objects(s.to_string()), Expect::Objects(s.to_string()));
        }
    }

    /// 書かれた (スキーマ, 名前) を、覚えている (スキーマ, テーブル) にする（大文字小文字は区別しない。スキーマが無ければ今のスキーマ）
    pub(crate) fn resolve(&self, schema: Option<&str>, name: &str) -> (String, String) {
        let s = schema.unwrap_or(&self.schema);
        let s = self.objects.keys().find(|k| k.eq_ignore_ascii_case(s)).cloned().or_else(|| self.schemas.as_ref().and_then(|v| v.iter().find(|k| k.eq_ignore_ascii_case(s)).cloned())).unwrap_or_else(|| s.to_string());
        let t = self.objects.get(&s).and_then(|o| o.tables.iter().find(|t| t.0.eq_ignore_ascii_case(name)).map(|t| t.0.clone())).unwrap_or_else(|| name.to_string());
        (s, t)
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
    /// (見出し, 入力欄)。0 番は種類（← → で切り替え）。入力欄は Vim 風
    pub fields: Vec<(&'static str, VimField)>,
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
        let p = |s: &str| VimField::new(s, false);
        let name = self.fields.get(1).map(|f| f.1.text()).unwrap_or_default();
        self.fields = match kind {
            Kind::Oracle => vec![("種類", p("Oracle")), ("名前", p(&name)), ("ホスト", p("localhost")), ("ポート", p("1521")), ("サービス名", p("")), ("利用者", p(""))],
            Kind::Sqlite => vec![("種類", p("SQLite")), ("名前", p(&name)), ("ファイル", p(""))],
            Kind::Postgres => vec![("種類", p("PostgreSQL")), ("名前", p(&name)), ("ホスト", p("localhost")), ("ポート", p("5432")), ("データベース", p("postgres")), ("利用者", p(""))],
            Kind::Salesforce => vec![("種類", p("Salesforce")), ("名前", p(&name)), ("ログイン URL", p("https://login.salesforce.com")), ("利用者", p(""))],
        };
        self.at = self.at.min(self.fields.len() - 1);
    }

    fn get(&self, label: &str) -> String {
        self.fields.iter().find(|f| f.0 == label).map_or(String::new(), |f| f.1.text().trim().to_string())
    }

    /// 入力から接続先を作る
    pub fn source(&self) -> nagi_core::Result<DataSource> {
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
            Kind::Postgres => {
                let (host, port, db) = (self.get("ホスト"), self.get("ポート"), self.get("データベース"));
                if host.is_empty() || db.is_empty() {
                    return Err("ホストとデータベースを入れてください".into());
                }
                (format!("jdbc:postgresql://{host}:{}/{db}", if port.is_empty() { "5432".into() } else { port }), self.get("利用者"))
            }
            Kind::Salesforce => {
                let u = self.get("ログイン URL");
                (format!("salesforce:{}", if u.is_empty() { "https://login.salesforce.com".into() } else { u }), self.get("利用者"))
            }
        };
        Ok(DataSource { name, url, user, origin: config_path().display().to_string(), schema: String::new() })
    }
}

pub struct DbPanel {
    pub sources: Vec<DataSource>,
    pub conns: Vec<Conn>,
    pub expanded: HashSet<String>,
    pub rows: Vec<DbRow>,
    pub selected: usize,
    pub scroll: usize,
    /// 名前の絞り込み（決めた文字）と、打っている間の Vim 風の入力欄
    pub filter_text: String,
    pub filter: Option<VimField>,
    /// パスワードの入力（接続先, 入力欄）
    pub password: Option<(usize, Prompt)>,
    /// ドライバーのダウンロードの確認（接続先）
    pub setup_ask: Option<usize>,
    /// ダウンロード中（接続先, 進み具合の文）
    pub download: Option<(usize, Download, String)>,
    pub form: Option<Form>,
    /// コンソールのファイル → 接続先
    pub consoles: HashMap<PathBuf, usize>,
    /// つながったら流す SQL（接続先 → SQL）
    pub then: HashMap<usize, String>,
}

pub fn config_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    home.join(".config").join("nagi").join("databases.txt")
}

/// テーブル `q`（スキーマ付きの名前）の SELECT・INSERT・UPDATE・DELETE を、列から作る。
/// 値は NULL を置き（列の名前を注釈に）、UPDATE・DELETE の条件は主キー（無ければ注釈だけ）
pub fn generate_sql(d: &nagi_lang::sql::Dialect, what: &str, q: &str, cols: &[Column]) -> String {
    let names: Vec<String> = cols.iter().map(|c| d.ident(&c.name)).collect();
    let pk: Vec<String> = cols.iter().filter(|c| c.pk).map(|c| format!("{} = NULL", d.ident(&c.name))).collect();
    let cond = if pk.is_empty() { "/* 条件 */".to_string() } else { pk.join("\n  AND ") };
    match what {
        "insert" => format!("INSERT INTO {q} ({})\nVALUES ({})", names.join(", "), names.iter().map(|n| format!("/* {n} */ NULL")).collect::<Vec<_>>().join(", ")),
        "update" => {
            let set: Vec<String> = cols.iter().filter(|c| !c.pk).map(|c| format!("{} = NULL", d.ident(&c.name))).collect();
            format!("UPDATE {q}\nSET {}\nWHERE {cond}", set.join(",\n    "))
        }
        "delete" => format!("DELETE FROM {q}\nWHERE {cond}"),
        _ if names.is_empty() => format!("SELECT *\nFROM {q}"),
        _ => format!("SELECT {}\nFROM {q}", names.join(",\n       ")),
    }
}

/// 設定ファイルの 1 行（`名前 | URL | 利用者` と、既定のスキーマがあれば ` | スキーマ`）
pub fn config_line(ds: &DataSource) -> String {
    if ds.schema.is_empty() { format!("{} | {} | {}\n", ds.name, ds.url, ds.user) } else { format!("{} | {} | {} | {}\n", ds.name, ds.url, ds.user, ds.schema) }
}

/// 設定ファイルの、その接続先（名前・URL・利用者が同じ行）を書き直す。無ければ足す（Spring の設定から見つけた接続先など）。
/// ほかの行（注釈・空行も）はそのまま
pub fn save_config_line(path: &std::path::Path, ds: &DataSource) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let same = !line.trim_start().starts_with('#')
            && nagi_workers::db::config_sources(line, "").first().is_some_and(|x| x.name == ds.name && x.url == ds.url && x.user == ds.user);
        if same && !found {
            out.push_str(&config_line(ds));
            found = true;
        } else {
            out.push_str(line);
        }
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&config_line(ds));
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, out)
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

pub use nagi_lang::sql::ident;

/// 値の書き方（NULL か '...'）
pub fn literal(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

/// スキーマ付きの名前（方言 `d` で引用符が要れば囲む）
pub fn qualified(d: &nagi_lang::sql::Dialect, schema: &str, name: &str) -> String {
    if schema.is_empty() { d.ident(name) } else { format!("{}.{}", d.ident(schema), d.ident(name)) }
}

impl DbPanel {
    pub fn kind(&self, i: usize) -> Option<Kind> {
        Kind::of(&self.sources.get(i)?.url)
    }

    pub fn is_open(&self, r: &DbRow) -> bool {
        r.key().is_some_and(|k| self.expanded.contains(&k)) || (!self.filter_text.is_empty() && matches!(r, DbRow::Group(..)))
    }

    fn matches(&self, name: &str) -> bool {
        self.filter_text.is_empty() || name.to_lowercase().contains(&self.filter_text.to_lowercase())
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
            if names.is_empty() && (g != Group::Tables || !self.filter_text.is_empty()) {
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
        !self.filter_text.is_empty() && self.conns[i].columns.get(&(s.to_string(), t.to_string())).is_some_and(|cs| cs.iter().any(|c| self.matches(&c.name)))
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
            DbRow::Schema(i, s, d) => (*d, ' ', s.clone(), if self.sources[*i].schema.eq_ignore_ascii_case(s) { "スキーマ（既定）".into() } else { "スキーマ".into() }),
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
                    match sources.iter_mut().find(|x| x.url == s.url && x.user == s.user) {
                        // Spring の設定から見つけた接続先と同じなら、設定ファイルの既定のスキーマだけを引き継ぐ
                        Some(x) => x.schema = s.schema,
                        None => sources.push(s),
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
                filter_text: String::new(),
                filter: None,
                password: None,
                setup_ask: None,
                download: None,
                form: None,
                consoles: HashMap::new(),
                then: HashMap::new(),
            };
            p.rebuild();
            self.db = Some(p);
        }
        if self.db.as_ref().is_some_and(|p| p.sources.is_empty()) {
            self.notify.info("接続先がありません。a（＋）で足せます");
        }
        self.focus_right(RightTab::Database);
    }

    /// `:DbAdd`（引数なしならフォーム、`名前 jdbc:URL [利用者]` ならそのまま足す）
    pub(crate) fn db_add(&mut self, arg: &str) {
        self.open_db_panel();
        let f: Vec<&str> = arg.split_whitespace().collect();
        let Some(p) = &mut self.db else { return };
        let (Some(name), Some(url)) = (f.first(), f.get(1).filter(|u| Kind::of(u).is_some())) else {
            p.form = Some(Form::new(Kind::Oracle));
            self.notify.info("接続先を足す: Tab で次の欄、種類は ← →、最後の欄で Enter で保存してつなぐ");
            return;
        };
        let ds = DataSource { name: name.to_string(), url: url.to_string(), user: f.get(2).unwrap_or(&"").to_string(), origin: ":DbAdd".into(), schema: String::new() };
        self.db_push_source(ds, false);
    }

    fn db_push_source(&mut self, ds: DataSource, save: bool) {
        if self.db.is_none() {
            return;
        }
        if save {
            let cfg = config_path();
            if let Some(dir) = cfg.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&config_line(&ds));
            match std::fs::write(&cfg, text) {
                Ok(()) => self.notify.info(format!("{} を {} に保存しました", ds.name, cfg.display())),
                Err(e) => self.notify.error(format!("保存できません: {e}")),
            }
        } else {
            self.notify.info(format!("接続先 {} を足しました", ds.name));
        }
        let Some(p) = &mut self.db else { return };
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
            // 何を・どこへ・どの条件で置くかは右の欄に出し、確認の窓で答える
            p.setup_ask = Some(i);
            let q = format!("Oracle のドライバー（約 {} MB）をダウンロードしますか", drivers::ORACLE_CLIENT_SIZE_MB);
            self.ask_or(q, move |e| e.db_download_driver(i), |e| {
                if let Some(p) = &mut e.db {
                    p.setup_ask = None;
                }
                e.notify.info("やめました");
            });
            return;
        }
        if matches!(Kind::of(&s.url), Some(Kind::Sqlite | Kind::Salesforce)) {
            self.db_connect(i, String::new());
            return;
        }
        if let Some(pass) = nagi_workers::db::saved_password(&s.url, &s.user) {
            self.db_connect_with(i, pass, true);
            return;
        }
        p.password = Some((i, Prompt::default()));
        self.notify.info(format!("{} のパスワード（{}。Enter でつなぐ・Esc でやめる）", s.name, s.url));
    }

    fn db_connect(&mut self, i: usize, pass: String) {
        self.db_connect_with(i, pass, false);
    }

    /// saved: 保存したパスワードか（違えば、つながったときに保存する）
    fn db_connect_with(&mut self, i: usize, pass: String, saved: bool) {
        let waker = self.waker();
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        p.conns[i] = Conn::default();
        p.expanded.insert(format!("s{i}"));
        let msg = match Client::connect(&s.url, &s.user, &pass, waker) {
            Ok(cl) => {
                p.conns[i].client = Some(cl);
                p.conns[i].expect = VecDeque::from([Expect::Connect]);
                p.conns[i].pass = Some((pass, saved));
                Ok(format!("{} につないでいます…", s.name))
            }
            Err(e) => {
                p.conns[i].error = Some(e.to_string());
                Err(e)
            }
        };
        p.rebuild();
        match msg {
            Ok(m) => self.notify.info(m),
            Err(e) => self.notify.error(e),
        }
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
        let dl = self.db.as_ref().and_then(|p| p.download.as_ref().map(|(i, d, _)| (*i, d.poll())));
        if let Some((i, polled)) = dl.filter(|(_, x)| x.status.is_some() || x.done.is_some()) {
            changed = true;
            let p = self.db.as_mut().unwrap();
            if let (Some(text), Some(d)) = (polled.status, &mut p.download) {
                d.2 = text;
            }
            if let Some(r) = polled.done {
                p.download = None;
                match r {
                    Ok(dir) => {
                        self.notify.info(format!("Oracle のドライバーを {} に置きました", dir.display()));
                        self.db_log_line(&format!("ドライバー: Oracle Instant Client を {} に置きました", dir.display()), false);
                        self.db_begin_connect(i);
                    }
                    Err(e) => {
                        p.conns[i].error = Some(e.to_string());
                        self.notify.error(e);
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
        // テーブル・列が届いたら、出している SQL の補完の一覧を作り直す
        let meta = matches!(r, Reply::Objects(_) | Reply::Columns(_) | Reply::Relations(_));
        self.db_reply_inner(i, r);
        if meta {
            self.sql_metadata_arrived(i);
        }
    }

    fn db_reply_inner(&mut self, i: usize, r: Reply) {
        // 途中の知らせは、頼みごとの答えではない
        if let Reply::Note(s) = r {
            self.notify.info(s.clone());
            self.db_log_line(&s, false);
            return;
        }
        if let Reply::Reconnected { tx_lost } = r {
            self.db_reconnected(i, tx_lost);
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
                // 既定のスキーマがあれば切り替える（SQLite・Salesforce はスキーマを切り替えない）
                let default = p.sources[i].schema.clone();
                let switch = !default.is_empty() && !default.eq_ignore_ascii_case(&schema) && !matches!(Kind::of(&p.sources[i].url), Some(Kind::Sqlite | Kind::Salesforce));
                if switch {
                    c.send(Req::SetSchema(default.clone()), Expect::SetSchema(default));
                }
                p.expanded.insert(format!("c{i}:{schema}"));
                p.expanded.insert(format!("g{i}:{schema}:Tables"));
                // 入れたパスワードは保存する（次から聞かない。:DbLogout で消せる）
                let mut saved = String::new();
                if let Some((pass, false)) = c.pass.take().filter(|(x, _)| !x.is_empty()) {
                    let s = &p.sources[i];
                    saved = match nagi_workers::db::save_password(&s.url, &s.user, &pass) {
                        Ok(()) => "。パスワードを保存しました".into(),
                        Err(e) => format!("。パスワードを保存できません: {e}"),
                    };
                }
                self.notify.info(format!("{name} につなぎました（{product}、{schema}）{saved}"));
                self.db_log_line(&format!("{name} につなぎました（{product}、{schema}）{saved}"), false);
                if let Some(sql) = self.db.as_mut().and_then(|p| p.then.remove(&i)) {
                    self.db_execute(i, sql);
                }
            }
            (Some(Expect::Connect), Reply::Error(e)) => {
                c.client = None;
                c.error = Some(e.clone());
                // 保存したパスワードで失敗した: 入れ直してもらう（つながったら上書きする）
                if c.pass.take().is_some_and(|(_, saved)| saved) {
                    p.password = Some((i, Prompt::default()));
                    self.notify.error(format!("{name} に保存したパスワードでつなげません: {e}。パスワードを入れ直してください（Enter でつなぐ・Esc でやめる）"));
                } else {
                    self.notify.error(format!("{name} につなげません: {e}"));
                }
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
            (Some(Expect::Relations(s, t)), Reply::Relations(r)) => {
                c.relations.insert((s, t), r);
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
                    self.notify.error(format!("SQL の誤り: {}", e.lines().next().unwrap_or("")));
                }
                // テーブルを作ったり消したりしたかもしれないので、木を読み直す
                if ok && changes {
                    if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
                        let s = c.schema.clone();
                        c.send(Req::Objects(s.clone()), Expect::Objects(s));
                    }
                }
                match target {
                    Target::Result(id) => {
                        let rows = matches!(reply, Reply::Rows { .. });
                        self.db_results.reply(id, reply);
                        if rows {
                            self.result_find_target(i, id);
                        }
                    }
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
            (Some(Expect::Batch(target, n)), reply) => match &reply {
                Reply::Updated { ms, .. } => {
                    if c.manual {
                        c.pending += n;
                    }
                    let manual = c.manual;
                    self.notify.info(format!("{n} 件の変更を送信しました{}", if manual { "（まだコミットしていません）" } else { "" }));
                    let what = if target == Target::Table { "テーブルの編集" } else { "結果の編集" };
                    self.db_log(&name, &format!("{n} 件の変更（{what}）"), &format!("{ms} ms"), false);
                    match target {
                        Target::Table => self.db_table_reload(),
                        Target::Result(id) => self.db_results.committed(id, format!("{n} 行を直しました · {ms} ms")),
                    }
                }
                Reply::Error(e) => {
                    self.notify.error(format!("送信できません: {}", e.lines().next().unwrap_or("")));
                    let what = if target == Target::Table { "テーブルの編集" } else { "結果の編集" };
                    self.db_log(&name, &format!("{n} 件の変更（{what}）"), e, true);
                    if let Some(te) = &mut self.db_table {
                        te.busy = false;
                    }
                }
                _ => {}
            },
            (Some(Expect::Tx), Reply::Error(e)) | (Some(Expect::SetSchema(_)), Reply::Error(e)) | (Some(Expect::Ddl(_)), Reply::Error(e)) => {
                self.notify.info(format!("{name}: {}", e.lines().next().unwrap_or("")));
                self.db_log_line(&format!("{name}: {e}"), true);
            }
            (Some(Expect::Tx), _) => {}
            (Some(Expect::Commit(commit, n)), reply) => {
                let what = if commit { "コミット" } else { "ロールバック" };
                if let Reply::Error(e) = &reply {
                    self.notify.error(format!("{name}: {what}できません: {}", e.lines().next().unwrap_or("")));
                    self.db_log_line(&format!("{name}: {what}できません: {e}"), true);
                    return;
                }
                c.pending = c.pending.saturating_sub(n);
                self.notify.info(format!("{what}しました（{n} 件）"));
                self.db_log_line(&format!("{what}しました（{n} 件）"), false);
                // 結果の表で直して送った値: ロールバックなら前の値に戻して読み直す
                for id in self.db_results.tx_ended(i, commit) {
                    self.db_requery(id);
                }
            }
            (Some(Expect::SetSchema(s)), _) => {
                c.schema = s.clone();
                if !c.objects.contains_key(&s) {
                    c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
                }
                p.expanded.insert(format!("c{i}:{s}"));
                p.expanded.insert(format!("g{i}:{s}:Tables"));
                self.notify.info(format!("スキーマを {s} にしました"));
            }
            (Some(Expect::Ddl(t)), Reply::Ddl(text)) => self.db_open_text(&format!("{}.ddl.sql", safe_name(&t)), &text),
            (_, Reply::Error(e)) => {
                self.notify.info(format!("{name}: {}", e.lines().next().unwrap_or("")));
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
            self.notify.error(format!("書けません: {e}"));
            return;
        }
        self.edit_file(path.clone());
        self.set_state(State::Normal);
        self.notify.info(format!("{} に書きました", path.display()));
    }

    /// SQL を流す（つないでいなければつないでから。自動のときは、データを変える文を確かめてから）
    pub(crate) fn db_execute(&mut self, i: usize, sql: String) {
        if sql.trim().is_empty() {
            self.notify.info("流す文がありません（カーソルを文の上に置いてください）");
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
        if changes && !p.conns[i].manual {
            let head: String = sql.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
            let q = format!("データを変える文です（{head} …）。{} で実行しますか（Tx を手動にすると確かめません）", p.sources[i].name);
            self.ask_or(q, move |e| e.db_run(i, sql), |e| e.notify.info("実行をやめました"));
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
        let d = self.db_dialect(i);
        if let Some(t) = self.db_results.tabs.iter_mut().find(|t| t.id == id) {
            t.dialect = d;
        }
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
            self.notify.info("まだつないでいません");
            return;
        }
        c.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset }, Expect::Query(Target::Result(id), sql, false));
        if let Some(cs) = count {
            c.send(Req::Count(cs), Expect::Count(Target::Result(id)));
        }
    }

    /// Oracle のドライバーをダウンロードする（確認の窓で「はい」）
    fn db_download_driver(&mut self, i: usize) {
        let waker = self.waker();
        let Some(p) = &mut self.db else { return };
        p.setup_ask = None;
        p.download = Some((i, drivers::download_oracle(waker), "ダウンロードを始めます…".into()));
        p.expanded.insert(format!("s{i}"));
        p.rebuild();
        self.notify.info("Oracle のドライバーをダウンロードしています…");
    }

    /// 今のバッファが SQL コンソールなら、その接続先
    /// 接続先 `i` の方言
    pub(crate) fn db_dialect(&self, i: usize) -> &'static nagi_lang::sql::Dialect {
        nagi_lang::sql::dialect(self.db.as_ref().and_then(|p| p.kind(i)))
    }

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
        let sql = if let State::Visual { .. } = self.state() {
            let (a, b, _) = self.visual_range();
            let s = self.buf.slice_string(a, b);
            self.set_state(State::Normal);
            s
        } else {
            match self.console_statement() {
                Some((s, e)) => (s..e).map(|l| self.buf.line(l)).filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n").trim().to_string(),
                None => String::new(),
            }
        };
        if self.state() == State::Insert {
            self.key(Key::Esc);
        }
        self.db_execute(i, sql);
    }

    /// Tx の自動・手動を切り替える
    pub(crate) fn db_toggle_tx(&mut self, i: usize) {
        if self.db.as_ref().and_then(|p| p.kind(i)) == Some(Kind::Salesforce) {
            self.notify.info("Salesforce にトランザクションはありません");
            return;
        }
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.notify.info("まだつないでいません");
            return;
        }
        if c.manual && c.pending > 0 {
            self.notify.info(format!("まだコミットしていない変更が {} 件あります。先にコミットかロールバックを", c.pending));
            return;
        }
        c.manual = !c.manual;
        let m = c.manual;
        c.send(Req::AutoCommit(!m), Expect::Tx);
        self.notify.info(if m { "Tx: 手動（コミットするまで確定しません）" } else { "Tx: 自動" });
        self.db_log_line(&format!("トランザクション: {}", if m { "手動" } else { "自動" }), false);
    }

    pub(crate) fn db_commit(&mut self, i: usize, commit: bool) {
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            return;
        }
        // 変更の数は、答えが届いてから減らす（その前に接続が切れていたら、失われた数として知らせる）
        let n = c.pending;
        c.send(if commit { Req::Commit } else { Req::Rollback }, Expect::Commit(commit, n));
    }

    /// 裏で接続が切れていたので、つなぎ直した（手動の Tx なら、コミットしていない変更は失われた）
    fn db_reconnected(&mut self, i: usize, tx_lost: bool) {
        let Some(p) = &mut self.db else { return };
        let name = p.sources[i].name.clone();
        let c = &mut p.conns[i];
        let n = if tx_lost { std::mem::take(&mut c.pending) } else { 0 };
        if n > 0 {
            self.notify.error(format!("{name} の接続が切れていたので、つなぎ直しました。コミットしていない {n} 件の変更は失われました"));
            self.db_log_line(&format!("{name} の接続が切れていたので、つなぎ直しました。コミットしていない {n} 件の変更は失われました"), true);
        } else {
            self.notify.info(format!("{name} の接続が切れていたので、つなぎ直しました"));
            self.db_log_line(&format!("{name} の接続が切れていたので、つなぎ直しました"), false);
        }
        if tx_lost {
            // 結果の表で直して送った値は、ロールバックと同じく前の値に戻して読み直す
            for id in self.db_results.tx_ended(i, false) {
                self.db_requery(id);
            }
        }
    }

    /// 今の接続先（コンソールの接続先、無ければ木で選んでいる接続先）
    fn db_current_source(&self) -> Option<usize> {
        self.console_source().or_else(|| self.db.as_ref().and_then(|p| p.rows.get(p.selected)).map(|r| r.source()))
    }

    /// `:DbLogout`: Salesforce の保存したログインを消して切る（ほかの接続先は、保存したパスワードを消す）
    pub(crate) fn db_logout(&mut self) {
        match self.db_current_source() {
            Some(i) => self.db_forget_login(i),
            None => self.notify.info("接続先がありません"),
        }
    }

    /// 接続先 `i` を切る（コミットしていない変更があれば、捨ててよいか聞く。接続が切れていても切れる）
    fn db_disconnect(&mut self, i: usize) {
        self.db_close(i, false);
    }

    /// 接続先 `i` をつなぎ直す（今の接続を捨てて、新しくつなぐ）
    fn db_reconnect(&mut self, i: usize) {
        self.db_close(i, true);
    }

    fn db_close(&mut self, i: usize, again: bool) {
        let Some(p) = &self.db else { return };
        let n = p.conns[i].pending;
        if n > 0 {
            let q = format!("まだコミットしていない変更が {n} 件あります。捨てて{}か", if again { "つなぎ直します" } else { "切ります" });
            self.ask_or(q, move |e| e.db_close_now(i, again), |e| e.notify.info("やめました"));
            return;
        }
        self.db_close_now(i, again);
    }

    fn db_close_now(&mut self, i: usize, again: bool) {
        let Some(p) = &mut self.db else { return };
        // 裏のスレッドは、送り口が無くなると今の頼みごとを終えてから接続を閉じる（コミットしていない変更は取り消される）
        p.conns[i] = Conn::default();
        p.then.remove(&i);
        p.expanded.remove(&format!("s{i}"));
        p.rebuild();
        p.selected = p.rows.iter().position(|r| *r == DbRow::Source(i)).unwrap_or(0);
        let name = p.sources[i].name.clone();
        if again {
            self.db_begin_connect(i);
        } else {
            self.notify.info(format!("{name} を切りました"));
        }
    }

    /// 接続先 `i` の木を読み直す（覚えたテーブル・列・索引を捨てて、今のスキーマから）
    fn db_reload(&mut self, i: usize) {
        let Some(p) = &mut self.db else { return };
        let c = &mut p.conns[i];
        if c.connected {
            c.objects.clear();
            c.columns.clear();
            c.indexes.clear();
            c.requested.clear();
            let s = c.schema.clone();
            c.send(Req::Schemas, Expect::Schemas);
            c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
            c.requested.insert(format!("o:{s}"));
            self.notify.info("読み直しています…");
        }
        p.rebuild();
    }

    /// 接続先 `i` の保存したパスワード（Salesforce はログイン）を消す
    fn db_forget_login(&mut self, i: usize) {
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        if p.kind(i) != Some(Kind::Salesforce) {
            let had = nagi_workers::db::forget_password(&s.url, &s.user);
            self.notify.info(if had { format!("{} の保存したパスワードを消しました", s.name) } else { "保存したパスワードはありません".to_string() });
            return;
        }
        let had = nagi_workers::salesforce::forget(&s.url, &s.user);
        p.conns[i] = Conn::default();
        p.rebuild();
        self.notify.info(if had { format!("{} の保存したログインを消しました", s.name) } else { "保存したログインはありません".to_string() });
    }

    /// `:DbSchema 名前`
    pub(crate) fn db_set_schema(&mut self, s: &str) {
        match self.db_current_source() {
            Some(i) => self.db_switch_schema(i, s),
            None => self.notify.info("接続先がありません"),
        }
    }

    /// 接続先 `i` のスキーマを切り替える（コンソール・木は切り替えた先を見る）
    fn db_switch_schema(&mut self, i: usize, s: &str) {
        let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.notify.info("まだつないでいません");
            return;
        }
        let s = s.trim().to_string();
        c.send(Req::SetSchema(s.clone()), Expect::SetSchema(s));
    }

    /// 接続先 `i` の既定のスキーマを決める（空なら外す）。設定ファイルに書き、つないでいれば切り替える
    pub(crate) fn db_set_default_schema(&mut self, i: usize, schema: &str) {
        let Some(p) = &mut self.db else { return };
        let Some(ds) = p.sources.get_mut(i) else { return };
        ds.schema = schema.trim().to_string();
        let ds = ds.clone();
        let cfg = config_path();
        if let Err(e) = save_config_line(&cfg, &ds) {
            self.notify.error(format!("設定ファイルに書けません: {e}"));
            return;
        }
        p.rebuild();
        let c = &mut p.conns[i];
        let switch = c.connected && !ds.schema.is_empty() && !ds.schema.eq_ignore_ascii_case(&c.schema) && !matches!(Kind::of(&ds.url), Some(Kind::Sqlite | Kind::Salesforce));
        if switch {
            c.send(Req::SetSchema(ds.schema.clone()), Expect::SetSchema(ds.schema.clone()));
        }
        self.notify.info(if ds.schema.is_empty() {
            format!("{} の既定のスキーマを外しました（{}）", ds.name, cfg.display())
        } else {
            format!("{} の既定のスキーマを {} にしました（つないだら切り替える。{}）", ds.name, ds.schema, cfg.display())
        });
    }

    /// `:DbDefaultSchema [名前]`: 今の接続先の既定のスキーマ（名前を省くと外す）
    pub(crate) fn db_default_schema_command(&mut self, s: &str) {
        match self.db_current_source() {
            Some(i) => self.db_set_default_schema(i, s),
            None => self.notify.info("接続先がありません"),
        }
    }

    /// スキーマの候補（:DbSchema の補完）
    pub fn db_schema_names(&self) -> Vec<String> {
        self.db_current_source().and_then(|i| self.db.as_ref()?.conns.get(i)?.schemas.clone()).unwrap_or_default()
    }

    /// `:DbCommit` / `:DbRollback` / `:DbTx`
    pub(crate) fn db_tx_command(&mut self, what: &str) {
        let Some(i) = self.db_current_source() else {
            self.notify.info("接続先がありません");
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
        self.set_state(State::Normal);
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

    /// 新しいクエリ（DataGrip の New Query Console）: 接続先の新しい .sql を Insert で開く。Ctrl+Enter で流す
    pub(crate) fn db_new_query(&mut self, i: usize, initial: Option<String>) {
        let Some(p) = &mut self.db else { return };
        let s = p.sources[i].clone();
        let dir = work_dir().join("consoles");
        let _ = std::fs::create_dir_all(&dir);
        let base = safe_name(&s.name);
        let n = (1..).find(|n| !dir.join(format!("{base}_query_{n}.sql")).exists()).unwrap_or(1);
        let path = dir.join(format!("{base}_query_{n}.sql"));
        let body = format!("-- {}  クエリ {n}\n\n{}", s.name, initial.map(|q| format!("{q}\n")).unwrap_or_default());
        if let Err(e) = std::fs::write(&path, body) {
            self.notify.error(format!("クエリのファイルを作れません: {e}"));
            return;
        }
        let abs = std::path::absolute(&path).unwrap_or(path.clone());
        p.consoles.insert(abs, i);
        self.edit_file(path);
        self.set_state(State::Normal);
        // 最後の行の終わりから Insert で書き始める
        self.key(Key::Char('G'));
        self.key(Key::Char('A'));
        self.notify.info(format!("新しいクエリ（{}）。Ctrl+Enter で実行", s.name));
    }

    /// `:DbNew`: 今の接続先（コンソール・木で選んでいるもの。無ければ最初の接続先）で新しいクエリ
    pub(crate) fn db_new_query_command(&mut self) {
        if self.db.is_none() {
            self.open_db_panel();
        }
        let i = self.db_current_source().or_else(|| self.db.as_ref().filter(|p| !p.sources.is_empty()).map(|_| 0));
        match i {
            Some(i) => self.db_new_query(i, None),
            None => self.notify.info("接続先がありません。データベースの欄の a で足せます"),
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
                self.set_state(State::List);
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
            self.db_ddl_of(i, &s, &t, g);
        }
    }

    fn db_ddl_of(&mut self, i: usize, s: &str, t: &str, g: Group) {
        if let Some(c) = self.db.as_mut().map(|p| &mut p.conns[i]) {
            let kind = if g == Group::Views { "VIEW" } else { "TABLE" };
            c.send(Req::Ddl(s.to_string(), t.to_string(), kind.into()), Expect::Ddl(format!("{s}.{t}")));
        }
    }

    /// 木の右クリック（欄の中の行。0 行目はタブ、1 行目は絞り込み）。押した行を選んでから、その行のメニューを出す（DataGrip と同じ操作）。
    /// `at` はメニューを出す画面の位置。出したら true
    pub(crate) fn db_context(&mut self, prow: usize, at: (usize, usize)) -> bool {
        let Some(p) = &mut self.db else { return false };
        // 右クリックは押した行を選ぶだけ（決めない）
        let len = p.rows.len();
        if prow < 2 || crate::list::pick_row(&mut p.selected, p.scroll, prow, 2, len, 1).is_none() {
            return false;
        }
        let row = p.rows[p.selected].clone();
        let i = row.source();
        // SQL を作る項目のために、列をいまのうちに読み始める
        if let DbRow::Object(_, s, t, Group::Tables | Group::Views, _) = &row {
            p.conns[i].ensure_columns(s, t);
        }
        self.focus_right(RightTab::Database);
        let Some(p) = &self.db else { return false };
        let src = p.sources[i].clone();
        let c = &p.conns[i];
        let kind = Kind::of(&src.url);
        let sf = kind == Some(Kind::Salesforce);
        let mut m = crate::menu::Menu::new(at.0, at.1);
        match row {
            DbRow::Source(_) => {
                if c.client.is_none() {
                    m = m.item("つなぐ", move |e| e.db_begin_connect(i));
                } else {
                    m = m.item("つなぎ直す", move |e| e.db_reconnect(i)).item("切る", move |e| e.db_disconnect(i));
                }
                m = m.item("新しいクエリ", move |e| e.db_new_query(i, None)).item("コンソールを開く", move |e| e.open_console(i, None));
                if c.connected {
                    m = m.item("読み直す", move |e| e.db_reload(i));
                }
                m = m.sep();
                if !src.schema.is_empty() {
                    m = m.item(format!("既定のスキーマ（{}）を外す", src.schema), move |e| e.db_set_default_schema(i, ""));
                }
                if kind != Some(Kind::Sqlite) {
                    m = m.item(if sf { "保存したログインを消す" } else { "保存したパスワードを消す" }, move |e| e.db_forget_login(i));
                }
                let url = src.url.clone();
                m = m.item("URL をコピー", move |e| e.copy_text(url, "URL"));
            }
            DbRow::Schema(_, s, _) => {
                if !s.eq_ignore_ascii_case(&c.schema) && !sf {
                    let s2 = s.clone();
                    m = m.item("このスキーマに切り替える", move |e| e.db_switch_schema(i, &s2));
                }
                if !sf && kind != Some(Kind::Sqlite) {
                    let s2 = s.clone();
                    m = if src.schema.eq_ignore_ascii_case(&s) {
                        m.item("既定のスキーマを外す", move |e| e.db_set_default_schema(i, ""))
                    } else {
                        m.item("既定のスキーマにする（つないだら切り替える）", move |e| e.db_set_default_schema(i, &s2))
                    };
                }
                let (s2, cur) = (s.clone(), c.schema.clone());
                m = m.item("新しいクエリ", move |e| {
                    // このスキーマで書けるように、先に切り替える
                    if !s2.eq_ignore_ascii_case(&cur) && !sf {
                        e.db_switch_schema(i, &s2);
                    }
                    e.db_new_query(i, None)
                });
                m = m.item("読み直す", move |e| e.db_reload(i)).sep();
                m = m.item("名前をコピー", move |e| e.copy_text(s, "名前"));
            }
            DbRow::Group(..) | DbRow::Others(_) => {
                m = m.item("新しいクエリ", move |e| e.db_new_query(i, None)).item("読み直す", move |e| e.db_reload(i));
            }
            DbRow::Object(_, s, t, g, _) => {
                let q = qualified(self.db_dialect(i), &s, &t);
                if matches!(g, Group::Tables | Group::Views) {
                    let (s2, t2) = (s.clone(), t.clone());
                    m = m.item("データを開く", move |e| {
                        if sf {
                            e.db_execute(i, format!("SELECT FIELDS(STANDARD) FROM {t2} LIMIT 200"));
                        } else {
                            e.db_open_table(i, &s2, &t2);
                        }
                    });
                    let init = if sf { format!("SELECT Id, Name FROM {t} LIMIT 100") } else { format!("SELECT * FROM {q}") };
                    m = m.item("新しいクエリ", move |e| e.db_new_query(i, Some(init)));
                    if !sf {
                        let q2 = q.clone();
                        m = m.item("件数を数える", move |e| e.db_execute(i, format!("SELECT COUNT(*) FROM {q2}")));
                        m = m.sep();
                        let what: &[(&str, &str)] = if g == Group::Tables { &[("SELECT", "select"), ("INSERT", "insert"), ("UPDATE", "update"), ("DELETE", "delete")] } else { &[("SELECT", "select")] };
                        for &(label, w) in what {
                            let (s2, t2) = (s.clone(), t.clone());
                            m = m.item(format!("{label} 文を作る（新しいクエリに）"), move |e| e.db_generate(i, &s2, &t2, w));
                        }
                    }
                    let (s2, t2) = (s.clone(), t.clone());
                    m = m.item("DDL を開く", move |e| e.db_ddl_of(i, &s2, &t2, g));
                }
                m = m.sep();
                let t2 = t.clone();
                m = m.item("名前をコピー", move |e| e.copy_text(t2, "名前"));
                let q2 = q.clone();
                m = m.item("スキーマ付きの名前をコピー", move |e| e.copy_text(q2, "名前"));
                if g == Group::Tables && !sf {
                    // 戻せない操作は、実行せずに新しいクエリに書くだけ（流すときにいつもの確認が出る）
                    m = m.sep();
                    let q2 = q.clone();
                    m = m.item("TRUNCATE 文を作る（新しいクエリに）", move |e| e.db_new_query(i, Some(format!("-- 実行するとデータを全部消します（戻せません）\nTRUNCATE TABLE {q2}"))));
                    m = m.item("DROP 文を作る（新しいクエリに）", move |e| e.db_new_query(i, Some(format!("-- 実行するとテーブルを消します（戻せません）\nDROP TABLE {q}"))));
                }
            }
            DbRow::Column(_, s, t, k, _) => {
                let Some(name) = c.columns.get(&(s.clone(), t.clone())).and_then(|v| v.get(k)).map(|x| x.name.clone()) else { return false };
                let q = format!("{}.{}", ident(&t), ident(&name));
                m = m.item("名前をコピー", move |e| e.copy_text(name, "名前")).item("テーブル名付きの名前をコピー", move |e| e.copy_text(q, "名前"));
            }
            DbRow::Sub(..) | DbRow::Key(..) | DbRow::Index(..) | DbRow::Info(..) => return false,
        }
        self.open_menu(m);
        true
    }

    /// テーブルの SELECT・INSERT・UPDATE・DELETE を作って新しいクエリに書く（列がまだ無ければ読み始めて、もう一度と知らせる）
    fn db_generate(&mut self, i: usize, s: &str, t: &str, what: &str) {
        let Some(p) = &mut self.db else { return };
        let c = &mut p.conns[i];
        let Some(cols) = c.columns.get(&(s.to_string(), t.to_string())).cloned() else {
            c.ensure_columns(s, t);
            self.notify.info("列を読んでいます。もう一度選んでください");
            return;
        };
        let d = self.db_dialect(i);
        self.db_new_query(i, Some(generate_sql(d, what, &qualified(d, s, t), &cols)));
    }

    /// 入力欄（絞り込み・フォーム）に IME の確定を入れる。入れたら true
    pub(crate) fn db_insert(&mut self, text: &str) -> bool {
        let Some(p) = &mut self.db else { return false };
        if let Some(f) = &mut p.form {
            if f.at > 0 {
                f.fields[f.at].1.insert_text(text);
            }
            return true;
        }
        if let Some(f) = &mut p.filter {
            if f.insert_text(text) == FieldOut::Changed {
                p.filter_text = f.text();
                p.rebuild();
            }
            return true;
        }
        false
    }

    /// Vim を使わないとき、木で文字を打ったら絞り込みの欄を開いて、その文字から打ち続けられるようにする（打った文字は今の絞り込みの後ろへ）。開いたら true
    pub(crate) fn db_type_to_filter(&mut self, key: Key) -> bool {
        if self.vim_on || !matches!(key, Key::Char(c) if c != ' ') || !self.right_focused() || self.right_tab != RightTab::Database {
            return false;
        }
        let Some(p) = &mut self.db else { return false };
        if p.filter.is_some() || p.form.is_some() || p.password.is_some() {
            return false;
        }
        p.filter = Some(VimField::new(&p.filter_text, false));
        self.db_key(key);
        true
    }

    /// IME を使う入力欄にいるか（パスワードでは使わない）
    pub fn db_typing(&self) -> bool {
        self.right_focused() && self.db.as_ref().is_some_and(|p| p.form.as_ref().is_some_and(|f| f.at > 0 && f.fields[f.at].1.is_insert()) || p.filter.as_ref().is_some_and(|f| f.is_insert()))
    }

    pub(crate) fn db_key(&mut self, key: Key) -> Effect {
        let half = (self.windows_rows() / 2).max(1);
        let Some(p) = &mut self.db else {
            self.set_state(State::Normal);
            return Effect::None;
        };
        // パスワードの入力中
        if let Some((i, prompt)) = &mut p.password {
            match key {
                Key::Esc => {
                    p.password = None;
                    self.notify.info("やめました");
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
            // 欄の行き来と種類はフォームが受け、ほかは今の欄（Vim 風）へ
            let out = match key {
                Key::Tab | Key::Down => {
                    f.at = (f.at + 1).min(f.fields.len() - 1);
                    f.fields[f.at].1.reinsert();
                    FieldOut::None
                }
                Key::Up => {
                    f.at = f.at.saturating_sub(1);
                    f.fields[f.at].1.reinsert();
                    FieldOut::None
                }
                Key::Left | Key::Right | Key::Char(' ') if f.at == 0 => {
                    // 名前の順
                    let order = [Kind::Oracle, Kind::Postgres, Kind::Salesforce, Kind::Sqlite];
                    let k = order.iter().position(|k| *k == f.kind).unwrap_or(0);
                    let k = if key == Key::Left { (k + order.len() - 1) % order.len() } else { (k + 1) % order.len() };
                    f.set_kind(order[k]);
                    FieldOut::None
                }
                Key::Esc if f.at == 0 => FieldOut::Cancel,
                Key::Enter if f.at == 0 => FieldOut::Submit,
                k => self.feed_field(k, |e| e.db.as_mut().and_then(|p| p.form.as_mut()).map(|f| &mut f.fields[f.at].1)),
            };
            let Some(p) = &mut self.db else { return Effect::None };
            let Some(f) = &mut p.form else { return Effect::None };
            match out {
                FieldOut::Cancel => {
                    p.form = None;
                    self.notify.info("やめました");
                }
                FieldOut::Submit if f.at + 1 < f.fields.len() => {
                    f.at += 1;
                    f.fields[f.at].1.reinsert();
                }
                FieldOut::Submit => match f.source() {
                    Ok(ds) => {
                        p.form = None;
                        self.db_push_source(ds, true);
                        let i = self.db.as_ref().unwrap().sources.len() - 1;
                        self.db_begin_connect(i);
                    }
                    Err(e) => self.notify.error(e),
                },
                _ => {}
            }
            return Effect::None;
        }
        // 絞り込みの入力中（Vim 風。Enter で決める、ノーマルの Esc で消す）
        if p.filter.is_some() {
            let out = self.feed_field(key, |e| e.db.as_mut().and_then(|p| p.filter.as_mut()));
            let Some(p) = &mut self.db else { return Effect::None };
            match out {
                FieldOut::Changed => {
                    p.filter_text = p.filter.as_ref().map(|f| f.text()).unwrap_or_default();
                    p.rebuild();
                    p.selected = p.rows.iter().position(|r| matches!(r, DbRow::Object(..))).unwrap_or(p.selected);
                }
                FieldOut::Submit => p.filter = None,
                FieldOut::Cancel => {
                    p.filter = None;
                    p.filter_text.clear();
                    p.rebuild();
                }
                FieldOut::None => {}
            }
            return Effect::None;
        }
        let moved = crate::list::step(key, p.selected, p.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => p.selected = moved.unwrap_or(p.selected),
            Key::Char('g') => p.selected = 0,
            Key::Char('/') => {
                p.filter = Some(VimField::new(&p.filter_text, false));
                self.notify.info("絞り込み: 名前の一部を打つ（Enter で決める・Esc 2 回で消す）");
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
                if let Some(f) = &mut p.form {
                    f.fields[f.at].1.reinsert();
                }
                self.notify.info("接続先を足す: Tab で次の欄、種類は ← →、最後の欄で Enter で保存してつなぐ");
            }
            Key::Char('D') => self.db_ddl(),
            // 新しいクエリ（テーブルの上なら SELECT を書き入れて）
            Key::Char('n') => {
                if let Some(r) = self.db_selected() {
                    let sf = self.db.as_ref().and_then(|p| p.kind(r.source())) == Some(Kind::Salesforce);
                    let init = match &r {
                        DbRow::Object(_, _, t, Group::Tables | Group::Views, _) if sf => Some(format!("SELECT Id, Name FROM {t} LIMIT 100")),
                        DbRow::Object(_, s, t, Group::Tables | Group::Views, _) => Some(format!("SELECT * FROM {}", qualified(self.db_dialect(r.source()), s, t))),
                        _ => None,
                    };
                    self.db_new_query(r.source(), init);
                }
            }
            // コンソール（テーブルの上なら SELECT を書き足して）
            Key::Char('c') => {
                if let Some(r) = self.db_selected() {
                    let sf = self.db.as_ref().and_then(|p| p.kind(r.source())) == Some(Kind::Salesforce);
                    let init = match &r {
                        DbRow::Object(_, _, t, Group::Tables | Group::Views, _) if sf => Some(format!("SELECT Id, Name FROM {t} LIMIT 100")),
                        DbRow::Object(_, s, t, Group::Tables | Group::Views, _) => Some(format!("SELECT * FROM {};", qualified(self.db_dialect(r.source()), s, t))),
                        _ => None,
                    };
                    self.open_console(r.source(), init);
                }
            }
            // 切る
            Key::Char('x') => {
                if let Some(r) = self.db_selected() {
                    self.db_disconnect(r.source());
                }
            }
            // 読み直す
            Key::Char('r') => {
                if let Some(r) = self.db_selected() {
                    self.db_reload(r.source());
                }
            }
            Key::Char('q') => {
                if self.db.as_ref().is_some_and(|p| p.conns.iter().any(|c| c.pending > 0)) {
                    self.notify.info("まだコミットしていない変更があります。先にコミットかロールバックを");
                } else {
                    self.db = None;
                    self.set_state(State::Normal);
                    self.rows = self.text_rows(self.active_rect());
                }
            }
            Key::Esc => self.set_state(State::Normal),
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
            if p.filter.is_none() {
                p.filter = Some(VimField::new(&p.filter_text, false));
            }
            self.focus_right(RightTab::Database);
            return;
        }
        let len = p.rows.len();
        let Some(pick) = crate::list::pick_row(&mut p.selected, p.scroll, row.max(2), 2, len, clicks) else { return };
        let accept = matches!(pick, crate::list::Pick::Accept(_));
        self.focus_right(RightTab::Database);
        match self.db_selected() {
            Some(DbRow::Object(_, _, _, Group::Tables | Group::Views, _)) if accept => self.db_open_selected(),
            // 開け閉めできる行（テーブル・ビューを除く。接続先は、つないでいなければつなぐ）は 1 回目のクリックで。
            // ダブルクリックの 2 回目では閉じ直さない
            Some(r) if r.key().is_some() && !matches!(r, DbRow::Object(..)) => {
                if clicks == 1 {
                    self.db_activate();
                }
            }
            Some(_) if accept => self.db_activate(),
            _ => {}
        }
    }

    /// コンソールの上の帯のボタン（見出し, 横の位置, 幅, 何をするか）。帯の左端から数える
    pub fn console_bar(&self, w: usize) -> Vec<(String, usize, usize, BarItem)> {
        let Some(i) = self.console_source() else { return Vec::new() };
        let Some(p) = &self.db else { return Vec::new() };
        let c = &p.conns[i];
        let width = |s: &str| nagi_core::display_col(s, s.len(), false);
        let right = format!("{} › {} ▾", p.sources[i].name, if c.schema.is_empty() { "…" } else { &c.schema });
        // 狭ければ、説明の字・件数を外し、ボタンを記号だけにし、それでも入らなければ接続先の名前を詰める（重ねない）
        for level in 0..3 {
            let short = level >= 1;
            let mut v = Vec::new();
            // 左端の「出す」ボタン（»）と重ならないように 2 列あける
            let mut x = 2;
            let mut push = |v: &mut Vec<(String, usize, usize, BarItem)>, s: &str, item: BarItem| {
                let n = width(s);
                v.push((s.to_string(), x, n, item));
                x += n + 2;
            };
            push(&mut v, if short { "▶" } else { "▶ 実行" }, BarItem::Run);
            if level == 0 {
                push(&mut v, "Ctrl+Enter", BarItem::Label);
            }
            push(&mut v, if short { "＋" } else { "＋ 新しいクエリ" }, BarItem::NewQuery);
            if p.kind(i) != Some(Kind::Salesforce) {
                let tx = if c.manual { "手動" } else { "自動" };
                push(&mut v, &if short { format!("{tx}▾") } else { format!("Tx: {tx} ▾") }, BarItem::Tx);
            }
            if c.manual {
                if level == 0 {
                    push(&mut v, &format!("未コミット {} 件", c.pending), BarItem::Pending);
                }
                push(&mut v, if short { "✓" } else { "✓ コミット" }, BarItem::Commit);
                push(&mut v, if short { "↺" } else { "↺ ロールバック" }, BarItem::Rollback);
            }
            // 左のボタンの終わりから 1 列あけて、右端の 1 列手前まで
            let room = w.saturating_sub(x + 1);
            let n = width(&right);
            if n <= room {
                v.push((right, w - n - 1, n, BarItem::Schema));
                return v;
            }
            if level == 2 {
                // 接続先の名前を後ろから詰める（▾ は残す）
                if room >= 3 {
                    let mut t: String = String::new();
                    for ch in right.chars() {
                        if width(&format!("{t}{ch}…▾")) > room {
                            break;
                        }
                        t.push(ch);
                    }
                    let t = format!("{t}…▾");
                    let n = width(&t);
                    v.push((t, w - n - 1, n, BarItem::Schema));
                }
                return v;
            }
        }
        unreachable!()
    }

    /// 帯のボタンを押す
    pub(crate) fn console_bar_press(&mut self, item: BarItem) {
        let Some(i) = self.console_source() else { return };
        match item {
            BarItem::Run => self.db_run_at_cursor(),
            BarItem::NewQuery => self.db_new_query(i, None),
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
    NewQuery,
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

    /// 欄の入力欄を Insert・Visual のまま本文を押しても、すぐ本文へ移る（前は Insert の間は押しても何もしなかった）
    #[test]
    fn click_text_from_field_in_any_mode() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("abc\ndef\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        for k in ["/or", "/or<Esc>v"] {
            e.db.as_mut().unwrap().filter = None;
            e.db.as_mut().unwrap().filter_text.clear();
            e.focus_right(RightTab::Database);
            keys(&mut e, k);
            assert!(e.right_focused() && e.db.as_ref().unwrap().filter.is_some(), "{k}");
            e.click(1, 7);
            assert_eq!((e.state(), e.cursor.line), (State::Normal, 1), "{k}");
            keys(&mut e, "x");
            assert_eq!(e.db.as_ref().unwrap().filter_text, "or", "{k}: 打ったキーは本文へ");
        }
        assert_eq!(e.buf.line(1).trim_end().len(), 1, "x を 2 回");
    }

    pub(crate) fn wait(e: &mut Editor, what: &dyn Fn(&Editor) -> bool) {
        for _ in 0..2000 {
            e.poll_db();
            if what(e) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("待ちきれない: {}", e.notify.text);
    }

    /// 偽の Salesforce で: 足す → つなぐ（ブラウザの代わりに curl）→ 木 → o で SOQL → 結果のタブ
    #[test]
    fn salesforce_flow() {
        let m = nagi_workers::salesforce::mock::start();
        let dir = std::env::temp_dir().join(format!("nagi-sfui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: テストの中だけで使う
        unsafe {
            std::env::set_var("NAGI_SF_DIR", &dir);
            std::env::set_var("NAGI_SF_BROWSER", "curl");
            std::env::set_var("NAGI_DB_DIR", &dir);
        }
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.db_add(&format!("sf salesforce:{} me@example.com", m.base));
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        assert_eq!(e.db.as_ref().unwrap().kind(i), Some(Kind::Salesforce));
        e.focus_right(RightTab::Database);
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        keys(&mut e, "<CR>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.values().any(|o| !o.tables.is_empty()));
        let p = e.db.as_ref().unwrap();
        let labels: Vec<String> = p.rows.iter().map(|r| p.row_label(r).2).collect();
        assert!(labels.contains(&"オブジェクト".to_string()) && labels.contains(&"カスタムオブジェクト".to_string()), "{labels:?}");
        assert!(e.db_results.log.iter().any(|l| l.text.contains("ブラウザ")), "ログインの知らせが出力に残る");
        // Account の上で o → SOQL が流れて結果のタブに出る（絞り込みの欄は出さない）
        let k = p.rows.iter().position(|r| matches!(r, DbRow::Object(_, _, t, _, _) if t == "Account")).unwrap();
        e.db.as_mut().unwrap().selected = k;
        keys(&mut e, "o");
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running));
        let t = e.db_results.active_tab().unwrap();
        assert_eq!(t.cols, vec!["Id", "Name", "Owner.Name"]);
        assert_eq!(t.rows.len(), 3);
        assert!(!t.filterable);
        assert!(m.seen.lock().unwrap().queries.iter().any(|q| q.contains("FIELDS(STANDARD) FROM Account")));
        // コンソールの帯に Tx は出さない
        e.open_console(i, None);
        assert!(!e.console_bar(120).iter().any(|b| b.3 == BarItem::Tx));
    }

    /// 設定ファイルの既定のスキーマ: その接続先の行だけを書き直し（注釈・ほかの行はそのまま）、無ければ足す
    #[test]
    fn default_schema_in_config() {
        let dir = std::env::temp_dir().join(format!("nagi-dbcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("databases.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "# 注釈\ndev | jdbc:oracle:thin:@//db:1521/X | APP\nlocal | jdbc:sqlite::memory: |\n").unwrap();
        let mut ds = nagi_workers::db::config_sources(&std::fs::read_to_string(&path).unwrap(), "").remove(0);
        ds.schema = "HR".into();
        save_config_line(&path, &ds).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# 注釈\ndev | jdbc:oracle:thin:@//db:1521/X | APP | HR\nlocal | jdbc:sqlite::memory: |\n");
        ds.schema.clear();
        save_config_line(&path, &ds).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("dev | jdbc:oracle:thin:@//db:1521/X | APP\n"), "外すと 3 つの欄に戻る");
        let other = DataSource { name: "spring".into(), url: "jdbc:postgresql://h/db".into(), user: "u".into(), origin: String::new(), schema: "app".into() };
        save_config_line(&path, &other).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("spring | jdbc:postgresql://h/db | u | app\n"), "無い接続先は足す");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_sql() {
        let col = |n: &str, pk: bool| Column { name: n.into(), ty: "INT".into(), nullable: !pk, pk, fk: String::new(), rel: String::new() };
        let cols = vec![col("ID", true), col("NAME", false), col("orderNo", false)];
        assert_eq!(generate_sql(&nagi_lang::sql::ORACLE, "select", "HR.EMP", &cols), "SELECT ID,\n       NAME,\n       \"orderNo\"\nFROM HR.EMP");
        assert_eq!(generate_sql(&nagi_lang::sql::ORACLE, "insert", "T", &cols[..2]), "INSERT INTO T (ID, NAME)\nVALUES (/* ID */ NULL, /* NAME */ NULL)");
        assert_eq!(generate_sql(&nagi_lang::sql::ORACLE, "update", "T", &cols[..2]), "UPDATE T\nSET NAME = NULL\nWHERE ID = NULL");
        assert_eq!(generate_sql(&nagi_lang::sql::ORACLE, "delete", "T", &cols[1..2]), "DELETE FROM T\nWHERE /* 条件 */", "主キーが無ければ条件は注釈だけ");
        // PostgreSQL は小文字に寄るので、大文字の名前は囲む
        assert_eq!(generate_sql(&nagi_lang::sql::POSTGRES, "insert", &qualified(&nagi_lang::sql::POSTGRES, "public", "EMP"), &cols[..2]), "INSERT INTO public.\"EMP\" (\"ID\", \"NAME\")\nVALUES (/* \"ID\" */ NULL, /* \"NAME\" */ NULL)");
    }

    /// Vim を使わないとき、木で文字を打つと絞り込みの欄が開き、そのまま打ち続けられる
    #[test]
    fn type_to_filter() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.set_vim(false);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        e.focus_right(RightTab::Database);
        assert!(e.right_focused());
        for c in "it".chars() {
            e.key(Key::Char(c));
        }
        let p = e.db.as_ref().unwrap();
        assert!(p.filter.is_some());
        assert_eq!(p.filter_text, "it");
        // 決めた後にまた打つと、今の絞り込みの後ろへ
        e.key(Key::Enter);
        assert!(e.db.as_ref().unwrap().filter.is_none());
        e.key(Key::Char('e'));
        assert_eq!(e.db.as_ref().unwrap().filter_text, "ite");
        // Ctrl+A で全部を選んで打ち直す
        e.key(Key::Ctrl('a'));
        e.key(Key::Char('m'));
        assert_eq!(e.db.as_ref().unwrap().filter_text, "m");
        e.set_vim(true);
    }

    /// 接続先は 1 回のクリックでつなぐ・開け閉めする。ダブルクリックの 2 回目では閉じ直さない
    #[test]
    fn click_source() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        let prow = |e: &Editor| {
            let p = e.db.as_ref().unwrap();
            p.rows.iter().position(|r| *r == DbRow::Source(i)).map(|k| k - p.scroll + 2).unwrap()
        };
        let open = |e: &Editor| e.db.as_ref().unwrap().expanded.contains(&format!("s{i}"));
        let r = prow(&e);
        e.db_click(r, 1);
        assert!(e.db.as_ref().unwrap().conns[i].client.is_some(), "1 回でつなぎ始める");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        assert!(open(&e));
        e.db_click(r, 1);
        assert!(!open(&e), "もう 1 回で閉じる");
        e.db_click(r, 1);
        e.db_click(r, 2);
        assert!(open(&e), "ダブルクリックでも開いたまま");
    }

    /// SQLite で: 木の右クリック → 接続先・テーブル・列のメニュー → SELECT 文を作る・件数・名前のコピー
    #[test]
    fn context_menu() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.db.as_ref().unwrap().sources.len() - 1;
        let prow = |e: &Editor, f: &dyn Fn(&DbRow) -> bool| {
            let p = e.db.as_ref().unwrap();
            p.rows.iter().position(|r| f(r)).map(|k| k - p.scroll + 2).unwrap()
        };
        let labels = |e: &Editor| e.menu.as_ref().unwrap().items.iter().filter(|it| it.run.is_some()).map(|it| it.label.clone()).collect::<Vec<_>>();
        let run = |e: &mut Editor, label: &str| {
            let k = e.menu.as_ref().unwrap().items.iter().position(|it| it.label == label).unwrap_or_else(|| panic!("{label} が無い"));
            e.menu_run(k);
        };
        // つないでいない接続先
        let r = prow(&e, &|r| *r == DbRow::Source(i));
        assert!(e.db_context(r, (r, 150)));
        assert_eq!(e.state(), State::Menu);
        assert!(labels(&e).contains(&"つなぐ".to_string()) && !labels(&e).iter().any(|l| l.contains("パスワード")), "SQLite にパスワードは無い: {:?}", labels(&e));
        run(&mut e, "つなぐ");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].connected);
        e.db.as_mut().unwrap().conns[i].manual = true;
        e.db_execute(i, "create table item(id integer primary key, name text)".into());
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        // テーブル: メニューを出すと列を読み始める
        let r = prow(&e, &|r| matches!(r, DbRow::Object(_, _, t, Group::Tables, _) if t == "item"));
        assert!(e.db_context(r, (r, 150)));
        for want in ["データを開く", "新しいクエリ", "件数を数える", "SELECT 文を作る（新しいクエリに）", "DDL を開く", "名前をコピー", "DROP 文を作る（新しいクエリに）"] {
            assert!(labels(&e).iter().any(|l| l == want), "{want}: {:?}", labels(&e));
        }
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].columns.contains_key(&("main".to_string(), "item".to_string())));
        run(&mut e, "SELECT 文を作る（新しいクエリに）");
        assert_eq!(e.console_source(), Some(i), "新しいクエリはこの接続先のコンソール");
        assert!(e.buf.text().contains("SELECT id,\n       name\nFROM main.item"), "{}", e.buf.text());
        // 件数は結果のタブに出る（読むだけなので確かめない）
        e.set_state(State::Normal);
        let r = prow(&e, &|r| matches!(r, DbRow::Object(_, _, t, Group::Tables, _) if t == "item"));
        assert!(e.db_context(r, (r, 150)));
        run(&mut e, "件数を数える");
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running));
        assert_eq!(e.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("0"));
        // スキーマ: SQLite は切り替えない・既定にしない
        let r = prow(&e, &|r| matches!(r, DbRow::Schema(..)));
        assert!(e.db_context(r, (r, 150)));
        assert!(!labels(&e).iter().any(|l| l.contains("既定") || l.contains("切り替える")), "{:?}", labels(&e));
        run(&mut e, "新しいクエリ");
        assert_eq!(e.console_source(), Some(i));
        // 見出しの行（タブ・絞り込み）ではメニューを出さない
        e.set_state(State::Normal);
        assert!(!e.db_context(1, (1, 150)));
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
        // a → 種類を SQLite に（名前の順で最後なので、Oracle から ← で回る）→ 名前 → ファイル → Enter で保存してつなぐ
        keys(&mut e, "a");
        assert!(e.db.as_ref().unwrap().form.is_some());
        keys(&mut e, "<Up><Left><Down>shop<Tab>");
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
        assert!(e.menu.as_ref().and_then(|m| m.title.as_deref()).is_some_and(|t| t.contains("データを変える文")), "確認の窓");
        keys(&mut e, "y");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        // 手動 Tx では確かめずに流し、未コミットを数える。ロールバックで消える
        e.db_toggle_tx(i);
        keys(&mut e, "o<CR>insert into item values (1, 'りんご')<Esc><C-m>");
        wait(&mut e, &|e| e.db.as_ref().unwrap().conns[i].pending == 1);
        let bar: Vec<String> = e.console_bar(120).into_iter().map(|b| b.0).collect();
        assert!(bar.contains(&"未コミット 1 件".to_string()), "{bar:?}");
        // 狭くしても、ボタンと接続先は重ならない（説明を外し、記号だけにし、名前を詰める）
        for w in [30, 45, 60, 80] {
            let bar = e.console_bar(w);
            for pair in bar.windows(2) {
                assert!(pair[0].1 + pair[0].2 < pair[1].1, "{w}: {bar:?}");
            }
            assert!(bar.last().is_some_and(|b| b.3 == BarItem::Schema && b.1 + b.2 < w), "{w}: {bar:?}");
        }
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
        // 新しいクエリ: 別のファイルを Insert で開き、そのまま書いて Ctrl+Enter で流せる
        e.focus_right(RightTab::Database);
        e.db.as_mut().unwrap().selected = e.db.as_ref().unwrap().rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        keys(&mut e, "n");
        assert_eq!(e.state(), State::Insert);
        assert!(e.buf.path.as_ref().unwrap().to_string_lossy().ends_with("shop_query_1.sql"));
        keys(&mut e, "select 42 as answer<C-m>");
        wait(&mut e, &|e| e.db_results.active_tab().is_some_and(|t| !t.running && !t.rows.is_empty()));
        assert_eq!(e.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("42"));
        keys(&mut e, "<Esc>");
        e.db_new_query_command();
        assert!(e.buf.path.as_ref().unwrap().to_string_lossy().ends_with("shop_query_2.sql"), "番号を進める");
        // 絞り込み
        e.focus_right(RightTab::Database);
        keys(&mut e, "/zzz<CR>");
        assert!(!e.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
        // Vim 風: Esc でノーマル、もう一度 Esc で消す
        keys(&mut e, "/<Esc><Esc>");
        assert!(e.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
    }
}


/// SQL コンソールの上の帯（layout.rs の View）
pub struct ConsoleBarView;

impl crate::layout::View for ConsoleBarView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        if ed.console_row() == 0 {
            return None;
        }
        let (x, w) = ed.windows_x();
        Some(crate::panes::Rect { x, y: ed.tabs_row(), w, h: 1 })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_console_bar(p, ed);
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, row, col, clicks);
        if let Some(item) = ed.console_bar(r.w).into_iter().find(|b| col >= r.x + b.1 && col < r.x + b.1 + b.2).map(|b| b.3) {
            ed.console_bar_press(item);
        }
        crate::layout::Click::Done
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// SQL コンソールの上の帯（▶ 実行・Tx・未コミット・コミット・ロールバック・接続先 › スキーマ）
    pub(crate) fn draw_console_bar(p: &mut Painter, ed: &Editor) {
        let cell = p.fonts.cell();
        let (x, w) = ed.windows_x();
        let (x0, wpx) = (x as f32 * cell.w, w as f32 * cell.w);
        // タブの行の下
        let y0 = ed.tabs_row() as f32 * cell.h;
        p.out.push(Instance::fill(x0, y0, wpx, cell.h, BG));
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, wpx, 1.0, SEPARATOR));
        let hover = ed.hover_bar;
        for (label, bx, bw, item) in ed.console_bar(w) {
            use crate::db_panel::BarItem;
            let color = match item {
                BarItem::Run | BarItem::Commit | BarItem::NewQuery => GIT_ADD,
                BarItem::Rollback => DIAG_ERROR,
                BarItem::Pending => DIAG_WARN,
                BarItem::Label => LINE_NO,
                BarItem::Tx if label.contains("手動") => DIAG_WARN,
                BarItem::Schema => FG,
                BarItem::Tx => STATUS_FG,
            };
            if hover == Some(item) && item != BarItem::Label && item != BarItem::Pending {
                p.out.push(Instance::fill((x + bx) as f32 * cell.w - 3.0, y0 + 1.0, bw as f32 * cell.w + 6.0, cell.h - 2.0, MATCH_BG));
            }
            p.text(&label, x + bx, y0, color);
        }
    }
}
