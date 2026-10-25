//! JDWP（Java Debug Wire Protocol）の通信。JVM を `-agentlib:jdwp=transport=dt_socket,server=y,...` で起こし、
//! TCP でつなぐ。LSP・IDE のデバッガは使わない（nagi の方針）。
//!
//! 読むのは受け取り用のスレッド（1 つのパケットを丸ごと読んで渡す）、送るのと返事を待つのは呼ぶ側のスレッド。
//! 返事を待つ間に届いたイベントは溜めておき、`next_event` で順に返す。
//! 仕様: https://docs.oracle.com/en/java/javase/17/docs/specs/jdwp/jdwp-protocol.html

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

pub type Id = u64;

/// 型の種類（TypeTag）
pub const TAG_CLASS: u8 = 1;

/// イベントの種類（EventKind）
pub mod kind {
    pub const SINGLE_STEP: u8 = 1;
    pub const BREAKPOINT: u8 = 2;
    pub const EXCEPTION: u8 = 4;
    pub const THREAD_START: u8 = 6;
    pub const THREAD_DEATH: u8 = 7;
    pub const CLASS_PREPARE: u8 = 8;
    pub const CLASS_UNLOAD: u8 = 9;
    pub const VM_START: u8 = 90;
    pub const VM_DEATH: u8 = 99;
}

/// 止め方（SuspendPolicy）
pub const SUSPEND_EVENT_THREAD: u8 = 1;
pub const SUSPEND_ALL: u8 = 2;

/// ステップの深さ（StepDepth）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Into = 0,
    Over = 1,
    Out = 2,
}

/// コードの位置（型, クラス, メソッド, バイトコードの位置）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    pub tag: u8,
    pub class: Id,
    pub method: Id,
    pub index: u64,
}

/// 値（タグ付き）
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    Bool(bool),
    Byte(i8),
    Char(u16),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// オブジェクト（タグ: L オブジェクト・s 文字列・[ 配列・t スレッドなど, ID。0 は null）
    Object(u8, Id),
}

impl Value {
    pub fn object(&self) -> Option<(u8, Id)> {
        match self {
            Value::Object(t, id) if *id != 0 => Some((*t, *id)),
            _ => None,
        }
    }
}

/// VM から届いたイベント
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    VmStart { thread: Id },
    VmDeath,
    Breakpoint { request: i32, thread: Id, at: Location },
    Step { request: i32, thread: Id, at: Location },
    Exception { thread: Id, at: Location, exception: Value, caught: bool },
    ClassPrepare { thread: Id, tag: u8, class: Id, signature: String },
    Other(u8),
}

/// 1 回のイベントの組（止め方と中身）
#[derive(Clone, Debug)]
pub struct Events {
    pub suspend: u8,
    pub events: Vec<Event>,
}

struct Packet {
    id: u32,
    reply: bool,
    /// 返事ならエラーの番号、命令なら (命令の組, 命令)
    code: u16,
    data: Vec<u8>,
}

/// ID の大きさ（IDSizes。ほとんどの VM で 8）
#[derive(Clone, Copy, Debug)]
struct Sizes {
    field: usize,
    method: usize,
    object: usize,
    reftype: usize,
    frame: usize,
}

/// 送るデータを組み立てる
pub struct Out<'a> {
    v: Vec<u8>,
    s: &'a Sizes,
}

impl Out<'_> {
    pub fn u8(&mut self, x: u8) -> &mut Self {
        self.v.push(x);
        self
    }
    pub fn i32(&mut self, x: i32) -> &mut Self {
        self.v.extend_from_slice(&x.to_be_bytes());
        self
    }
    pub fn u64(&mut self, x: u64) -> &mut Self {
        self.v.extend_from_slice(&x.to_be_bytes());
        self
    }
    fn id(&mut self, x: Id, n: usize) -> &mut Self {
        self.v.extend_from_slice(&x.to_be_bytes()[8 - n..]);
        self
    }
    pub fn object(&mut self, x: Id) -> &mut Self {
        let n = self.s.object;
        self.id(x, n)
    }
    pub fn reftype(&mut self, x: Id) -> &mut Self {
        let n = self.s.reftype;
        self.id(x, n)
    }
    pub fn method(&mut self, x: Id) -> &mut Self {
        let n = self.s.method;
        self.id(x, n)
    }
    pub fn field(&mut self, x: Id) -> &mut Self {
        let n = self.s.field;
        self.id(x, n)
    }
    pub fn frame(&mut self, x: Id) -> &mut Self {
        let n = self.s.frame;
        self.id(x, n)
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.i32(s.len() as i32);
        self.v.extend_from_slice(s.as_bytes());
        self
    }
    pub fn location(&mut self, l: &Location) -> &mut Self {
        self.u8(l.tag).reftype(l.class).method(l.method).u64(l.index)
    }
}

/// 受け取ったデータを読む
pub struct In {
    d: Vec<u8>,
    i: usize,
    s: Sizes,
}

impl In {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let b = self.d.get(self.i..self.i + n).ok_or("JDWP: データが短い")?;
        self.i += n;
        Ok(b)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().unwrap()))
    }
    fn id(&mut self, n: usize) -> Result<Id, String> {
        let b = self.take(n)?;
        Ok(b.iter().fold(0u64, |a, x| (a << 8) | *x as u64))
    }
    pub fn object(&mut self) -> Result<Id, String> {
        let n = self.s.object;
        self.id(n)
    }
    pub fn reftype(&mut self) -> Result<Id, String> {
        let n = self.s.reftype;
        self.id(n)
    }
    pub fn method(&mut self) -> Result<Id, String> {
        let n = self.s.method;
        self.id(n)
    }
    pub fn field(&mut self) -> Result<Id, String> {
        let n = self.s.field;
        self.id(n)
    }
    pub fn frame(&mut self) -> Result<Id, String> {
        let n = self.s.frame;
        self.id(n)
    }
    pub fn string(&mut self) -> Result<String, String> {
        let n = self.i32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    pub fn location(&mut self) -> Result<Location, String> {
        Ok(Location { tag: self.u8()?, class: self.reftype()?, method: self.method()?, index: self.u64()? })
    }
    /// タグの後の値
    pub fn value_of(&mut self, tag: u8) -> Result<Value, String> {
        Ok(match tag {
            b'V' => Value::Void,
            b'Z' => Value::Bool(self.u8()? != 0),
            b'B' => Value::Byte(self.u8()? as i8),
            b'C' => Value::Char(self.u16()?),
            b'S' => Value::Short(self.u16()? as i16),
            b'I' => Value::Int(self.i32()?),
            b'J' => Value::Long(self.u64()? as i64),
            b'F' => Value::Float(f32::from_bits(self.i32()? as u32)),
            b'D' => Value::Double(f64::from_bits(self.u64()?)),
            t => Value::Object(t, self.object()?),
        })
    }
    pub fn value(&mut self) -> Result<Value, String> {
        let t = self.u8()?;
        self.value_of(t)
    }
}

/// つないだ JVM
pub struct Conn {
    stream: TcpStream,
    rx: Receiver<Packet>,
    next: u32,
    sizes: Sizes,
    queued: VecDeque<Events>,
}

/// JDWP のエラーの名前（よく出るもの）
fn error_name(code: u16) -> String {
    match code {
        10 => "INVALID_THREAD".into(),
        13 => "THREAD_NOT_SUSPENDED".into(),
        20 => "INVALID_OBJECT".into(),
        21 => "INVALID_CLASS".into(),
        23 => "INVALID_METHODID".into(),
        30 => "INVALID_FRAMEID".into(),
        32 => "OPAQUE_FRAME".into(),
        35 => "INVALID_SLOT".into(),
        41 => "NOT_FOUND".into(),
        101 => "ABSENT_INFORMATION（-g なしでコンパイルされている）".into(),
        112 => "VM_DEAD".into(),
        c => format!("エラー {c}"),
    }
}

fn read_packet(s: &mut TcpStream) -> std::io::Result<Packet> {
    let mut h = [0u8; 11];
    s.read_exact(&mut h)?;
    let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
    let id = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
    let reply = h[8] & 0x80 != 0;
    let code = u16::from_be_bytes([h[9], h[10]]);
    let mut data = vec![0u8; len.saturating_sub(11)];
    s.read_exact(&mut data)?;
    Ok(Packet { id, reply, code, data })
}

impl Conn {
    /// `addr`（localhost:5005 など）につなぐ。待つのは `wait` まで（JVM が起きるまで繰り返す）
    pub fn connect(addr: &str, wait: Duration, cancelled: &dyn Fn() -> bool) -> Result<Conn, String> {
        let until = std::time::Instant::now() + wait;
        let mut stream = loop {
            match TcpStream::connect(addr) {
                Ok(s) => break s,
                Err(e) if std::time::Instant::now() >= until => return Err(format!("{addr} につなげません: {e}")),
                Err(_) if cancelled() => return Err("やめました".into()),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        };
        stream.set_nodelay(true).ok();
        stream.write_all(b"JDWP-Handshake").map_err(|e| e.to_string())?;
        let mut hs = [0u8; 14];
        stream.read_exact(&mut hs).map_err(|e| format!("JDWP の挨拶が返りません: {e}"))?;
        if &hs != b"JDWP-Handshake" {
            return Err("JDWP ではありません".into());
        }
        let (tx, rx) = channel();
        let mut reader = stream.try_clone().map_err(|e| e.to_string())?;
        std::thread::Builder::new()
            .name("jdwp-read".into())
            .spawn(move || {
                while let Ok(p) = read_packet(&mut reader) {
                    if tx.send(p).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let default = Sizes { field: 8, method: 8, object: 8, reftype: 8, frame: 8 };
        let mut c = Conn { stream, rx, next: 1, sizes: default, queued: VecDeque::new() };
        let mut r = c.request(1, 7, |_| {})?;
        let n = |r: &mut In| r.i32().map(|x| x as usize);
        c.sizes = Sizes { field: n(&mut r)?, method: n(&mut r)?, object: n(&mut r)?, reftype: n(&mut r)?, frame: n(&mut r)? };
        Ok(c)
    }

    fn parse_events(&self, data: &[u8]) -> Result<Events, String> {
        let mut r = In { d: data.to_vec(), i: 0, s: self.sizes };
        let suspend = r.u8()?;
        let n = r.i32()?;
        let mut events = Vec::new();
        for _ in 0..n {
            let k = r.u8()?;
            let request = r.i32()?;
            events.push(match k {
                kind::VM_START => Event::VmStart { thread: r.object()? },
                kind::VM_DEATH => Event::VmDeath,
                kind::BREAKPOINT => Event::Breakpoint { request, thread: r.object()?, at: r.location()? },
                kind::SINGLE_STEP => Event::Step { request, thread: r.object()?, at: r.location()? },
                kind::EXCEPTION => {
                    let thread = r.object()?;
                    let at = r.location()?;
                    let exception = r.value()?;
                    let catch = r.location()?;
                    Event::Exception { thread, at, exception, caught: catch.class != 0 }
                }
                kind::CLASS_PREPARE => {
                    let thread = r.object()?;
                    let tag = r.u8()?;
                    let class = r.reftype()?;
                    let signature = r.string()?;
                    let _status = r.i32()?;
                    Event::ClassPrepare { thread, tag, class, signature }
                }
                kind::THREAD_START | kind::THREAD_DEATH => {
                    r.object()?;
                    Event::Other(k)
                }
                kind::CLASS_UNLOAD => {
                    r.string()?;
                    Event::Other(k)
                }
                // ほかの種類は中身を読めないので、ここで終わる（頼んでいないので来ない）
                k => {
                    events.push(Event::Other(k));
                    break;
                }
            });
        }
        Ok(Events { suspend, events })
    }

    /// 命令を送って返事を待つ（待つ間のイベントは溜める）
    pub fn request(&mut self, set: u8, cmd: u8, body: impl FnOnce(&mut Out)) -> Result<In, String> {
        let mut o = Out { v: Vec::new(), s: &self.sizes };
        body(&mut o);
        let data = o.v;
        let id = self.next;
        self.next += 1;
        let mut p = Vec::with_capacity(11 + data.len());
        p.extend_from_slice(&((11 + data.len()) as u32).to_be_bytes());
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&[0, set, cmd]);
        p.extend_from_slice(&data);
        self.stream.write_all(&p).map_err(|e| format!("JDWP に書けません: {e}"))?;
        loop {
            let pk = self.rx.recv_timeout(Duration::from_secs(30)).map_err(|_| "JDWP の返事がありません（VM が終わった）".to_string())?;
            if pk.reply && pk.id == id {
                if pk.code != 0 {
                    return Err(format!("JDWP {set}/{cmd}: {}", error_name(pk.code)));
                }
                return Ok(In { d: pk.data, i: 0, s: self.sizes });
            }
            if !pk.reply && pk.code == 0x4064 {
                let ev = self.parse_events(&pk.data)?;
                self.queued.push_back(ev);
            }
        }
    }

    /// 次のイベントの組（`wait` まで待つ）。VM が終わって切れたら Err
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<Events>, String> {
        if let Some(e) = self.queued.pop_front() {
            return Ok(Some(e));
        }
        match self.rx.recv_timeout(wait) {
            Ok(pk) if !pk.reply && pk.code == 0x4064 => Ok(Some(self.parse_events(&pk.data)?)),
            Ok(_) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err("JVM との接続が切れました".into()),
        }
    }

    // ---------------------------------------------------------------- 命令

    pub fn version(&mut self) -> Result<String, String> {
        let mut r = self.request(1, 1, |_| {})?;
        r.string()
    }

    /// 読み込まれたクラス（シグネチャ "Lcom/x/Foo;"）。(型, ID)
    pub fn classes_by_signature(&mut self, sig: &str) -> Result<Vec<(u8, Id)>, String> {
        let mut r = self.request(1, 2, |o| {
            o.string(sig);
        })?;
        let n = r.i32()?;
        (0..n).map(|_| Ok((r.u8()?, r.reftype().and_then(|id| r.i32().map(|_| id))?))).collect()
    }

    /// 読み込まれたすべてのクラス (型, ID, シグネチャ)
    pub fn all_classes(&mut self) -> Result<Vec<(u8, Id, String)>, String> {
        let mut r = self.request(1, 3, |_| {})?;
        let n = r.i32()?;
        (0..n)
            .map(|_| {
                let t = r.u8()?;
                let id = r.reftype()?;
                let s = r.string()?;
                r.i32()?;
                Ok((t, id, s))
            })
            .collect()
    }

    pub fn resume(&mut self) -> Result<(), String> {
        self.request(1, 9, |_| {}).map(|_| ())
    }

    pub fn dispose(&mut self) {
        let _ = self.request(1, 6, |_| {});
    }

    pub fn exit(&mut self, code: i32) {
        let _ = self.request(1, 10, |o| {
            o.i32(code);
        });
    }

    pub fn signature(&mut self, class: Id) -> Result<String, String> {
        let mut r = self.request(2, 1, |o| {
            o.reftype(class);
        })?;
        r.string()
    }

    /// クラスのメソッド (ID, 名前, シグネチャ)
    pub fn methods(&mut self, class: Id) -> Result<Vec<(Id, String, String)>, String> {
        let mut r = self.request(2, 5, |o| {
            o.reftype(class);
        })?;
        let n = r.i32()?;
        (0..n)
            .map(|_| {
                let id = r.method()?;
                let name = r.string()?;
                let sig = r.string()?;
                r.i32()?;
                Ok((id, name, sig))
            })
            .collect()
    }

    /// クラスのフィールド (ID, 名前, シグネチャ, static か)
    pub fn fields(&mut self, class: Id) -> Result<Vec<(Id, String, String, bool)>, String> {
        let mut r = self.request(2, 4, |o| {
            o.reftype(class);
        })?;
        let n = r.i32()?;
        (0..n)
            .map(|_| {
                let id = r.field()?;
                let name = r.string()?;
                let sig = r.string()?;
                let m = r.i32()?;
                Ok((id, name, sig, m & 0x0008 != 0))
            })
            .collect()
    }

    pub fn source_file(&mut self, class: Id) -> Result<String, String> {
        let mut r = self.request(2, 7, |o| {
            o.reftype(class);
        })?;
        r.string()
    }

    /// 親クラス（無ければ 0）
    pub fn superclass(&mut self, class: Id) -> Result<Id, String> {
        let mut r = self.request(3, 1, |o| {
            o.reftype(class);
        })?;
        r.reftype()
    }

    /// 行の表 (始め, 終わり, [(バイトコードの位置, 行)])。行は 1 始まり
    pub fn line_table(&mut self, class: Id, method: Id) -> Result<(u64, u64, Vec<(u64, i32)>), String> {
        let mut r = self.request(6, 1, |o| {
            o.reftype(class).method(method);
        })?;
        let start = r.u64()?;
        let end = r.u64()?;
        let n = r.i32()?;
        let lines = (0..n).map(|_| Ok((r.u64()?, r.i32()?))).collect::<Result<Vec<_>, String>>()?;
        Ok((start, end, lines))
    }

    /// 変数の表 [(始め, 名前, シグネチャ, 長さ, スロット)]
    pub fn variable_table(&mut self, class: Id, method: Id) -> Result<Vec<(u64, String, String, u32, i32)>, String> {
        let mut r = self.request(6, 2, |o| {
            o.reftype(class).method(method);
        })?;
        let _args = r.i32()?;
        let n = r.i32()?;
        (0..n).map(|_| Ok((r.u64()?, r.string()?, r.string()?, r.i32()? as u32, r.i32()?))).collect()
    }

    /// イベントの頼み（種類, 止め方, 条件）。頼みの番号
    pub fn set_request(&mut self, k: u8, suspend: u8, mods: impl FnOnce(&mut Out) -> i32) -> Result<i32, String> {
        let (body, count) = {
            let mut o = Out { v: Vec::new(), s: &self.sizes };
            let n = mods(&mut o);
            (o.v, n)
        };
        let mut r = self.request(15, 1, |o| {
            o.u8(k).u8(suspend).i32(count);
            o.v.extend_from_slice(&body);
        })?;
        r.i32()
    }

    pub fn clear_request(&mut self, k: u8, id: i32) {
        let _ = self.request(15, 2, |o| {
            o.u8(k).i32(id);
        });
    }

    /// その位置で止める
    pub fn set_breakpoint(&mut self, at: &Location) -> Result<i32, String> {
        self.set_request(kind::BREAKPOINT, SUSPEND_ALL, |o| {
            o.u8(7).location(at);
            1
        })
    }

    /// クラスが読み込まれたら知らせる（パターン "com.x.Foo" "com.x.Foo$*"）。スレッドだけ止める
    pub fn on_class_prepare(&mut self, pattern: &str) -> Result<i32, String> {
        self.set_request(kind::CLASS_PREPARE, SUSPEND_EVENT_THREAD, |o| {
            o.u8(5).string(pattern);
            1
        })
    }

    /// 例外で止める（捕まえないものだけか、捕まえるものも）
    pub fn on_exception(&mut self, caught: bool) -> Result<i32, String> {
        self.set_request(kind::EXCEPTION, SUSPEND_ALL, |o| {
            o.u8(8).reftype(0).u8(u8::from(caught)).u8(1);
            1
        })
    }

    /// 1 行ステップ（JDK のクラスには入らない）
    pub fn step(&mut self, thread: Id, depth: Depth) -> Result<i32, String> {
        self.set_request(kind::SINGLE_STEP, SUSPEND_ALL, |o| {
            o.u8(10).object(thread).i32(1).i32(depth as i32);
            o.u8(1).i32(1);
            let skip = ["java.*", "javax.*", "jdk.*", "sun.*", "com.sun.*"];
            for s in skip {
                o.u8(6).string(s);
            }
            2 + skip.len() as i32
        })
    }

    pub fn thread_name(&mut self, thread: Id) -> Result<String, String> {
        let mut r = self.request(11, 1, |o| {
            o.object(thread);
        })?;
        r.string()
    }

    pub fn resume_thread(&mut self, thread: Id) -> Result<(), String> {
        self.request(11, 3, |o| {
            o.object(thread);
        })
        .map(|_| ())
    }

    /// スタック (フレーム, 位置)。上から
    pub fn frames(&mut self, thread: Id) -> Result<Vec<(Id, Location)>, String> {
        let mut r = self.request(11, 6, |o| {
            o.object(thread).i32(0).i32(-1);
        })?;
        let n = r.i32()?;
        (0..n).map(|_| Ok((r.frame()?, r.location()?))).collect()
    }

    /// フレームの変数の値（スロット, 型のタグ）
    pub fn frame_values(&mut self, thread: Id, frame: Id, slots: &[(i32, u8)]) -> Result<Vec<Value>, String> {
        let mut r = self.request(16, 1, |o| {
            o.object(thread).frame(frame).i32(slots.len() as i32);
            for (s, t) in slots {
                o.i32(*s).u8(*t);
            }
        })?;
        let n = r.i32()?;
        (0..n).map(|_| r.value()).collect()
    }

    pub fn this_object(&mut self, thread: Id, frame: Id) -> Result<Value, String> {
        let mut r = self.request(16, 3, |o| {
            o.object(thread).frame(frame);
        })?;
        r.value()
    }

    /// オブジェクトのクラス
    pub fn object_class(&mut self, obj: Id) -> Result<Id, String> {
        let mut r = self.request(9, 1, |o| {
            o.object(obj);
        })?;
        r.u8()?;
        r.reftype()
    }

    pub fn object_values(&mut self, obj: Id, fields: &[Id]) -> Result<Vec<Value>, String> {
        let mut r = self.request(9, 2, |o| {
            o.object(obj).i32(fields.len() as i32);
            for f in fields {
                o.field(*f);
            }
        })?;
        let n = r.i32()?;
        (0..n).map(|_| r.value()).collect()
    }

    pub fn string_value(&mut self, s: Id) -> Result<String, String> {
        let mut r = self.request(10, 1, |o| {
            o.object(s);
        })?;
        r.string()
    }

    pub fn array_length(&mut self, a: Id) -> Result<i32, String> {
        let mut r = self.request(13, 1, |o| {
            o.object(a);
        })?;
        r.i32()
    }

    /// 配列の値（始め, 数）
    pub fn array_values(&mut self, a: Id, first: i32, n: i32) -> Result<Vec<Value>, String> {
        let mut r = self.request(13, 2, |o| {
            o.object(a).i32(first).i32(n);
        })?;
        let tag = r.u8()?;
        let count = r.i32()?;
        // 基本型の配列はタグが 1 つだけ、オブジェクトの配列は 1 つずつタグ付き
        let prim = matches!(tag, b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D');
        (0..count).map(|_| if prim { r.value_of(tag) } else { r.value() }).collect()
    }
}
