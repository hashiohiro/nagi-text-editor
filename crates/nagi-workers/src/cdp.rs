//! ブラウザ（Chrome・Edge）の JavaScript・TypeScript のデバッグ（Chrome DevTools Protocol。debug.rs と同じ Cmd・Ev）。
//!
//! ブラウザを `--remote-debugging-port` で起こし（または起こしてあるものに）、`/json/list` のページに WebSocket でつなぐ。
//! 読み込まれたスクリプトのソースマップ（埋め込みの data: か、URL）を読み、TypeScript の行と JS の行を行き来する。
//! ブレークポイントはスクリプトが読み込まれるたびに置く（ページを読み直しても効く）。node_modules のものは見ない。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde_json::{Value as J, json};

use crate::debug::{Bp, Cmd, Ev, Frame, Var};
use crate::sourcemap::{SourceMap, normalize};
use crate::ws::Ws;

/// つなぐページ（/json/list から。`hint` を含む URL のページ、無ければ最初のページ）
pub fn page_ws_url(endpoint: &str, hint: &str) -> Result<(String, String), String> {
    let body = crate::ws::get(&format!("http://{endpoint}/json/list"))?;
    let list: Vec<J> = serde_json::from_slice(&body).map_err(|e| format!("/json/list: {e}"))?;
    let pages: Vec<&J> = list.iter().filter(|t| t["type"] == "page" && !t["url"].as_str().unwrap_or("").starts_with("devtools://")).collect();
    let pick = pages.iter().find(|t| !hint.is_empty() && t["url"].as_str().unwrap_or("").contains(hint)).or_else(|| pages.first()).ok_or("ブラウザにページがありません")?;
    let ws = pick["webSocketDebuggerUrl"].as_str().ok_or("ページにつなげません（ほかの DevTools がつないでいる）")?;
    Ok((ws.to_string(), pick["url"].as_str().unwrap_or("").to_string()))
}

/// セッションを始める（`endpoint` は 127.0.0.1:9222。ブラウザが起きるまで待つ）
pub fn start(endpoint: String, hint: String, root: PathBuf, bps: Vec<Bp>, waker: crate::Waker) -> (Sender<Cmd>, nagi_core::task::Task<Ev>) {
    let (tx, rx) = channel();
    let task = nagi_core::task::spawn("cdp", waker, move |ctx| {
        let send = |e: Ev| {
            ctx.send(e);
        };
        let until = std::time::Instant::now() + Duration::from_secs(60);
        let (ws_url, page) = loop {
            match page_ws_url(&endpoint, &hint) {
                Ok(v) => break v,
                Err(e) if std::time::Instant::now() >= until || ctx.cancelled() => {
                    send(Ev::Ended(format!("ブラウザ（{endpoint}）につなげません: {e}")));
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(300)),
            }
        };
        let r = Session::connect(&ws_url, rx, root).and_then(|mut s| {
            send(Ev::Connected(page));
            s.run(bps, &send, &|| ctx.cancelled())
        });
        send(Ev::Ended(r.unwrap_or_else(|e| e)));
    });
    (tx, task)
}

struct Script {
    url: String,
    map: Option<SourceMap>,
    /// 元のソースの名前（normalize したもの）
    sources: Vec<String>,
}

struct Session {
    ws: Ws,
    rx_ws: Receiver<Result<J, String>>,
    rx: Receiver<Cmd>,
    root: PathBuf,
    next: u64,
    queued: std::collections::VecDeque<J>,
    scripts: HashMap<String, Script>,
    bps: Vec<Bp>,
    /// 置いたもの（ブレークポイントの番号, スクリプト）
    placed: Vec<(String, String, Bp)>,
    /// 止まっているときのフレーム（callFrame の JSON）
    frames: Vec<J>,
    /// 開ける中身の ID（objectId）
    handles: Vec<String>,
}

/// ローカルのファイルと、元のソースの名前が同じものか（/src/App.tsx と プロジェクト/src/App.tsx）
fn same_source(name: &str, file: &std::path::Path, root: &std::path::Path) -> bool {
    let abs = file.to_string_lossy().replace('\\', "/");
    let rel = file.strip_prefix(root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    let n = name.trim_start_matches('/');
    (!rel.is_empty() && (n == rel || n.ends_with(&format!("/{rel}")))) || abs.eq_ignore_ascii_case(n) || abs.to_ascii_lowercase().ends_with(&format!("/{}", n.to_ascii_lowercase()))
}

impl Session {
    fn connect(url: &str, rx: Receiver<Cmd>, root: PathBuf) -> Result<Session, String> {
        let ws = Ws::connect(url)?;
        let mut reader = ws.try_clone()?;
        let (tx, rx_ws) = channel();
        std::thread::Builder::new()
            .name("cdp-read".into())
            .spawn(move || loop {
                let m = reader.recv().and_then(|t| serde_json::from_str::<J>(&t).map_err(|e| e.to_string()));
                let end = m.is_err();
                if tx.send(m).is_err() || end {
                    break;
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Session { ws, rx_ws, rx, root, next: 1, queued: Default::default(), scripts: HashMap::new(), bps: Vec::new(), placed: Vec::new(), frames: Vec::new(), handles: Vec::new() })
    }

    /// 命令を送って返事を待つ（待つ間のイベントは溜める）
    fn call(&mut self, method: &str, params: J) -> Result<J, String> {
        let id = self.next;
        self.next += 1;
        self.ws.send(&json!({"id": id, "method": method, "params": params}).to_string())?;
        loop {
            let m = self.rx_ws.recv_timeout(Duration::from_secs(30)).map_err(|_| format!("{method}: ブラウザの返事がありません"))??;
            if m["id"].as_u64() == Some(id) {
                if let Some(e) = m.get("error") {
                    return Err(format!("{method}: {}", e["message"].as_str().unwrap_or("エラー")));
                }
                return Ok(m["result"].clone());
            }
            if m.get("method").is_some() {
                self.queued.push_back(m);
            }
        }
    }

    fn next_event(&mut self, wait: Duration) -> Result<Option<J>, String> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(Some(e));
        }
        match self.rx_ws.recv_timeout(wait) {
            Ok(m) => Ok(Some(m?)).map(|m| m.filter(|m| m.get("method").is_some())),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err("ブラウザとの接続が切れました".into()),
        }
    }

    fn run(&mut self, bps: Vec<Bp>, send: &dyn Fn(Ev), cancelled: &dyn Fn() -> bool) -> Result<String, String> {
        self.bps = bps;
        self.call("Runtime.enable", json!({}))?;
        self.call("Debugger.enable", json!({}))?;
        self.call("Debugger.setPauseOnExceptions", json!({"state": "uncaught"}))?;
        let _ = self.call("Runtime.runIfWaitingForDebugger", json!({}));
        send(Ev::Running);
        loop {
            if cancelled() {
                let _ = self.call("Debugger.disable", json!({}));
                return Ok("切り離しました".into());
            }
            while let Ok(c) = self.rx.try_recv() {
                if let Some(end) = self.command(c, send)? {
                    return Ok(end);
                }
            }
            let Some(ev) = self.next_event(Duration::from_millis(50))? else { continue };
            let p = &ev["params"];
            match ev["method"].as_str().unwrap_or("") {
                "Debugger.scriptParsed" => self.script_parsed(p, send),
                "Debugger.paused" => {
                    self.frames = p["callFrames"].as_array().cloned().unwrap_or_default();
                    let reason = match p["reason"].as_str().unwrap_or("") {
                        "exception" | "promiseRejection" => format!("例外 {}", p["data"]["description"].as_str().unwrap_or("").lines().next().unwrap_or("")),
                        "other" | "" => "ブレークポイント".into(),
                        r => r.to_string(),
                    };
                    self.handles.clear();
                    let frames: Vec<Frame> = self.frames.clone().iter().map(|f| self.frame_info(f)).collect();
                    let vars = self.frame_vars(0);
                    send(Ev::Stopped { reason, thread: "main".into(), frames, frame: 0, vars });
                }
                "Debugger.resumed" => send(Ev::Running),
                "Inspector.detached" => return Ok("ブラウザから切り離されました".into()),
                "Runtime.executionContextsCleared" => {
                    // ページを読み直した: スクリプトは読み込み直されるので、置いたものも置き直す
                    self.scripts.clear();
                    self.placed.clear();
                }
                _ => {}
            }
        }
    }

    fn command(&mut self, c: Cmd, send: &dyn Fn(Ev)) -> Result<Option<String>, String> {
        match c {
            Cmd::Breakpoints(b) => {
                for (id, ..) in std::mem::take(&mut self.placed) {
                    let _ = self.call("Debugger.removeBreakpoint", json!({"breakpointId": id}));
                }
                self.bps = b;
                let ids: Vec<String> = self.scripts.keys().cloned().collect();
                for id in ids {
                    self.place(&id, send);
                }
            }
            Cmd::Resume => {
                self.call("Debugger.resume", json!({}))?;
            }
            Cmd::Step(d) => {
                let m = match d {
                    crate::jdwp::Depth::Over => "Debugger.stepOver",
                    crate::jdwp::Depth::Into => "Debugger.stepInto",
                    crate::jdwp::Depth::Out => "Debugger.stepOut",
                };
                self.call(m, json!({}))?;
            }
            Cmd::Frame(i) => {
                let vars = self.frame_vars(i);
                send(Ev::Vars { frame: i, vars });
            }
            Cmd::Expand(t, h) => {
                let vars = self.handles.get(h as usize).cloned().map(|id| self.props(&id)).unwrap_or_default();
                send(Ev::Children { parent: (t, h), vars });
            }
            Cmd::Stop | Cmd::Detach => {
                let _ = self.call("Debugger.disable", json!({}));
                return Ok(Some("切り離しました（ブラウザはそのまま）".into()));
            }
        }
        Ok(None)
    }

    /// 読み込まれたスクリプト: ソースマップを読み、ブレークポイントを置く
    fn script_parsed(&mut self, p: &J, send: &dyn Fn(Ev)) {
        let id = p["scriptId"].as_str().unwrap_or("").to_string();
        let url = p["url"].as_str().unwrap_or("").to_string();
        if url.is_empty() || url.contains("/node_modules/") || url.starts_with("chrome") {
            return;
        }
        let map_url = p["sourceMapURL"].as_str().unwrap_or("");
        let map = if map_url.is_empty() {
            None
        } else if let Some(data) = map_url.strip_prefix("data:") {
            // data:application/json;base64,...
            data.split_once(',').and_then(|(kind, body)| if kind.ends_with(";base64") { crate::ws::base64_decode(body) } else { Some(body.as_bytes().to_vec()) }).and_then(|b| SourceMap::parse(&String::from_utf8_lossy(&b)))
        } else {
            let abs = if map_url.contains("://") { map_url.to_string() } else { format!("{}/{map_url}", url.rsplit_once('/').map_or("", |x| x.0)) };
            crate::ws::get(&abs).ok().and_then(|b| SourceMap::parse(&String::from_utf8_lossy(&b)))
        };
        let sources = match &map {
            Some(m) => m.sources.iter().map(|s| normalize(s, &url)).collect(),
            None => vec![normalize(&url, "")],
        };
        self.scripts.insert(id.clone(), Script { url, map, sources });
        self.place(&id, send);
    }

    /// スクリプト `id` に、そのソースのブレークポイントを置く
    fn place(&mut self, id: &str, send: &dyn Fn(Ev)) {
        let Some(s) = self.scripts.get(id) else { return };
        let mut todo = Vec::new();
        for b in &self.bps {
            if self.placed.iter().any(|(_, sid, pb)| sid == id && pb == b) {
                continue;
            }
            for (k, name) in s.sources.iter().enumerate() {
                if !same_source(name, &b.file, &self.root) {
                    continue;
                }
                let at = match &s.map {
                    Some(m) => m.generated(k, b.line as u32 - 1),
                    None => Some((b.line as u32 - 1, 0)),
                };
                match at {
                    Some((l, c)) => todo.push((b.clone(), l, c)),
                    None => send(Ev::Note(format!("{}:{} にはコードが無いので止まりません", b.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), b.line))),
                }
            }
        }
        for (b, l, c) in todo {
            if let Ok(r) = self.call("Debugger.setBreakpoint", json!({"location": {"scriptId": id, "lineNumber": l, "columnNumber": c}})) {
                if let Some(bid) = r["breakpointId"].as_str() {
                    self.placed.push((bid.to_string(), id.to_string(), b));
                }
            }
        }
    }

    fn frame_info(&self, f: &J) -> Frame {
        let loc = &f["location"];
        let (sid, line, col) = (loc["scriptId"].as_str().unwrap_or(""), loc["lineNumber"].as_u64().unwrap_or(0) as u32, loc["columnNumber"].as_u64().unwrap_or(0) as u32);
        let name = f["functionName"].as_str().filter(|n| !n.is_empty()).unwrap_or("（無名）").to_string();
        let script = self.scripts.get(sid);
        let (source, l) = match script.and_then(|s| s.map.as_ref().map(|m| (s, m))).and_then(|(s, m)| m.original(line, col).map(|(k, l)| (s.sources.get(k).cloned(), l))) {
            Some((src, l)) => (src, l),
            None => (script.map(|s| normalize(&s.url, "")), line),
        };
        let file = source.as_deref().and_then(|s| s.rsplit('/').next()).unwrap_or("?").to_string();
        Frame { class: file, method: name, source, line: Some(l as usize + 1) }
    }

    fn handle(&mut self, id: &str) -> u64 {
        self.handles.push(id.to_string());
        (self.handles.len() - 1) as u64
    }

    /// フレームの変数（ローカル・ブロック・クロージャの範囲。グローバルは出さない）と this
    fn frame_vars(&mut self, i: usize) -> Vec<Var> {
        let Some(f) = self.frames.get(i).cloned() else { return Vec::new() };
        let mut out = Vec::new();
        for sc in f["scopeChain"].as_array().cloned().unwrap_or_default() {
            let kind = sc["type"].as_str().unwrap_or("");
            if matches!(kind, "global" | "script" | "module") && !out.is_empty() {
                continue;
            }
            if let Some(id) = sc["object"]["objectId"].as_str() {
                if matches!(kind, "local" | "block" | "catch") {
                    out.extend(self.props(id));
                } else if matches!(kind, "closure" | "script" | "module") {
                    let h = self.handle(id);
                    out.push(Var { name: format!("（{}）", if kind == "closure" { "クロージャ" } else { kind }), ty: String::new(), value: String::new(), children: Some((b'o', h)) });
                }
            }
        }
        if let Some(this) = f.get("this").filter(|t| t["type"] == "object" && t["subtype"] != "null") {
            let v = self.var("this", this);
            out.insert(0, v);
        }
        out
    }

    fn var(&mut self, name: &str, v: &J) -> Var {
        let ty = v["type"].as_str().unwrap_or("");
        let sub = v["subtype"].as_str().unwrap_or("");
        let value = match ty {
            "string" => format!("\"{}\"", v["value"].as_str().unwrap_or("").replace('\n', "\\n")),
            "undefined" => "undefined".into(),
            "object" if sub == "null" => "null".into(),
            _ => v.get("description").and_then(|d| d.as_str()).map(String::from).unwrap_or_else(|| v["value"].to_string()),
        };
        let children = v["objectId"].as_str().map(str::to_string).filter(|_| ty == "object").map(|id| (b'o', self.handle(&id)));
        Var { name: name.into(), ty: v["className"].as_str().unwrap_or(if sub.is_empty() { ty } else { sub }).to_string(), value, children }
    }

    /// オブジェクトの持ち物（自分のものだけ。関数は出さない）
    fn props(&mut self, id: &str) -> Vec<Var> {
        let r = match self.call("Runtime.getProperties", json!({"objectId": id, "ownProperties": true, "generatePreview": false})) {
            Ok(r) => r,
            Err(e) => return vec![Var { name: "（読めません）".into(), ty: String::new(), value: e, children: None }],
        };
        let mut out = Vec::new();
        for p in r["result"].as_array().cloned().unwrap_or_default() {
            let Some(v) = p.get("value") else { continue };
            if v["type"] == "function" || p["name"] == "__proto__" {
                continue;
            }
            out.push(self.var(p["name"].as_str().unwrap_or("?"), v));
        }
        out
    }
}

/// Chrome・Edge の exe（Chrome を先に）
pub fn find_browser() -> Option<PathBuf> {
    let mut c = Vec::new();
    for v in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(d) = std::env::var_os(v).map(PathBuf::from) {
            c.push(d.join("Google/Chrome/Application/chrome.exe"));
        }
    }
    for v in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(d) = std::env::var_os(v).map(PathBuf::from) {
            c.push(d.join("Microsoft/Edge/Application/msedge.exe"));
        }
    }
    c.into_iter().find(|p| p.is_file())
}

/// デバッグの口を開けてブラウザを起こす（いつものプロフィールとは別のフォルダで。いつものブラウザは閉じなくてよい）
pub fn launch_browser(exe: &std::path::Path, port: u16, url: &str, headless: bool) -> Result<std::process::Child, String> {
    let profile = std::env::temp_dir().join(if headless { "nagi-browser-headless" } else { "nagi-browser-debug" });
    let mut c = std::process::Command::new(exe);
    if headless {
        c.arg("--headless=new");
    }
    c.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--no-first-run", "--no-default-browser-check"])
        .arg(url)
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_match_files() {
        let root = std::path::Path::new("C:/p/app");
        assert!(same_source("/src/App.tsx", std::path::Path::new("C:/p/app/src/App.tsx"), root));
        assert!(same_source("C:/p/app/src/App.tsx", std::path::Path::new("C:/p/app/src/App.tsx"), root));
        assert!(!same_source("/src/Other.tsx", std::path::Path::new("C:/p/app/src/App.tsx"), root));
    }

    /// 本物の Chrome（Edge）で: TypeScript のソースマップのブレークポイントで止まり、変数を読み、次へ・続ける。
    /// ブラウザ（窓なし）を起こすので普段は流さない
    #[test]
    #[ignore]
    fn debugs_a_real_browser() {
        let exe = find_browser().expect("Chrome か Edge が要る");
        let dir = std::env::temp_dir().join(format!("nagi-cdp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // a.ts（元）と、それを tsc で出したような a.js（ソースマップは埋め込み）
        let ts = "function add(a: number, b: number): number {\n  const c = a + b;\n  return c;\n}\nsetInterval(() => { const r = add(2, 3); document.title = String(r); }, 300);\n";
        std::fs::write(dir.join("src/a.ts"), ts).unwrap();
        let js = "function add(a, b) {\n    const c = a + b;\n    return c;\n}\nsetInterval(() => { const r = add(2, 3); document.title = String(r); }, 300);\n";
        // 行ごとに同じ行へ（JS の n 行目の頭 → a.ts の n 行目の頭）
        let map = r#"{"version":3,"file":"a.js","sources":["a.ts"],"mappings":"AAAA;AACA;AACA;AACA;AACA"}"#;
        let js = format!("{js}//# sourceMappingURL=data:application/json;base64,{}\n", crate::ws::base64_encode(map.as_bytes()));
        std::fs::write(dir.join("src/a.js"), js).unwrap();
        std::fs::write(dir.join("index.html"), "<html><body><script src=\"src/a.js\"></script></body></html>").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("file:///{}", dir.join("index.html").display().to_string().replace('\\', "/"));
        // 窓を出さない（試験で画面の前に出さないように）
        let mut child = launch_browser(&exe, port, &url, true).unwrap();
        let bp = Bp { class: String::new(), file: dir.join("src/a.ts"), line: 2 };
        let (tx, task) = start(format!("127.0.0.1:{port}"), "index.html".into(), dir.clone(), vec![bp], std::sync::Arc::new(|| {}));
        let stop = || loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Stopped { frames, vars, .. }) => return (frames, vars),
                Some(Ev::Ended(e)) => panic!("終わった: {e}"),
                Some(_) => continue,
                None => panic!("止まらない"),
            }
        };
        // 読み込みの後に置くので、止まるのは次の setInterval（ページを読み直さなくてよい）
        let (frames, vars) = stop();
        assert_eq!((frames[0].method.as_str(), frames[0].line), ("add", Some(2)), "{frames:?}");
        assert!(frames[0].source.as_deref().is_some_and(|s| s.ends_with("/src/a.ts")), "{frames:?}");
        assert!(vars.iter().any(|v| v.name == "a" && v.value == "2"), "{vars:?}");
        tx.send(Cmd::Step(crate::jdwp::Depth::Over)).unwrap();
        let (frames, vars) = stop();
        assert_eq!(frames[0].line, Some(3));
        assert!(vars.iter().any(|v| v.name == "c" && v.value == "5"), "{vars:?}");
        tx.send(Cmd::Detach).unwrap();
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
