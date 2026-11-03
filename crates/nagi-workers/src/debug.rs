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

/// ブレークポイント（クラスは com.x.Foo、行は 1 始まり）。条件があれば、それが true のときだけ止まる
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bp {
    pub class: String,
    pub file: PathBuf,
    pub line: usize,
    pub condition: Option<String>,
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
    /// 止まっている間に、フレーム（番号）で式を評価する
    Evaluate(usize, String),
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
    /// 式の評価の結果（名前は式。評価できなければ Err）
    Evaluated { expr: String, result: Result<Var, String> },
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
        let mut s = Session { conn, rx, bps: Vec::new(), placed: HashMap::new(), prepare_reqs: Vec::new(), cache: Cache::default(), stopped: None, step_req: None, eval_frames: None };
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
    /// 評価の間のスタック（メソッドを呼ぶとフレームの ID が変わるので、呼んだら取り直す）
    eval_frames: Option<Vec<(Id, Location)>>,
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
            // 条件が false で止まらなかった
            let mut skipped = None;
            for e in &evs.events {
                match e {
                    Event::VmDeath => return Ok("JVM が終わりました".into()),
                    Event::ClassPrepare { thread, class, signature, .. } => {
                        self.place_in_class(*class, signature, send)?;
                        resume_thread = Some(*thread);
                    }
                    Event::Breakpoint { request, thread, .. } => match self.condition_of(*request) {
                        None => stop = Some(("ブレークポイント".into(), *thread)),
                        Some(cond) => match self.check(*thread, &cond) {
                            Ok(true) => stop = Some((format!("ブレークポイント（{cond}）"), *thread)),
                            Ok(false) => skipped = Some(*thread),
                            Err(e) => {
                                send(Ev::Note(format!("ブレークポイントの条件を評価できません: {cond}（{e}）")));
                                stop = Some(("ブレークポイント（条件を評価できません）".into(), *thread));
                            }
                        },
                    },
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
            } else if let Some(t) = resume_thread.or(skipped) {
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
            Cmd::Evaluate(i, expr) => {
                let result = self.evaluate(i, &expr);
                send(Ev::Evaluated { expr, result });
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

    /// フレームの見えているローカル変数（名前, 型のシグネチャ, 値。スロットの順）
    fn frame_locals(&mut self, thread: Id, frame: Id, at: &Location) -> Result<Vec<(String, String, Value)>, String> {
        let key = (at.class, at.method);
        let table = match self.cache.vars.get(&key) {
            Some(t) => t.clone(),
            None => {
                let t = self.conn.variable_table(at.class, at.method)?;
                self.cache.vars.insert(key, t.clone());
                t
            }
        };
        let mut seen: Vec<(u64, String, String, u32, i32)> = table.into_iter().filter(|v| at.index >= v.0 && at.index < v.0 + v.3 as u64).collect();
        seen.sort_by_key(|v| v.4);
        let slots: Vec<(i32, u8)> = seen.iter().map(|v| (v.4, v.2.as_bytes()[0])).collect();
        let values = self.conn.frame_values(thread, frame, &slots).unwrap_or_default();
        Ok(seen.into_iter().zip(values).map(|(v, val)| (v.1, v.2, val)).collect())
    }

    /// フレームの変数（今の位置で見えるもの。スロットの順）
    fn frame_vars(&mut self, thread: Id, frame: Id, at: &Location) -> Vec<Var> {
        let locals = match self.frame_locals(thread, frame, at) {
            Ok(l) => l,
            Err(e) => return vec![Var { name: "（変数）".into(), ty: String::new(), value: e, children: None }],
        };
        let mut out: Vec<Var> = locals.iter().map(|(n, sig, val)| self.var(n, &type_name(sig), val)).collect();
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

    // ---------------------------------------------------------------- 式の評価（jeval.rs で読んだ式）

    /// 置いた頼みの番号のブレークポイントの条件
    fn condition_of(&self, request: i32) -> Option<String> {
        self.placed.iter().find(|(_, rs)| rs.contains(&request)).and_then(|(b, _)| b.condition.clone()).filter(|c| !c.trim().is_empty())
    }

    /// 条件を止まったスレッドのいちばん上のフレームで評価する
    fn check(&mut self, thread: Id, cond: &str) -> Result<bool, String> {
        let e = crate::jeval::parse(cond)?;
        self.eval_frames = None;
        match self.eval(thread, 0, &e)? {
            JV::V(Value::Bool(b)) => Ok(b),
            _ => Err("条件が true・false になりません".into()),
        }
    }

    /// 止まっている間に式を評価する（フレームは番号）
    fn evaluate(&mut self, i: usize, expr: &str) -> Result<Var, String> {
        let (thread, _) = self.stopped.clone().ok_or("止まっていません")?;
        let e = crate::jeval::parse(expr)?;
        self.eval_frames = None;
        let r = self.eval(thread, i, &e);
        // メソッドを呼ぶとスレッドを一度動かすので、フレームを取り直す
        if let Ok(f) = self.conn.frames(thread) {
            self.stopped = Some((thread, f));
        }
        let v = self.jvalue(r?)?;
        let ty = self.type_of(&v);
        Ok(self.var(expr, &ty, &v))
    }

    /// 値の型の名前
    fn type_of(&mut self, v: &Value) -> String {
        match v {
            Value::Void => "void".into(),
            Value::Bool(_) => "boolean".into(),
            Value::Byte(_) => "byte".into(),
            Value::Char(_) => "char".into(),
            Value::Short(_) => "short".into(),
            Value::Int(_) => "int".into(),
            Value::Long(_) => "long".into(),
            Value::Float(_) => "float".into(),
            Value::Double(_) => "double".into(),
            Value::Object(_, 0) => "null".into(),
            Value::Object(_, id) => self.conn.object_class(*id).ok().and_then(|c| self.sig(c).ok()).map(|s| type_name(&s)).unwrap_or_default(),
        }
    }

    /// 評価した値を VM の値に（文字列のリテラルは VM の中に作る）
    fn jvalue(&mut self, v: JV) -> Result<Value, String> {
        match v {
            JV::V(v) => Ok(v),
            JV::Str(t) => Ok(Value::Object(b's', self.conn.create_string(&t)?)),
        }
    }

    /// 文字列として読む（VM の文字列・リテラル）。文字列でなければ None
    fn text_of(&mut self, v: &JV) -> Option<String> {
        match v {
            JV::Str(t) => Some(t.clone()),
            JV::V(Value::Object(b's', id)) if *id != 0 => self.conn.string_value(*id).ok(),
            _ => None,
        }
    }

    /// クラスと親クラスのフィールド（ID, 名前, シグネチャ, static か）から名前で
    fn find_field(&mut self, mut class: Id, name: &str) -> Result<Option<(Id, Id, String, bool)>, String> {
        while class != 0 {
            if let Some(f) = self.conn.fields(class)?.into_iter().find(|f| f.1 == name) {
                return Ok(Some((class, f.0, f.2, f.3)));
            }
            class = self.conn.superclass(class).unwrap_or(0);
        }
        Ok(None)
    }

    /// 評価しているフレーム（番号）の今の ID と位置
    fn frame_at(&mut self, thread: Id, fi: usize) -> Result<(Id, Location), String> {
        if self.eval_frames.is_none() {
            self.eval_frames = Some(self.conn.frames(thread)?);
        }
        self.eval_frames.as_ref().and_then(|f| f.get(fi)).copied().ok_or_else(|| "フレームがありません".to_string())
    }

    /// 名前: ローカル変数、this のフィールド、今のクラスの static のフィールドの順に探す
    fn name(&mut self, thread: Id, fi: usize, name: &str) -> Result<JV, String> {
        let (frame, at) = self.frame_at(thread, fi)?;
        let at = &at;
        if name == "this" {
            return Ok(JV::V(self.conn.this_object(thread, frame)?));
        }
        if let Some((_, _, v)) = self.frame_locals(thread, frame, at).unwrap_or_default().into_iter().find(|l| l.0 == name) {
            return Ok(JV::V(v));
        }
        if let Some((_, id)) = self.conn.this_object(thread, frame).ok().and_then(|t| t.object()) {
            let class = self.conn.object_class(id)?;
            if let Some((_, f, _, stat)) = self.find_field(class, name)? {
                return Ok(JV::V(if stat { self.conn.static_values(class, &[f])? } else { self.conn.object_values(id, &[f])? }.remove(0)));
            }
        }
        if let Some((owner, f, _, true)) = self.find_field(at.class, name)? {
            return Ok(JV::V(self.conn.static_values(owner, &[f])?.remove(0)));
        }
        Err(format!("{name} が見つかりません"))
    }

    /// 式を評価する
    fn eval(&mut self, thread: Id, fi: usize, e: &crate::jeval::Expr) -> Result<JV, String> {
        use crate::jeval::{Expr, Lit, Op};
        Ok(match e {
            Expr::Lit(l) => JV::V(match l {
                Lit::Int(n) => i32::try_from(*n).map_or(Value::Long(*n), Value::Int),
                Lit::Long(n) => Value::Long(*n),
                Lit::Float(x) => Value::Double(*x),
                Lit::Char(c) => Value::Char(*c as u16),
                Lit::Bool(b) => Value::Bool(*b),
                Lit::Null => Value::Object(b'L', 0),
                Lit::Str(t) => return Ok(JV::Str(t.clone())),
            }),
            Expr::Name(n) => self.name(thread, fi, n)?,
            Expr::Field(obj, name) => {
                let o = self.eval(thread, fi, obj)?;
                let o = self.jvalue(o)?;
                let Some((tag, id)) = o.object() else { return Err(format!("null の {name} は読めません")) };
                if tag == b'[' && name == "length" {
                    return Ok(JV::V(Value::Int(self.conn.array_length(id)?)));
                }
                let class = self.conn.object_class(id)?;
                let Some((owner, f, _, stat)) = self.find_field(class, name)? else { return Err(format!("フィールド {name} がありません")) };
                JV::V(if stat { self.conn.static_values(owner, &[f])? } else { self.conn.object_values(id, &[f])? }.remove(0))
            }
            Expr::Index(arr, ix) => {
                let a = self.eval(thread, fi, arr)?;
                let Some((b'[', id)) = self.jvalue(a)?.object() else { return Err("配列ではありません".into()) };
                let i = self.eval(thread, fi, ix)?;
                let Some(Num::I(i)) = num(&i) else { return Err("添字が整数ではありません".into()) };
                let n = self.conn.array_length(id)? as i64;
                if i < 0 || i >= n {
                    return Err(format!("添字 {i} は範囲の外です（長さ {n}）"));
                }
                JV::V(self.conn.array_values(id, i as i32, 1)?.remove(0))
            }
            Expr::Call(recv, name, args) => {
                let target = match recv {
                    Some(r) => {
                        let v = self.eval(thread, fi, r)?;
                        let v = self.jvalue(v)?;
                        match v.object() {
                            Some((_, id)) => Some(id),
                            None => return Err(format!("null の {name}() は呼べません")),
                        }
                    }
                    None => {
                        let (frame, _) = self.frame_at(thread, fi)?;
                        self.conn.this_object(thread, frame).ok().and_then(|t| t.object()).map(|o| o.1)
                    }
                };
                let mut vals = Vec::new();
                for a in args {
                    vals.push(self.eval(thread, fi, a)?);
                }
                let here = self.frame_at(thread, fi)?.1.class;
                self.call(thread, target, here, name, vals)?
            }
            Expr::Not(x) => match self.eval(thread, fi, x)? {
                JV::V(Value::Bool(b)) => JV::V(Value::Bool(!b)),
                _ => return Err("! の後が true・false ではありません".into()),
            },
            Expr::Neg(x) => match num(&self.eval(thread, fi, x)?) {
                Some(Num::I(i)) => JV::V(int_value(-i, false)),
                Some(Num::L(i)) => JV::V(Value::Long(-i)),
                Some(Num::F(f)) => JV::V(Value::Double(-f)),
                None => return Err("- の後が数ではありません".into()),
            },
            Expr::Bin(l, op, r) => {
                // && と || は左だけで決まれば右を評価しない（null の確かめの後ろで落ちない）
                if matches!(op, Op::And | Op::Or) {
                    let JV::V(Value::Bool(a)) = self.eval(thread, fi, l)? else { return Err("&&・|| の左が true・false ではありません".into()) };
                    if (*op == Op::And && !a) || (*op == Op::Or && a) {
                        return Ok(JV::V(Value::Bool(a)));
                    }
                    let JV::V(Value::Bool(b)) = self.eval(thread, fi, r)? else { return Err("&&・|| の右が true・false ではありません".into()) };
                    return Ok(JV::V(Value::Bool(b)));
                }
                let a = self.eval(thread, fi, l)?;
                let b = self.eval(thread, fi, r)?;
                self.binary(a, *op, b)?
            }
        })
    }

    fn binary(&mut self, a: JV, op: crate::jeval::Op, b: JV) -> Result<JV, String> {
        use crate::jeval::Op;
        // 文字列をつなぐ
        if op == Op::Add {
            let (ta, tb) = (self.text_of(&a), self.text_of(&b));
            if ta.is_some() || tb.is_some() {
                let sa = match ta {
                    Some(t) => t,
                    None => self.show(&a),
                };
                let sb = match tb {
                    Some(t) => t,
                    None => self.show(&b),
                };
                return Ok(JV::Str(sa + &sb));
            }
        }
        if matches!(op, Op::Eq | Op::Ne) {
            let same = match (&a, &b) {
                (JV::V(Value::Bool(x)), JV::V(Value::Bool(y))) => x == y,
                // 文字列のリテラルと比べるときは中身で（VM の文字列どうしは Java と同じく参照で）
                (JV::Str(_), _) | (_, JV::Str(_)) => self.text_of(&a).is_some() && self.text_of(&a) == self.text_of(&b),
                (JV::V(Value::Object(_, x)), JV::V(Value::Object(_, y))) => x == y,
                _ => match (num(&a), num(&b)) {
                    (Some(x), Some(y)) => x.f() == y.f(),
                    _ => return Err("比べられない組み合わせです".into()),
                },
            };
            return Ok(JV::V(Value::Bool(same == (op == Op::Eq))));
        }
        let (Some(x), Some(y)) = (num(&a), num(&b)) else { return Err("数ではありません".into()) };
        if matches!(op, Op::Lt | Op::Le | Op::Gt | Op::Ge) {
            let (x, y) = (x.f(), y.f());
            return Ok(JV::V(Value::Bool(match op {
                Op::Lt => x < y,
                Op::Le => x <= y,
                Op::Gt => x > y,
                _ => x >= y,
            })));
        }
        Ok(JV::V(match (x, y) {
            (Num::F(_), _) | (_, Num::F(_)) => {
                let (x, y) = (x.f(), y.f());
                Value::Double(match op {
                    Op::Add => x + y,
                    Op::Sub => x - y,
                    Op::Mul => x * y,
                    Op::Div => x / y,
                    _ => x % y,
                })
            }
            _ => {
                let long = matches!(x, Num::L(_)) || matches!(y, Num::L(_));
                let (x, y) = (x.i(), y.i());
                if matches!(op, Op::Div | Op::Rem) && y == 0 {
                    return Err("0 で割りました".into());
                }
                let r = match op {
                    Op::Add => x.wrapping_add(y),
                    Op::Sub => x.wrapping_sub(y),
                    Op::Mul => x.wrapping_mul(y),
                    Op::Div => x / y,
                    _ => x % y,
                };
                int_value(r, long)
            }
        }))
    }

    /// 文字列をつなぐときの値の書き方
    fn show(&mut self, v: &JV) -> String {
        match v {
            JV::Str(t) => t.clone(),
            JV::V(Value::Char(c)) => char::from_u32(*c as u32).unwrap_or('?').to_string(),
            JV::V(Value::Object(_, 0)) => "null".into(),
            JV::V(x) => {
                let ty = self.type_of(x);
                self.var("", &ty, x).value
            }
        }
    }

    /// メソッドを呼ぶ（`target` が None なら今のクラスの static）。名前と引数の数が合うものを、クラスから親へ探す
    fn call(&mut self, thread: Id, target: Option<Id>, here: Id, name: &str, args: Vec<JV>) -> Result<JV, String> {
        let mut class = match target {
            Some(id) => self.conn.object_class(id)?,
            None => here,
        };
        let found = loop {
            if class == 0 {
                return Err(format!("メソッド {name}（引数 {} 個）がありません", args.len()));
            }
            if let Some(m) = self.methods(class)?.into_iter().find(|m| m.1 == name && params(&m.2).len() == args.len()) {
                break (class, m);
            }
            class = self.conn.superclass(class).unwrap_or(0);
        };
        let (owner, (mid, _, sig)) = found;
        let mut vals = Vec::new();
        for (a, p) in args.into_iter().zip(params(&sig)) {
            vals.push(self.argument(a, &p)?);
        }
        let r = self.conn.invoke(target, thread, owner, mid, &vals);
        self.eval_frames = None;
        let (ret, exc) = r?;
        if let Some((_, e)) = exc.object() {
            let what = self.conn.object_class(e).ok().and_then(|c| self.sig(c).ok()).map_or("例外".into(), |s| type_name(&s));
            return Err(format!("{what} が投げられました"));
        }
        Ok(JV::V(ret))
    }

    /// 引数を引数の型（シグネチャ）に合わせる
    fn argument(&mut self, a: JV, p: &str) -> Result<Value, String> {
        let tag = p.as_bytes()[0];
        if matches!(tag, b'L' | b'[') {
            return match a {
                JV::V(Value::Object(t, id)) => Ok(Value::Object(t, id)),
                JV::Str(_) => self.jvalue(a),
                _ => Err(format!("基本型の値は {} の引数に渡せません", type_name(p))),
            };
        }
        let n = num(&a);
        Ok(match (tag, a, n) {
            (b'Z', JV::V(Value::Bool(b)), _) => Value::Bool(b),
            (b'C', JV::V(Value::Char(c)), _) => Value::Char(c),
            (b'I', _, Some(n)) => Value::Int(n.i() as i32),
            (b'J', _, Some(n)) => Value::Long(n.i()),
            (b'S', _, Some(n)) => Value::Short(n.i() as i16),
            (b'B', _, Some(n)) => Value::Byte(n.i() as i8),
            (b'F', _, Some(n)) => Value::Float(n.f() as f32),
            (b'D', _, Some(n)) => Value::Double(n.f()),
            _ => return Err(format!("引数の型が {} に合いません", type_name(p))),
        })
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

/// 評価の途中の値（VM の値か、まだ VM に作っていない文字列のリテラル）
enum JV {
    V(Value),
    Str(String),
}

/// 数（整数・long・小数）
#[derive(Clone, Copy)]
enum Num {
    I(i64),
    L(i64),
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) | Num::L(i) => i as f64,
            Num::F(f) => f,
        }
    }
    fn i(self) -> i64 {
        match self {
            Num::I(i) | Num::L(i) => i,
            Num::F(f) => f as i64,
        }
    }
}

fn num(v: &JV) -> Option<Num> {
    Some(match v {
        JV::V(Value::Byte(x)) => Num::I(*x as i64),
        JV::V(Value::Short(x)) => Num::I(*x as i64),
        JV::V(Value::Char(x)) => Num::I(*x as i64),
        JV::V(Value::Int(x)) => Num::I(*x as i64),
        JV::V(Value::Long(x)) => Num::L(*x),
        JV::V(Value::Float(x)) => Num::F(*x as f64),
        JV::V(Value::Double(x)) => Num::F(*x),
        _ => return None,
    })
}

/// 整数の結果（int に収まり long が混ざっていなければ int）
fn int_value(r: i64, long: bool) -> Value {
    match i32::try_from(r) {
        Ok(i) if !long => Value::Int(i),
        _ => Value::Long(r),
    }
}

/// メソッドのシグネチャの引数の型（"(ILjava/lang/String;[I)V" → ["I", "Ljava/lang/String;", "[I"]）
fn params(sig: &str) -> Vec<String> {
    let inner = sig.strip_prefix('(').and_then(|s| s.split_once(')')).map_or("", |x| x.0);
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let st = i;
        while b[i] == b'[' {
            i += 1;
        }
        if b[i] == b'L' {
            while b[i] != b';' {
                i += 1;
            }
        }
        i += 1;
        out.push(inner[st..i].to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_types() {
        assert_eq!(params("(ILjava/lang/String;[I[[Ljava/lang/Object;)V"), ["I", "Ljava/lang/String;", "[I", "[[Ljava/lang/Object;"]);
        assert!(params("()I").is_empty());
    }

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
        let bp = Bp { class: class.clone(), file, line, condition: None };
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

    /// 本物の JVM で: 条件付きのブレークポイントは条件が true のときだけ止まる。止まったところで式を評価する
    /// （変数・フィールド・配列・メソッド呼び出し・演算。評価できない式は理由を返す）
    #[test]
    fn conditions_and_evaluate() {
        let Some(bin) = javac() else {
            eprintln!("JDK が無いので飛ばす");
            return;
        };
        let dir = std::env::temp_dir().join(format!("nagi-jeval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = "import java.util.*;\npublic class Calc {\n  static int LIMIT = 7;\n  String name = \"calc\";\n  int run() {\n    List<String> list = new ArrayList<>();\n    int[] xs = {4, 5, 6};\n    int total = 0;\n    for (int i = 0; i < 5; i++) {\n      total += i;\n      list.add(\"v\" + i);\n    }\n    return total + xs.length;\n  }\n  public static void main(String[] args) {\n    System.out.println(new Calc().run());\n  }\n}\n";
        std::fs::write(dir.join("Calc.java"), src).unwrap();
        assert!(std::process::Command::new(bin.join("javac")).args(["-g", "Calc.java"]).current_dir(&dir).status().unwrap().success());
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut jvm = std::process::Command::new(bin.join("java"))
            .args([&format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:{port}"), "-cp", ".", "Calc"])
            .current_dir(&dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // 10 行目（total += i）で、i == 3 のときだけ
        let bp = Bp { class: "Calc".into(), file: dir.join("Calc.java"), line: 10, condition: Some("i == 3 && list.size() == 3".into()) };
        let (tx, task) = start(format!("127.0.0.1:{port}"), vec![bp], false, std::sync::Arc::new(|| {}));
        let vars = loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Stopped { vars, .. }) => break vars,
                Some(Ev::Ended(e)) => panic!("終わった: {e}"),
                Some(Ev::Note(n)) => panic!("{n}"),
                Some(_) => continue,
                None => panic!("止まらない"),
            }
        };
        assert!(vars.iter().any(|v| v.name == "i" && v.value == "3"), "{vars:?}");
        let eval = |e: &str| {
            tx.send(Cmd::Evaluate(0, e.into())).unwrap();
            loop {
                match task.recv_timeout(Duration::from_secs(10)) {
                    Some(Ev::Evaluated { expr, result }) => {
                        assert_eq!(expr, e);
                        return result.map(|v| v.value);
                    }
                    Some(_) => continue,
                    None => panic!("返事が来ない: {e}"),
                }
            }
        };
        assert_eq!(eval("total"), Ok("3".into()));
        assert_eq!(eval("xs[1] * 10 + i"), Ok("53".into()));
        assert_eq!(eval("xs.length"), Ok("3".into()));
        assert_eq!(eval("list.size()"), Ok("3".into()));
        assert_eq!(eval("list.get(2)"), Ok("\"v2\"".into()));
        assert_eq!(eval("list.contains(\"v1\")"), Ok("true".into()));
        assert_eq!(eval("name"), Ok("\"calc\"".into()));
        assert_eq!(eval("this.name.length() + LIMIT"), Ok("11".into()));
        assert_eq!(eval("name + \"!\" + i"), Ok("\"calc!3\"".into()));
        assert_eq!(eval("name.equals(\"calc\") && !(i > 3)"), Ok("true".into()));
        assert_eq!(eval("total / 2.0"), Ok("1.5".into()));
        assert!(eval("nope").unwrap_err().contains("nope"));
        assert!(eval("xs[9]").unwrap_err().contains("範囲"));
        assert!(eval("name.charAt(99)").unwrap_err().contains("StringIndexOutOfBoundsException"));
        assert!(eval("list.size(").is_err());
        // 評価の後も続けられる（条件は i が 3 のときだけなので、そのまま終わる）
        tx.send(Cmd::Resume).unwrap();
        loop {
            match task.recv_timeout(Duration::from_secs(30)) {
                Some(Ev::Ended(_)) => break,
                Some(Ev::Stopped { .. }) => panic!("条件が false でも止まった"),
                Some(_) => continue,
                None => panic!("終わらない"),
            }
        }
        let _ = jvm.wait();
        let _ = std::fs::remove_dir_all(&dir);
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
        let bp = Bp { class: "Hello".into(), file: dir.join("Hello.java"), line: 9, condition: None };
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
