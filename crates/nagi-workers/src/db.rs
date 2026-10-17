//! データベースにつなぐ（右の欄の「データベース」タブ用）。ドライバーはすべて Rust。
//!
//! - Oracle: `oracle` クレート（Oracle 公式の ODPI-C）。実行時に Oracle Instant Client の DLL を読み込む。
//!   Instant Client は、初めてつなぐときに Nagi がダウンロードして `%LOCALAPPDATA%\nagi\drivers\oracle` に置く
//!   （drivers.rs）。PATH などに入っているものがあればそれも使う。
//! - SQLite: `rusqlite`（SQLite を中に組み込む）。準備は要らない。
//!
//! 接続ごとに裏のスレッドを 1 つ立て、頼みごと（Req）を順に処理して答え（Reply）を返し、UI を起こす。
//! 値はすべて文字列（NULL は None）で返す。日時は `YYYY-MM-DD HH24:MI:SS` の形にそろえる（書き戻すときも同じ形）。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use crate::Waker;

/// 接続先
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSource {
    pub name: String,
    pub url: String,
    pub user: String,
    /// どこで見つけたか（ファイルのパス）
    pub origin: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Oracle,
    Sqlite,
    Salesforce,
}

impl Kind {
    pub fn of(url: &str) -> Option<Kind> {
        let u = url.to_ascii_lowercase();
        if u.starts_with("jdbc:oracle:") {
            Some(Kind::Oracle)
        } else if u.starts_with("jdbc:sqlite:") {
            Some(Kind::Sqlite)
        } else if u.starts_with("salesforce:") {
            Some(Kind::Salesforce)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Oracle => "Oracle",
            Kind::Sqlite => "SQLite",
            Kind::Salesforce => "Salesforce",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: String,
    pub nullable: bool,
    pub pk: bool,
    /// 外部キーの参照先のテーブル（無ければ空）
    pub fk: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub cols: String,
}

/// 主キー（refs が空）と外部キー
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub name: String,
    pub cols: String,
    pub refs: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Objects {
    /// (名前, TABLE / VIEW)
    pub tables: Vec<(String, String)>,
    pub sequences: Vec<String>,
    pub routines: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Reply {
    Connected { product: String, schema: String },
    Schemas(Vec<String>),
    Objects(Objects),
    Columns(Vec<Column>),
    Indexes(Vec<Index>, Vec<Key>),
    Rows { cols: Vec<String>, types: Vec<String>, rows: Vec<Vec<Option<String>>>, more: bool, ms: u64 },
    Updated { count: i64, ms: u64 },
    Count(i64),
    Ddl(String),
    Done,
    /// 途中の知らせ（「ブラウザでログインしてください」など。頼みごとの答えではない）
    Note(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Req {
    Schemas,
    SetSchema(String),
    Objects(String),
    Columns(String, String),
    Indexes(String, String),
    Query { sql: String, max: usize, offset: usize },
    Count(String),
    /// 1 つのトランザクションで流す。どの文もちょうど 1 行を変えなければ全部取り消す
    Batch(Vec<String>),
    AutoCommit(bool),
    Commit,
    Rollback,
    /// (スキーマ, 名前, TABLE / VIEW)
    Ddl(String, String, String),
}

type R<T> = Result<T, String>;

/// ドライバーごとの中身
pub(crate) trait Backend {
    fn info(&mut self) -> R<(String, String)>;
    fn schemas(&mut self) -> R<Vec<String>>;
    fn set_schema(&mut self, s: &str) -> R<()>;
    fn objects(&mut self, schema: &str) -> R<Objects>;
    fn columns(&mut self, schema: &str, table: &str) -> R<Vec<Column>>;
    fn indexes(&mut self, schema: &str, table: &str) -> R<(Vec<Index>, Vec<Key>)>;
    /// (列名, 型, 行, 続きがあるか) か、変えた行の数
    fn query(&mut self, sql: &str, max: usize, offset: usize) -> R<Result<(Vec<String>, Vec<String>, Vec<Vec<Option<String>>>, bool), i64>>;
    fn count(&mut self, sql: &str) -> R<i64>;
    fn batch(&mut self, stmts: &[String]) -> R<i64>;
    fn set_autocommit(&mut self, on: bool) -> R<()>;
    fn commit(&mut self) -> R<()>;
    fn rollback(&mut self) -> R<()>;
    fn ddl(&mut self, schema: &str, name: &str, kind: &str) -> R<String>;
}

/// 最後の ; を外す（PL/SQL のブロックは残す。Oracle は ; で誤りになる）
pub fn strip_semicolon(sql: &str) -> String {
    let s = sql.trim();
    let up = s.to_ascii_uppercase();
    if s.ends_with(';') && !(up.starts_with("BEGIN") || up.starts_with("DECLARE") || up.contains("END;")) {
        s[..s.len() - 1].trim_end().to_string()
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------- Oracle

/// JDBC の URL を Oracle の接続文字列にする
pub fn oracle_connect_string(url: &str) -> String {
    let rest = url.split_once('@').map_or(url, |(_, r)| r).trim();
    if rest.starts_with('(') || rest.starts_with("//") {
        return rest.to_string();
    }
    // host:port:SID（古い形）
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() == 3 && !rest.contains('/') {
        return format!("(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST={})(PORT={}))(CONNECT_DATA=(SID={})))", parts[0], parts[1], parts[2]);
    }
    rest.to_string()
}

struct OracleDb {
    conn: oracle::Connection,
}

fn oe(e: oracle::Error) -> String {
    e.to_string().lines().next().unwrap_or("").to_string()
}

impl OracleDb {
    fn connect(url: &str, user: &str, pass: &str) -> R<Self> {
        crate::drivers::init_oracle()?;
        let mut conn = oracle::Connection::connect(user, pass, oracle_connect_string(url)).map_err(oe)?;
        conn.set_autocommit(true);
        // 表示と書き戻しで同じ形にする
        conn.execute("ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF TZH:TZM'", &[]).map_err(oe)?;
        Ok(OracleDb { conn })
    }

    fn strings(&self, sql: &str, params: &[&dyn oracle::sql_type::ToSql]) -> R<Vec<Vec<Option<String>>>> {
        let rows = self.conn.query(sql, params).map_err(oe)?;
        let mut out = Vec::new();
        for r in rows {
            let r = r.map_err(oe)?;
            let n = r.sql_values().len();
            out.push((0..n).map(|i| r.get::<usize, Option<String>>(i).ok().flatten()).collect());
        }
        Ok(out)
    }
}

fn col(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

impl Backend for OracleDb {
    fn info(&mut self) -> R<(String, String)> {
        let (v, banner) = self.conn.server_version().map_err(oe)?;
        let product = banner.lines().next().map_or_else(|| format!("Oracle {v}"), |b| b.split(" Release").next().unwrap_or(b).to_string());
        Ok((format!("{product} {}", v.major()), self.conn.current_schema().map_err(oe)?))
    }

    fn schemas(&mut self) -> R<Vec<String>> {
        Ok(self.strings("SELECT username FROM all_users ORDER BY username", &[])?.iter().map(|r| col(r, 0)).collect())
    }

    fn set_schema(&mut self, s: &str) -> R<()> {
        self.conn.set_current_schema(s).map_err(oe)
    }

    fn objects(&mut self, schema: &str) -> R<Objects> {
        let rows = self.strings(
            "SELECT object_name, object_type FROM all_objects WHERE owner = :1 AND object_type IN ('TABLE','VIEW','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE') AND object_name NOT LIKE 'BIN$%' ORDER BY object_name",
            &[&schema],
        )?;
        let mut o = Objects::default();
        for r in rows {
            let (n, t) = (col(&r, 0), col(&r, 1));
            match t.as_str() {
                "TABLE" | "VIEW" => o.tables.push((n, t)),
                "SEQUENCE" => o.sequences.push(n),
                _ => o.routines.push(n),
            }
        }
        Ok(o)
    }

    fn columns(&mut self, schema: &str, table: &str) -> R<Vec<Column>> {
        let pk: Vec<String> = self
            .strings("SELECT cc.column_name FROM all_constraints k JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name WHERE k.owner = :1 AND k.table_name = :2 AND k.constraint_type = 'P'", &[&schema, &table])?
            .iter()
            .map(|r| col(r, 0))
            .collect();
        let fk = self.strings(
            "SELECT cc.column_name, r.table_name FROM all_constraints k JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name WHERE k.owner = :1 AND k.table_name = :2 AND k.constraint_type = 'R'",
            &[&schema, &table],
        )?;
        let rows = self.strings(
            "SELECT column_name, data_type, char_length, data_precision, data_scale, nullable FROM all_tab_columns WHERE owner = :1 AND table_name = :2 ORDER BY column_id",
            &[&schema, &table],
        )?;
        Ok(rows
            .iter()
            .map(|r| {
                let (name, dt) = (col(r, 0), col(r, 1));
                let ty = if matches!(dt.as_str(), "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" | "RAW") {
                    format!("{dt}({})", col(r, 2))
                } else if dt == "NUMBER" && !col(r, 3).is_empty() {
                    let s = col(r, 4);
                    if s.is_empty() || s == "0" { format!("NUMBER({})", col(r, 3)) } else { format!("NUMBER({},{s})", col(r, 3)) }
                } else {
                    dt
                };
                let fk = fk.iter().find(|f| col(f, 0) == name).map(|f| col(f, 1)).unwrap_or_default();
                Column { pk: pk.contains(&name), fk, nullable: col(r, 5) == "Y", name, ty }
            })
            .collect())
    }

    fn indexes(&mut self, schema: &str, table: &str) -> R<(Vec<Index>, Vec<Key>)> {
        let rows = self.strings(
            "SELECT i.index_name, i.uniqueness, ic.column_name FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name WHERE i.table_owner = :1 AND i.table_name = :2 ORDER BY i.index_name, ic.column_position",
            &[&schema, &table],
        )?;
        let mut idx: Vec<Index> = Vec::new();
        for r in rows {
            let n = col(&r, 0);
            match idx.last_mut().filter(|i| i.name == n) {
                Some(i) => i.cols = format!("{}, {}", i.cols, col(&r, 2)),
                None => idx.push(Index { name: n, unique: col(&r, 1) == "UNIQUE", cols: col(&r, 2) }),
            }
        }
        let rows = self.strings(
            "SELECT k.constraint_name, k.constraint_type, cc.column_name, r.table_name FROM all_constraints k JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name LEFT JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name WHERE k.owner = :1 AND k.table_name = :2 AND k.constraint_type IN ('P','R') ORDER BY k.constraint_type, k.constraint_name, cc.position",
            &[&schema, &table],
        )?;
        let mut keys: Vec<Key> = Vec::new();
        for r in rows {
            let n = col(&r, 0);
            match keys.last_mut().filter(|k| k.name == n) {
                Some(k) => k.cols = format!("{}, {}", k.cols, col(&r, 2)),
                None => keys.push(Key { name: n, cols: col(&r, 2), refs: col(&r, 3) }),
            }
        }
        Ok((idx, keys))
    }

    fn query(&mut self, sql: &str, max: usize, offset: usize) -> R<Result<(Vec<String>, Vec<String>, Vec<Vec<Option<String>>>, bool), i64>> {
        let sql = strip_semicolon(sql);
        let mut stmt = self.conn.statement(&sql).build().map_err(oe)?;
        if !stmt.is_query() {
            stmt.execute(&[]).map_err(oe)?;
            return Ok(Err(stmt.row_count().map_err(oe)? as i64));
        }
        let rs = stmt.query(&[]).map_err(oe)?;
        let info = rs.column_info();
        let cols: Vec<String> = info.iter().map(|c| c.name().to_string()).collect();
        let types: Vec<String> = info.iter().map(|c| c.oracle_type().to_string()).collect();
        let n = cols.len();
        let mut rows = Vec::new();
        let mut more = false;
        for (k, r) in rs.enumerate() {
            let r = r.map_err(oe)?;
            if k < offset {
                continue;
            }
            if rows.len() == max {
                more = true;
                break;
            }
            rows.push((0..n).map(|i| r.get::<usize, Option<String>>(i).unwrap_or_else(|e| Some(format!("({})", oe(e))))).collect());
        }
        Ok(Ok((cols, types, rows, more)))
    }

    fn count(&mut self, sql: &str) -> R<i64> {
        self.conn.query_row_as::<i64>(&strip_semicolon(sql), &[]).map_err(oe)
    }

    fn batch(&mut self, stmts: &[String]) -> R<i64> {
        let auto = self.conn.autocommit();
        if auto {
            self.conn.set_autocommit(false);
        } else {
            self.conn.execute("SAVEPOINT nagi_batch", &[]).map_err(oe)?;
        }
        let run = |c: &oracle::Connection| -> R<i64> {
            let mut total = 0;
            for (k, s) in stmts.iter().enumerate() {
                let st = c.execute(&strip_semicolon(s), &[]).map_err(oe)?;
                let n = st.row_count().map_err(oe)?;
                if n != 1 {
                    return Err(format!("{} 文目が {n} 行を変えようとしました（1 行のはず）。すべて取り消しました", k + 1));
                }
                total += 1;
            }
            Ok(total)
        };
        let r = run(&self.conn);
        match (&r, auto) {
            (Ok(_), true) => self.conn.commit().map_err(oe)?,
            (Err(_), true) => self.conn.rollback().map_err(oe)?,
            (Err(_), false) => {
                let _ = self.conn.execute("ROLLBACK TO SAVEPOINT nagi_batch", &[]);
            }
            _ => {}
        }
        if auto {
            self.conn.set_autocommit(true);
        }
        r
    }

    fn set_autocommit(&mut self, on: bool) -> R<()> {
        self.conn.set_autocommit(on);
        Ok(())
    }

    fn commit(&mut self) -> R<()> {
        self.conn.commit().map_err(oe)
    }

    fn rollback(&mut self) -> R<()> {
        self.conn.rollback().map_err(oe)
    }

    fn ddl(&mut self, schema: &str, name: &str, kind: &str) -> R<String> {
        let k = if kind.contains("VIEW") { "VIEW" } else { "TABLE" };
        self.conn.query_row_as::<String>("SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM DUAL", &[&k, &name, &schema]).map(|s| s.trim().to_string()).map_err(oe)
    }
}

// ---------------------------------------------------------------- SQLite

struct SqliteDb {
    conn: rusqlite::Connection,
    auto: bool,
}

fn se(e: rusqlite::Error) -> String {
    e.to_string()
}

fn sqlite_value(v: rusqlite::types::ValueRef) -> Option<String> {
    use rusqlite::types::ValueRef::*;
    match v {
        Null => None,
        Integer(i) => Some(i.to_string()),
        Real(f) => Some(f.to_string()),
        Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        Blob(b) => Some(format!("(BLOB {} バイト)", b.len())),
    }
}

impl SqliteDb {
    fn connect(url: &str) -> R<Self> {
        let path = url.split_once("sqlite:").map_or(url, |(_, p)| p);
        let conn = if path.is_empty() || path == ":memory:" { rusqlite::Connection::open_in_memory() } else { rusqlite::Connection::open(path) }.map_err(se)?;
        Ok(SqliteDb { conn, auto: true })
    }

    fn strings(&self, sql: &str) -> R<Vec<Vec<Option<String>>>> {
        let mut st = self.conn.prepare(sql).map_err(se)?;
        let n = st.column_count();
        let mut rows = st.query([]).map_err(se)?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().map_err(se)? {
            out.push((0..n).map(|i| r.get_ref(i).ok().and_then(sqlite_value)).collect());
        }
        Ok(out)
    }

    fn quote(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
}

impl Backend for SqliteDb {
    fn info(&mut self) -> R<(String, String)> {
        Ok((format!("SQLite {}", rusqlite::version()), "main".into()))
    }

    fn schemas(&mut self) -> R<Vec<String>> {
        Ok(vec!["main".into()])
    }

    fn set_schema(&mut self, _s: &str) -> R<()> {
        Ok(())
    }

    fn objects(&mut self, _schema: &str) -> R<Objects> {
        let rows = self.strings("SELECT name, type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
        Ok(Objects { tables: rows.iter().map(|r| (col(r, 0), col(r, 1).to_ascii_uppercase())).collect(), ..Default::default() })
    }

    fn columns(&mut self, _schema: &str, table: &str) -> R<Vec<Column>> {
        let fk = self.strings(&format!("PRAGMA foreign_key_list({})", Self::quote(table)))?;
        let rows = self.strings(&format!("PRAGMA table_info({})", Self::quote(table)))?;
        Ok(rows
            .iter()
            .map(|r| {
                let name = col(r, 1);
                let fk = fk.iter().find(|f| col(f, 3) == name).map(|f| col(f, 2)).unwrap_or_default();
                Column { ty: col(r, 2), nullable: col(r, 3) == "0", pk: col(r, 5) != "0", fk, name }
            })
            .collect())
    }

    fn indexes(&mut self, _schema: &str, table: &str) -> R<(Vec<Index>, Vec<Key>)> {
        let mut idx = Vec::new();
        for r in self.strings(&format!("PRAGMA index_list({})", Self::quote(table)))? {
            let name = col(&r, 1);
            let cols: Vec<String> = self.strings(&format!("PRAGMA index_info({})", Self::quote(&name)))?.iter().map(|c| col(c, 2)).collect();
            idx.push(Index { name, unique: col(&r, 2) == "1", cols: cols.join(", ") });
        }
        let pk: Vec<String> = self.columns("", table)?.into_iter().filter(|c| c.pk).map(|c| c.name).collect();
        let mut keys = Vec::new();
        if !pk.is_empty() {
            keys.push(Key { name: "PRIMARY KEY".into(), cols: pk.join(", "), refs: String::new() });
        }
        for f in self.strings(&format!("PRAGMA foreign_key_list({})", Self::quote(table)))? {
            keys.push(Key { name: format!("FK_{}", col(&f, 0)), cols: col(&f, 3), refs: col(&f, 2) });
        }
        Ok((idx, keys))
    }

    fn query(&mut self, sql: &str, max: usize, offset: usize) -> R<Result<(Vec<String>, Vec<String>, Vec<Vec<Option<String>>>, bool), i64>> {
        let sql = strip_semicolon(sql);
        let mut st = self.conn.prepare(&sql).map_err(se)?;
        let n = st.column_count();
        if n == 0 {
            let c = st.execute([]).map_err(se)?;
            return Ok(Err(c as i64));
        }
        let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let types: Vec<String> = st.columns().iter().map(|c| c.decl_type().unwrap_or("").to_string()).collect();
        let mut q = st.query([]).map_err(se)?;
        let mut rows = Vec::new();
        let mut more = false;
        let mut k = 0;
        while let Some(r) = q.next().map_err(se)? {
            k += 1;
            if k <= offset {
                continue;
            }
            if rows.len() == max {
                more = true;
                break;
            }
            rows.push((0..n).map(|i| r.get_ref(i).ok().and_then(sqlite_value)).collect());
        }
        Ok(Ok((cols, types, rows, more)))
    }

    fn count(&mut self, sql: &str) -> R<i64> {
        self.conn.query_row(&strip_semicolon(sql), [], |r| r.get::<_, i64>(0)).map_err(se)
    }

    fn batch(&mut self, stmts: &[String]) -> R<i64> {
        self.conn.execute_batch("SAVEPOINT nagi_batch").map_err(se)?;
        let mut total = 0;
        for (k, s) in stmts.iter().enumerate() {
            let n = match self.conn.execute(&strip_semicolon(s), []) {
                Ok(n) => n,
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK TO nagi_batch; RELEASE nagi_batch");
                    return Err(se(e));
                }
            };
            if n != 1 {
                let _ = self.conn.execute_batch("ROLLBACK TO nagi_batch; RELEASE nagi_batch");
                return Err(format!("{} 文目が {n} 行を変えようとしました（1 行のはず）。すべて取り消しました", k + 1));
            }
            total += 1;
        }
        self.conn.execute_batch("RELEASE nagi_batch").map_err(se)?;
        Ok(total)
    }

    fn set_autocommit(&mut self, on: bool) -> R<()> {
        if on == self.auto {
            return Ok(());
        }
        self.auto = on;
        // 手動: BEGIN しておく。自動に戻すときは、残っている変更を確定する
        self.conn.execute_batch(if on { "COMMIT" } else { "BEGIN" }).map_err(se)
    }

    fn commit(&mut self) -> R<()> {
        if self.auto {
            return Ok(());
        }
        self.conn.execute_batch("COMMIT; BEGIN").map_err(se)
    }

    fn rollback(&mut self) -> R<()> {
        if self.auto {
            return Ok(());
        }
        self.conn.execute_batch("ROLLBACK; BEGIN").map_err(se)
    }

    fn ddl(&mut self, _schema: &str, name: &str, _kind: &str) -> R<String> {
        self.conn.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [name], |r| r.get::<_, String>(0)).map_err(se)
    }
}

// ---------------------------------------------------------------- 接続（裏のスレッド）

/// 1 つの接続
pub struct Client {
    tx: Sender<Req>,
    pub rx: Receiver<Reply>,
}

impl Client {
    /// 裏のスレッドで `url` につなぎ始める（つながったら Reply::Connected が届く）
    pub fn connect(url: &str, user: &str, pass: &str, waker: Waker) -> Result<Client, String> {
        let kind = Kind::of(url).ok_or_else(|| format!("この種類の接続はまだ使えません（Oracle・SQLite・Salesforce）: {}", url.split(':').take(2).collect::<Vec<_>>().join(":")))?;
        let (tx, req_rx) = channel::<Req>();
        let (reply_tx, rx) = channel::<Reply>();
        let (url, user, pass) = (url.to_string(), user.to_string(), pass.to_string());
        std::thread::Builder::new()
            .name("db".into())
            .spawn(move || {
                let send = |r: Reply| {
                    let ok = reply_tx.send(r).is_ok();
                    waker();
                    ok
                };
                let note = |s: String| {
                    send(Reply::Note(s));
                };
                let opened: R<Box<dyn Backend>> = match kind {
                    Kind::Oracle => OracleDb::connect(&url, &user, &pass).map(|d| Box::new(d) as Box<dyn Backend>),
                    Kind::Sqlite => SqliteDb::connect(&url).map(|d| Box::new(d) as Box<dyn Backend>),
                    Kind::Salesforce => crate::salesforce::SalesforceDb::connect(&url, &user, &note).map(|d| Box::new(d) as Box<dyn Backend>),
                };
                let mut db = match opened.and_then(|mut d| d.info().map(|i| (d, i))) {
                    Ok((d, (product, schema))) => {
                        if !send(Reply::Connected { product, schema }) {
                            return;
                        }
                        d
                    }
                    Err(e) => {
                        send(Reply::Error(e));
                        return;
                    }
                };
                for req in req_rx {
                    let t0 = Instant::now();
                    let ms = || t0.elapsed().as_millis() as u64;
                    let r = match req {
                        Req::Schemas => db.schemas().map(Reply::Schemas),
                        Req::SetSchema(s) => db.set_schema(&s).map(|_| Reply::Done),
                        Req::Objects(s) => db.objects(&s).map(Reply::Objects),
                        Req::Columns(s, t) => db.columns(&s, &t).map(Reply::Columns),
                        Req::Indexes(s, t) => db.indexes(&s, &t).map(|(i, k)| Reply::Indexes(i, k)),
                        Req::Query { sql, max, offset } => db.query(&sql, max, offset).map(|r| match r {
                            Ok((cols, types, rows, more)) => Reply::Rows { cols, types, rows, more, ms: ms() },
                            Err(count) => Reply::Updated { count, ms: ms() },
                        }),
                        Req::Count(sql) => db.count(&sql).map(Reply::Count),
                        Req::Batch(stmts) => db.batch(&stmts).map(|count| Reply::Updated { count, ms: ms() }),
                        Req::AutoCommit(on) => db.set_autocommit(on).map(|_| Reply::Done),
                        Req::Commit => db.commit().map(|_| Reply::Done),
                        Req::Rollback => db.rollback().map(|_| Reply::Done),
                        Req::Ddl(s, n, k) => db.ddl(&s, &n, &k).map(Reply::Ddl),
                    };
                    if !send(r.unwrap_or_else(Reply::Error)) {
                        return;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Client { tx, rx })
    }

    pub fn send(&self, r: Req) {
        let _ = self.tx.send(r);
    }

    pub fn query(&self, sql: &str, max: usize) {
        self.send(Req::Query { sql: sql.into(), max, offset: 0 });
    }
}

// ---------------------------------------------------------------- 接続先を集める

/// Spring の設定（application*.properties）から接続先を集める。パスワードは読まない（つなぐときに入力する）
pub fn detect_sources(root: &Path) -> Vec<DataSource> {
    let mut files = Vec::new();
    fn walk(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if depth < 6 && !matches!(name.as_str(), "target" | "node_modules" | ".git" | ".idea" | "build" | "out" | "test") {
                    walk(&p, depth + 1, out);
                }
            } else if name.starts_with("application") && name.ends_with(".properties") {
                out.push(p);
            }
        }
    }
    walk(root, 0, &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let get = |k: &str| {
            text.lines().find_map(|l| {
                let (a, b) = l.split_once('=')?;
                (a.trim() == k).then(|| b.trim().to_string())
            })
        };
        let Some(url) = get("spring.datasource.url").filter(|u| u.starts_with("jdbc:")) else { continue };
        let user = get("spring.datasource.username").unwrap_or_default();
        // 名前: モジュールのフォルダ名と、プロファイル（application-dev → dev）
        let module = f.ancestors().find(|a| a.join("pom.xml").is_file() || a.join("build.gradle").is_file()).and_then(|a| a.file_name()).map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let stem = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
        let profile = stem.strip_prefix("application-").map(|p| format!(" ({p})")).unwrap_or_default();
        out.push(DataSource { name: format!("{module}{profile}"), url, user, origin: f.display().to_string() });
    }
    // 同じ接続先（URL と利用者）は 1 つに
    let mut seen = std::collections::HashSet::new();
    out.retain(|d| seen.insert((d.url.clone(), d.user.clone())));
    out
}

/// 設定ファイル（1 行に `名前 | jdbc:URL | 利用者`）の接続先
pub fn config_sources(text: &str, origin: &str) -> Vec<DataSource> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').map(str::trim).collect();
            let url = f.get(1)?.to_string();
            url.starts_with("jdbc:").then(|| DataSource { name: f[0].to_string(), url, user: f.get(2).unwrap_or(&"").to_string(), origin: origin.into() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_strings() {
        assert_eq!(oracle_connect_string("jdbc:oracle:thin:@//db:1521/SHOP"), "//db:1521/SHOP");
        assert_eq!(oracle_connect_string("jdbc:oracle:thin:@db:1521/SHOP"), "db:1521/SHOP");
        assert_eq!(oracle_connect_string("jdbc:oracle:thin:@db:1521:ORCL"), "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db)(PORT=1521))(CONNECT_DATA=(SID=ORCL)))");
        assert_eq!(oracle_connect_string("jdbc:oracle:thin:@MYALIAS"), "MYALIAS");
        assert_eq!(strip_semicolon("select 1 from dual;"), "select 1 from dual");
        assert_eq!(strip_semicolon("BEGIN x; END;"), "BEGIN x; END;");
        assert_eq!(Kind::of("jdbc:sqlite::memory:"), Some(Kind::Sqlite));
        assert_eq!(Kind::of("jdbc:postgresql://x"), None);
    }

    #[test]
    fn detect() {
        let dir = std::env::temp_dir().join(format!("nagi-dbsrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rest/src/main/resources")).unwrap();
        std::fs::write(dir.join("rest/pom.xml"), "<project/>").unwrap();
        std::fs::write(
            dir.join("rest/src/main/resources/application-dev.properties"),
            "spring.datasource.url=jdbc:oracle:thin:@//db:1521/X\nspring.datasource.username=APP\nspring.datasource.password=secret\n",
        )
        .unwrap();
        let v = detect_sources(&dir);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].name.as_str(), v[0].url.as_str(), v[0].user.as_str()), ("rest (dev)", "jdbc:oracle:thin:@//db:1521/X", "APP"));
        let c = config_sources("# x\nlocal | jdbc:sqlite::memory: |\n", "cfg");
        assert_eq!(c[0].name, "local");
    }

    /// SQLite のメモリ DB で、つなぐ・表を作る・行を入れる・読む・木・キー・まとめて送る・トランザクション
    #[test]
    fn sqlite_round_trip() {
        let c = Client::connect("jdbc:sqlite::memory:", "", "", std::sync::Arc::new(|| {})).unwrap();
        let wait = || c.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("応答が無い");
        assert!(matches!(wait(), Reply::Connected { product, schema } if product.starts_with("SQLite") && schema == "main"));
        c.query("create table customer(id integer primary key, name text not null)", 10);
        assert!(matches!(wait(), Reply::Updated { .. }));
        c.query("create table item(id integer primary key, name text, cust integer references customer(id))", 10);
        wait();
        c.query("create index item_name on item(name)", 10);
        wait();
        c.query("insert into item values (1, 'a\tb', null), (2, 'りんご', null), (3, 'c', null)", 10);
        assert!(matches!(wait(), Reply::Updated { count: 3, .. }));
        c.send(Req::Query { sql: "select id, name, cust from item order by id;".into(), max: 1, offset: 1 });
        match wait() {
            Reply::Rows { cols, types, rows, more, .. } => {
                assert_eq!(cols, vec!["id", "name", "cust"]);
                assert_eq!(types, vec!["INTEGER", "TEXT", "INTEGER"]);
                assert_eq!(rows, vec![vec![Some("2".to_string()), Some("りんご".into()), None]], "1 行飛ばして 1 行");
                assert!(more);
            }
            r => panic!("{r:?}"),
        }
        c.send(Req::Objects("main".into()));
        assert!(matches!(wait(), Reply::Objects(o) if o.tables == vec![("customer".to_string(), "TABLE".to_string()), ("item".into(), "TABLE".into())]));
        c.send(Req::Columns("main".into(), "item".into()));
        match wait() {
            Reply::Columns(cols) => {
                assert!(cols[0].pk && cols[2].fk == "customer", "{cols:?}");
            }
            r => panic!("{r:?}"),
        }
        c.send(Req::Indexes("main".into(), "item".into()));
        assert!(matches!(wait(), Reply::Indexes(i, k) if i.iter().any(|x| x.name == "item_name") && k.iter().any(|x| x.refs == "customer")));
        c.send(Req::Count("select count(*) from item".into()));
        assert!(matches!(wait(), Reply::Count(3)));
        // まとめて送る: 2 文目が 0 行なら全部取り消す
        c.send(Req::Batch(vec!["update item set name = 'x' where id = 1".into(), "update item set name = 'y' where id = 99".into()]));
        assert!(matches!(wait(), Reply::Error(m) if m.contains("2 文目")));
        c.send(Req::Batch(vec!["update item set name = 'x' where id = 1".into(), "delete from item where id = 3".into()]));
        assert!(matches!(wait(), Reply::Updated { count: 2, .. }));
        // 手動のトランザクション: ロールバックで消える
        c.send(Req::AutoCommit(false));
        wait();
        c.query("delete from item", 10);
        wait();
        c.send(Req::Rollback);
        wait();
        c.send(Req::Count("select count(*) from item".into()));
        assert!(matches!(wait(), Reply::Count(2)));
        c.send(Req::Ddl("main".into(), "item".into(), "TABLE".into()));
        assert!(matches!(wait(), Reply::Ddl(d) if d.starts_with("CREATE TABLE item")));
        c.query("select * from nothing", 10);
        assert!(matches!(wait(), Reply::Error(m) if m.contains("nothing")));
    }
}
