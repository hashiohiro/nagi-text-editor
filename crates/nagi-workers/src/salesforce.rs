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
use std::process::Stdio;
use std::time::{Duration, Instant};

use nagi_core::hash::sha256;

use serde_json::Value;

use crate::db::{Backend, Column, Index, Key, Objects};

pub const CLIENT_ID: &str = "PlatformCLI";
pub const REDIRECT: &str = "http://localhost:1717/OauthRedirect";
pub const API: &str = "v62.0";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

type R<T> = nagi_core::Result<T>;

/// JDBC 風の URL（`salesforce:https://login.salesforce.com`）からログインの URL
pub fn login_url(url: &str) -> String {
    let rest = url.split_once(':').map_or("", |(_, r)| r).trim();
    if rest.is_empty() { "https://login.salesforce.com".into() } else { rest.trim_end_matches('/').to_string() }
}

/// curl.exe に設定（`-K -`）を標準入力で渡して流す。(HTTP の状態, 本文)
fn curl(config: &str) -> R<(u16, String)> {
    let mut child = crate::exec::system("curl.exe")
        .args(["-sS", "-K", "-", "-w", "\n%{http_code}"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl.exe を起動できません: {e}"))?;
    child.stdin.take().ok_or("標準入力を開けません")?.write_all(config.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if code == 0 {
        return Err(format!("つなげません: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
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
        return Err(format!("乱数を作れません（{st:#x}）").into());
    }
    Ok(buf)
}

/// DPAPI で暗号化・復号する（このユーザーだけが読める。データベースのパスワードの保存にも使う）
#[cfg(windows)]
pub(crate) fn dpapi(data: &[u8], protect: bool) -> R<Vec<u8>> {
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
        return Err("暗号化・復号できません（DPAPI）".into());
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
pub(crate) fn dpapi(_: &[u8], _: bool) -> R<Vec<u8>> {
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
    // 自動テスト: ブラウザの代わりに curl でログイン画面の転送をたどる（偽の Salesforce が戻り先へ転送する）
    if std::env::var("NAGI_SF_BROWSER").as_deref() == Ok("curl") {
        return crate::exec::system("curl.exe").args(["-s", "-L", "-o", "NUL", url]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ()).map_err(nagi_core::Error::of);
    }
    crate::config::open_url(url)
}

/// 自動テスト用の偽の Salesforce（OAuth の認可・トークン、REST API の sobjects / describe / query に答える）
#[doc(hidden)]
pub mod mock {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Debug)]
    pub struct Seen {
        /// 認可（ブラウザでのログイン）を受けた回数
        pub authorize: usize,
        /// トークンの要求（grant_type ごと）
        pub grants: Vec<String>,
        /// PKCE の照合に通ったか
        pub pkce_ok: bool,
        /// API に来た Authorization
        pub bearers: Vec<String>,
        pub queries: Vec<String>,
    }

    pub struct Mock {
        pub base: String,
        pub seen: Arc<Mutex<Seen>>,
    }

    fn query_param(path: &str, k: &str) -> Option<String> {
        path.split_once('?')?.1.split('&').find_map(|kv| {
            let (a, b) = kv.split_once('=')?;
            (a == k).then(|| super::urldecode(b))
        })
    }

    /// 127.0.0.1 の空いたポートで待つ（スレッドで答え続ける）
    pub fn start() -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (b2, s2) = (base.clone(), seen.clone());
        std::thread::spawn(move || {
            let mut challenge = String::new();
            let mut refreshes = 0;
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let (method, path) = {
                    let mut it = line.split_whitespace();
                    (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
                };
                let mut len = 0;
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    let low = h.to_ascii_lowercase();
                    if let Some(v) = low.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if low.starts_with("authorization:") {
                        auth = h.split_once(':').map_or("", |x| x.1).trim().to_string();
                    }
                }
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let body = String::from_utf8_lossy(&body).into_owned();
                let mut seen = s2.lock().unwrap();
                let (status, extra, json) = if path.starts_with("/services/oauth2/authorize") {
                    seen.authorize += 1;
                    challenge = query_param(&path, "code_challenge").unwrap_or_default();
                    let to = format!("{}?code=CODE123&state={}", query_param(&path, "redirect_uri").unwrap_or_default(), query_param(&path, "state").unwrap_or_default());
                    ("302 Found", format!("Location: {to}\r\n"), String::new())
                } else if path.starts_with("/services/oauth2/token") && method == "POST" {
                    let form = |k: &str| body.split('&').find_map(|kv| kv.split_once('=').filter(|x| x.0 == k).map(|x| super::urldecode(x.1)));
                    let grant = form("grant_type").unwrap_or_default();
                    seen.grants.push(grant.clone());
                    if grant == "authorization_code" {
                        let v = form("code_verifier").unwrap_or_default();
                        seen.pkce_ok = super::base64url(&super::sha256(v.as_bytes()).unwrap()) == challenge && form("code").as_deref() == Some("CODE123");
                        ("200 OK", String::new(), format!(r#"{{"access_token":"tok1","refresh_token":"REFRESH-SECRET","instance_url":"{b2}"}}"#))
                    } else {
                        refreshes += 1;
                        ("200 OK", String::new(), format!(r#"{{"access_token":"tok{}","instance_url":"{b2}"}}"#, refreshes + 1))
                    }
                } else {
                    seen.bearers.push(auth.clone());
                    if path.ends_with("/sobjects") {
                        ("200 OK", String::new(), r#"{"sobjects":[{"name":"Account","custom":false,"queryable":true},{"name":"Invoice__c","custom":true,"queryable":true},{"name":"Secret","custom":false,"queryable":false}]}"#.into())
                    } else if path.contains("/sobjects/Account/describe") {
                        ("200 OK", String::new(), r#"{"fields":[{"name":"Id","type":"id","label":"取引先 ID","length":18,"nillable":false,"referenceTo":[]},{"name":"Name","type":"string","label":"取引先名","length":255,"nillable":false,"referenceTo":[]},{"name":"OwnerId","type":"reference","label":"所有者","length":18,"nillable":false,"referenceTo":["User"]}]}"#.into())
                    } else if path.contains("/query/next1") {
                        ("200 OK", String::new(), r#"{"totalSize":3,"done":true,"records":[{"attributes":{"type":"Account"},"Id":"001C","Name":"丙","Owner":null}]}"#.into())
                    } else if path.contains("/query?q=") {
                        seen.queries.push(query_param(&path, "q").unwrap_or_default());
                        (
                            "200 OK",
                            String::new(),
                            format!(r#"{{"totalSize":3,"done":false,"nextRecordsUrl":"/services/data/{}/query/next1","records":[{{"attributes":{{"type":"Account"}},"Id":"001A","Name":"甲","Owner":{{"attributes":{{"type":"User"}},"Name":"佐藤"}}}},{{"attributes":{{"type":"Account"}},"Id":"001B","Name":"乙","Owner":{{"attributes":{{"type":"User"}},"Name":"鈴木"}}}}]}}"#, super::API),
                        )
                    } else {
                        ("404 Not Found", String::new(), r#"[{"message":"not found"}]"#.into())
                    }
                };
                drop(seen);
                let mut s = s;
                let _ = s.write_all(format!("HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).as_bytes());
            }
        });
        Mock { base, seen }
    }
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
            return Err(err.unwrap_or_else(|| "state が合いません".into()).into());
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
        return Err(format!("ログインできません: {}", v.get("error_description").and_then(Value::as_str).unwrap_or(&body)).into());
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
                return Err(msg.into());
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

    /// 偽の Salesforce に対して: ブラウザでのログイン（PKCE）→ トークンの暗号化保存 → オブジェクト・項目 → SOQL（続きのページ）
    /// → 2 回目はブラウザを開かずリフレッシュトークンでつなぐ
    #[test]
    fn oauth_and_query_against_mock() {
        use crate::db::{Client, Kind, Reply, Req};
        let m = mock::start();
        let dir = std::env::temp_dir().join(format!("nagi-sf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: テストの中だけで使う
        unsafe {
            std::env::set_var("NAGI_SF_DIR", &dir);
            std::env::set_var("NAGI_SF_BROWSER", "curl");
        }
        let url = format!("salesforce:{}", m.base);
        assert_eq!(Kind::of(&url), Some(Kind::Salesforce));
        let c = Client::connect(&url, "me@example.com", "", std::sync::Arc::new(|| {})).unwrap();
        let next = |c: &Client| loop {
            match c.rx.recv_timeout(std::time::Duration::from_secs(30)).expect("応答が無い") {
                Reply::Note(_) => continue,
                r => break r,
            }
        };
        assert!(matches!(next(&c), Reply::Connected { product, .. } if product.starts_with("Salesforce")));
        {
            let s = m.seen.lock().unwrap();
            assert_eq!(s.authorize, 1, "ブラウザ（の代わりの curl）で 1 回ログイン");
            assert!(s.pkce_ok, "PKCE の照合に通る");
        }
        // リフレッシュトークンは暗号化して保存（平文で残らない）
        let saved: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(saved.len(), 1);
        let bytes = std::fs::read(saved[0].path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("REFRESH-SECRET"));
        c.send(Req::Objects(String::new()));
        match next(&c) {
            Reply::Objects(o) => assert_eq!(o.tables, vec![("Account".to_string(), "TABLE".to_string()), ("Invoice__c".into(), "VIEW".into())], "queryable だけ。カスタムは分ける"),
            r => panic!("{r:?}"),
        }
        c.send(Req::Columns(String::new(), "Account".into()));
        match next(&c) {
            Reply::Columns(cols) => {
                assert!(cols[0].pk && cols[0].name == "Id");
                assert_eq!((cols[2].name.as_str(), cols[2].fk.as_str()), ("OwnerId", "User"));
                assert_eq!(cols[1].ty, "string(255) 取引先名");
            }
            r => panic!("{r:?}"),
        }
        c.query("SELECT Id, Name, Owner.Name FROM Account", 500);
        match next(&c) {
            Reply::Rows { cols, rows, more, .. } => {
                assert_eq!(cols, vec!["Id", "Name", "Owner.Name"]);
                assert_eq!(rows.len(), 3, "続きのページもたどる");
                assert_eq!(rows[0], vec![Some("001A".to_string()), Some("甲".into()), Some("佐藤".into())]);
                assert_eq!(rows[2][2], None);
                assert!(!more);
            }
            r => panic!("{r:?}"),
        }
        c.query("DELETE FROM Account", 10);
        assert!(matches!(next(&c), Reply::Error(e) if e.contains("SELECT だけ")));
        assert!(m.seen.lock().unwrap().bearers.iter().all(|b| b == "Bearer tok1"));
        drop(c);
        // 2 回目: ブラウザを開かず、保存したリフレッシュトークンでつなぐ
        let c = Client::connect(&url, "me@example.com", "", std::sync::Arc::new(|| {})).unwrap();
        assert!(matches!(next(&c), Reply::Connected { .. }));
        let s = m.seen.lock().unwrap();
        assert_eq!(s.authorize, 1, "2 回目はブラウザを開かない");
        assert_eq!(s.grants, vec!["authorization_code", "refresh_token"]);
        drop(s);
        // ログアウトで消える
        assert!(forget(&url, "me@example.com"));
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
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
