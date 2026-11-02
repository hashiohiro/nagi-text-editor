//! 右の欄の「データベース」タブ（DataGrip と同じ使い勝手を、ターミナルの見た目で）。
//!
//! - 接続先: Spring の設定（application*.properties）、`~/.config/nagi/databases.txt`（`名前 | jdbc:URL | 利用者`）、
//!   ＋ で足したもの（プロパティで直す・複製・除去もできる）。パスワードはつなぐときに伏せ字で打ち、メモリにだけ置く。
//! - ドライバー: Oracle は Instant Client が要る。無ければ、つなぐ前に「ダウンロードしますか」と聞いて取ってくる
//!   （nagi_workers::drivers）。SQLite は中に組み込んであるので準備は要らない。
//! - 木: 接続先 › スキーマ › テーブル・ビュー・シーケンス・ルーチン（数）› 列・キー・インデックス。
//!   K は主キー、F は外部キー（参照先も出す）。ほかのスキーマは「ほかのスキーマ」の下に、開いたときに読む。
//!   `/` で名前を絞り込む。
//! - コンソール: 接続先ごとの .sql ファイル。上の帯に ▶ 実行・Tx 自動／手動・未コミットの数・コミット／
//!   ロールバック・接続先 › スキーマ。Ctrl+Enter でカーソルのある文を流し、結果は下のパネルのタブに出す。
//!   DataGrip と同じく確かめずに流す（WHERE の無い UPDATE・DELETE だけ確かめる）。手動のときは未コミットの数を数える。

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

/// データソースの作成・プロパティのフォーム
pub struct Form {
    pub kind: Kind,
    /// (見出し, 入力欄)。作るときは 0 番が種類（← → で切り替え）。入力欄は Vim 風
    pub fields: Vec<(&'static str, VimField)>,
    pub at: usize,
    /// プロパティ（直す接続先の番号）。名前・URL・利用者・既定のスキーマをそのまま直す（Spring の設定の URL も表せるように）
    pub edit: Option<usize>,
}

impl Form {
    fn new(kind: Kind) -> Form {
        let mut f = Form { kind, fields: Vec::new(), at: 1, edit: None };
        f.set_kind(kind);
        f
    }

    fn edit(i: usize, ds: &DataSource) -> Form {
        let p = |s: &str| VimField::new(s, false);
        let fields = vec![("名前", p(&ds.name)), ("URL", p(&ds.url)), ("利用者", p(&ds.user)), ("既定のスキーマ", p(&ds.schema))];
        Form { kind: Kind::of(&ds.url).unwrap_or(Kind::Oracle), fields, at: 0, edit: Some(i) }
    }

    /// 種類を ← → で選ぶ行か（作るときの 0 番）
    pub fn kind_row(&self, k: usize) -> bool {
        self.edit.is_none() && k == 0
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
        if self.edit.is_some() {
            let url = self.get("URL");
            if Kind::of(&url).is_none() {
                return Err("URL は jdbc:oracle:・jdbc:postgresql:・jdbc:sqlite:・salesforce: で始めてください".into());
            }
            return Ok(DataSource { name, url, user: self.get("利用者"), origin: config_path().display().to_string(), schema: self.get("既定のスキーマ") });
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
    /// 開いている接続先・スキーマ・グループ・テーブル（DbRow::key。初めは閉じている）
    pub folds: nagi_ui::Folds<String>,
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
    /// 除去した接続先（番号は結果のタブ・コンソールが覚えているので、詰めずに木から外す）
    pub removed: HashSet<usize>,
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
    replace_config_line(path, ds, Some(ds))
}

/// 設定ファイルの `old` の行（名前・URL・利用者が同じ行）を `new` に書き換える（None なら消す）。
/// 無ければ `new` を足す。ほかの行（注釈・空行も）はそのまま
pub fn replace_config_line(path: &std::path::Path, old: &DataSource, new: Option<&DataSource>) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let same = !line.trim_start().starts_with('#')
            && nagi_workers::db::config_sources(line, "").first().is_some_and(|x| x.name == old.name && x.url == old.url && x.user == old.user);
        if same && !found {
            if let Some(ds) = new {
                out.push_str(&config_line(ds));
            }
            found = true;
        } else {
            out.push_str(line);
        }
    }
    if let (false, Some(ds)) = (found, new) {
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

/// 設定ファイル・`:DbAdd` の接続先か（Spring の設定から読んだものでないか）
pub fn from_config(ds: &DataSource) -> bool {
    ds.origin == ":DbAdd" || ds.origin == config_path().display().to_string()
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

/// WHERE の無い UPDATE・DELETE があれば、その文の頭（DataGrip の「安全でないクエリ」の警告と同じ）
pub fn unsafe_statement(sql: &str) -> Option<String> {
    let text: String = sql.lines().map(|l| l.split("--").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    text.split(';').find_map(|s| {
        let words: Vec<String> = s.split_whitespace().map(|w| w.to_ascii_uppercase()).collect();
        let kind = words.first().map(String::as_str);
        (matches!(kind, Some("UPDATE" | "DELETE")) && !words.iter().any(|w| w == "WHERE")).then(|| s.split_whitespace().take(4).collect::<Vec<_>>().join(" "))
    })
}

/// カーソルのある文の範囲（`text` の中の byte）。文は `;` で区切る（DataGrip と同じ）。`;` の無い台本のために空行でも区切る。
/// 文字列・注釈の中の `;` は数えない。頭の注釈と空白は除き、終わりの `;` は含めない。
/// `;` の後（その行の残りが空白だけ）にいれば、その前の文。空行・注釈だけの所なら None
pub fn statement_span(text: &str, cur: usize) -> Option<std::ops::Range<usize>> {
    let b = text.as_bytes();
    let cur = cur.min(text.len());
    // 文の区切り: (この文の終わり, 次の文の始まり, `;` で区切ったか)
    let mut segs: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut line_blank = true;
    let mut line_start = 0;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                line_blank = false;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                line_blank = false;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |k| i + 2 + k + 1);
                line_blank = false;
            }
            b';' => {
                segs.push((start, i, true));
                start = i + 1;
                line_blank = false;
            }
            b'\n' => {
                if line_blank && line_start > start {
                    segs.push((start, line_start, false));
                    start = i + 1;
                } else if line_blank && line_start == start && start > 0 {
                    start = i + 1;
                }
                line_blank = true;
                line_start = i + 1;
            }
            c if !c.is_ascii_whitespace() => line_blank = false,
            _ => {}
        }
        i += 1;
    }
    segs.push((start, text.len(), false));
    // 頭の空白と注釈の行を除き、終わりの空白を除く
    let trim = |(a, e): (usize, usize)| -> Option<std::ops::Range<usize>> {
        let mut a = a;
        loop {
            let rest = &text[a..e];
            let t = rest.trim_start();
            a += rest.len() - t.len();
            if t.starts_with("--") {
                a = text[a..e].find('\n').map_or(e, |k| a + k + 1);
            } else {
                break;
            }
        }
        let body = text[a..e].trim_end();
        (!body.is_empty()).then(|| a..a + body.len())
    };
    let k = segs.iter().position(|&(a, e, _)| a <= cur && cur <= e)?;
    if let Some(r) = trim((segs[k].0, segs[k].1)) {
        // 文の前の空白・注釈の上にいても、その文
        return Some(r);
    }
    // `;` の後ろの、同じ行の空白にいる: その前の文
    let prev = k.checked_sub(1).map(|j| segs[j])?;
    (prev.2 && !text[prev.1..cur].contains('\n')).then(|| trim((prev.0, prev.1))).flatten()
}

/// カーソルの行を含む文（コメントだけの行は除く）
#[cfg(test)]
pub fn statement_at(text: &str, line: usize) -> String {
    let at: usize = text.split_inclusive('\n').take(line).map(str::len).sum();
    let Some(r) = statement_span(text, at) else { return String::new() };
    text[r].lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n").trim().to_string()
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

/// ← の動き（nagi-ui の木）に渡す行: 字下げと、開いているフォルダならその鍵（字下げは前の行から決まるので、行だけでは作れない）
struct TreeLine(usize, Option<String>);

impl nagi_ui::TreeRow for TreeLine {
    type Key = String;
    fn depth(&self) -> usize {
        self.0
    }
    fn folder(&self) -> Option<&String> {
        self.1.as_ref()
    }
}

impl DbPanel {
    /// 木に出す接続先があるか（除去したものを除く）
    pub fn has_sources(&self) -> bool {
        self.sources.len() > self.removed.len()
    }

    pub fn kind(&self, i: usize) -> Option<Kind> {
        Kind::of(&self.sources.get(i)?.url)
    }

    pub fn is_open(&self, r: &DbRow) -> bool {
        r.key().is_some_and(|k| self.folds.is_open(&k)) || (!self.filter_text.is_empty() && matches!(r, DbRow::Group(..)))
    }

    fn matches(&self, name: &str) -> bool {
        self.filter_text.is_empty() || name.to_lowercase().contains(&self.filter_text.to_lowercase())
    }

    pub(crate) fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for i in 0..self.conns.len() {
            if self.removed.contains(&i) {
                continue;
            }
            rows.push(DbRow::Source(i));
            if !self.folds.is_open(&format!("s{i}")) {
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

/// データベースの木のキーで、部品の外に及ぶこと（Editor が行う。ADR 0018 の §5）
#[derive(Clone, Debug)]
pub enum DbAction {
    None,
    /// パスワードを入れてつなぐ（接続先の番号, パスワード）
    Connect(usize, String),
    /// フォームで作った接続先を足してつなぐ
    AddSource(DataSource),
    /// プロパティのフォームで直した接続先（番号, 直した後）
    EditSource(usize, DataSource),
    /// 選んだ行の接続先のプロパティ（フォーム）を開く
    Properties,
    /// 選んだ行の接続先を複製する
    Duplicate,
    /// 選んだ行の接続先を除去する（確かめてから）
    Remove,
    /// 選んだ行で Enter（開く・つなぐ・開閉）
    Activate,
    /// その行を畳む・開く（読み込みが要れば読み込む）
    Toggle(DbRow),
    OpenSelected,
    Ddl,
    /// 新しいクエリ（false）・コンソール（true）を、選んだ接続先で
    Query(bool),
    Disconnect,
    Reload,
    Close,
    ToText,
    /// 接続先を足すフォームを開く（欄が無ければ出す）
    AddForm,
    /// 絞り込みの欄へ
    Filter,
}

/// データベースの欄のアクション（ui_action.rs。ADR 0019）。メニュー・ボタン・パレット・ショートカット（既定は付けない）はここを引く
pub static ACTIONS: [crate::ui_action::UiAction; 13] = {
    use crate::ui_action::{always, Scope::Database, UiAction};
    // 選んでいる行の接続先がつないでいるか
    fn connected(e: &Editor) -> bool {
        e.db_selected().and_then(|r| e.panels.db.as_ref().map(|p| p.conns[r.source()].client.is_some())).unwrap_or(false)
    }
    fn not_connected(e: &Editor) -> bool {
        e.db_selected().is_some() && !connected(e)
    }
    fn selected(e: &Editor) -> bool {
        e.db_selected().is_some()
    }
    fn table(e: &Editor) -> bool {
        matches!(e.db_selected(), Some(DbRow::Object(_, _, _, Group::Tables | Group::Views, _)))
    }
    fn open(e: &Editor) -> bool {
        e.panels.db.is_some()
    }
    // 除去できる接続先か（Spring の設定から読んだものは、その設定を直す）
    fn removable(e: &Editor) -> bool {
        e.db_selected().and_then(|r| e.panels.db.as_ref().map(|p| from_config(&p.sources[r.source()]))).unwrap_or(false)
    }
    [
        UiAction { id: "db.addSource", label: "データソースの作成…", scope: Database, enabled: always, run: |e| e.db_do(DbAction::AddForm) },
        UiAction { id: "db.connect", label: "接続", scope: Database, enabled: not_connected, run: |e| e.db_do(DbAction::Activate) },
        UiAction { id: "db.disconnect", label: "非アクティブ化", scope: Database, enabled: connected, run: |e| e.db_do(DbAction::Disconnect) },
        UiAction { id: "db.reload", label: "リフレッシュ", scope: Database, enabled: connected, run: |e| e.db_do(DbAction::Reload) },
        UiAction { id: "db.newQuery", label: "新規クエリファイルを開く", scope: Database, enabled: selected, run: |e| e.db_do(DbAction::Query(false)) },
        UiAction { id: "db.console", label: "クエリファイルを開く", scope: Database, enabled: selected, run: |e| e.db_do(DbAction::Query(true)) },
        UiAction { id: "db.openData", label: "データの編集", scope: Database, enabled: table, run: |e| e.db_do(DbAction::OpenSelected) },
        UiAction { id: "db.ddl", label: "DDL に移動", scope: Database, enabled: table, run: |e| e.db_do(DbAction::Ddl) },
        UiAction { id: "db.properties", label: "プロパティ", scope: Database, enabled: selected, run: |e| e.db_do(DbAction::Properties) },
        UiAction { id: "db.duplicate", label: "複製", scope: Database, enabled: selected, run: |e| e.db_do(DbAction::Duplicate) },
        UiAction { id: "db.remove", label: "データソースの除去…", scope: Database, enabled: removable, run: |e| e.db_do(DbAction::Remove) },
        UiAction { id: "db.filter", label: "フィルター", scope: Database, enabled: open, run: |e| e.db_do(DbAction::Filter) },
        UiAction { id: "db.close", label: "非表示", scope: Database, enabled: open, run: |e| e.db_do(DbAction::Close) },
    ]
};

impl DbPanel {
    /// データベースの木のキー（`half` は半画面の行数）。パスワード・接続先のフォーム・絞り込み（入力欄は `svc` で動かす）・選び・畳みはここで行い、外に及ぶことを返す
    pub fn key(&mut self, key: Key, half: usize, svc: &mut crate::editor::Services) -> DbAction {
        // パスワードの入力中
        if let Some((i, prompt)) = &mut self.password {
            match key {
                Key::Esc => {
                    self.password = None;
                    svc.notify.info("やめました");
                }
                Key::Enter => {
                    let (i, pass) = (*i, std::mem::take(&mut prompt.text));
                    self.password = None;
                    return DbAction::Connect(i, pass);
                }
                k => {
                    prompt.key(k);
                }
            }
            return DbAction::None;
        }
        // データソースの作成・プロパティのフォーム
        if let Some(f) = &mut self.form {
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
                Key::Left | Key::Right | Key::Char(' ') if f.kind_row(f.at) => {
                    // 名前の順
                    let order = [Kind::Oracle, Kind::Postgres, Kind::Salesforce, Kind::Sqlite];
                    let k = order.iter().position(|k| *k == f.kind).unwrap_or(0);
                    let k = if key == Key::Left { (k + order.len() - 1) % order.len() } else { (k + 1) % order.len() };
                    f.set_kind(order[k]);
                    FieldOut::None
                }
                Key::Esc if f.kind_row(f.at) => FieldOut::Cancel,
                Key::Enter if f.kind_row(f.at) => FieldOut::Submit,
                k => {
                    let at = f.at;
                    f.fields[at].1.feed(k, svc)
                }
            };
            match out {
                FieldOut::Cancel => {
                    self.form = None;
                    svc.notify.info("やめました");
                }
                FieldOut::Submit if f.at + 1 < f.fields.len() => {
                    f.at += 1;
                    f.fields[f.at].1.reinsert();
                }
                FieldOut::Submit => match f.source() {
                    Ok(ds) => {
                        let edit = f.edit;
                        self.form = None;
                        return match edit {
                            Some(i) => DbAction::EditSource(i, ds),
                            None => DbAction::AddSource(ds),
                        };
                    }
                    Err(e) => svc.notify.error(e),
                },
                _ => {}
            }
            return DbAction::None;
        }
        // 絞り込みの入力中（Vim 風。Enter で決める、ノーマルの Esc で消す）
        if let Some(f) = &mut self.filter {
            match f.feed(key, svc) {
                FieldOut::Changed => {
                    self.filter_text = f.text();
                    self.rebuild();
                    self.selected = self.rows.iter().position(|r| matches!(r, DbRow::Object(..))).unwrap_or(self.selected);
                }
                FieldOut::Submit => self.filter = None,
                FieldOut::Cancel => {
                    self.filter = None;
                    self.filter_text.clear();
                    self.rebuild();
                }
                FieldOut::None => {}
            }
            return DbAction::None;
        }
        // 木の上は標準のキーだけ（ADR 0019。操作はアクション: メニュー・＋ ボタン・パレット・付けたショートカット）
        let moved = nagi_ui::list::nav(key, self.selected, self.rows.len(), half * 2);
        match key {
            _ if moved.is_some() => self.selected = moved.unwrap_or(self.selected),
            Key::Enter | Key::Right => return DbAction::Activate,
            Key::Left => {
                // 開いたフォルダなら畳む、それ以外は親へ（木の共通の動き。nagi-ui）
                let rows: Vec<TreeLine> = (0..self.rows.len()).map(|k| TreeLine(self.depth(k), self.rows[k].key().filter(|_| self.is_open(&self.rows[k])))).collect();
                // 開いているフォルダにだけ鍵を入れたので、鍵のあるものは開いているとみなす
                match nagi_ui::tree::left(&rows, self.selected, &nagi_ui::Folds::open()) {
                    nagi_ui::Step::Fold(..) => {
                        if let Some(r) = self.rows.get(self.selected).cloned() {
                            return DbAction::Toggle(r);
                        }
                    }
                    nagi_ui::Step::Select(j) => self.selected = j,
                    nagi_ui::Step::None => {}
                }
            }
            Key::Esc => return DbAction::ToText,
            _ => {}
        }
        DbAction::None
    }

    /// 接続先を足すフォームを開く
    pub fn open_form(&mut self) {
        self.form = Some(Form::new(Kind::Oracle));
        if let Some(f) = &mut self.form {
            f.fields[f.at].1.reinsert();
        }
    }

    /// 接続先 `i` のプロパティのフォームを開く
    pub fn open_properties(&mut self, i: usize) {
        let Some(ds) = self.sources.get(i) else { return };
        let mut f = Form::edit(i, ds);
        f.fields[0].1.reinsert();
        self.form = Some(f);
    }

    /// 絞り込みの欄へ
    pub fn open_filter(&mut self, svc: &mut crate::editor::Services) {
        if self.filter.is_none() {
            self.filter = Some(VimField::new(&self.filter_text, false));
        }
        svc.notify.info("絞り込み: 名前の一部を打つ（Enter で決める・Esc 2 回で消す）");
    }
}

impl Editor {
    /// `:Db`（ActivateDatabaseToolWindow）: データベースのタブを出して移る
    pub(crate) fn open_db_panel(&mut self) {
        if self.panels.db.is_none() {
            let mut sources = nagi_workers::db::detect_sources(&self.nav_root());
            let cfg = config_path();
            if let Ok(t) = std::fs::read_to_string(&cfg) {
                for s in nagi_workers::db::config_sources(&t, &cfg.display().to_string()) {
                    match sources.iter_mut().find(|x| x.name == s.name) {
                        // Spring の設定から見つけた接続先と同じ名前なら、設定ファイルの行で上書きする（プロパティで直したもの・既定のスキーマ）。
                        // Spring の設定のファイルは変えない
                        Some(x) => *x = s,
                        None => sources.push(s),
                    }
                }
            }
            let conns = sources.iter().map(|_| Conn::default()).collect();
            let mut p = DbPanel {
                sources,
                conns,
                folds: nagi_ui::Folds::closed(),
                rows: Vec::new(),
                selected: 0,
                scroll: 0,
                filter_text: String::new(),
                filter: None,
                password: None,
                setup_ask: None,
                download: None,
                form: None,
                removed: HashSet::new(),
                consoles: HashMap::new(),
                then: HashMap::new(),
            };
            p.rebuild();
            self.panels.db = Some(p);
        }
        if self.panels.db.as_ref().is_some_and(|p| !p.has_sources()) {
            self.svc.notify.info("接続先がありません。欄の上の ＋ で足せます");
        }
        self.focus_right(RightTab::Database);
    }

    /// `:DbAdd`（引数なしならフォーム、`名前 jdbc:URL [利用者]` ならそのまま足す）
    pub(crate) fn db_add(&mut self, arg: &str) {
        self.open_db_panel();
        let f: Vec<&str> = arg.split_whitespace().collect();
        let Some(p) = &mut self.panels.db else { return };
        let (Some(name), Some(url)) = (f.first(), f.get(1).filter(|u| Kind::of(u).is_some())) else {
            p.form = Some(Form::new(Kind::Oracle));
            return;
        };
        let ds = DataSource { name: name.to_string(), url: url.to_string(), user: f.get(2).unwrap_or(&"").to_string(), origin: ":DbAdd".into(), schema: String::new() };
        self.db_push_source(ds, false);
    }

    fn db_push_source(&mut self, ds: DataSource, save: bool) {
        if self.panels.db.is_none() {
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
                Ok(()) => self.svc.notify.info(format!("{} を {} に保存しました", ds.name, cfg.display())),
                Err(e) => self.svc.notify.error(format!("保存できません: {e}")),
            }
        } else {
            self.svc.notify.info(format!("接続先 {} を足しました", ds.name));
        }
        let Some(p) = &mut self.panels.db else { return };
        p.sources.push(ds);
        p.conns.push(Conn::default());
        p.rebuild();
        p.selected = p.rows.iter().position(|r| *r == DbRow::Source(p.sources.len() - 1)).unwrap_or(0);
    }

    /// プロパティで直した接続先 `i` を設定ファイルに書く（Spring の設定から読んだものは、同じ名前の行で上書きする）。
    /// URL・利用者が変わったら、今の接続は切る（次につなぐときに新しい値で）
    fn db_update_source(&mut self, i: usize, ds: DataSource) {
        let Some(p) = &mut self.panels.db else { return };
        let Some(old) = p.sources.get(i).cloned() else { return };
        if p.sources.iter().enumerate().any(|(k, x)| k != i && !p.removed.contains(&k) && x.name == ds.name) {
            self.svc.notify.error(format!("{} という名前の接続先がもうあります", ds.name));
            p.open_properties(i);
            if let Some(f) = &mut p.form {
                f.fields = Form::edit(i, &ds).fields;
            }
            return;
        }
        let cfg = config_path();
        // Spring の設定の接続先は、設定ファイルに行が無いので足す
        if let Err(e) = replace_config_line(&cfg, &old, Some(&ds)) {
            self.svc.notify.error(format!("設定ファイルに書けません: {e}"));
            return;
        }
        let changed = old.url != ds.url || old.user != ds.user;
        p.sources[i] = ds.clone();
        p.rebuild();
        if changed && p.conns[i].client.is_some() {
            self.db_close(i, false);
        }
        self.svc.notify.info(format!("{} を保存しました（{}）", ds.name, cfg.display()));
    }

    /// 接続先 `i` を複製する（名前に [2] などを付けて設定ファイルに足す。つながない）
    fn db_duplicate(&mut self, i: usize) {
        let Some(p) = &self.panels.db else { return };
        let Some(src) = p.sources.get(i) else { return };
        let taken = |n: &str| p.sources.iter().enumerate().any(|(k, x)| !p.removed.contains(&k) && x.name == n);
        let name = (2..).map(|n| format!("{} [{n}]", src.name)).find(|n| !taken(n)).unwrap_or_default();
        let ds = DataSource { name, origin: config_path().display().to_string(), ..src.clone() };
        self.db_push_source(ds, true);
    }

    /// 接続先 `i` を除去してよいか聞く（コミットしていない変更があれば先に）
    fn db_ask_remove(&mut self, i: usize) {
        let Some(p) = &self.panels.db else { return };
        let Some(src) = p.sources.get(i) else { return };
        if !from_config(src) {
            self.svc.notify.info(format!("{} は Spring の設定（{}）から読んでいます。除去はその設定で", src.name, src.origin));
            return;
        }
        if p.conns[i].pending > 0 {
            self.svc.notify.info("まだコミットしていない変更があります。先にコミットかロールバックを");
            return;
        }
        let q = format!("データソース {} を除去しますか", src.name);
        self.ask_or(q, move |e| e.db_remove(i), |e| e.svc.notify.info("やめました"));
    }

    fn db_remove(&mut self, i: usize) {
        let Some(p) = &mut self.panels.db else { return };
        let ds = p.sources[i].clone();
        let cfg = config_path();
        if ds.origin != ":DbAdd" {
            if let Err(e) = replace_config_line(&cfg, &ds, None) {
                self.svc.notify.error(format!("設定ファイルに書けません: {e}"));
                return;
            }
        }
        // 裏のスレッドは送り口が無くなると接続を閉じる
        p.conns[i] = Conn::default();
        p.then.remove(&i);
        p.consoles.retain(|_, k| *k != i);
        p.removed.insert(i);
        p.rebuild();
        self.svc.notify.info(format!("{} を除去しました", ds.name));
    }

    /// つなぐ（準備ができていなければダウンロードを聞く。パスワードが要ればそれを聞く）
    fn db_begin_connect(&mut self, i: usize) {
        let Some(p) = &mut self.panels.db else { return };
        let s = p.sources[i].clone();
        if Kind::of(&s.url) == Some(Kind::Oracle) && !drivers::oracle_ready() {
            // 何を・どこへ・どの条件で置くかは右の欄に出し、確認の窓で答える
            p.setup_ask = Some(i);
            let q = format!("Oracle のドライバー（約 {} MB）をダウンロードしますか", drivers::ORACLE_CLIENT_SIZE_MB);
            self.ask_or(q, move |e| e.db_download_driver(i), |e| {
                if let Some(p) = &mut e.panels.db {
                    p.setup_ask = None;
                }
                e.svc.notify.info("やめました");
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
        self.svc.notify.info(format!("{} のパスワード（{}。Enter でつなぐ・Esc でやめる）", s.name, s.url));
    }

    fn db_connect(&mut self, i: usize, pass: String) {
        self.db_connect_with(i, pass, false);
    }

    /// saved: 保存したパスワードか（違えば、つながったときに保存する）
    fn db_connect_with(&mut self, i: usize, pass: String, saved: bool) {
        let waker = self.waker();
        let Some(p) = &mut self.panels.db else { return };
        let s = p.sources[i].clone();
        p.conns[i] = Conn::default();
        p.folds.set_open(&format!("s{i}"), true);
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
            Ok(m) => self.svc.notify.info(m),
            Err(e) => self.svc.notify.error(e),
        }
    }

    /// 開いたところの中身を読む
    fn db_ensure(&mut self, r: &DbRow) {
        let Some(p) = &mut self.panels.db else { return };
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
            let Some(p) = &mut self.panels.db else { return };
            let open = !p.folds.is_open(&k);
            p.folds.set_open(&k, open);
            open
        };
        if opened {
            self.db_ensure(r);
        }
        if let Some(p) = &mut self.panels.db {
            p.rebuild();
        }
    }

    /// 裏の応答を受け取る。変わったら true
    pub fn poll_db(&mut self) -> bool {
        let mut changed = false;
        // ダウンロード
        let dl = self.panels.db.as_ref().and_then(|p| p.download.as_ref().map(|(i, d, _)| (*i, d.poll())));
        if let Some((i, polled)) = dl.filter(|(_, x)| x.status.is_some() || x.done.is_some()) {
            changed = true;
            let p = self.panels.db.as_mut().unwrap();
            if let (Some(text), Some(d)) = (polled.status, &mut p.download) {
                d.2 = text;
            }
            if let Some(r) = polled.done {
                p.download = None;
                match r {
                    Ok(dir) => {
                        self.svc.notify.info(format!("Oracle のドライバーを {} に置きました", dir.display()));
                        self.db_log_line(&format!("ドライバー: Oracle Instant Client を {} に置きました", dir.display()), false);
                        self.db_begin_connect(i);
                    }
                    Err(e) => {
                        p.conns[i].error = Some(e.to_string());
                        self.svc.notify.error(e);
                    }
                }
            }
            if let Some(p) = &mut self.panels.db {
                p.rebuild();
            }
        }
        let Some(p) = &mut self.panels.db else { return changed };
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
            if let Some(p) = &mut self.panels.db {
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
            self.svc.notify.info(s.clone());
            self.db_log_line(&s, false);
            return;
        }
        if let Reply::Reconnected { tx_lost } = r {
            self.db_reconnected(i, tx_lost);
            return;
        }
        let Some(p) = &mut self.panels.db else { return };
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
                p.folds.set_open(&format!("c{i}:{schema}"), true);
                p.folds.set_open(&format!("g{i}:{schema}:Tables"), true);
                // 入れたパスワードは保存する（次から聞かない。:DbLogout で消せる）
                let mut saved = String::new();
                if let Some((pass, false)) = c.pass.take().filter(|(x, _)| !x.is_empty()) {
                    let s = &p.sources[i];
                    saved = match nagi_workers::db::save_password(&s.url, &s.user, &pass) {
                        Ok(()) => "。パスワードを保存しました".into(),
                        Err(e) => format!("。パスワードを保存できません: {e}"),
                    };
                }
                self.svc.notify.info(format!("{name} につなぎました（{product}、{schema}）{saved}"));
                self.db_log_line(&format!("{name} につなぎました（{product}、{schema}）{saved}"), false);
                if let Some(sql) = self.panels.db.as_mut().and_then(|p| p.then.remove(&i)) {
                    self.db_execute(i, sql);
                }
            }
            (Some(Expect::Connect), Reply::Error(e)) => {
                c.client = None;
                c.error = Some(e.clone());
                // 保存したパスワードで失敗した: 入れ直してもらう（つながったら上書きする）
                if c.pass.take().is_some_and(|(_, saved)| saved) {
                    p.password = Some((i, Prompt::default()));
                    self.svc.notify.error(format!("{name} に保存したパスワードでつなげません: {e}。パスワードを入れ直してください（Enter でつなぐ・Esc でやめる）"));
                } else {
                    self.svc.notify.error(format!("{name} につなげません: {e}"));
                }
                self.db_log_line(&format!("{name} につなげません: {e}"), true);
            }
            (Some(Expect::Schemas), Reply::Schemas(s)) => c.schemas = Some(s),
            (Some(Expect::Objects(s)), Reply::Objects(o)) => {
                c.objects.insert(s, o);
            }
            (Some(Expect::Columns(s, t)), Reply::Columns(cols)) => {
                if let Some(te) = &mut self.panels.db_table {
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
                    self.svc.notify.error(format!("SQL の誤り: {}", e.lines().next().unwrap_or("")));
                }
                // テーブルを作ったり消したりしたかもしれないので、木を読み直す
                if ok && changes {
                    if let Some(c) = self.panels.db.as_mut().map(|p| &mut p.conns[i]) {
                        let s = c.schema.clone();
                        c.send(Req::Objects(s.clone()), Expect::Objects(s));
                    }
                }
                match target {
                    Target::Result(id) => {
                        let rows = matches!(reply, Reply::Rows { .. });
                        self.panels.db_results.reply(id, reply);
                        if rows {
                            self.result_find_target(i, id);
                        }
                    }
                    Target::Table => {
                        if let Some(te) = &mut self.panels.db_table {
                            te.reply(reply);
                        }
                    }
                }
            }
            (Some(Expect::Count(target)), Reply::Count(n)) => match target {
                Target::Result(id) => self.panels.db_results.set_total(id, n),
                Target::Table => {
                    if let Some(te) = &mut self.panels.db_table {
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
                    self.svc.notify.info(format!("{n} 件の変更を送信しました{}", if manual { "（まだコミットしていません）" } else { "" }));
                    let what = if target == Target::Table { "テーブルの編集" } else { "結果の編集" };
                    self.db_log(&name, &format!("{n} 件の変更（{what}）"), &format!("{ms} ms"), false);
                    match target {
                        Target::Table => self.db_table_reload(),
                        Target::Result(id) => self.panels.db_results.committed(id, format!("{n} 行を直しました · {ms} ms")),
                    }
                }
                Reply::Error(e) => {
                    self.svc.notify.error(format!("送信できません: {}", e.lines().next().unwrap_or("")));
                    let what = if target == Target::Table { "テーブルの編集" } else { "結果の編集" };
                    self.db_log(&name, &format!("{n} 件の変更（{what}）"), e, true);
                    if let Some(te) = &mut self.panels.db_table {
                        te.busy = false;
                    }
                }
                _ => {}
            },
            (Some(Expect::Tx), Reply::Error(e)) | (Some(Expect::SetSchema(_)), Reply::Error(e)) | (Some(Expect::Ddl(_)), Reply::Error(e)) => {
                self.svc.notify.info(format!("{name}: {}", e.lines().next().unwrap_or("")));
                self.db_log_line(&format!("{name}: {e}"), true);
            }
            (Some(Expect::Tx), _) => {}
            (Some(Expect::Commit(commit, n)), reply) => {
                let what = if commit { "コミット" } else { "ロールバック" };
                if let Reply::Error(e) = &reply {
                    self.svc.notify.error(format!("{name}: {what}できません: {}", e.lines().next().unwrap_or("")));
                    self.db_log_line(&format!("{name}: {what}できません: {e}"), true);
                    return;
                }
                c.pending = c.pending.saturating_sub(n);
                self.svc.notify.info(format!("{what}しました（{n} 件）"));
                self.db_log_line(&format!("{what}しました（{n} 件）"), false);
                // 結果の表で直して送った値: ロールバックなら前の値に戻して読み直す
                for id in self.panels.db_results.tx_ended(i, commit) {
                    self.db_requery(id);
                }
            }
            (Some(Expect::SetSchema(s)), _) => {
                c.schema = s.clone();
                if !c.objects.contains_key(&s) {
                    c.send(Req::Objects(s.clone()), Expect::Objects(s.clone()));
                }
                p.folds.set_open(&format!("c{i}:{s}"), true);
                p.folds.set_open(&format!("g{i}:{s}:Tables"), true);
                self.svc.notify.info(format!("スキーマを {s} にしました"));
            }
            (Some(Expect::Ddl(t)), Reply::Ddl(text)) => self.db_open_text(&format!("{}.ddl.sql", safe_name(&t)), &text),
            (_, Reply::Error(e)) => {
                self.svc.notify.info(format!("{name}: {}", e.lines().next().unwrap_or("")));
            }
            _ => {}
        }
    }

    /// 出力のタブに 1 行書く
    pub(crate) fn db_log_line(&mut self, text: &str, error: bool) {
        self.panels.db_results.log(String::new(), text.to_string(), String::new(), error);
    }

    fn db_log(&mut self, src: &str, sql: &str, outcome: &str, error: bool) {
        let first = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        self.panels.db_results.log(src.to_string(), first, outcome.to_string(), error);
    }

    /// 文字をファイルに書いて開く（DDL・書き出し）
    pub(crate) fn db_open_text(&mut self, file: &str, text: &str) {
        let dir = work_dir().join("consoles").join("out");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(file);
        if let Err(e) = std::fs::write(&path, text) {
            self.svc.notify.error(format!("書けません: {e}"));
            return;
        }
        self.edit_file(path.clone());
        self.set_state(State::Normal);
        self.svc.notify.info(format!("{} に書きました", path.display()));
    }

    /// SQL を流す（つないでいなければつないでから。WHERE の無い UPDATE・DELETE だけ確かめてから）
    pub(crate) fn db_execute(&mut self, i: usize, sql: String) {
        if sql.trim().is_empty() {
            self.svc.notify.info("流す文がありません（カーソルを文の上に置いてください）");
            return;
        }
        let Some(p) = &mut self.panels.db else { return };
        if !p.conns[i].connected {
            p.then.insert(i, sql);
            if p.conns[i].client.is_none() {
                self.focus_right(RightTab::Database);
                self.db_begin_connect(i);
            }
            return;
        }
        // DataGrip と同じく確かめずに流す。WHERE の無い UPDATE・DELETE（全部の行に及ぶ）だけ確かめる
        if let Some(head) = unsafe_statement(&sql) {
            let q = format!("WHERE の無い文です（{head} …）。{} の全部の行に及びます。実行しますか", p.sources[i].name);
            self.ask_or(q, move |e| e.db_run(i, sql), |e| e.svc.notify.info("実行をやめました"));
            return;
        }
        self.db_run(i, sql);
    }

    fn db_run(&mut self, i: usize, sql: String) {
        let Some(p) = &self.panels.db else { return };
        let changes = !is_read_only(&sql);
        let first = sql.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        let title = format!("{} · {first}", p.sources[i].name);
        let sf = p.kind(i) == Some(Kind::Salesforce);
        let id = self.panels.db_results.new_tab(i, &title, &sql, !changes && !sf);
        let d = self.db_dialect(i);
        if let Some(t) = self.panels.db_results.tabs.iter_mut().find(|t| t.id == id) {
            t.dialect = d;
        }
        let c = &mut self.panels.db.as_mut().unwrap().conns[i];
        c.send(Req::Query { sql: sql.clone(), max: MAX_ROWS, offset: 0 }, Expect::Query(Target::Result(id), sql, changes));
        self.show_results();
    }

    /// 結果のタブを、絞り込み・並べ替え・ページを変えて読み直す
    pub(crate) fn db_requery(&mut self, id: u64) {
        let Some(t) = self.panels.db_results.tab(id) else { return };
        let (i, sql, count, offset) = (t.src, t.effective_sql(), t.count_sql(), t.offset);
        let Some(c) = self.panels.db.as_mut().and_then(|p| p.conns.get_mut(i)) else { return };
        if !c.connected {
            self.svc.notify.info("まだつないでいません");
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
        let Some(p) = &mut self.panels.db else { return };
        p.setup_ask = None;
        p.download = Some((i, drivers::download_oracle(waker), "ダウンロードを始めます…".into()));
        p.folds.set_open(&format!("s{i}"), true);
        p.rebuild();
        self.svc.notify.info("Oracle のドライバーをダウンロードしています…");
    }

    /// 今のバッファが SQL コンソールなら、その接続先
    /// 接続先 `i` の方言
    pub(crate) fn db_dialect(&self, i: usize) -> &'static nagi_lang::sql::Dialect {
        nagi_lang::sql::dialect(self.panels.db.as_ref().and_then(|p| p.kind(i)))
    }

    pub fn console_source(&self) -> Option<usize> {
        let path = self.core.buf.path.as_deref().and_then(|p| std::path::absolute(p).ok())?;
        self.panels.db.as_ref()?.consoles.get(&path).copied()
    }

    /// コンソールでカーソルのある文の行の範囲（start..end）
    pub fn console_statement(&self) -> Option<(usize, usize)> {
        let (_, l0, a, e) = self.console_statement_text()?;
        let text = self.console_window(l0);
        let line = |b: usize| l0 + text[..b].matches('\n').count();
        Some((line(a), line(e.saturating_sub(1).max(a)) + 1))
    }

    /// カーソルのまわり（前後 200 行）の本文と、その始まりの行
    fn console_window(&self, l0: usize) -> String {
        let hi = (self.core.cursor.line + 200).min(self.core.buf.line_count());
        let (a, b) = (self.core.buf.line_start(l0), if hi < self.core.buf.line_count() { self.core.buf.line_start(hi) } else { self.core.buf.len_bytes() });
        self.core.buf.slice_string(a, b.max(a))
    }

    /// コンソールでカーソルのある文（`;` で区切る）: (文, まわりの始まりの行, まわりの中の byte の範囲)
    pub(crate) fn console_statement_text(&self) -> Option<(String, usize, usize, usize)> {
        self.console_source()?;
        let l0 = self.core.cursor.line.saturating_sub(200);
        let text = self.console_window(l0);
        let cur = self.core.buf.line_start(self.core.cursor.line) + self.core.cursor.byte - self.core.buf.line_start(l0);
        let r = statement_span(&text, cur)?;
        Some((text[r.clone()].to_string(), l0, r.start, r.end))
    }

    /// Ctrl+Enter（コンソールで）: カーソルのある文、Visual なら選んだ範囲を流す
    pub(crate) fn db_run_at_cursor(&mut self) {
        let Some(i) = self.console_source() else { return };
        let sql = if let State::Visual { .. } = self.state() {
            let (a, b, _) = self.visual_range();
            let s = self.core.buf.slice_string(a, b);
            self.set_state(State::Normal);
            s
        } else {
            match self.console_statement_text() {
                Some((sql, ..)) => sql.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n").trim().to_string(),
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
        if self.panels.db.as_ref().and_then(|p| p.kind(i)) == Some(Kind::Salesforce) {
            self.svc.notify.info("Salesforce にトランザクションはありません");
            return;
        }
        let Some(c) = self.panels.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.svc.notify.info("まだつないでいません");
            return;
        }
        if c.manual && c.pending > 0 {
            self.svc.notify.info(format!("まだコミットしていない変更が {} 件あります。先にコミットかロールバックを", c.pending));
            return;
        }
        c.manual = !c.manual;
        let m = c.manual;
        c.send(Req::AutoCommit(!m), Expect::Tx);
        self.svc.notify.info(if m { "Tx: 手動（コミットするまで確定しません）" } else { "Tx: 自動" });
        self.db_log_line(&format!("トランザクション: {}", if m { "手動" } else { "自動" }), false);
    }

    pub(crate) fn db_commit(&mut self, i: usize, commit: bool) {
        let Some(c) = self.panels.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            return;
        }
        // 変更の数は、答えが届いてから減らす（その前に接続が切れていたら、失われた数として知らせる）
        let n = c.pending;
        c.send(if commit { Req::Commit } else { Req::Rollback }, Expect::Commit(commit, n));
    }

    /// 裏で接続が切れていたので、つなぎ直した（手動の Tx なら、コミットしていない変更は失われた）
    fn db_reconnected(&mut self, i: usize, tx_lost: bool) {
        let Some(p) = &mut self.panels.db else { return };
        let name = p.sources[i].name.clone();
        let c = &mut p.conns[i];
        let n = if tx_lost { std::mem::take(&mut c.pending) } else { 0 };
        if n > 0 {
            self.svc.notify.error(format!("{name} の接続が切れていたので、つなぎ直しました。コミットしていない {n} 件の変更は失われました"));
            self.db_log_line(&format!("{name} の接続が切れていたので、つなぎ直しました。コミットしていない {n} 件の変更は失われました"), true);
        } else {
            self.svc.notify.info(format!("{name} の接続が切れていたので、つなぎ直しました"));
            self.db_log_line(&format!("{name} の接続が切れていたので、つなぎ直しました"), false);
        }
        if tx_lost {
            // 結果の表で直して送った値は、ロールバックと同じく前の値に戻して読み直す
            for id in self.panels.db_results.tx_ended(i, false) {
                self.db_requery(id);
            }
        }
    }

    /// 今の接続先（コンソールの接続先、無ければ木で選んでいる接続先）
    fn db_current_source(&self) -> Option<usize> {
        self.console_source().or_else(|| self.panels.db.as_ref().and_then(|p| p.rows.get(p.selected)).map(|r| r.source()))
    }

    /// `:DbLogout`: Salesforce の保存したログインを消して切る（ほかの接続先は、保存したパスワードを消す）
    pub(crate) fn db_logout(&mut self) {
        match self.db_current_source() {
            Some(i) => self.db_forget_login(i),
            None => self.svc.notify.info("接続先がありません"),
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
        let Some(p) = &self.panels.db else { return };
        let n = p.conns[i].pending;
        if n > 0 {
            let q = format!("まだコミットしていない変更が {n} 件あります。捨てて{}か", if again { "つなぎ直します" } else { "切ります" });
            self.ask_or(q, move |e| e.db_close_now(i, again), |e| e.svc.notify.info("やめました"));
            return;
        }
        self.db_close_now(i, again);
    }

    fn db_close_now(&mut self, i: usize, again: bool) {
        let Some(p) = &mut self.panels.db else { return };
        // 裏のスレッドは、送り口が無くなると今の頼みごとを終えてから接続を閉じる（コミットしていない変更は取り消される）
        p.conns[i] = Conn::default();
        p.then.remove(&i);
        p.folds.set_open(&format!("s{i}"), false);
        p.rebuild();
        p.selected = p.rows.iter().position(|r| *r == DbRow::Source(i)).unwrap_or(0);
        let name = p.sources[i].name.clone();
        if again {
            self.db_begin_connect(i);
        } else {
            self.svc.notify.info(format!("{name} を切りました"));
        }
    }

    /// 接続先 `i` の木を読み直す（覚えたテーブル・列・索引を捨てて、今のスキーマから）
    fn db_reload(&mut self, i: usize) {
        let Some(p) = &mut self.panels.db else { return };
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
            self.svc.notify.info("読み直しています…");
        }
        p.rebuild();
    }

    /// 接続先 `i` の保存したパスワード（Salesforce はログイン）を消す
    fn db_forget_login(&mut self, i: usize) {
        let Some(p) = &mut self.panels.db else { return };
        let s = p.sources[i].clone();
        if p.kind(i) != Some(Kind::Salesforce) {
            let had = nagi_workers::db::forget_password(&s.url, &s.user);
            self.svc.notify.info(if had { format!("{} の保存したパスワードを消しました", s.name) } else { "保存したパスワードはありません".to_string() });
            return;
        }
        let had = nagi_workers::salesforce::forget(&s.url, &s.user);
        p.conns[i] = Conn::default();
        p.rebuild();
        self.svc.notify.info(if had { format!("{} の保存したログインを消しました", s.name) } else { "保存したログインはありません".to_string() });
    }

    /// `:DbSchema 名前`
    pub(crate) fn db_set_schema(&mut self, s: &str) {
        match self.db_current_source() {
            Some(i) => self.db_switch_schema(i, s),
            None => self.svc.notify.info("接続先がありません"),
        }
    }

    /// 接続先 `i` のスキーマを切り替える（コンソール・木は切り替えた先を見る）
    fn db_switch_schema(&mut self, i: usize, s: &str) {
        let Some(c) = self.panels.db.as_mut().map(|p| &mut p.conns[i]) else { return };
        if !c.connected {
            self.svc.notify.info("まだつないでいません");
            return;
        }
        let s = s.trim().to_string();
        c.send(Req::SetSchema(s.clone()), Expect::SetSchema(s));
    }

    /// 接続先 `i` の既定のスキーマを決める（空なら外す）。設定ファイルに書き、つないでいれば切り替える
    pub(crate) fn db_set_default_schema(&mut self, i: usize, schema: &str) {
        let Some(p) = &mut self.panels.db else { return };
        let Some(ds) = p.sources.get_mut(i) else { return };
        ds.schema = schema.trim().to_string();
        let ds = ds.clone();
        let cfg = config_path();
        if let Err(e) = save_config_line(&cfg, &ds) {
            self.svc.notify.error(format!("設定ファイルに書けません: {e}"));
            return;
        }
        p.rebuild();
        let c = &mut p.conns[i];
        let switch = c.connected && !ds.schema.is_empty() && !ds.schema.eq_ignore_ascii_case(&c.schema) && !matches!(Kind::of(&ds.url), Some(Kind::Sqlite | Kind::Salesforce));
        if switch {
            c.send(Req::SetSchema(ds.schema.clone()), Expect::SetSchema(ds.schema.clone()));
        }
        self.svc.notify.info(if ds.schema.is_empty() {
            format!("{} の既定のスキーマを外しました（{}）", ds.name, cfg.display())
        } else {
            format!("{} の既定のスキーマを {} にしました（つないだら切り替える。{}）", ds.name, ds.schema, cfg.display())
        });
    }

    /// `:DbDefaultSchema [名前]`: 今の接続先の既定のスキーマ（名前を省くと外す）
    pub(crate) fn db_default_schema_command(&mut self, s: &str) {
        match self.db_current_source() {
            Some(i) => self.db_set_default_schema(i, s),
            None => self.svc.notify.info("接続先がありません"),
        }
    }

    /// スキーマの候補（:DbSchema の補完）
    pub fn db_schema_names(&self) -> Vec<String> {
        self.db_current_source().and_then(|i| self.panels.db.as_ref()?.conns.get(i)?.schemas.clone()).unwrap_or_default()
    }

    /// `:DbCommit` / `:DbRollback` / `:DbTx`
    pub(crate) fn db_tx_command(&mut self, what: &str) {
        let Some(i) = self.db_current_source() else {
            self.svc.notify.info("接続先がありません");
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
        let Some(p) = &mut self.panels.db else { return };
        let s = p.sources[i].clone();
        let dir = work_dir().join("consoles");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.sql", safe_name(&s.name)));
        if !path.exists() {
            let _ = std::fs::write(&path, format!("-- {}  {}\n-- Ctrl+Enter でカーソルのある文（; で区切る）を実行。選んでいれば選んだ範囲\n\n", s.name, s.url));
        }
        let abs = std::path::absolute(&path).unwrap_or(path.clone());
        p.consoles.insert(abs, i);
        self.edit_file(path);
        self.set_state(State::Normal);
        if let Some(sql) = initial {
            let end = self.core.buf.len_bytes();
            let text = self.core.buf.text();
            let nl = if text.is_empty() || text.ends_with("\n\n") { "" } else if text.ends_with('\n') { "\n" } else { "\n\n" };
            self.core.hist.begin();
            self.edit(end, end, &format!("{nl}{sql}\n"));
            self.core.hist.close();
            self.core.cursor.line = self.core.buf.line_count().saturating_sub(2);
            self.core.cursor.byte = 0;
            self.clamp_normal();
        }
    }

    /// 新しいクエリ（DataGrip の New Query Console）: 接続先の新しい .sql を Insert で開く。Ctrl+Enter で流す
    pub(crate) fn db_new_query(&mut self, i: usize, initial: Option<String>) {
        let Some(p) = &mut self.panels.db else { return };
        let s = p.sources[i].clone();
        let dir = work_dir().join("consoles");
        let _ = std::fs::create_dir_all(&dir);
        let base = safe_name(&s.name);
        let n = (1..).find(|n| !dir.join(format!("{base}_query_{n}.sql")).exists()).unwrap_or(1);
        let path = dir.join(format!("{base}_query_{n}.sql"));
        let body = format!("-- {}  クエリ {n}\n\n{}", s.name, initial.map(|q| format!("{q}\n")).unwrap_or_default());
        if let Err(e) = std::fs::write(&path, body) {
            self.svc.notify.error(format!("クエリのファイルを作れません: {e}"));
            return;
        }
        let abs = std::path::absolute(&path).unwrap_or(path.clone());
        p.consoles.insert(abs, i);
        self.edit_file(path);
        self.set_state(State::Normal);
        // 最後の行の終わりから Insert で書き始める
        self.key(Key::Char('G'));
        self.key(Key::Char('A'));
        self.svc.notify.info(format!("新しいクエリ（{}）。Ctrl+Enter で実行", s.name));
    }

    /// `:DbNew`: 今の接続先（コンソール・木で選んでいるもの。無ければ最初の接続先）で新しいクエリ
    pub(crate) fn db_new_query_command(&mut self) {
        if self.panels.db.is_none() {
            self.open_db_panel();
        }
        let i = self.db_current_source().or_else(|| self.panels.db.as_ref().and_then(|p| (0..p.sources.len()).find(|i| !p.removed.contains(i))));
        match i {
            Some(i) => self.db_new_query(i, None),
            None => self.svc.notify.info("接続先がありません。データベースの欄の ＋ で足せます"),
        }
    }

    fn db_selected(&self) -> Option<DbRow> {
        let p = self.panels.db.as_ref()?;
        p.rows.get(p.selected).cloned()
    }

    /// Enter: 接続先はつなぐか開け閉め、ほかは開け閉め
    fn db_activate(&mut self) {
        let Some(r) = self.db_selected() else { return };
        if let DbRow::Source(i) = r {
            let p = self.panels.db.as_ref().unwrap();
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
            Some(DbRow::Object(i, _, t, Group::Tables | Group::Views, _)) if self.panels.db.as_ref().and_then(|p| p.kind(i)) == Some(Kind::Salesforce) => {
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
        if let Some(c) = self.panels.db.as_mut().map(|p| &mut p.conns[i]) {
            let kind = if g == Group::Views { "VIEW" } else { "TABLE" };
            c.send(Req::Ddl(s.to_string(), t.to_string(), kind.into()), Expect::Ddl(format!("{s}.{t}")));
        }
    }

    /// 木の右クリック（欄の中の行。0 行目はタブ、1 行目は絞り込み）。押した行を選んでから、その行のメニューを出す（DataGrip と同じ操作）。
    /// `at` はメニューを出す画面の位置。出したら true
    pub(crate) fn db_context(&mut self, prow: usize, at: (usize, usize)) -> bool {
        let Some(p) = &mut self.panels.db else { return false };
        // 右クリックは押した行を選ぶだけ（決めない）
        let len = p.rows.len();
        if prow < 2 {
            return false;
        }
        // 行の無い所（接続先が無いときを含む）: 接続先を足すだけのメニュー
        if nagi_ui::list::pick_row(&mut p.selected, p.scroll, prow, 2, len, 1).is_none() {
            self.focus_right(RightTab::Database);
            let m = crate::menu::Menu::new(at.0, at.1).action(self, "db.addSource");
            self.open_menu(m);
            return true;
        }
        let row = p.rows[p.selected].clone();
        let i = row.source();
        // SQL を作る項目のために、列をいまのうちに読み始める
        if let DbRow::Object(_, s, t, Group::Tables | Group::Views, _) = &row {
            p.conns[i].ensure_columns(s, t);
        }
        self.focus_right(RightTab::Database);
        let Some(p) = &self.panels.db else { return false };
        let src = p.sources[i].clone();
        let c = &p.conns[i];
        let kind = Kind::of(&src.url);
        let sf = kind == Some(Kind::Salesforce);
        // DataGrip と同じ並び: 新規 → クエリファイル → コピー・複製・除去 → リフレッシュ・接続 → プロパティ
        let mut m = crate::menu::Menu::new(at.0, at.1).action(self, "db.addSource").sep();
        match row {
            DbRow::Source(_) => {
                m = m.action(self, "db.newQuery").action(self, "db.console").sep();
                let url = src.url.clone();
                m = m.item("URL のコピー", move |e| e.copy_text(url, "URL")).action(self, "db.duplicate").action(self, "db.remove").sep();
                if c.connected {
                    m = m.action(self, "db.reload");
                }
                if c.client.is_none() {
                    m = m.action(self, "db.connect");
                } else {
                    m = m.item("再接続", move |e| e.db_reconnect(i)).action(self, "db.disconnect");
                }
                if !src.schema.is_empty() {
                    m = m.item(format!("既定のスキーマ（{}）を外す", src.schema), move |e| e.db_set_default_schema(i, ""));
                }
                if kind != Some(Kind::Sqlite) {
                    m = m.item(if sf { "保存したログインの破棄" } else { "保存したパスワードの破棄" }, move |e| e.db_forget_login(i));
                }
                m = m.sep().action(self, "db.properties");
            }
            DbRow::Schema(_, s, _) => {
                let (s2, cur) = (s.clone(), c.schema.clone());
                m = m.item("新規クエリファイルを開く", move |e| {
                    // このスキーマで書けるように、先に切り替える
                    if !s2.eq_ignore_ascii_case(&cur) && !sf {
                        e.db_switch_schema(i, &s2);
                    }
                    e.db_new_query(i, None)
                });
                let s2 = s.clone();
                m = m.sep().item("名前のコピー", move |e| e.copy_text(s2, "名前")).sep();
                m = m.item("リフレッシュ", move |e| e.db_reload(i));
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
            }
            DbRow::Group(..) | DbRow::Others(_) => {
                m = m.item("新規クエリファイルを開く", move |e| e.db_new_query(i, None)).sep().item("リフレッシュ", move |e| e.db_reload(i));
            }
            DbRow::Object(_, s, t, g, _) => {
                let q = qualified(self.db_dialect(i), &s, &t);
                if matches!(g, Group::Tables | Group::Views) {
                    // データの編集・新規クエリファイル（SELECT を書き入れて）は選んでいる行に対するアクション
                    m = m.action(self, "db.openData").action(self, "db.ddl").action(self, "db.newQuery");
                    if !sf {
                        let q2 = q.clone();
                        m = m.item("行のカウント", move |e| e.db_execute(i, format!("SELECT COUNT(*) FROM {q2}")));
                        m = m.sep();
                        let what: &[(&str, &str)] = if g == Group::Tables { &[("SELECT", "select"), ("INSERT", "insert"), ("UPDATE", "update"), ("DELETE", "delete")] } else { &[("SELECT", "select")] };
                        for &(label, w) in what {
                            let (s2, t2) = (s.clone(), t.clone());
                            m = m.item(format!("SQL ジェネレーター: {label}"), move |e| e.db_generate(i, &s2, &t2, w));
                        }
                    }
                }
                m = m.sep();
                let q2 = q.clone();
                m = m.item("参照をコピー", move |e| e.copy_text(q2, "名前"));
                let t2 = t.clone();
                m = m.item("名前のコピー", move |e| e.copy_text(t2, "名前"));
                if g == Group::Tables && !sf {
                    // 戻せない操作は、実行せずに新しいクエリに書くだけ（流すときにいつもの確認が出る）
                    m = m.sep();
                    let q2 = q.clone();
                    m = m.item("すべての行を除去…", move |e| e.db_new_query(i, Some(format!("-- 実行するとデータを全部消します（戻せません）\nTRUNCATE TABLE {q2}"))));
                    m = m.item("削除…", move |e| e.db_new_query(i, Some(format!("-- 実行するとテーブルを消します（戻せません）\nDROP TABLE {q}"))));
                }
            }
            DbRow::Column(_, s, t, k, _) => {
                let Some(name) = c.columns.get(&(s.clone(), t.clone())).and_then(|v| v.get(k)).map(|x| x.name.clone()) else { return false };
                let q = format!("{}.{}", ident(&t), ident(&name));
                m = m.item("参照をコピー", move |e| e.copy_text(q, "名前")).item("名前のコピー", move |e| e.copy_text(name, "名前"));
            }
            DbRow::Sub(..) | DbRow::Key(..) | DbRow::Index(..) | DbRow::Info(..) => {}
        }
        let m = m.sep().action(self, "db.filter").action(self, "db.close");
        self.open_menu(m);
        true
    }

    /// テーブルの SELECT・INSERT・UPDATE・DELETE を作って新しいクエリに書く（列がまだ無ければ読み始めて、もう一度と知らせる）
    fn db_generate(&mut self, i: usize, s: &str, t: &str, what: &str) {
        let Some(p) = &mut self.panels.db else { return };
        let c = &mut p.conns[i];
        let Some(cols) = c.columns.get(&(s.to_string(), t.to_string())).cloned() else {
            c.ensure_columns(s, t);
            self.svc.notify.info("列を読んでいます。もう一度選んでください");
            return;
        };
        let d = self.db_dialect(i);
        self.db_new_query(i, Some(generate_sql(d, what, &qualified(d, s, t), &cols)));
    }

    /// 入力欄（絞り込み・フォーム）に IME の確定を入れる。入れたら true
    pub(crate) fn db_insert(&mut self, text: &str) -> bool {
        let Some(p) = &mut self.panels.db else { return false };
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

    /// 木で文字を打ったら絞り込みの欄を開いて、その文字から打ち続けられるようにする（打った文字は今の絞り込みの後ろへ。
    /// IntelliJ のスピードサーチと同じ。Vim を使うときも木の上では Vim のキーを受けない: ADR 0019）。開いたら true
    pub(crate) fn db_type_to_filter(&mut self, key: Key) -> bool {
        if !matches!(key, Key::Char(c) if c != ' ') || !self.right_focused() || self.right_tab != RightTab::Database {
            return false;
        }
        let Some(p) = &mut self.panels.db else { return false };
        if p.filter.is_some() || p.form.is_some() || p.password.is_some() {
            return false;
        }
        p.filter = Some(VimField::new(&p.filter_text, false));
        self.db_key(key);
        true
    }

    /// IME を使う入力欄にいるか（パスワードでは使わない）
    pub fn db_typing(&self) -> bool {
        self.right_focused() && self.panels.db.as_ref().is_some_and(|p| p.form.as_ref().is_some_and(|f| f.at > 0 && f.fields[f.at].1.is_insert()) || p.filter.as_ref().is_some_and(|f| f.is_insert()))
    }

    pub(crate) fn db_key(&mut self, key: Key) -> Effect {
        let half = (self.windows_rows() / 2).max(1);
        let Some(p) = &mut self.panels.db else {
            self.set_state(State::Normal);
            return Effect::None;
        };
        // 部品の中で済むこと（パスワード・接続先のフォーム・絞り込み・選び・畳み）は DbPanel::key が行い、外に及ぶことを返す（ADR 0018 の §5）
        let a = p.key(key, half, &mut self.svc);
        self.db_do(a);
        Effect::None
    }

    /// データベースの欄の操作を行う（キー・アクションから）
    pub(crate) fn db_do(&mut self, a: DbAction) {
        match a {
            DbAction::None => {}
            DbAction::Connect(i, pass) => self.db_connect(i, pass),
            DbAction::AddSource(ds) => {
                self.db_push_source(ds, true);
                let i = self.panels.db.as_ref().map_or(0, |p| p.sources.len().saturating_sub(1));
                self.db_begin_connect(i);
            }
            DbAction::EditSource(i, ds) => self.db_update_source(i, ds),
            DbAction::Properties => {
                if let (Some(r), Some(p)) = (self.db_selected(), &mut self.panels.db) {
                    p.open_properties(r.source());
                }
            }
            DbAction::Duplicate => {
                if let Some(r) = self.db_selected() {
                    self.db_duplicate(r.source());
                }
            }
            DbAction::Remove => {
                if let Some(r) = self.db_selected() {
                    self.db_ask_remove(r.source());
                }
            }
            DbAction::Activate => self.db_activate(),
            DbAction::Toggle(r) => self.db_toggle_row(&r),
            DbAction::OpenSelected => self.db_open_selected(),
            DbAction::Ddl => self.db_ddl(),
            DbAction::Query(console) => {
                if let Some(r) = self.db_selected() {
                    let sf = self.panels.db.as_ref().and_then(|p| p.kind(r.source())) == Some(Kind::Salesforce);
                    // テーブルの上なら SELECT を書き入れる（コンソールは書き足す）
                    let end = if console { ";" } else { "" };
                    let init = match &r {
                        DbRow::Object(_, _, t, Group::Tables | Group::Views, _) if sf => Some(format!("SELECT Id, Name FROM {t} LIMIT 100")),
                        DbRow::Object(_, s, t, Group::Tables | Group::Views, _) => Some(format!("SELECT * FROM {}{end}", qualified(self.db_dialect(r.source()), s, t))),
                        _ => None,
                    };
                    if console {
                        self.open_console(r.source(), init);
                    } else {
                        self.db_new_query(r.source(), init);
                    }
                }
            }
            DbAction::Disconnect => {
                if let Some(r) = self.db_selected() {
                    self.db_disconnect(r.source());
                }
            }
            DbAction::Reload => {
                if let Some(r) = self.db_selected() {
                    self.db_reload(r.source());
                }
            }
            DbAction::Close => {
                if self.panels.db.as_ref().is_some_and(|p| p.conns.iter().any(|c| c.pending > 0)) {
                    self.svc.notify.info("まだコミットしていない変更があります。先にコミットかロールバックを");
                    return;
                }
                self.panels.db = None;
                self.set_state(State::Normal);
                self.rows = self.text_rows(self.active_rect());
            }
            DbAction::ToText => self.set_state(State::Normal),
            DbAction::AddForm => {
                if self.panels.db.is_none() {
                    self.open_db_panel();
                }
                if let Some(p) = &mut self.panels.db {
                    p.open_form();
                }
                self.focus_right(RightTab::Database);
            }
            DbAction::Filter => {
                if let Some(p) = &mut self.panels.db {
                    p.open_filter(&mut self.svc);
                }
                self.focus_right(RightTab::Database);
            }
        }
    }

    pub(crate) fn db_click(&mut self, row: usize, clicks: u32) {
        let Some(p) = &mut self.panels.db else { return };
        // 1 行目はタブ、2 行目は絞り込みの欄
        if row == 1 {
            if p.filter.is_none() {
                p.filter = Some(VimField::new(&p.filter_text, false));
            }
            self.focus_right(RightTab::Database);
            return;
        }
        // 接続先が無いときの案内（「＋ で足す」）を押したら、足すフォーム
        if !p.has_sources() && row == 2 {
            self.db_do(DbAction::AddForm);
            return;
        }
        let len = p.rows.len();
        let Some(pick) = nagi_ui::list::pick_row(&mut p.selected, p.scroll, row.max(2), 2, len, clicks) else { return };
        let accept = matches!(pick, nagi_ui::list::Pick::Accept(_));
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
        let Some(p) = &self.panels.db else { return Vec::new() };
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
        assert_eq!(statement_at(text, 2), "select 1\nfrom dual");
        assert_eq!(statement_at(text, 3), "select 1\nfrom dual");
        assert_eq!(statement_at(text, 6), "update t set a = 1");
        assert_eq!(statement_at(text, 5), "update t set a = 1");
        assert_eq!(statement_at(text, 1), "");
        // `;` で区切る（空行が無くても）。文字列・注釈の中の ; は数えない。`;` の後ろにいれば、その前の文
        let t = "select 1; select 'a;b' -- x;y\nfrom t;\nupdate u set a = 2;  \n";
        let span = |at: usize| statement_span(t, at).map(|r| &t[r]);
        assert_eq!(span(0), Some("select 1"));
        assert_eq!(span(t.find("'a").unwrap()), Some("select 'a;b' -- x;y\nfrom t"));
        assert_eq!(span(t.find("from").unwrap()), Some("select 'a;b' -- x;y\nfrom t"));
        assert_eq!(span(t.find("2;").unwrap() + 3), Some("update u set a = 2"), "; の後ろの空白");
        assert_eq!(span(t.len()), None, "最後の空の行");
        assert_eq!(span(t.find(" select").unwrap()), Some("select 'a;b' -- x;y\nfrom t"), "; の直後の空白は次の文の頭");
        assert!(is_read_only("-- x\n  select * from t"));
        assert!(!is_read_only("update t set a=1"));
        // WHERE の無い UPDATE・DELETE だけ確かめる
        assert_eq!(unsafe_statement("update t set a=1").as_deref(), Some("update t set a=1"));
        assert_eq!(unsafe_statement("delete from t -- where x\n"), Some("delete from t".into()));
        assert!(unsafe_statement("delete from t where id = 1").is_none());
        assert!(unsafe_statement("insert into t values (1); create table x(a int)").is_none());
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

    /// 接続先を足す入口（ADR 0019）: 見出しの ＋、行の無い所の右クリック、付けたショートカット（その欄にいるときだけ）
    #[test]
    fn add_source_entries() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("abc\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        let r = e.right_rect().unwrap();
        let s = crate::layout::Screen { rows: 50, cols: 160 };
        // 見出しの ＋
        crate::layout::press(&mut e, s, r.y, crate::right_panel::plus_col(r), |_| 1);
        assert!(e.panels.db.as_ref().unwrap().form.is_some(), "＋ でフォーム");
        e.panels.db.as_mut().unwrap().form = None;
        // 行の無い所の右クリック: 接続先を足すだけのメニュー
        assert!(crate::layout::context(&mut e, s, r.y + 10, r.x + 3));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels, ["データソースの作成…"]);
        e.menu_run(0);
        assert!(e.panels.db.as_ref().unwrap().form.is_some(), "メニューからフォーム");
        e.panels.db.as_mut().unwrap().form = None;
        // ショートカットは既定で付けない。付けたら、その欄にいるときだけ効く
        e.focus_right(RightTab::Database);
        e.key(Key::Alt('i'));
        assert!(e.panels.db.as_ref().unwrap().form.is_none(), "既定は付けない");
        e.settings.set("keymap.ui.db.addSource", "Alt+I", false);
        e.key(Key::Alt('i'));
        assert!(e.panels.db.as_ref().unwrap().form.is_some(), "付けたキーで");
        e.panels.db.as_mut().unwrap().form = None;
        e.set_state(State::Normal);
        e.key(Key::Alt('i'));
        assert!(e.panels.db.as_ref().unwrap().form.is_none(), "本文では効かない");
    }

    /// 木の上で印字できる文字を打っても、絞り込みの欄に入るだけ（1 文字キーの操作は無い。ADR 0019）。Vim を使うときも
    #[test]
    fn printable_keys_only_filter() {
        for vim in [false, true] {
            let mut e = Editor::new(nagi_core::Buffer::from_str("abc\n"), None);
            e.set_screen(160, 50);
            e.set_vim(vim);
            e.open_db_panel();
            for c in (0x21u8..0x7f).map(char::from) {
                let p = e.panels.db.as_mut().unwrap();
                (p.filter, p.filter_text) = (None, String::new());
                e.focus_right(RightTab::Database);
                e.key(Key::Char(c));
                let p = e.panels.db.as_ref().unwrap();
                assert!(e.right_focused() && p.form.is_none() && p.password.is_none() && e.menu.is_none(), "{vim} {c}");
                // 括弧・引用符は入力欄が閉じる側も足す
                assert!(p.filter_text.starts_with(c), "{vim} {c}: {:?}", p.filter_text);
            }
        }
    }

    /// 入力欄の右クリック: 描いた入力欄の上なら 切り取り・コピー・貼り付け・すべて選択。メニューのキーは今の欄に届く
    #[test]
    fn field_context_menu() {
        thread_local! { static CLIP: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) }; }
        fn set(t: &str) { CLIP.with(|c| *c.borrow_mut() = t.to_string()); }
        fn get() -> Option<String> { CLIP.with(|c| Some(c.borrow().clone())) }
        let mut e = Editor::new(nagi_core::Buffer::from_str("abc\n"), None);
        e.set_screen(160, 50);
        e.svc.clipboard = Some(crate::editor::Clipboard { set, get });
        e.open_db_panel();
        e.focus_right(RightTab::Database);
        keys(&mut e, "orders");
        assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "orders");
        // 描いたときに覚える位置（ここでは手で置く）
        *e.field_hits.borrow_mut() = vec![(3, 120, 160, true)];
        assert!(!e.field_menu(4, 130), "欄の外では出さない");
        assert!(e.field_menu(3, 130));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().filter(|i| i.run.is_some()).map(|i| i.label.clone()).collect();
        assert_eq!(labels, ["切り取り", "コピー", "貼り付け", "すべて選択"]);
        e.menu_run(1);
        assert_eq!(get().as_deref(), Some("orders"), "コピー");
        assert!(e.field_menu(3, 130));
        e.menu_run(0);
        assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "", "切り取り");
        assert!(e.field_menu(3, 130));
        e.menu_run(2);
        assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "orders", "貼り付け");
    }

    /// 欄の入力欄を Insert・Visual のまま本文を押しても、すぐ本文へ移る（前は Insert の間は押しても何もしなかった）
    #[test]
    fn click_text_from_field_in_any_mode() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("abc\ndef\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        // 木で打った文字は絞り込みの欄へ（ADR 0019。Vim を使うときも木の上で / は要らない）
        for k in ["or", "or<Esc>v"] {
            e.panels.db.as_mut().unwrap().filter = None;
            e.panels.db.as_mut().unwrap().filter_text.clear();
            e.focus_right(RightTab::Database);
            keys(&mut e, k);
            assert!(e.right_focused() && e.panels.db.as_ref().unwrap().filter.is_some(), "{k}");
            e.click(1, 7);
            assert_eq!((e.state(), e.core.cursor.line), (State::Normal, 1), "{k}");
            keys(&mut e, "x");
            assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "or", "{k}: 打ったキーは本文へ");
        }
        assert_eq!(e.core.buf.line(1).trim_end().len(), 1, "x を 2 回");
    }

    pub(crate) fn wait(e: &mut Editor, what: &dyn Fn(&Editor) -> bool) {
        for _ in 0..2000 {
            e.poll_db();
            if what(e) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("待ちきれない: {}", e.svc.notify.text);
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
        let i = e.panels.db.as_ref().unwrap().sources.len() - 1;
        assert_eq!(e.panels.db.as_ref().unwrap().kind(i), Some(Kind::Salesforce));
        e.focus_right(RightTab::Database);
        e.panels.db.as_mut().unwrap().selected = e.panels.db.as_ref().unwrap().rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        keys(&mut e, "<CR>");
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].objects.values().any(|o| !o.tables.is_empty()));
        let p = e.panels.db.as_ref().unwrap();
        let labels: Vec<String> = p.rows.iter().map(|r| p.row_label(r).2).collect();
        assert!(labels.contains(&"オブジェクト".to_string()) && labels.contains(&"カスタムオブジェクト".to_string()), "{labels:?}");
        assert!(e.panels.db_results.log.iter().any(|l| l.text.contains("ブラウザ")), "ログインの知らせが出力に残る");
        // Account の上で「データを開く」→ SOQL が流れて結果のタブに出る（絞り込みの欄は出さない）
        let k = p.rows.iter().position(|r| matches!(r, DbRow::Object(_, _, t, _, _) if t == "Account")).unwrap();
        e.panels.db.as_mut().unwrap().selected = k;
        assert!(e.run_ui_action("db.openData"));
        wait(&mut e, &|e| e.panels.db_results.active_tab().is_some_and(|t| !t.running));
        let t = e.panels.db_results.active_tab().unwrap();
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
        let p = e.panels.db.as_ref().unwrap();
        assert!(p.filter.is_some());
        assert_eq!(p.filter_text, "it");
        // 決めた後にまた打つと、今の絞り込みの後ろへ
        e.key(Key::Enter);
        assert!(e.panels.db.as_ref().unwrap().filter.is_none());
        e.key(Key::Char('e'));
        assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "ite");
        // Ctrl+A で全部を選んで打ち直す
        e.key(Key::Ctrl('a'));
        e.key(Key::Char('m'));
        assert_eq!(e.panels.db.as_ref().unwrap().filter_text, "m");
        e.set_vim(true);
    }

    /// 接続先は 1 回のクリックでつなぐ・開け閉めする。ダブルクリックの 2 回目では閉じ直さない
    #[test]
    fn click_source() {
        let mut e = Editor::new(nagi_core::Buffer::from_str("x\n"), None);
        e.set_screen(160, 50);
        e.open_db_panel();
        e.db_add("mem jdbc:sqlite::memory:");
        let i = e.panels.db.as_ref().unwrap().sources.len() - 1;
        let prow = |e: &Editor| {
            let p = e.panels.db.as_ref().unwrap();
            p.rows.iter().position(|r| *r == DbRow::Source(i)).map(|k| k - p.scroll + 2).unwrap()
        };
        let open = |e: &Editor| e.panels.db.as_ref().unwrap().folds.is_open(&format!("s{i}"));
        let r = prow(&e);
        e.db_click(r, 1);
        assert!(e.panels.db.as_ref().unwrap().conns[i].client.is_some(), "1 回でつなぎ始める");
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].connected);
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
        let i = e.panels.db.as_ref().unwrap().sources.len() - 1;
        let prow = |e: &Editor, f: &dyn Fn(&DbRow) -> bool| {
            let p = e.panels.db.as_ref().unwrap();
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
        assert!(labels(&e).contains(&"接続".to_string()) && !labels(&e).iter().any(|l| l.contains("パスワード")), "SQLite にパスワードは無い: {:?}", labels(&e));
        // DataGrip と同じ並び（新規が先頭・プロパティが最後の組）
        assert_eq!(labels(&e)[..3], ["データソースの作成…", "新規クエリファイルを開く", "クエリファイルを開く"]);
        for want in ["複製", "データソースの除去…", "プロパティ"] {
            assert!(labels(&e).iter().any(|l| l == want), "{want}: {:?}", labels(&e));
        }
        run(&mut e, "接続");
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].connected);
        e.panels.db.as_mut().unwrap().conns[i].manual = true;
        e.db_execute(i, "create table item(id integer primary key, name text)".into());
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        // テーブル: メニューを出すと列を読み始める
        let r = prow(&e, &|r| matches!(r, DbRow::Object(_, _, t, Group::Tables, _) if t == "item"));
        assert!(e.db_context(r, (r, 150)));
        for want in ["データの編集", "新規クエリファイルを開く", "行のカウント", "SQL ジェネレーター: SELECT", "DDL に移動", "参照をコピー", "名前のコピー", "削除…"] {
            assert!(labels(&e).iter().any(|l| l == want), "{want}: {:?}", labels(&e));
        }
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].columns.contains_key(&("main".to_string(), "item".to_string())));
        run(&mut e, "SQL ジェネレーター: SELECT");
        assert_eq!(e.console_source(), Some(i), "新しいクエリはこの接続先のコンソール");
        assert!(e.core.buf.text().contains("SELECT id,\n       name\nFROM main.item"), "{}", e.core.buf.text());
        // 件数は結果のタブに出る（読むだけなので確かめない）
        e.set_state(State::Normal);
        let r = prow(&e, &|r| matches!(r, DbRow::Object(_, _, t, Group::Tables, _) if t == "item"));
        assert!(e.db_context(r, (r, 150)));
        run(&mut e, "行のカウント");
        wait(&mut e, &|e| e.panels.db_results.active_tab().is_some_and(|t| !t.running));
        assert_eq!(e.panels.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("0"));
        // スキーマ: SQLite は切り替えない・既定にしない
        let r = prow(&e, &|r| matches!(r, DbRow::Schema(..)));
        assert!(e.db_context(r, (r, 150)));
        assert!(!labels(&e).iter().any(|l| l.contains("既定") || l.contains("切り替える")), "{:?}", labels(&e));
        run(&mut e, "新規クエリファイルを開く");
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
        // 接続先を足す → 種類を SQLite に（名前の順で最後なので、Oracle から ← で回る）→ 名前 → ファイル → Enter で保存してつなぐ
        assert!(e.run_ui_action("db.addSource"));
        assert!(e.panels.db.as_ref().unwrap().form.is_some());
        keys(&mut e, "<Up><Left><Down>shop<Tab>");
        let file = dir.join("shop.db");
        for c in file.display().to_string().chars() {
            e.key(Key::Char(c));
        }
        keys(&mut e, "<CR>");
        assert!(std::fs::read_to_string(dir.join(".config/nagi/databases.txt")).unwrap().contains("shop | jdbc:sqlite:"));
        let i = e.panels.db.as_ref().unwrap().sources.len() - 1;
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].objects.contains_key("main"));
        // コンソール: CREATE は確かめずに流す（DataGrip と同じ）
        assert!(e.run_ui_action("db.console"));
        assert_eq!(e.console_source(), Some(i));
        keys(&mut e, "Gocreate table item(id integer primary key, name text)<Esc><C-m>");
        assert!(e.menu.is_none(), "確かめない");
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].objects.get("main").is_some_and(|o| !o.tables.is_empty()));
        // 手動 Tx では確かめずに流し、未コミットを数える。ロールバックで消える
        e.db_toggle_tx(i);
        keys(&mut e, "o<CR>insert into item values (1, 'りんご')<Esc><C-m>");
        wait(&mut e, &|e| e.panels.db.as_ref().unwrap().conns[i].pending == 1);
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
        wait(&mut e, &|e| e.panels.db_results.active_tab().is_some_and(|t| !t.running && !t.rows.is_empty()));
        assert_eq!(e.panels.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("0"), "ロールバックで消えた");
        // 出力のタブに記録が残る
        assert!(e.panels.db_results.log.iter().any(|l| l.text.contains("insert into item")));
        // 木: テーブル（1）› item
        let p = e.panels.db.as_ref().unwrap();
        assert!(p.rows.iter().any(|r| matches!(r, DbRow::Group(_, _, Group::Tables, 1))), "{:?}", p.rows);
        assert!(p.rows.iter().any(|r| matches!(r, DbRow::Object(_, _, t, _, _) if t == "item")));
        // 新しいクエリ: 別のファイルを Insert で開き、そのまま書いて Ctrl+Enter で流せる
        e.focus_right(RightTab::Database);
        e.panels.db.as_mut().unwrap().selected = e.panels.db.as_ref().unwrap().rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        assert!(e.run_ui_action("db.newQuery"));
        assert_eq!(e.state(), State::Insert);
        assert!(e.core.buf.path.as_ref().unwrap().to_string_lossy().ends_with("shop_query_1.sql"));
        keys(&mut e, "select 42 as answer<C-m>");
        wait(&mut e, &|e| e.panels.db_results.active_tab().is_some_and(|t| !t.running && !t.rows.is_empty()));
        assert_eq!(e.panels.db_results.active_tab().unwrap().rows[0][0].as_deref(), Some("42"));
        keys(&mut e, "<Esc>");
        e.db_new_query_command();
        assert!(e.core.buf.path.as_ref().unwrap().to_string_lossy().ends_with("shop_query_2.sql"), "番号を進める");
        // 絞り込み
        e.focus_right(RightTab::Database);
        keys(&mut e, "zzz<CR>");
        assert!(!e.panels.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
        // 絞り込みの欄（Vim 風）: Esc でノーマル、もう一度 Esc で消す
        assert!(e.run_ui_action("db.filter"));
        keys(&mut e, "<Esc><Esc>");
        assert!(e.panels.db.as_ref().unwrap().rows.iter().any(|r| matches!(r, DbRow::Object(..))));
        // プロパティ: 今の値が入ったフォームで直し、設定ファイルの行を書き換える（開き直しても残る）
        let cfg = dir.join(".config/nagi/databases.txt");
        e.set_state(State::Normal);
        e.focus_right(RightTab::Database);
        let select = |e: &mut Editor, i: usize| {
            let p = e.panels.db.as_mut().unwrap();
            p.selected = p.rows.iter().position(|r| *r == DbRow::Source(i)).unwrap();
        };
        select(&mut e, i);
        assert!(e.run_ui_action("db.properties"));
        let f = e.panels.db.as_ref().unwrap().form.as_ref().unwrap();
        assert_eq!(f.edit, Some(i));
        assert_eq!(f.fields.iter().map(|f| f.1.text()).collect::<Vec<_>>()[..2], ["shop".to_string(), format!("jdbc:sqlite:{}", file.display())]);
        e.panels.db.as_mut().unwrap().form.as_mut().unwrap().fields[0].1 = VimField::new("shop2", false);
        keys(&mut e, "<CR><CR><CR><CR>");
        assert!(e.panels.db.as_ref().unwrap().form.is_none(), "最後の欄の Enter で保存");
        assert_eq!(e.panels.db.as_ref().unwrap().sources[i].name, "shop2");
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("shop2 | jdbc:sqlite:") && !text.contains("shop |"), "{text}");
        // 複製: 名前に [2] を付けて足す。除去: 確かめてから設定ファイルの行も消す
        select(&mut e, i);
        assert!(e.run_ui_action("db.duplicate"));
        let j = e.panels.db.as_ref().unwrap().sources.len() - 1;
        assert_eq!(e.panels.db.as_ref().unwrap().sources[j].name, "shop2 [2]");
        assert!(std::fs::read_to_string(&cfg).unwrap().contains("shop2 [2] | jdbc:sqlite:"));
        select(&mut e, j);
        assert!(e.run_ui_action("db.remove"));
        keys(&mut e, "y");
        let p = e.panels.db.as_ref().unwrap();
        assert!(!p.rows.contains(&DbRow::Source(j)) && p.rows.contains(&DbRow::Source(i)), "木から外す");
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(!text.contains("shop2 [2]") && text.contains("shop2 | "), "{text}");
        // 開き直しても直した名前のまま
        e.db_do(DbAction::Close);
        e.open_db_panel();
        assert!(e.panels.db.as_ref().unwrap().sources.iter().any(|s| s.name == "shop2"));
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
        Some(crate::panes::Rect { x, y: ed.area().y + ed.tabs_row(), w, h: 1 })
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_console_bar(p, ed);
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, at: crate::layout::At, clicks: u32) -> crate::layout::Click {
        let _ = clicks;
        if let Some(item) = ed.console_bar(r.w).into_iter().find(|b| at.col >= b.1 && at.col < b.1 + b.2).map(|b| b.3) {
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
        let y0 = (ed.area().y + ed.tabs_row()) as f32 * cell.h;
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
