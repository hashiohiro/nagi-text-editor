//! データベースにつなぐ（右の欄の「データベース」タブ用）。
//!
//! JDBC のドライバーを Java の補助プログラム（NagiDb.java、ソースのまま動かす）で使う。ドライバーは Maven の
//! ローカルリポジトリ（~/.m2/repository）から URL の種類で探すので、Oracle の Instant Client などは要らない。
//! 補助プログラムは接続ごとに 1 つ動かしたままにし、標準入出力の 1 行ずつでやり取りする。やり取りは裏の
//! スレッドで行い、終わったら UI を起こす。落とすと補助プログラムを止める。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::Waker;

const HELPER: &str = include_str!("NagiDb.java");

/// 接続先
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSource {
    pub name: String,
    pub url: String,
    pub user: String,
    /// どこで見つけたか（ファイルのパス）
    pub origin: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub ty: String,
    pub size: String,
    pub nullable: bool,
}

#[derive(Clone, Debug)]
pub enum Reply {
    Connected { product: String, schema: String },
    /// (名前, TABLE / VIEW)
    Tables(Vec<(String, String)>),
    Columns(String, Vec<Column>),
    Rows { cols: Vec<String>, rows: Vec<Vec<Option<String>>>, more: bool, ms: u64 },
    Updated { count: i64, ms: u64 },
    Error(String),
}

enum Req {
    Connect { url: String, user: String, pass: String },
    Tables,
    Columns(String),
    Query { sql: String, max: usize },
}

/// 1 つの接続（補助プログラム）
pub struct Client {
    tx: Sender<Req>,
    pub rx: Receiver<Reply>,
    child: Child,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unesc(s: &str) -> Option<String> {
    if s == "\\N" {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn fields(l: &str) -> Vec<Option<String>> {
    l.split('\t').map(unesc).collect()
}

fn text(f: &[Option<String>], i: usize) -> String {
    f.get(i).cloned().flatten().unwrap_or_default()
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default()
}

/// Java を探す: PATH の java、無ければ ~/.jdks のいちばん新しいもの（JAVA_HOME は古いことがあるので後回し）
pub fn find_java() -> Option<PathBuf> {
    let ok = |p: &Path| {
        let mut c = Command::new(p);
        c.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
        no_window(&mut c);
        c.status().is_ok_and(|s| s.success())
    };
    if ok(Path::new("java")) {
        return Some(PathBuf::from("java"));
    }
    let mut jdks: Vec<PathBuf> = std::fs::read_dir(home().join(".jdks")).into_iter().flatten().flatten().map(|e| e.path().join("bin").join("java.exe")).filter(|p| p.is_file()).collect();
    jdks.sort();
    if let Some(p) = jdks.pop() {
        return Some(p);
    }
    let jh = PathBuf::from(std::env::var_os("JAVA_HOME")?).join("bin").join("java.exe");
    ok(&jh).then_some(jh)
}

#[allow(unused_variables)]
fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
}

/// 版の名前を数の並びとして比べる（21.9.0.0 < 23.7.0.25.01）
fn version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0)).collect()
}

/// JDBC の URL に合うドライバーの jar を ~/.m2 から探す（いちばん新しい版）
pub fn find_driver(url: &str) -> Option<PathBuf> {
    find_driver_in(&home().join(".m2").join("repository"), url)
}

pub fn find_driver_in(repo: &Path, url: &str) -> Option<PathBuf> {
    let kind = url.strip_prefix("jdbc:")?.split(':').next()?;
    let cands: &[&str] = match kind {
        "oracle" => &["com/oracle/database/jdbc/ojdbc11", "com/oracle/database/jdbc/ojdbc8", "com/oracle/database/jdbc/ojdbc10"],
        "postgresql" => &["org/postgresql/postgresql"],
        "mysql" => &["com/mysql/mysql-connector-j", "mysql/mysql-connector-java"],
        "mariadb" => &["org/mariadb/jdbc/mariadb-java-client"],
        "sqlserver" => &["com/microsoft/sqlserver/mssql-jdbc"],
        "h2" => &["com/h2database/h2"],
        "sqlite" => &["org/xerial/sqlite-jdbc"],
        _ => return None,
    };
    for c in cands {
        let dir = repo.join(c);
        let art = c.rsplit('/').next().unwrap_or("");
        let mut versions: Vec<String> = std::fs::read_dir(&dir).into_iter().flatten().flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
        versions.sort_by_key(|v| version_key(v));
        for v in versions.iter().rev() {
            let jar = dir.join(v).join(format!("{art}-{v}.jar"));
            if jar.is_file() {
                return Some(jar);
            }
        }
    }
    None
}

/// 補助プログラムを置く（中身が同じなら書かない）
fn helper_path(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join("NagiDb.java");
    if std::fs::read_to_string(&p).ok().as_deref() != Some(HELPER) {
        std::fs::write(&p, HELPER)?;
    }
    Ok(p)
}

fn helper_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("NAGI_DB_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("db")
}

impl Client {
    /// 補助プログラムを動かして `url` につなぎ始める（つながったら Reply::Connected が届く）
    pub fn connect(url: &str, user: &str, pass: &str, waker: Waker) -> Result<Client, String> {
        let java = find_java().ok_or("Java が見つかりません（PATH か ~/.jdks に JDK 11 以上が要ります）")?;
        let jar = find_driver(url).ok_or_else(|| format!("{} のドライバーが ~/.m2 にありません（プロジェクトを一度 mvn でビルドすると入ります）", url.split(':').take(2).collect::<Vec<_>>().join(":")))?;
        let helper = helper_path(&helper_dir()).map_err(|e| format!("補助プログラムを置けません: {e}"))?;
        let mut cmd = Command::new(java);
        cmd.arg("-Dfile.encoding=UTF-8").arg("-cp").arg(&jar).arg(&helper).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("Java を起動できません: {e}"))?;
        let stdin = child.stdin.take().ok_or("標準入力を開けません")?;
        let stdout = child.stdout.take().ok_or("標準出力を開けません")?;
        let (tx, req_rx) = channel::<Req>();
        let (reply_tx, rx) = channel::<Reply>();
        let _ = std::thread::Builder::new().name("db".into()).spawn(move || serve(stdin, BufReader::new(stdout), req_rx, reply_tx, waker));
        let c = Client { tx, rx, child };
        c.send(Req::Connect { url: url.into(), user: user.into(), pass: pass.into() });
        Ok(c)
    }

    fn send(&self, r: Req) {
        let _ = self.tx.send(r);
    }

    pub fn tables(&self) {
        self.send(Req::Tables);
    }

    pub fn columns(&self, table: &str) {
        self.send(Req::Columns(table.into()));
    }

    pub fn query(&self, sql: &str, max: usize) {
        self.send(Req::Query { sql: sql.into(), max });
    }
}

fn serve(mut stdin: ChildStdin, mut out: BufReader<std::process::ChildStdout>, rx: Receiver<Req>, tx: Sender<Reply>, waker: Waker) {
    let read_line = |out: &mut BufReader<std::process::ChildStdout>| -> Option<String> {
        let mut s = String::new();
        match out.read_line(&mut s) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(s.trim_end_matches(['\r', '\n']).to_string()),
        }
    };
    // 起動を待つ（コンパイルに 1 秒ほどかかる）
    match read_line(&mut out) {
        Some(l) if l == "READY" => {}
        _ => {
            let _ = tx.send(Reply::Error("補助プログラムが起動しませんでした（Java 11 以上か確かめてください）".into()));
            waker();
            return;
        }
    }
    for req in rx {
        let (line, columns_of) = match &req {
            Req::Connect { url, user, pass } => (format!("CONNECT\t{}\t{}\t{}", esc(url), esc(user), esc(pass)), None),
            Req::Tables => ("TABLES\t".to_string(), None),
            Req::Columns(t) => (format!("COLUMNS\t\t{}", esc(t)), Some(t.clone())),
            Req::Query { sql, max } => (format!("QUERY\t{max}\t{}", esc(sql)), None),
        };
        if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
            let _ = tx.send(Reply::Error("接続が切れました".into()));
            waker();
            return;
        }
        let mut tables = Vec::new();
        let mut cols = Vec::new();
        let mut head = Vec::new();
        let mut rows = Vec::new();
        let reply = loop {
            let Some(l) = read_line(&mut out) else { break Reply::Error("接続が切れました".into()) };
            let f = fields(&l);
            match text(&f, 0).as_str() {
                // H2 2.x などは BASE TABLE と返すので TABLE にそろえる
                "T" => tables.push((text(&f, 1), text(&f, 2).replace("BASE TABLE", "TABLE"))),
                "C" => cols.push(Column { name: text(&f, 1), ty: text(&f, 2), size: text(&f, 3), nullable: text(&f, 4) != "0" }),
                "H" => head = f[1..].iter().map(|x| x.clone().unwrap_or_default()).collect(),
                "R" => rows.push(f[1..].to_vec()),
                "ERR" => break Reply::Error(text(&f, 1)),
                "OK" => {
                    break match &req {
                        Req::Connect { .. } => Reply::Connected { product: text(&f, 1), schema: text(&f, 2) },
                        Req::Tables => Reply::Tables(std::mem::take(&mut tables)),
                        Req::Columns(_) => Reply::Columns(columns_of.clone().unwrap_or_default(), std::mem::take(&mut cols)),
                        Req::Query { .. } if text(&f, 1) == "updated" => Reply::Updated { count: text(&f, 2).parse().unwrap_or(0), ms: text(&f, 3).parse().unwrap_or(0) },
                        Req::Query { .. } => Reply::Rows { cols: std::mem::take(&mut head), rows: std::mem::take(&mut rows), more: text(&f, 3) == "1", ms: text(&f, 4).parse().unwrap_or(0) },
                    };
                }
                _ => {}
            }
        };
        if tx.send(reply).is_err() {
            return;
        }
        waker();
    }
}

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
    fn escape_round_trip() {
        let s = "a\tb\\c\nd";
        assert_eq!(unesc(&esc(s)).as_deref(), Some(s));
        assert_eq!(unesc("\\N"), None);
        assert_eq!(version_key("23.7.0.25.01") > version_key("21.9.0.0"), true);
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
        let c = config_sources("# x\nlocal | jdbc:h2:mem:t | sa\n", "cfg");
        assert_eq!(c[0].name, "local");
    }

    /// 本物の JDBC（~/.m2 の H2 のメモリ DB）でつなぎ、表を作って読む。Java か H2 が無ければ飛ばす
    #[test]
    fn h2_round_trip() {
        if find_java().is_none() || find_driver("jdbc:h2:mem:x").is_none() {
            eprintln!("Java か H2 が無いので飛ばす");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nagi-dbhelper-{}", std::process::id()));
        // SAFETY: テストの中だけで使う
        unsafe { std::env::set_var("NAGI_DB_DIR", &dir) };
        let c = Client::connect("jdbc:h2:mem:nagi", "sa", "", std::sync::Arc::new(|| {})).unwrap();
        let wait = |c: &Client| c.rx.recv_timeout(std::time::Duration::from_secs(60)).expect("応答が無い");
        let r = wait(&c);
        assert!(matches!(&r, Reply::Connected { product, .. } if product.starts_with("H2")), "{r:?}");
        c.query("create table item(id int primary key, name varchar(20), note varchar(20))", 100);
        assert!(matches!(wait(&c), Reply::Updated { .. }));
        c.query("insert into item values (1, 'a\tb', null), (2, 'りんご', 'x')", 100);
        assert!(matches!(wait(&c), Reply::Updated { count: 2, .. }));
        c.query("select * from item order by id;", 1);
        match wait(&c) {
            Reply::Rows { cols, rows, more, .. } => {
                assert_eq!(cols, vec!["ID", "NAME", "NOTE"]);
                assert_eq!(rows, vec![vec![Some("1".to_string()), Some("a\tb".into()), None]]);
                assert!(more, "max を超えた分がある");
            }
            r => panic!("{r:?}"),
        }
        c.tables();
        match wait(&c) {
            Reply::Tables(t) => assert!(t.contains(&("ITEM".to_string(), "TABLE".to_string())), "{t:?}"),
            r => panic!("{r:?}"),
        }
        c.columns("ITEM");
        match wait(&c) {
            Reply::Columns(t, cols) => {
                assert_eq!(t, "ITEM");
                assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["ID", "NAME", "NOTE"]);
                assert!(!cols[0].nullable);
            }
            r => panic!("{r:?}"),
        }
        c.query("select * from nothing", 10);
        assert!(matches!(wait(&c), Reply::Error(m) if m.contains("NOTHING")));
    }
}
