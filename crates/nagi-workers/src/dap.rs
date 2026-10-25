//! DAP（Debug Adapter Protocol）のデバッグ。いまは Python の debugpy（`python -m debugpy --listen 127.0.0.1:N
//! --wait-for-client ...`）に TCP でつなぐ。debug.rs と同じ Cmd・Ev。
//!
//! 流れ: initialize → attach → （initialized が届いたら）setBreakpoints・setExceptionBreakpoints → configurationDone。
//! 止まったら（stopped）stackTrace・scopes・variables で読む。中身は variablesReference で開く。

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde_json::{Value as J, json};

use crate::debug::{Bp, Cmd, Ev, Frame, Var};

pub fn start(addr: String, bps: Vec<Bp>, waker: crate::Waker) -> (Sender<Cmd>, nagi_core::task::Task<Ev>) {
    let (tx, rx) = channel();
    let task = nagi_core::task::spawn("dap", waker, move |ctx| {
        let send = |e: Ev| {
            ctx.send(e);
        };
        let until = std::time::Instant::now() + Duration::from_secs(60);
        let stream = loop {
            match TcpStream::connect(&addr) {
                Ok(s) => break s,
                Err(e) if std::time::Instant::now() >= until || ctx.cancelled() => {
                    send(Ev::Ended(format!("{addr} につなげません: {e}")));
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        };
        let r = Session::new(stream, rx).and_then(|mut s| s.run(bps, &send, &|| ctx.cancelled()));
        send(Ev::Ended(r.unwrap_or_else(|e| e)));
    });
    (tx, task)
}

fn read_msg(r: &mut BufReader<TcpStream>) -> Result<J, String> {
    let mut len = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).map_err(|e| e.to_string())? == 0 {
            return Err("切れました".into());
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some(v) = h.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; len.ok_or("Content-Length がありません")?];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

struct Session {
    out: TcpStream,
    rx_in: Receiver<Result<J, String>>,
    rx: Receiver<Cmd>,
    seq: u64,
    queued: VecDeque<J>,
    bps: Vec<Bp>,
    /// 止まっているスレッドとフレーム（id）
    thread: Option<i64>,
    frames: Vec<i64>,
}

impl Session {
    fn new(stream: TcpStream, rx: Receiver<Cmd>) -> Result<Session, String> {
        let out = stream.try_clone().map_err(|e| e.to_string())?;
        let (tx, rx_in) = channel();
        std::thread::Builder::new()
            .name("dap-read".into())
            .spawn(move || {
                let mut r = BufReader::new(stream);
                loop {
                    let m = read_msg(&mut r);
                    let end = m.is_err();
                    if tx.send(m).is_err() || end {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Session { out, rx_in, rx, seq: 1, queued: Default::default(), bps: Vec::new(), thread: None, frames: Vec::new() })
    }

    fn write(&mut self, command: &str, args: J) -> Result<u64, String> {
        let seq = self.seq;
        self.seq += 1;
        let body = json!({"seq": seq, "type": "request", "command": command, "arguments": args}).to_string();
        write!(self.out, "Content-Length: {}\r\n\r\n{body}", body.len()).map_err(|e| format!("DAP に書けません: {e}"))?;
        Ok(seq)
    }

    /// 送って返事を待つ（待つ間のイベントは溜める）
    fn call(&mut self, command: &str, args: J) -> Result<J, String> {
        let seq = self.write(command, args)?;
        self.wait_response(seq, command)
    }

    fn wait_response(&mut self, seq: u64, command: &str) -> Result<J, String> {
        loop {
            let m = self.rx_in.recv_timeout(Duration::from_secs(30)).map_err(|_| format!("{command}: 返事がありません"))??;
            if m["type"] == "response" && m["request_seq"].as_u64() == Some(seq) {
                if m["success"] == false {
                    return Err(format!("{command}: {}", m["message"].as_str().unwrap_or("失敗")));
                }
                return Ok(m["body"].clone());
            }
            if m["type"] == "event" {
                self.queued.push_back(m);
            }
        }
    }

    fn next_event(&mut self, wait: Duration) -> Result<Option<J>, String> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(Some(e));
        }
        match self.rx_in.recv_timeout(wait) {
            Ok(m) => Ok(Some(m?).filter(|m| m["type"] == "event")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => Err("切れました".into()),
        }
    }

    fn set_breakpoints(&mut self) -> Result<(), String> {
        let mut by_file: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for b in &self.bps {
            by_file.entry(b.file.clone()).or_default().push(b.line);
        }
        for (f, lines) in by_file {
            let bps: Vec<J> = lines.iter().map(|l| json!({"line": l})).collect();
            self.call("setBreakpoints", json!({"source": {"path": f.display().to_string()}, "breakpoints": bps}))?;
        }
        Ok(())
    }

    fn run(&mut self, bps: Vec<Bp>, send: &dyn Fn(Ev), cancelled: &dyn Fn() -> bool) -> Result<String, String> {
        self.bps = bps;
        self.call("initialize", json!({"clientID": "nagi", "clientName": "nagi", "adapterID": "debugpy", "linesStartAt1": true, "columnsStartAt1": true, "pathFormat": "path"}))?;
        // attach の返事は configurationDone の後に来ることがあるので、先に送って後で待つ
        let attach = self.write("attach", json!({"justMyCode": true}))?;
        // initialized を待ってから設定を送る
        let until = std::time::Instant::now() + Duration::from_secs(30);
        let mut later = Vec::new();
        loop {
            if std::time::Instant::now() > until {
                return Err("initialized が届きません".into());
            }
            match self.next_event(Duration::from_millis(100))? {
                Some(e) if e["event"] == "initialized" => break,
                // ほかのイベントは後で
                Some(e) => later.push(e),
                None => {}
            }
        }
        for e in later.into_iter().rev() {
            self.queued.push_front(e);
        }
        self.set_breakpoints()?;
        self.call("setExceptionBreakpoints", json!({"filters": ["uncaught"]}))?;
        self.call("configurationDone", json!({}))?;
        let _ = self.wait_response(attach, "attach");
        send(Ev::Connected("debugpy".into()));
        send(Ev::Running);
        loop {
            if cancelled() {
                let _ = self.call("disconnect", json!({"terminateDebuggee": false}));
                return Ok("切り離しました".into());
            }
            while let Ok(c) = self.rx.try_recv() {
                if let Some(end) = self.command(c, send)? {
                    return Ok(end);
                }
            }
            let Some(ev) = self.next_event(Duration::from_millis(50))? else { continue };
            let b = &ev["body"];
            match ev["event"].as_str().unwrap_or("") {
                "stopped" => {
                    let thread = b["threadId"].as_i64().unwrap_or(1);
                    let reason = match b["reason"].as_str().unwrap_or("") {
                        "breakpoint" => "ブレークポイント".to_string(),
                        "step" => "ステップ".into(),
                        "exception" => format!("例外 {}", b["text"].as_str().unwrap_or("")),
                        r => r.to_string(),
                    };
                    self.stop_at(thread, reason, send)?;
                }
                "continued" => send(Ev::Running),
                "output" => {}
                "terminated" | "exited" => return Ok("終わりました".into()),
                _ => {}
            }
        }
    }

    fn command(&mut self, c: Cmd, send: &dyn Fn(Ev)) -> Result<Option<String>, String> {
        match c {
            Cmd::Breakpoints(b) => {
                // 外したファイルは空で送り直す
                let gone: Vec<PathBuf> = self.bps.iter().map(|x| x.file.clone()).filter(|f| !b.iter().any(|n| n.file == *f)).collect();
                for f in gone {
                    self.call("setBreakpoints", json!({"source": {"path": f.display().to_string()}, "breakpoints": []}))?;
                }
                self.bps = b;
                self.set_breakpoints()?;
            }
            Cmd::Resume => {
                if let Some(t) = self.thread.take() {
                    self.call("continue", json!({"threadId": t}))?;
                    send(Ev::Running);
                }
            }
            Cmd::Step(d) => {
                if let Some(t) = self.thread.take() {
                    let c = match d {
                        crate::jdwp::Depth::Over => "next",
                        crate::jdwp::Depth::Into => "stepIn",
                        crate::jdwp::Depth::Out => "stepOut",
                    };
                    self.call(c, json!({"threadId": t}))?;
                    send(Ev::Running);
                }
            }
            Cmd::Frame(i) => {
                if let Some(&f) = self.frames.get(i) {
                    let vars = self.frame_vars(f);
                    send(Ev::Vars { frame: i, vars });
                }
            }
            Cmd::Expand(t, r) => {
                let vars = self.variables(r as i64);
                send(Ev::Children { parent: (t, r), vars });
            }
            Cmd::Stop => {
                let _ = self.call("disconnect", json!({"terminateDebuggee": true}));
                return Ok(Some("止めました".into()));
            }
            Cmd::Detach => {
                let _ = self.call("disconnect", json!({"terminateDebuggee": false}));
                return Ok(Some("切り離しました".into()));
            }
        }
        Ok(None)
    }

    fn stop_at(&mut self, thread: i64, reason: String, send: &dyn Fn(Ev)) -> Result<(), String> {
        let st = self.call("stackTrace", json!({"threadId": thread, "startFrame": 0, "levels": 50}))?;
        let raw = st["stackFrames"].as_array().cloned().unwrap_or_default();
        self.frames = raw.iter().filter_map(|f| f["id"].as_i64()).collect();
        let frames: Vec<Frame> = raw
            .iter()
            .map(|f| {
                let path = f["source"]["path"].as_str().map(String::from);
                let file = path.as_deref().and_then(|p| p.rsplit(['/', '\\']).next()).unwrap_or("?").to_string();
                Frame { class: file, method: f["name"].as_str().unwrap_or("?").into(), source: path, line: f["line"].as_u64().map(|l| l as usize) }
            })
            .collect();
        let vars = self.frames.first().copied().map(|f| self.frame_vars(f)).unwrap_or_default();
        self.thread = Some(thread);
        send(Ev::Stopped { reason, thread: format!("thread {thread}"), frames, frame: 0, vars });
        Ok(())
    }

    /// フレームのローカル変数（Locals の範囲。無ければ最初の範囲）
    fn frame_vars(&mut self, frame: i64) -> Vec<Var> {
        let Ok(sc) = self.call("scopes", json!({"frameId": frame})) else { return Vec::new() };
        let scopes = sc["scopes"].as_array().cloned().unwrap_or_default();
        let local = scopes.iter().find(|s| s["name"].as_str().is_some_and(|n| n.starts_with("Local"))).or(scopes.first());
        let mut out = local.and_then(|s| s["variablesReference"].as_i64()).map(|r| self.variables(r)).unwrap_or_default();
        // ほかの範囲（Globals など）は開ける行に
        for s in scopes.iter().filter(|s| Some(*s) != local) {
            if let Some(r) = s["variablesReference"].as_i64().filter(|r| *r > 0) {
                out.push(Var { name: format!("（{}）", s["name"].as_str().unwrap_or("")), ty: String::new(), value: String::new(), children: Some((b'd', r as u64)) });
            }
        }
        out
    }

    fn variables(&mut self, r: i64) -> Vec<Var> {
        match self.call("variables", json!({"variablesReference": r})) {
            Ok(v) => v["variables"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                // debugpy が出す「special variables」「function variables」などのまとめは除く
                .filter(|v| !v["name"].as_str().unwrap_or("").ends_with(" variables"))
                .map(|v| {
                    let child = v["variablesReference"].as_i64().filter(|r| *r > 0).map(|r| (b'd', r as u64));
                    Var { name: v["name"].as_str().unwrap_or("?").into(), ty: v["type"].as_str().unwrap_or("").into(), value: v["value"].as_str().unwrap_or("").into(), children: child }
                })
                .collect(),
            Err(e) => vec![Var { name: "（読めません）".into(), ty: String::new(), value: e, children: None }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本物の debugpy で: ブレークポイントで止まり、変数を読み、次へ・中へ・続ける。
    /// debugpy の入った Python が要るので普段は流さない（NAGI_DEBUGPY_PYTHON にその python）
    #[test]
    #[ignore]
    fn debugs_python() {
        let py = std::env::var("NAGI_DEBUGPY_PYTHON").unwrap_or_else(|_| "python".into());
        let dir = std::env::temp_dir().join(format!("nagi-dap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("hello.py");
        std::fs::write(&f, "def add(a, b):\n    c = a + b\n    return c\n\nxs = [1, 2, 3]\nr = add(2, 3)\nprint(r, xs)\n").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut child = std::process::Command::new(&py)
            .args(["-m", "debugpy", "--listen", &format!("127.0.0.1:{port}"), "--wait-for-client", &f.display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let (tx, task) = start(format!("127.0.0.1:{port}"), vec![Bp { class: String::new(), file: f.clone(), line: 6 }], std::sync::Arc::new(|| {}));
        let stop = || loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Stopped { frames, vars, .. }) => return (frames, vars),
                Some(Ev::Ended(e)) => panic!("終わった: {e}"),
                Some(_) => continue,
                None => panic!("止まらない"),
            }
        };
        let (frames, vars) = stop();
        assert_eq!((frames[0].method.as_str(), frames[0].line), ("<module>", Some(6)), "{frames:?}");
        let xs = vars.iter().find(|v| v.name == "xs").unwrap();
        assert_eq!(xs.value, "[1, 2, 3]");
        tx.send(Cmd::Step(crate::jdwp::Depth::Into)).unwrap();
        let (frames, vars) = stop();
        assert_eq!((frames[0].method.as_str(), frames[0].line), ("add", Some(2)));
        assert!(vars.iter().any(|v| v.name == "a" && v.value == "2"), "{vars:?}");
        tx.send(Cmd::Resume).unwrap();
        loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Ended(_)) => break,
                Some(_) => continue,
                None => panic!("終わらない"),
            }
        }
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
