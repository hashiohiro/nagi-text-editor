//! Java のデバッグのセッション（裏のスレッド。jdwp.rs でつなぐ）。
//!
//! UI からは `Cmd`（ブレークポイント・続ける・ステップ・フレームを選ぶ・オブジェクトを開く・止める）を送り、
//! `Ev`（つながった・動いている・止まった（スタックと変数）・開いた中身・終わった）を受け取る。
//! ブレークポイントは「クラス名（com.x.Foo）と行」で持ち、読み込み済みのクラスにはすぐ、まだのクラスには
//! 読み込まれたとき（ClassPrepare）に置く。内部クラス（Foo$Inner・ラムダ）も同じソースなので一緒に見る。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use crate::jdwp::{self, Conn, Depth, Event, Id, Location, Value};

/// ブレークポイント（クラスは com.x.Foo、行は 1 始まり）
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bp {
    pub class: String,
    pub file: PathBuf,
    pub line: usize,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    /// ブレークポイントを丸ごと入れ替える
    Breakpoints(Vec<Bp>),
    Resume,
    Step(Depth),
    /// 止まっている間にスタックのフレームを選ぶ（変数を出し直す）
    Frame(usize),
    /// オブジェクト・配列の中身を開く（タグ, ID）
    Expand(u8, Id),
    /// 止める（JVM を終わらせる）
    Stop,
    /// 切り離す（JVM は動かしたまま）
    Detach,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// com.x.Foo
    pub class: String,
    pub method: String,
    /// パッケージのフォルダつきのソースのファイル（com/x/Foo.java）
    pub source: Option<String>,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub name: String,
    /// 型（int・String・com.x.Foo）
    pub ty: String,
    pub value: String,
    /// 開ける中身（オブジェクト・配列のタグと ID）
    pub children: Option<(u8, Id)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    Connected(String),
    Running,
    Stopped { reason: String, thread: String, frames: Vec<Frame>, frame: usize, vars: Vec<Var> },
    /// フレームを選んだ・止まったフレームの変数
    Vars { frame: usize, vars: Vec<Var> },
    Children { parent: (u8, Id), vars: Vec<Var> },
    /// 知らせ（置けなかったブレークポイントなど）
    Note(String),
    Ended(String),
}

/// セッションを始める（`addr` に JVM が起きるまで待ってつなぐ）。命令の送り口と、イベントを受ける Task
pub fn start(addr: String, bps: Vec<Bp>, break_on_uncaught: bool, waker: crate::Waker) -> (Sender<Cmd>, nagi_core::task::Task<Ev>) {
    let (tx, rx) = channel();
    let task = nagi_core::task::spawn("debug", waker, move |ctx| {
        let send = |e: Ev| {
            ctx.send(e);
        };
        let cancelled = || ctx.cancelled();
        let conn = match Conn::connect(&addr, Duration::from_secs(180), &cancelled) {
            Ok(c) => c,
            Err(e) => {
                send(Ev::Ended(e));
                return;
            }
        };
        let mut s = Session { conn, rx, bps: Vec::new(), placed: HashMap::new(), prepare_reqs: Vec::new(), cache: Cache::default(), stopped: None, step_req: None };
        let r = s.run(bps, break_on_uncaught, &send, &cancelled);
        send(Ev::Ended(match r {
            Ok(m) => m,
            Err(e) => e,
        }));
    });
    (tx, task)
}

#[derive(Default)]
struct Cache {
    sig: HashMap<Id, String>,
    methods: HashMap<Id, Vec<(Id, String, String)>>,
    lines: HashMap<(Id, Id), Vec<(u64, i32)>>,
    vars: HashMap<(Id, Id), Vec<(u64, String, String, u32, i32)>>,
    source: HashMap<Id, Option<String>>,
    fields: HashMap<Id, Vec<(Id, String, String)>>,
}

struct Session {
    conn: Conn,
    rx: Receiver<Cmd>,
    bps: Vec<Bp>,
    /// 置いたブレークポイント（ブレークポイント → 頼みの番号）
    placed: HashMap<Bp, Vec<i32>>,
    prepare_reqs: Vec<i32>,
    cache: Cache,
    /// 止まっているスレッドとフレーム
    stopped: Option<(Id, Vec<(Id, Location)>)>,
    step_req: Option<i32>,
}

/// "Lcom/x/Foo$Bar;" → "com.x.Foo$Bar"、"[I" → "int[]"
pub fn type_name(sig: &str) -> String {
    match sig.as_bytes().first() {
        Some(b'[') => format!("{}[]", type_name(&sig[1..])),
        Some(b'L') => sig[1..].trim_end_matches(';').replace('/', "."),
        Some(b'Z') => "boolean".into(),
        Some(b'B') => "byte".into(),
        Some(b'C') => "char".into(),
        Some(b'S') => "short".into(),
        Some(b'I') => "int".into(),
        Some(b'J') => "long".into(),
        Some(b'F') => "float".into(),
        Some(b'D') => "double".into(),
        Some(b'V') => "void".into(),
        _ => sig.into(),
    }
}

/// 名前の短い形（com.x.Foo → Foo）
fn short(t: &str) -> &str {
    t.rsplit('.').next().unwrap_or(t)
}

impl Session {
    fn run(&mut self, bps: Vec<Bp>, uncaught: bool, send: &dyn Fn(Ev), cancelled: &dyn Fn() -> bool) -> Result<String, String> {
        let v = self.conn.version()?;
        send(Ev::Connected(v.lines().next().unwrap_or("").to_string()));
        self.set_breakpoints(bps, send)?;
        if uncaught {
            self.conn.on_exception(false)?;
        }
        // suspend=y で起こした VM は止まっているので動かす（動いている VM につないだときは何もしない）
        self.conn.resume()?;
        send(Ev::Running);
        loop {
            if cancelled() {
                self.conn.dispose();
                return Ok("切り離しました".into());
            }
            while let Ok(c) = self.rx.try_recv() {
                if let Some(end) = self.command(c, send)? {
                    return Ok(end);
                }
            }
            let Some(evs) = self.conn.next_event(Duration::from_millis(50))? else { continue };
            let mut stop: Option<(String, Id)> = None;
            let mut resume_thread = None;
            for e in &evs.events {
                match e {
                    Event::VmDeath => return Ok("JVM が終わりました".into()),
                    Event::ClassPrepare { thread, class, signature, .. } => {
                        self.place_in_class(*class, signature, send)?;
                        resume_thread = Some(*thread);
                    }
                    Event::Breakpoint { thread, .. } => stop = Some(("ブレークポイント".into(), *thread)),
                    Event::Step { thread, .. } => stop = Some(("ステップ".into(), *thread)),
                    Event::Exception { thread, exception, .. } => {
                        let what = exception.object().and_then(|(_, o)| self.conn.object_class(o).ok()).and_then(|c| self.sig(c).ok()).map_or("例外".into(), |s| type_name(&s));
                        stop = Some((format!("例外 {what}"), *thread));
                    }
                    _ => {}
                }
            }
            if let Some((reason, thread)) = stop {
                if let Some(r) = self.step_req.take() {
                    self.conn.clear_request(jdwp::kind::SINGLE_STEP, r);
                }
                self.stop_at(reason, thread, send)?;
            } else if let Some(t) = resume_thread {
                if evs.suspend == jdwp::SUSPEND_ALL {
                    self.conn.resume()?;
                } else {
                    self.conn.resume_thread(t)?;
                }
            } else if evs.suspend == jdwp::SUSPEND_ALL && evs.events.iter().any(|e| matches!(e, Event::VmStart { .. })) {
                self.conn.resume()?;
            }
        }
    }

    /// UI の命令。終わるなら Some(理由)
    fn command(&mut self, c: Cmd, send: &dyn Fn(Ev)) -> Result<Option<String>, String> {
        match c {
            Cmd::Breakpoints(b) => self.set_breakpoints(b, send)?,
            Cmd::Resume => {
                if self.stopped.take().is_some() {
                    self.conn.resume()?;
                    send(Ev::Running);
                }
            }
            Cmd::Step(d) => {
                if let Some((t, _)) = self.stopped.take() {
                    self.step_req = Some(self.conn.step(t, d)?);
                    self.conn.resume()?;
                    send(Ev::Running);
                }
            }
            Cmd::Frame(i) => {
                if let Some((t, frames)) = self.stopped.clone() {
                    if let Some(f) = frames.get(i) {
                        let vars = self.frame_vars(t, f.0, &f.1);
                        send(Ev::Vars { frame: i, vars });
                    }
                }
            }
            Cmd::Expand(tag, id) => {
                let vars = self.children(tag, id).unwrap_or_else(|e| vec![Var { name: "（読めません）".into(), ty: String::new(), value: e, children: None }]);
                send(Ev::Children { parent: (tag, id), vars });
            }
            Cmd::Stop => {
                self.conn.exit(1);
                return Ok(Some("止めました".into()));
            }
            Cmd::Detach => {
                self.conn.dispose();
                return Ok(Some("切り離しました".into()));
            }
        }
        Ok(None)
    }

    fn sig(&mut self, class: Id) -> Result<String, String> {
        if let Some(s) = self.cache.sig.get(&class) {
            return Ok(s.clone());
        }
        let s = self.conn.signature(class)?;
        self.cache.sig.insert(class, s.clone());
        Ok(s)
    }

    fn methods(&mut self, class: Id) -> Result<Vec<(Id, String, String)>, String> {
        if let Some(m) = self.cache.methods.get(&class) {
            return Ok(m.clone());
        }
        let m = self.conn.methods(class)?;
        self.cache.methods.insert(class, m.clone());
        Ok(m)
    }

    fn lines(&mut self, class: Id, method: Id) -> Vec<(u64, i32)> {
        if let Some(l) = self.cache.lines.get(&(class, method)) {
            return l.clone();
        }
        // 抽象・ネイティブのメソッドは行の表が無い
        let l = self.conn.line_table(class, method).map(|t| t.2).unwrap_or_default();
        self.cache.lines.insert((class, method), l.clone());
        l
    }

    /// ブレークポイントを入れ替える（外したものは消し、足したものは置く）
    fn set_breakpoints(&mut self, bps: Vec<Bp>, send: &dyn Fn(Ev)) -> Result<(), String> {
        let gone: Vec<Bp> = self.placed.keys().filter(|b| !bps.contains(b)).cloned().collect();
        for b in gone {
            for r in self.placed.remove(&b).unwrap_or_default() {
                self.conn.clear_request(jdwp::kind::BREAKPOINT, r);
            }
        }
        for r in std::mem::take(&mut self.prepare_reqs) {
            self.conn.clear_request(jdwp::kind::CLASS_PREPARE, r);
        }
        self.bps = bps;
        // まだのクラスは読み込まれたときに。読み込み済みのものは今
        let classes: HashSet<String> = self.bps.iter().map(|b| b.class.clone()).collect();
        for c in &classes {
            self.prepare_reqs.push(self.conn.on_class_prepare(c)?);
            self.prepare_reqs.push(self.conn.on_class_prepare(&format!("{c}$*"))?);
        }
        if !classes.is_empty() {
            for (_, id, sig) in self.conn.all_classes()? {
                let name = type_name(&sig);
                if classes.iter().any(|c| name == *c || name.starts_with(&format!("{c}$"))) {
                    self.cache.sig.insert(id, sig.clone());
                    self.place_in_class(id, &sig, send)?;
                }
            }
        }
        Ok(())
    }

    /// 読み込まれたクラスに、そのクラス（と内部クラス）のブレークポイントを置く
    fn place_in_class(&mut self, class: Id, sig: &str, send: &dyn Fn(Ev)) -> Result<(), String> {
        let name = type_name(sig);
        let outer = name.split('$').next().unwrap_or(&name).to_string();
        let mine: Vec<Bp> = self.bps.iter().filter(|b| b.class == outer).cloned().collect();
        if mine.is_empty() {
            return Ok(());
        }
        let methods = self.methods(class)?;
        for b in mine {
            let mut placed = false;
            for (m, ..) in &methods {
                let lines = self.lines(class, *m);
                // その行のいちばん前の位置
                if let Some(idx) = lines.iter().filter(|(_, l)| *l as usize == b.line).map(|(i, _)| *i).min() {
                    let at = Location { tag: jdwp::TAG_CLASS, class, method: *m, index: idx };
                    let r = self.conn.set_breakpoint(&at)?;
                    self.placed.entry(b.clone()).or_default().push(r);
                    placed = true;
                }
            }
            // 内部クラスの方に行があることもあるので、外側のクラスで置けなかったときだけ知らせる
            if !placed && !name.contains('$') && !self.placed.contains_key(&b) {
                send(Ev::Note(format!("{}:{} にはコードが無いので止まりません（読み込まれた {}）", short(&b.class), b.line, short(&name))));
            }
        }
        Ok(())
    }

    fn frame_info(&mut self, at: &Location) -> Frame {
        let sig = self.sig(at.class).unwrap_or_default();
        let class = type_name(&sig);
        let method = self.methods(at.class).ok().and_then(|ms| ms.into_iter().find(|m| m.0 == at.method)).map_or("?".into(), |m| m.1);
        let line = self.lines(at.class, at.method).into_iter().filter(|(i, _)| *i <= at.index).max_by_key(|(i, _)| *i).map(|(_, l)| l as usize);
        let src = match self.cache.source.get(&at.class) {
            Some(s) => s.clone(),
            None => {
                let s = self.conn.source_file(at.class).ok();
                self.cache.source.insert(at.class, s.clone());
                s
            }
        };
        let pkg = class.rsplit_once('.').map_or(String::new(), |(p, _)| format!("{}/", p.replace('.', "/")));
        Frame { class, method, source: src.map(|f| format!("{pkg}{f}")), line }
    }

    fn stop_at(&mut self, reason: String, thread: Id, send: &dyn Fn(Ev)) -> Result<(), String> {
        let frames = self.conn.frames(thread)?;
        let infos: Vec<Frame> = frames.iter().map(|(_, at)| self.frame_info(at)).collect();
        let vars = frames.first().map(|(f, at)| self.frame_vars(thread, *f, at)).unwrap_or_default();
        let name = self.conn.thread_name(thread).unwrap_or_default();
        self.stopped = Some((thread, frames));
        send(Ev::Stopped { reason, thread: name, frames: infos, frame: 0, vars });
        Ok(())
    }

    /// フレームの変数（今の位置で見えるもの。スロットの順）
    fn frame_vars(&mut self, thread: Id, frame: Id, at: &Location) -> Vec<Var> {
        let key = (at.class, at.method);
        let table = match self.cache.vars.get(&key) {
            Some(t) => t.clone(),
            None => match self.conn.variable_table(at.class, at.method) {
                Ok(t) => {
                    self.cache.vars.insert(key, t.clone());
                    t
                }
                Err(e) => return vec![Var { name: "（変数）".into(), ty: String::new(), value: e, children: None }],
            },
        };
        let mut seen: Vec<(u64, String, String, u32, i32)> = table.into_iter().filter(|v| at.index >= v.0 && at.index < v.0 + v.3 as u64).collect();
        seen.sort_by_key(|v| v.4);
        let slots: Vec<(i32, u8)> = seen.iter().map(|v| (v.4, v.2.as_bytes()[0])).collect();
        let values = self.conn.frame_values(thread, frame, &slots).unwrap_or_default();
        let mut out: Vec<Var> = seen.iter().zip(values).map(|(v, val)| self.var(&v.1, &type_name(&v.2), &val)).collect();
        // static メソッドでなければ this を頭に（変数の表に無いとき）
        if !out.iter().any(|v| v.name == "this") {
            if let Ok(t) = self.conn.this_object(thread, frame) {
                if t.object().is_some() {
                    let ty = t.object().and_then(|(_, o)| self.conn.object_class(o).ok()).and_then(|c| self.sig(c).ok()).map(|s| type_name(&s)).unwrap_or_default();
                    out.insert(0, self.var("this", &ty, &t));
                }
            }
        }
        out
    }

    /// 値を画面に出す形に
    fn var(&mut self, name: &str, ty: &str, v: &Value) -> Var {
        let (value, children) = match v {
            Value::Void => ("void".into(), None),
            Value::Bool(b) => (b.to_string(), None),
            Value::Byte(x) => (x.to_string(), None),
            Value::Char(c) => (format!("'{}'", char::from_u32(*c as u32).unwrap_or('?')), None),
            Value::Short(x) => (x.to_string(), None),
            Value::Int(x) => (x.to_string(), None),
            Value::Long(x) => (x.to_string(), None),
            Value::Float(x) => (x.to_string(), None),
            Value::Double(x) => (x.to_string(), None),
            Value::Object(_, 0) => ("null".into(), None),
            Value::Object(b's', id) => {
                let s = self.conn.string_value(*id).unwrap_or_default();
                let s: String = if s.chars().count() > 200 { format!("{}…", s.chars().take(200).collect::<String>()) } else { s };
                (format!("\"{}\"", s.replace('\n', "\\n")), None)
            }
            Value::Object(b'[', id) => {
                let n = self.conn.array_length(*id).unwrap_or(0);
                let cls = self.conn.object_class(*id).ok().and_then(|c| self.sig(c).ok()).map(|s| type_name(&s)).unwrap_or_else(|| ty.to_string());
                // String[3]（要素の型は短い名前。多次元は最初の次元だけ数を）
                let elem = cls.strip_suffix("[]").unwrap_or(&cls);
                let (base, dims) = (elem.trim_end_matches("[]"), (elem.len() - elem.trim_end_matches("[]").len()) / 2);
                (format!("{}[{n}]{}", short(base), "[]".repeat(dims)), Some((b'[', *id)))
            }
            Value::Object(t, id) => {
                let cls = self.conn.object_class(*id).ok().and_then(|c| self.sig(c).ok()).map(|s| type_name(&s)).unwrap_or_else(|| ty.to_string());
                // 包んだ値（Integer など）は中の値を
                let boxed = matches!(cls.as_str(), "java.lang.Integer" | "java.lang.Long" | "java.lang.Short" | "java.lang.Byte" | "java.lang.Boolean" | "java.lang.Character" | "java.lang.Double" | "java.lang.Float");
                if boxed {
                    if let Some(inner) = self.children(*t, *id).ok().and_then(|c| c.into_iter().find(|v| v.name == "value")) {
                        return Var { name: name.into(), ty: short(&cls).into(), value: inner.value, children: None };
                    }
                }
                (format!("{}@{id:x}", short(&cls)), Some((*t, *id)))
            }
        };
        Var { name: name.into(), ty: short(ty).into(), value, children }
    }

    /// オブジェクトのフィールド（親クラスのものも）・配列の要素（100 まで）
    fn children(&mut self, tag: u8, id: Id) -> Result<Vec<Var>, String> {
        if tag == b'[' {
            let n = self.conn.array_length(id)?.min(100);
            let elem = self.conn.object_class(id).ok().and_then(|c| self.sig(c).ok()).map(|s| type_name(&s[1..])).unwrap_or_default();
            let vals = self.conn.array_values(id, 0, n)?;
            return Ok(vals.iter().enumerate().map(|(i, v)| self.var(&format!("[{i}]"), &elem, v)).collect());
        }
        let mut class = self.conn.object_class(id)?;
        let mut fields: Vec<(Id, String, String)> = Vec::new();
        while class != 0 {
            let fs = match self.cache.fields.get(&class) {
                Some(f) => f.clone(),
                None => {
                    let f: Vec<(Id, String, String)> = self.conn.fields(class)?.into_iter().filter(|f| !f.3).map(|f| (f.0, f.1, f.2)).collect();
                    self.cache.fields.insert(class, f.clone());
                    f
                }
            };
            fields.extend(fs);
            class = self.conn.superclass(class).unwrap_or(0);
            // java.lang.Object の上は無い。JDK の中のクラスは深く追わない
            if self.sig(class).is_ok_and(|s| s == "Ljava/lang/Object;") {
                break;
            }
        }
        let ids: Vec<Id> = fields.iter().map(|f| f.0).collect();
        let vals = self.conn.object_values(id, &ids)?;
        Ok(fields.iter().zip(vals).map(|(f, v)| self.var(&f.1, &type_name(&f.2), &v)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(type_name("Lcom/x/Foo$Bar;"), "com.x.Foo$Bar");
        assert_eq!(type_name("[[I"), "int[][]");
        assert_eq!(type_name("[Ljava/lang/String;"), "java.lang.String[]");
    }

    fn javac() -> Option<PathBuf> {
        let jh = std::env::var_os("JAVA_HOME").map(PathBuf::from);
        let home = std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".jdks"));
        let mut cands: Vec<PathBuf> = jh.into_iter().collect();
        if let Some(h) = home {
            cands.extend(std::fs::read_dir(h).into_iter().flatten().flatten().map(|e| e.path()));
        }
        cands.into_iter().map(|d| d.join("bin")).find(|b| b.join("javac.exe").is_file() || b.join("javac").is_file())
    }

    /// Maven のテスト（surefire に JDWP の口を開けさせる）を、@Nested の内部クラスのブレークポイントで止める。
    /// 手元の試験用のリポジトリを使うので普段は流さない（NAGI_DEBUG_REPO にリポジトリ、NAGI_DEBUG_CLASS にテストのクラスの完全な名前、
    /// NAGI_DEBUG_LINE に @Nested の内部クラスの中の行、NAGI_DEBUG_METHOD にその行のメソッドの名前）
    #[test]
    #[ignore]
    fn debugs_maven_surefire() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} がありません"));
        let (repo, class, method) = (var("NAGI_DEBUG_REPO"), var("NAGI_DEBUG_CLASS"), var("NAGI_DEBUG_METHOD"));
        let line: usize = var("NAGI_DEBUG_LINE").parse().unwrap();
        let simple = class.rsplit('.').next().unwrap().to_string();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let agent = format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}");
        let mut mvn = std::process::Command::new("cmd")
            .args(["/C", "mvn", "-q", "test", &format!("-Dtest={simple}"), &format!("-Dmaven.surefire.debug={agent}")])
            .current_dir(&repo)
            .env("JAVA_HOME", javac().and_then(|b| b.parent().map(|p| p.to_path_buf())).unwrap())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let file = PathBuf::from(&repo).join("src/test/java").join(format!("{}.java", class.replace('.', "/")));
        let bp = Bp { class: class.clone(), file, line };
        let (tx, task) = start(format!("127.0.0.1:{port}"), vec![bp], false, std::sync::Arc::new(|| {}));
        let frames = loop {
            match task.recv_timeout(Duration::from_secs(240)) {
                Some(Ev::Stopped { frames, .. }) => break frames,
                Some(Ev::Ended(e)) => panic!("終わった: {e}"),
                Some(_) => continue,
                None => panic!("止まらない"),
            }
        };
        assert_eq!((frames[0].method.as_str(), frames[0].line), (method.as_str(), Some(line)), "{frames:?}");
        assert!(frames[0].class.contains(&format!("{simple}$")), "{}", frames[0].class);
        tx.send(Cmd::Stop).unwrap();
        let _ = mvn.wait();
    }

    /// 本物の JVM で: ブレークポイントで止まり、変数を読み、中へ・外へステップし、続けると終わる
    #[test]
    fn debugs_a_real_jvm() {
        let Some(bin) = javac() else {
            eprintln!("JDK が無いので飛ばす");
            return;
        };
        let dir = std::env::temp_dir().join(format!("nagi-jdwp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = "public class Hello {\n  static int add(int a, int b) {\n    int c = a + b;\n    return c;\n  }\n  public static void main(String[] args) {\n    String s = \"hi\";\n    int[] xs = {1, 2, 3};\n    int r = add(2, 3);\n    System.out.println(s + r + xs.length);\n  }\n}\n";
        std::fs::write(dir.join("Hello.java"), src).unwrap();
        let st = std::process::Command::new(bin.join("javac")).args(["-g", "Hello.java"]).current_dir(&dir).status().unwrap();
        assert!(st.success());
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut jvm = std::process::Command::new(bin.join("java"))
            .args([&format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}"), "-cp", ".", "Hello"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let bp = Bp { class: "Hello".into(), file: dir.join("Hello.java"), line: 9 };
        let (tx, task) = start(format!("127.0.0.1:{port}"), vec![bp], true, std::sync::Arc::new(|| {}));
        let next_stop = || loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Stopped { frames, vars, .. }) => return (frames, vars),
                Some(Ev::Ended(e)) => panic!("終わった: {e}"),
                Some(_) => continue,
                None => panic!("止まらない"),
            }
        };
        let (frames, vars) = next_stop();
        assert_eq!((frames[0].class.as_str(), frames[0].method.as_str(), frames[0].line), ("Hello", "main", Some(9)));
        assert_eq!(frames[0].source.as_deref(), Some("Hello.java"));
        let s = vars.iter().find(|v| v.name == "s").unwrap();
        assert_eq!(s.value, "\"hi\"");
        let xs = vars.iter().find(|v| v.name == "xs").unwrap();
        assert_eq!(xs.value, "int[3]");
        tx.send(Cmd::Expand(xs.children.unwrap().0, xs.children.unwrap().1)).unwrap();
        loop {
            match task.recv_timeout(Duration::from_secs(10)) {
                Some(Ev::Children { vars, .. }) => {
                    assert_eq!(vars.iter().map(|v| v.value.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
                    break;
                }
                Some(_) => continue,
                None => panic!("中身が来ない"),
            }
        }
        // 中へ: add の 1 行目
        tx.send(Cmd::Step(Depth::Into)).unwrap();
        let (frames, vars) = next_stop();
        assert_eq!((frames[0].method.as_str(), frames[0].line), ("add", Some(3)));
        assert_eq!(vars.iter().map(|v| (v.name.as_str(), v.value.as_str())).collect::<Vec<_>>(), [("a", "2"), ("b", "3")]);
        // 次の行
        tx.send(Cmd::Step(Depth::Over)).unwrap();
        let (frames, vars) = next_stop();
        assert_eq!(frames[0].line, Some(4));
        assert!(vars.iter().any(|v| v.name == "c" && v.value == "5"));
        // 外へ: main に戻る
        tx.send(Cmd::Step(Depth::Out)).unwrap();
        let (frames, _) = next_stop();
        assert_eq!(frames[0].method, "main");
        tx.send(Cmd::Resume).unwrap();
        loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Ended(_)) => break,
                Some(_) => continue,
                None => panic!("終わらない"),
            }
        }
        let _ = jvm.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
