//! Salesforce（SOQL）。データベースと同じ形（db.rs の Backend）で、木・SOQL コンソール・結果の表・補完に乗せる。
//!
//! - ログイン: ブラウザで OAuth 2.0（認可コード + PKCE）。接続アプリは Salesforce CLI と同じ公開のもの（PlatformCLI）、
//!   戻り先は http://localhost:1717/OauthRedirect（Nagi が受ける）。SSO や多要素認証もいつもの画面のまま。
//! - トークン: リフレッシュトークンを Windows の DPAPI（このユーザーだけが読める）で暗号化して
//!   `%LOCALAPPDATA%\nagi\salesforce\` に置き、次からはブラウザを開かずにつなぐ。
//! - REST API との通信は Windows の curl.exe（トークンは標準入力で渡し、コマンドラインに出さない）。
//! - 読むだけ: SOQL の SELECT と、オブジェクト・項目の一覧（describe）。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::db::{Backend, Column, Index, Key, Objects};

pub const CLIENT_ID: &str = "PlatformCLI";
pub const REDIRECT: &str = "http://localhost:1717/OauthRedirect";
pub const API: &str = "v62.0";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

type R<T> = Result<T, String>;

/// JDBC 風の URL（`salesforce:https://login.salesforce.com`）からログインの URL
pub fn login_url(url: &str) -> String {
    let rest = url.split_once(':').map_or("", |(_, r)| r).trim();
    if rest.is_empty() { "https://login.salesforce.com".into() } else { rest.trim_end_matches('/').to_string() }
}

fn win(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn curl_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let p = root.join("System32").join("curl.exe");
    if p.is_file() { p } else { PathBuf::from("curl.exe") }
}

/// curl.exe に設定（`-K -`）を標準入力で渡して流す。(HTTP の状態, 本文)
fn curl(config: &str) -> R<(u16, String)> {
    let mut child = win(Command::new(curl_exe()).args(["-sS", "-K", "-", "-w", "\n%{http_code}"]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl.exe を起動できません: {e}"))?;
    child.stdin.take().ok_or("標準入力を開けません")?.write_all(config.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if code == 0 {
        return Err(format!("つなげません: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((code, body.to_string()))
}

/// curl の設定ファイルの中の文字列（"..." の中の \ と " を逃がす）
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn base64url(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        let k = c.len() + 1;
        for i in 0..k {
            out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(windows)]
fn random(n: usize) -> R<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut buf = vec![0u8; n];
    let st = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), n as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if st != 0 {
        return Err(format!("乱数を作れません（{st:#x}）"));
    }
    Ok(buf)
}

#[cfg(windows)]
pub fn sha256(data: &[u8]) -> R<[u8; 32]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, std::ptr::null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32) };
    if st != 0 {
        return Err(format!("SHA-256 を計算できません（{st:#x}）"));
    }
    Ok(out)
}

/// DPAPI で暗号化・復号する（このユーザーだけが読める）
#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> R<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 {
        return Err("トークンを暗号化・復号できません".into());
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as _) };
    Ok(v)
}

#[cfg(not(windows))]
fn random(n: usize) -> R<Vec<u8>> {
    Err(format!("この OS では使えません（{n}）"))
}
#[cfg(not(windows))]
pub fn sha256(_: &[u8]) -> R<[u8; 32]> {
    Err("この OS では使えません".into())
}
#[cfg(not(windows))]
fn dpapi(_: &[u8], _: bool) -> R<Vec<u8>> {
    Err("この OS では使えません".into())
}

fn token_path(url: &str, user: &str) -> PathBuf {
    let dir = std::env::var_os("NAGI_SF_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("nagi").join("salesforce"));
    let key: String = format!("{}_{user}", login_url(url)).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    dir.join(format!("{key}.token"))
}

/// 保存したトークンを消す（ログアウト）
pub fn forget(url: &str, user: &str) -> bool {
    std::fs::remove_file(token_path(url, user)).is_ok()
}

fn save_refresh(url: &str, user: &str, token: &str) {
    let p = token_path(url, user);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(enc) = dpapi(token.as_bytes(), true) {
        let _ = std::fs::write(p, enc);
    }
}

fn load_refresh(url: &str, user: &str) -> Option<String> {
    let enc = std::fs::read(token_path(url, user)).ok()?;
    String::from_utf8(dpapi(&enc, false).ok()?).ok()
}

/// ブラウザで開く
fn open_browser(url: &str) -> R<()> {
    win(Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url])).spawn().map(|_| ()).map_err(|e| format!("ブラウザを開けません: {e}"))
}

/// 戻り先で受けた要求の行から code と state を取り出す
pub fn parse_callback(request_line: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let path = request_line.split_whitespace().nth(1)?;
    let query = path.split_once('?').map_or("", |(_, q)| q);
    let get = |k: &str| {
        query.split('&').find_map(|kv| {
            let (a, b) = kv.split_once('=')?;
            (a == k).then(|| urldecode(b))
        })
    };
    path.starts_with("/OauthRedirect").then(|| (get("code"), get("state"), get("error_description").or_else(|| get("error"))))
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// ブラウザでログインして認可コードを受け取る
fn browser_login(login: &str, user: &str, note: &dyn Fn(String)) -> R<(String, String)> {
    let verifier = base64url(&random(32)?);
    let challenge = base64url(&sha256(verifier.as_bytes())?);
    let state = base64url(&random(16)?);
    // localhost は ::1 にも 127.0.0.1 にもなりうるので両方で待つ
    let listeners: Vec<TcpListener> = ["127.0.0.1:1717", "[::1]:1717"].iter().filter_map(|a| TcpListener::bind(a).ok()).collect();
    if listeners.is_empty() {
        return Err("1717 番のポートを使えません（Salesforce CLI のログインが動いていれば閉じてください）".into());
    }
    for l in &listeners {
        let _ = l.set_nonblocking(true);
    }
    let mut url = format!(
        "{login}/services/oauth2/authorize?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state={state}&scope={}",
        urlencode(REDIRECT),
        urlencode("api refresh_token web")
    );
    if !user.is_empty() {
        url.push_str(&format!("&login_hint={}", urlencode(user)));
    }
    open_browser(&url)?;
    note("ブラウザで Salesforce にログインしてください（5 分まで待ちます）".into());
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > LOGIN_TIMEOUT {
            return Err("ログインを待ちきれませんでした（5 分）".into());
        }
        for l in &listeners {
            let Ok((mut s, _)) = l.accept() else { continue };
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let line = req.lines().next().unwrap_or("");
            let Some((code, st, err)) = parse_callback(line) else {
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            };
            let (ok, msg) = match (&code, &st, &err) {
                (Some(_), Some(s2), _) if *s2 == state => (true, "ログインしました。Nagi に戻ってください。"),
                (_, _, Some(_)) => (false, "ログインできませんでした。Nagi の表示を見てください。"),
                _ => (false, "ログインの確認に失敗しました（state が合いません）。"),
            };
            let body = format!("<!doctype html><meta charset=\"utf-8\"><title>Nagi</title><body style=\"font-family:sans-serif;background:#0c0c0c;color:#ccc;padding:40px\"><p>{msg}</p></body>");
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            if ok {
                return Ok((code.unwrap(), verifier));
            }
            return Err(err.unwrap_or_else(|| "state が合いません".into()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// トークンの要求（本文は標準入力で渡す）
fn token_request(login: &str, form: &str) -> R<Value> {
    let config = format!("url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/x-www-form-urlencoded\"\ndata = {}\n", q(&format!("{login}/services/oauth2/token")), q(form));
    let (code, body) = curl(&config)?;
    let v: Value = serde_json::from_str(&body).map_err(|_| format!("トークンの応答を読めません（{code}）"))?;
    if code != 200 {
        return Err(format!("ログインできません: {}", v.get("error_description").and_then(Value::as_str).unwrap_or(&body)));
    }
    Ok(v)
}

pub struct SalesforceDb {
    login: String,
    user: String,
    url: String,
    instance: String,
    access: String,
    refresh: Option<String>,
}

impl SalesforceDb {
    pub fn connect(url: &str, user: &str, note: &dyn Fn(String)) -> R<Self> {
        let login = login_url(url);
        let mut db = SalesforceDb { login: login.clone(), user: user.into(), url: url.into(), instance: String::new(), access: String::new(), refresh: load_refresh(url, user) };
        if db.refresh.is_some() && db.refresh_token().is_ok() {
            return Ok(db);
        }
        let (code, verifier) = browser_login(&login, user, note)?;
        let v = token_request(&login, &format!("grant_type=authorization_code&code={}&client_id={CLIENT_ID}&redirect_uri={}&code_verifier={verifier}", urlencode(&code), urlencode(REDIRECT)))?;
        db.take_tokens(&v)?;
        Ok(db)
    }

    fn take_tokens(&mut self, v: &Value) -> R<()> {
        self.access = v.get("access_token").and_then(Value::as_str).ok_or("アクセストークンがありません")?.to_string();
        if let Some(i) = v.get("instance_url").and_then(Value::as_str) {
            self.instance = i.trim_end_matches('/').to_string();
        }
        if let Some(r) = v.get("refresh_token").and_then(Value::as_str) {
            self.refresh = Some(r.to_string());
            save_refresh(&self.url, &self.user, r);
        }
        Ok(())
    }

    fn refresh_token(&mut self) -> R<()> {
        let r = self.refresh.clone().ok_or("リフレッシュトークンがありません")?;
        let v = token_request(&self.login, &format!("grant_type=refresh_token&refresh_token={}&client_id={CLIENT_ID}", urlencode(&r)))?;
        self.take_tokens(&v)
    }

    /// API を呼ぶ（期限切れなら 1 度だけ更新してやり直す）
    fn get(&mut self, path: &str) -> R<Value> {
        for attempt in 0..2 {
            let url = if path.starts_with("http") { path.to_string() } else { format!("{}{path}", self.instance) };
            let config = format!("url = {}\nheader = {}\nheader = \"Accept: application/json\"\n", q(&url), q(&format!("Authorization: Bearer {}", self.access)));
            let (code, body) = curl(&config)?;
            if code == 401 && attempt == 0 && self.refresh.is_some() {
                self.refresh_token()?;
                continue;
            }
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if code >= 400 {
                let msg = v.get(0).and_then(|e| e.get("message")).and_then(Value::as_str).map(String::from).unwrap_or(body);
                return Err(msg);
            }
            return Ok(v);
        }
        Err("ログインし直してください".into())
    }
}

/// レコードを平らな列にする（`Account.Name` の形。子のリレーションは件数）
pub fn flatten(rec: &Value, prefix: &str, cols: &mut Vec<String>, out: &mut Vec<(String, Option<String>)>) {
    let Some(o) = rec.as_object() else { return };
    for (k, v) in o {
        if k == "attributes" {
            continue;
        }
        let name = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            Value::Object(m) if m.contains_key("records") => {
                let n = m.get("totalSize").and_then(Value::as_i64).unwrap_or(0);
                push(cols, out, name, Some(format!("({n} 件)")));
            }
            Value::Object(_) => flatten(v, &name, cols, out),
            Value::Null => push(cols, out, name, None),
            Value::String(s) => push(cols, out, name, Some(s.clone())),
            other => push(cols, out, name, Some(other.to_string())),
        }
    }
}

fn push(cols: &mut Vec<String>, out: &mut Vec<(String, Option<String>)>, name: String, v: Option<String>) {
    if !cols.contains(&name) {
        cols.push(name.clone());
    }
    out.push((name, v));
}

/// SOQL の応答を表にする
pub fn records_to_rows(records: &[Value]) -> (Vec<String>, Vec<Vec<Option<String>>>) {
    let mut cols = Vec::new();
    let mut flat = Vec::new();
    for r in records {
        let mut one = Vec::new();
        flatten(r, "", &mut cols, &mut one);
        flat.push(one);
    }
    let rows: Vec<Vec<Option<String>>> = flat.into_iter().map(|one| cols.iter().map(|c| one.iter().find(|(n, _)| n == c).and_then(|x| x.1.clone())).collect()).collect();
    // 空の参照（Account: null）だけの列は、Account.Name などがあれば出さない
    let keep: Vec<bool> = cols.iter().enumerate().map(|(k, c)| !(rows.iter().all(|r| r[k].is_none()) && cols.iter().any(|o| o.starts_with(&format!("{c}."))))).collect();
    let cols2 = cols.iter().zip(&keep).filter(|x| *x.1).map(|x| x.0.clone()).collect();
    let rows2 = rows.into_iter().map(|r| r.into_iter().zip(&keep).filter(|x| *x.1).map(|x| x.0).collect()).collect();
    (cols2, rows2)
}

impl Backend for SalesforceDb {
    fn info(&mut self) -> R<(String, String)> {
        let host = self.instance.trim_start_matches("https://").to_string();
        Ok((format!("Salesforce API {API}"), host))
    }

    fn schemas(&mut self) -> R<Vec<String>> {
        Ok(vec![self.instance.trim_start_matches("https://").to_string()])
    }

    fn set_schema(&mut self, _s: &str) -> R<()> {
        Err("Salesforce ではスキーマを切り替えません".into())
    }

    fn objects(&mut self, _schema: &str) -> R<Objects> {
        let v = self.get(&format!("/services/data/{API}/sobjects"))?;
        let mut o = Objects::default();
        for s in v.get("sobjects").and_then(Value::as_array).into_iter().flatten() {
            if s.get("queryable").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let name = s.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            // カスタムオブジェクトはビューの欄に分けて出す
            let custom = s.get("custom").and_then(Value::as_bool) == Some(true);
            o.tables.push((name, if custom { "VIEW".into() } else { "TABLE".into() }));
        }
        Ok(o)
    }

    fn columns(&mut self, _schema: &str, table: &str) -> R<Vec<Column>> {
        let v = self.get(&format!("/services/data/{API}/sobjects/{}/describe", urlencode(table)))?;
        Ok(v.get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|f| {
                let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let ty = s("type");
                let len = f.get("length").and_then(Value::as_i64).unwrap_or(0);
                let ty = if len > 0 && matches!(ty.as_str(), "string" | "textarea" | "picklist" | "email" | "phone" | "url") { format!("{ty}({len})") } else { ty };
                let refs: Vec<String> = f.get("referenceTo").and_then(Value::as_array).into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
                Column { name: s("name"), ty: format!("{ty} {}", s("label")), nullable: f.get("nillable").and_then(Value::as_bool).unwrap_or(true), pk: s("type") == "id", fk: refs.join("/") }
            })
            .collect())
    }

    fn indexes(&mut self, _schema: &str, _table: &str) -> R<(Vec<Index>, Vec<Key>)> {
        Ok((Vec::new(), Vec::new()))
    }

    fn query(&mut self, sql: &str, max: usize, offset: usize) -> R<Result<(Vec<String>, Vec<String>, Vec<Vec<Option<String>>>, bool), i64>> {
        let soql = crate::db::strip_semicolon(sql);
        if !soql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
            return Err("Salesforce では SOQL の SELECT だけを流せます".into());
        }
        let mut v = self.get(&format!("/services/data/{API}/query?q={}", urlencode(&soql)))?;
        let mut records: Vec<Value> = Vec::new();
        let total = v.get("totalSize").and_then(Value::as_i64).unwrap_or(0);
        loop {
            records.extend(v.get("records").and_then(Value::as_array).cloned().unwrap_or_default());
            let done = v.get("done").and_then(Value::as_bool).unwrap_or(true);
            if done || records.len() > offset + max {
                break;
            }
            let Some(next) = v.get("nextRecordsUrl").and_then(Value::as_str).map(String::from) else { break };
            v = self.get(&next)?;
        }
        // COUNT() だけの問い合わせは件数を 1 行に
        if records.is_empty() && soql.to_ascii_uppercase().contains("COUNT()") {
            return Ok(Ok((vec!["COUNT".into()], vec![String::new()], vec![vec![Some(total.to_string())]], false)));
        }
        let more = (records.len() as i64) < total || records.len() > offset + max;
        let page: Vec<Value> = records.into_iter().skip(offset).take(max).collect();
        let (cols, rows) = records_to_rows(&page);
        let types = vec![String::new(); cols.len()];
        Ok(Ok((cols, types, rows, more && page.len() == max)))
    }

    fn count(&mut self, sql: &str) -> R<i64> {
        let v = self.get(&format!("/services/data/{API}/query?q={}", urlencode(&crate::db::strip_semicolon(sql))))?;
        Ok(v.get("totalSize").and_then(Value::as_i64).unwrap_or(0))
    }

    fn batch(&mut self, _stmts: &[String]) -> R<i64> {
        Err("Salesforce のデータはここでは直せません（読むだけ）".into())
    }

    fn set_autocommit(&mut self, _on: bool) -> R<()> {
        Err("Salesforce にトランザクションはありません".into())
    }

    fn commit(&mut self) -> R<()> {
        Ok(())
    }

    fn rollback(&mut self) -> R<()> {
        Ok(())
    }

    fn ddl(&mut self, _schema: &str, _name: &str, _kind: &str) -> R<String> {
        Err("Salesforce には DDL がありません".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(login_url("salesforce:"), "https://login.salesforce.com");
        assert_eq!(login_url("salesforce:https://test.salesforce.com/"), "https://test.salesforce.com");
        assert_eq!(urlencode("SELECT Id FROM Account WHERE Name = 'A&B'"), "SELECT%20Id%20FROM%20Account%20WHERE%20Name%20%3D%20%27A%26B%27");
        // RFC 7636 の付録 B の例
        assert_eq!(base64url(&sha256(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap()), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(base64url(b"ab"), "YWI");
        let (code, state, err) = parse_callback("GET /OauthRedirect?code=aPrx%3D%3D&state=xyz HTTP/1.1").unwrap();
        assert_eq!((code.as_deref(), state.as_deref(), err), (Some("aPrx=="), Some("xyz"), None));
        assert!(parse_callback("GET /favicon.ico HTTP/1.1").is_none());
        // DPAPI で暗号化して戻す
        let enc = dpapi(b"secret", true).unwrap();
        assert_ne!(enc, b"secret");
        assert_eq!(dpapi(&enc, false).unwrap(), b"secret");
    }

    #[test]
    fn records() {
        let v: Value = serde_json::from_str(
            r#"[{"attributes":{"type":"Opportunity"},"Id":"006A","Name":"大型案件","Amount":1200000.5,"Account":{"attributes":{"type":"Account"},"Name":"株式会社サンプル"},"OpportunityLineItems":{"totalSize":3,"done":true,"records":[]}},
                {"attributes":{"type":"Opportunity"},"Id":"006B","Name":"小口","Amount":null,"Account":null,"OpportunityLineItems":null}]"#,
        )
        .unwrap();
        let (cols, rows) = records_to_rows(v.as_array().unwrap());
        assert_eq!(cols, vec!["Id", "Name", "Amount", "Account.Name", "OpportunityLineItems"]);
        assert_eq!(rows[0], vec![Some("006A".into()), Some("大型案件".into()), Some("1200000.5".into()), Some("株式会社サンプル".into()), Some("(3 件)".into())]);
        assert_eq!(rows[1][3], None);
    }
}
