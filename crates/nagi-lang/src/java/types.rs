//! Java のライブラリの型（jar・JDK の索引）のメンバーを `.` の後の補完に出す。
//!
//! 受け手の連なり（`names.stream().filter(..).`）を、変数の宣言の型（`List<String>` の型引数も）から始めて、
//! メソッドの戻り値・フィールドの型を class ファイルのジェネリクスの情報（Signature）でたどる。
//! 型の名前は import（1 つの型・`pkg.*`）・同じパッケージ・java.lang の順で探す。
//! 型変数は、受け手の型引数で決まるものだけ置き換える（`List<String>.get()` → String）。
//! ラムダから決まるもの（`map(x -> ..)` の R）は分からないので、そこで止める。

use std::collections::HashMap;

use nagi_syntax::defs::Receiver;

use crate::text::{self, Lexis};
use crate::project::Ranker;
use crate::eco::jvm::{ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, Class, JarIndex};

const ACC_ABSTRACT: u16 = 0x0400;

/// Java の字句
pub const LX: &Lexis = &text::C_LIKE;

/// 型（ジェネリクスの情報から）
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JType {
    /// 内部名（`java/util/List`）と型引数
    Class(String, Vec<JType>),
    /// 型変数（`E`）
    Var(String),
    /// 基本型（記述子の文字 `I`・`Z` など）
    Prim(u8),
    /// 配列
    Array(Box<JType>),
    /// `?` など（メンバーは引かない）
    Other,
}

impl JType {
    /// プロジェクトの型（`?Name`）に、その名前を書いたファイルを付ける（`?Name@path`。同じ名前の型を選ぶ手がかり）
    pub fn written_in(self, path: &str) -> JType {
        match self {
            JType::Class(n, args) => {
                let n = if n.starts_with('?') && !n.contains('@') { format!("{n}@{path}") } else { n };
                JType::Class(n, args.into_iter().map(|a| a.written_in(path)).collect())
            }
            t => t,
        }
    }
}

/// 基本型の名前の記述子の文字
fn prim_code(name: &str) -> Option<u8> {
    Some(match name {
        "byte" => b'B',
        "char" => b'C',
        "double" => b'D',
        "float" => b'F',
        "int" => b'I',
        "long" => b'J',
        "short" => b'S',
        "boolean" => b'Z',
        _ => return None,
    })
}

/// 基本型の包む型（`I` → `java/lang/Integer`）
fn boxed(c: u8) -> &'static str {
    match c {
        b'B' => "java/lang/Byte",
        b'C' => "java/lang/Character",
        b'D' => "java/lang/Double",
        b'F' => "java/lang/Float",
        b'I' => "java/lang/Integer",
        b'J' => "java/lang/Long",
        b'S' => "java/lang/Short",
        b'Z' => "java/lang/Boolean",
        _ => "java/lang/Void",
    }
}

/// `?Name@path` → (Name, path)
pub fn project_name(n: &str) -> Option<(&str, Option<&str>)> {
    let n = n.strip_prefix('?')?;
    Some(match n.split_once('@') {
        Some((a, b)) => (a, Some(b)),
        None => (n, None),
    })
}

/// Signature・記述子を読む
struct Sig<'a> {
    s: &'a [u8],
    i: usize,
}

impl Sig<'_> {
    fn peek(&self) -> u8 {
        self.s.get(self.i).copied().unwrap_or(0)
    }

    fn eat(&mut self, c: u8) -> bool {
        let ok = self.peek() == c;
        if ok {
            self.i += 1;
        }
        ok
    }

    /// `<T:Ljava/lang/Object;U::Ljava/lang/Comparable<TU;>;>` → [T, U]
    fn type_params(&mut self) -> Vec<String> {
        self.type_params_bounds().into_iter().map(|x| x.0).collect()
    }

    /// 型変数とその最初の境界（`SELF extends AbstractAssert<SELF, A>` の AbstractAssert）
    fn type_params_bounds(&mut self) -> Vec<(String, Option<JType>)> {
        let mut out = Vec::new();
        if !self.eat(b'<') {
            return out;
        }
        while self.peek() != b'>' && self.peek() != 0 {
            let start = self.i;
            while self.peek() != b':' && self.peek() != 0 {
                self.i += 1;
            }
            let name = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
            let mut bound = None;
            // クラスの境界（空のこともある）と、インターフェースの境界
            while self.eat(b':') {
                if !matches!(self.peek(), b':' | b'>') {
                    let t = self.ty();
                    bound.get_or_insert(t);
                }
            }
            out.push((name, bound));
        }
        self.eat(b'>');
        out
    }

    fn ty(&mut self) -> JType {
        match self.peek() {
            b'L' => {
                self.i += 1;
                let mut name = String::new();
                let mut args = Vec::new();
                loop {
                    let start = self.i;
                    while !matches!(self.peek(), b'<' | b';' | b'.' | 0) {
                        self.i += 1;
                    }
                    name.push_str(&String::from_utf8_lossy(&self.s[start..self.i]));
                    args.clear();
                    if self.eat(b'<') {
                        while self.peek() != b'>' && self.peek() != 0 {
                            args.push(match self.peek() {
                                b'*' => {
                                    self.i += 1;
                                    JType::Other
                                }
                                b'+' | b'-' => {
                                    self.i += 1;
                                    self.ty()
                                }
                                _ => self.ty(),
                            });
                        }
                        self.eat(b'>');
                    }
                    // 入れ子の型（`Lpkg/Outer<TT;>.Inner;`）
                    if self.eat(b'.') {
                        name.push('$');
                        continue;
                    }
                    self.eat(b';');
                    break;
                }
                JType::Class(name, args)
            }
            b'T' => {
                self.i += 1;
                let start = self.i;
                while !matches!(self.peek(), b';' | 0) {
                    self.i += 1;
                }
                let v = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                self.eat(b';');
                JType::Var(v)
            }
            b'[' => {
                self.i += 1;
                JType::Array(Box::new(self.ty()))
            }
            0 => JType::Other,
            c => {
                self.i += 1;
                if b"BCDFIJSZV".contains(&c) { JType::Prim(c) } else { JType::Other }
            }
        }
    }
}

/// クラスの型引数の名前と、親・インターフェースの型（型引数つき）
/// Signature・記述子を外から読む（骨組みを作るのに）
pub struct SigReader<'a>(Sig<'a>);

impl<'a> SigReader<'a> {
    pub fn new(s: &'a str) -> Self {
        SigReader(Sig { s: s.as_bytes(), i: 0 })
    }

    pub fn type_params_bounds(&mut self) -> Vec<(String, Option<JType>)> {
        self.0.type_params_bounds()
    }

    /// 次の型（終わりなら None）
    pub fn next_type(&mut self) -> Option<JType> {
        (self.0.peek() != 0).then(|| self.0.ty())
    }

    /// メソッドの (戻り値, 引数)（型変数の並びは読んだ後で）
    pub fn method(&mut self) -> (JType, Vec<JType>) {
        let mut params = Vec::new();
        if self.0.eat(b'(') {
            while self.0.peek() != b')' && self.0.peek() != 0 {
                params.push(self.0.ty());
            }
            self.0.eat(b')');
        }
        (self.0.ty(), params)
    }
}

fn class_sig(c: &Class) -> (Vec<String>, Vec<JType>) {
    match &c.signature {
        Some(s) => {
            let mut p = Sig { s: s.as_bytes(), i: 0 };
            let params = p.type_params();
            let mut supers = Vec::new();
            while p.peek() != 0 {
                supers.push(p.ty());
            }
            (params, supers)
        }
        None => (Vec::new(), c.super_name.iter().chain(&c.interfaces).map(|n| JType::Class(n.clone(), Vec::new())).collect()),
    }
}

/// メソッドの戻り値の型・引数の数（Signature があればそれ、無ければ記述子）
fn method_sig(desc: &str, signature: Option<&str>) -> (JType, usize) {
    let (ret, params) = method_types(desc, signature);
    (ret, params.len())
}

/// メソッドの戻り値の型と引数の型
fn method_types(desc: &str, signature: Option<&str>) -> (JType, Vec<JType>) {
    let s = signature.unwrap_or(desc);
    let mut p = Sig { s: s.as_bytes(), i: 0 };
    p.type_params();
    let mut params = Vec::new();
    if p.eat(b'(') {
        while p.peek() != b')' && p.peek() != 0 {
            params.push(p.ty());
        }
        p.eat(b')');
    }
    (p.ty(), params)
}

/// 型変数を置き換える
fn subst(t: &JType, env: &HashMap<String, JType>) -> JType {
    match t {
        JType::Var(v) => env.get(v).cloned().unwrap_or_else(|| t.clone()),
        JType::Class(n, args) => JType::Class(n.clone(), args.iter().map(|a| subst(a, env)).collect()),
        JType::Array(e) => JType::Array(Box::new(subst(e, env))),
        t => t.clone(),
    }
}

/// メンバーの候補 1 つ
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibMember {
    pub name: String,
    pub method: bool,
    /// 引数のある形があるか（メソッド）
    pub params: bool,
    /// 持っている型の単純名
    pub owner: String,
}

/// ファイルの import と、ライブラリの索引で型を引く
pub struct LibTypes<'a> {
    jars: &'a JarIndex,
    /// 単純名 → 内部名（1 つの型の import）
    single: HashMap<String, String>,
    /// `pkg.*` のパッケージ（内部名の形 `java/util`）
    wild: Vec<String>,
    package: String,
    /// static の import（メソッド名、`*` なら何でも → クラスの `a.b.C` の書き方）
    statics: Vec<(String, String)>,
}

/// プロジェクトの型（ライブラリの索引に無い型。`?名前` で表す）のメンバーの型を、プロジェクトの索引で引く（型, メンバー, 呼び出しか → 型の名前）
/// プロジェクトの型（`?` の後の名前。`Name@path` もある。空なら今の型）のメンバーの型
pub type ProjectMember<'b> = dyn FnMut(&str, &str, bool) -> Option<JType> + 'b;

impl<'a> LibTypes<'a> {
    pub fn new(jars: &'a JarIndex, src: &str) -> Self {
        Self::from_lines(jars, src.lines())
    }

    /// ファイルの行から（package と import を読む）
    pub fn from_lines<'l>(jars: &'a JarIndex, lines: impl Iterator<Item = &'l str>) -> Self {
        let mut single = HashMap::new();
        let mut wild = Vec::new();
        let mut package = String::new();
        let mut statics = Vec::new();
        for line in lines {
            let l = line.trim();
            if let Some(p) = l.strip_prefix("package ") {
                package = p.trim_end_matches(';').trim().replace('.', "/");
            } else if let Some(i) = l.strip_prefix("import ") {
                if let Some(st) = i.strip_prefix("static ") {
                    if let Some((class, member)) = st.trim_end_matches(';').trim().rsplit_once('.') {
                        statics.push((member.to_string(), class.to_string()));
                    }
                    continue;
                }
                let i = i.trim_end_matches(';').trim();
                match i.strip_suffix(".*") {
                    Some(p) => wild.push(p.replace('.', "/")),
                    None => {
                        if let Some((_, simple)) = i.rsplit_once('.') {
                            single.insert(simple.to_string(), i.to_string());
                        }
                    }
                }
            } else if l.starts_with("public ") || l.starts_with("class ") || l.starts_with("final ") || l.starts_with("abstract ") || l.starts_with("interface ") {
                break;
            }
        }
        LibTypes { jars, single, wild, package, statics }
    }

    /// 単純名を完全な名前（`java.awt.Color`）の型にする（import と同じ扱い）
    pub fn alias(&mut self, simple: &str, fqn: &str) {
        self.single.insert(simple.to_string(), fqn.to_string());
    }

    fn exists(&self, internal: &str) -> bool {
        self.jars.class(internal).is_some()
    }

    /// `a.b.Outer.Inner`（import の書き方）を内部名に（後ろの `.` から `$` に変えて、あるものを）
    fn dotted(&self, name: &str) -> Option<String> {
        let mut s = name.replace('.', "/");
        loop {
            if self.exists(&s) {
                return Some(s);
            }
            let i = s.rfind('/')?;
            s.replace_range(i..=i, "$");
        }
    }

    /// 単純名（`List`・`Map.Entry`）の型の内部名
    pub fn resolve(&self, name: &str) -> Option<String> {
        if let Some((outer, rest)) = name.split_once('.') {
            if outer.starts_with(char::is_uppercase) {
                let o = self.resolve(outer)?;
                let n = format!("{o}${}", rest.replace('.', "$"));
                return self.exists(&n).then_some(n);
            }
            return self.dotted(name);
        }
        if let Some(full) = self.single.get(name) {
            return self.dotted(full);
        }
        let mut pkgs: Vec<&str> = Vec::new();
        if !self.package.is_empty() {
            pkgs.push(&self.package);
        }
        pkgs.push("java/lang");
        pkgs.extend(self.wild.iter().map(String::as_str));
        pkgs.into_iter().map(|p| format!("{p}/{name}")).find(|n| self.exists(n))
    }

    /// 型の文字（`List<String>`・`Map<String, List<Integer>>`・`String[]`）を読む
    pub fn parse(&self, text: &str) -> Option<JType> {
        let text = text.trim();
        if let Some(e) = text.strip_suffix("[]") {
            return Some(JType::Array(Box::new(self.parse(e)?)));
        }
        if text.ends_with(']') {
            return Some(JType::Other);
        }
        if let Some(c) = prim_code(text) {
            return Some(JType::Prim(c));
        }
        let (head, args) = match text.find('<') {
            Some(i) => (&text[..i], Some(text[i + 1..].strip_suffix('>')?)),
            None => (text, None),
        };
        let head = head.trim();
        if head.is_empty() || !head.starts_with(|c: char| c.is_alphabetic()) {
            return None;
        }
        // ライブラリに無い型はプロジェクトの型として名前で持つ
        let internal = self.resolve(head).unwrap_or_else(|| format!("?{}", head.rsplit('.').next().unwrap_or(head)));
        let mut out = Vec::new();
        if let Some(a) = args {
            // 入れ子の < > の外の , で分ける
            let (mut depth, mut start) = (0, 0);
            for (i, c) in a.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    ',' if depth == 0 => {
                        out.push(self.arg(&a[start..i]));
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            if !a.trim().is_empty() {
                out.push(self.arg(&a[start..]));
            }
        }
        Some(JType::Class(internal, out))
    }

    fn arg(&self, a: &str) -> JType {
        let a = a.trim();
        let a = a.strip_prefix("? extends ").or_else(|| a.strip_prefix("? super ")).unwrap_or(a);
        self.parse(a).unwrap_or(JType::Other)
    }

    /// 型と、その親・インターフェース（型引数を置き換えて。近い順。最後に Object）
    fn hierarchy(&self, t: &JType) -> Vec<(Class, HashMap<String, JType>)> {
        let mut out = Vec::new();
        let mut todo = std::collections::VecDeque::from([t.clone()]);
        let mut seen = std::collections::HashSet::new();
        while let Some(t) = todo.pop_front() {
            let JType::Class(name, args) = t else { continue };
            if !seen.insert(name.clone()) || out.len() > 40 {
                continue;
            }
            let Some(c) = self.jars.class(&name) else { continue };
            let (_, supers) = class_sig(&c);
            // 型引数が ? か無い（生の型）なら、型変数の境界のクラス（型引数は外す）。境界が無ければ型変数のまま
            let bounds = c.signature.as_deref().map(|s| Sig { s: s.as_bytes(), i: 0 }.type_params_bounds()).unwrap_or_default();
            let mut env: HashMap<String, JType> = HashMap::new();
            for (k, (v, bound)) in bounds.into_iter().enumerate() {
                match (args.get(k), bound) {
                    (Some(a), _) if *a != JType::Other => {
                        env.insert(v, a.clone());
                    }
                    (_, Some(JType::Class(b, _))) if b != "java/lang/Object" => {
                        env.insert(v, JType::Class(b, Vec::new()));
                    }
                    _ => {}
                }
            }
            for s in &supers {
                todo.push_back(subst(s, &env));
            }
            out.push((c, env));
        }
        if matches!(t, JType::Class(..) | JType::Array(_)) && !seen.contains("java/lang/Object") {
            if let Some(c) = self.jars.class("java/lang/Object") {
                out.push((c, HashMap::new()));
            }
        }
        out
    }

    /// 型のメンバー（public。`protected` なら protected も。`statics` なら static だけ、でなければ static でないもの）。名前ごとに 1 つ、近い型から
    pub fn members(&self, t: &JType, statics: bool, protected: bool) -> Vec<LibMember> {
        let open = if protected { ACC_PUBLIC | ACC_PROTECTED } else { ACC_PUBLIC };
        let mut out: Vec<LibMember> = Vec::new();
        let mut at: HashMap<String, usize> = HashMap::new();
        if matches!(t, JType::Array(_)) && !statics {
            at.insert("length".into(), 0);
            out.push(LibMember { name: "length".into(), method: false, params: false, owner: String::new() });
        }
        for (c, _) in self.hierarchy(t) {
            let owner = c.name.rsplit(['/', '$']).next().unwrap_or(&c.name).to_string();
            let fields = c.fields.iter().map(|m| (m, false));
            let methods = c.methods.iter().map(|m| (m, true));
            for (m, method) in fields.chain(methods) {
                if m.access & open == 0 || (m.access & ACC_STATIC != 0) != statics || m.name.starts_with('<') {
                    continue;
                }
                let params = method && !m.desc.starts_with("()");
                match at.get(&m.name) {
                    Some(&i) => out[i].params |= params,
                    None => {
                        at.insert(m.name.clone(), out.len());
                        out.push(LibMember { name: m.name.clone(), method, params, owner: owner.clone() });
                    }
                }
            }
        }
        out
    }

    /// メソッド（`call`）・フィールドの型（受け手の型引数で置き換えて。分からない型変数なら None）
    pub fn member_type(&self, t: &JType, name: &str, call: bool) -> Option<JType> {
        for (c, env) in self.hierarchy(t) {
            let list = if call { &c.methods } else { &c.fields };
            let Some(m) = list.iter().find(|m| m.name == name && m.access & ACC_PUBLIC != 0) else { continue };
            let ty = if call {
                method_sig(&m.desc, m.signature.as_deref()).0
            } else {
                let s = m.signature.as_deref().unwrap_or(&m.desc);
                Sig { s: s.as_bytes(), i: 0 }.ty()
            };
            return match subst(&ty, &env) {
                JType::Var(_) | JType::Other => None,
                t => Some(t),
            };
        }
        None
    }

    /// 受け手の連なりの型。`base` は連なりの頭の型（変数の宣言の型。型引数つき）、無ければ頭の型名を型として。
    /// 途中でプロジェクトの型（`?名前`）になったら、その先は `project` で引く。
    /// `λ名前_k_p` の呼び出しは「受け手の 名前 メソッドの k 番目の引数のラムダの p 番目の引数」（expand_head が作る）
    /// `args` は連なりの各部分の呼び出しの引数の型（chain_args の並び。分からない引数は None）。形を選ぶのに使う
    pub fn receiver(&self, r: &Receiver, base: &dyn Fn(&str) -> Option<JType>, project: &mut ProjectMember, args: &[Option<Vec<Option<JType>>>]) -> Option<JType> {
        // 連なりの中の位置（頭が 0）
        fn seg(r: &Receiver) -> usize {
            match r {
                Receiver::This => 0,
                Receiver::Type(_) => 1,
                Receiver::Member(i, _) | Receiver::Call(i, _) => seg(i) + 1,
            }
        }
        let call_args = |inner: &Receiver| args.get(seg(inner)).and_then(Option::as_deref);
        let call_type = |t: &JType, n: &str, inner: &Receiver| match call_args(inner) {
            Some(a) => self.call_type(t, n, a),
            None => self.member_type(t, n, true),
        };
        match r {
            Receiver::This => None,
            Receiver::Type(t) => base(t).or_else(|| self.parse(t)),
            // static の import のメソッド（assertThat(..) など）
            // 今の型のメソッド（プロジェクトの型・そのライブラリの親は `project("", ..)`）、無ければ Object のもの
            Receiver::Call(inner, n) if matches!(**inner, Receiver::This) => {
                let st = self.statics.iter().find(|(m, _)| m == n).or_else(|| self.statics.iter().find(|(m, _)| m == "*"));
                if let Some(t) = st.and_then(|(_, class)| self.dotted(class)).and_then(|c| call_type(&JType::Class(c, Vec::new()), n, inner)) {
                    return Some(t);
                }
                project("", n, true).or_else(|| self.member_type(&JType::Class("java/lang/Object".into(), Vec::new()), n, true))
            }
            Receiver::Member(inner, n) | Receiver::Call(inner, n) => {
                let call = matches!(r, Receiver::Call(..));
                let it = self.receiver(inner, base, project, args)?;
                if let Some(rest) = n.strip_prefix('λ') {
                    let mut parts = rest.rsplitn(3, '_');
                    let (p, k, m) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?);
                    return self.lambda_param(&it, m, k, p);
                }
                // X.class は Class<X>
                if n == "class" && !call {
                    return Some(JType::Class("java/lang/Class".into(), vec![it]));
                }
                // 入れ子の型（Map.Entry・Outer.Inner）
                if let JType::Class(name, _) = &it {
                    if !call && n.starts_with(char::is_uppercase) {
                        if let Some((_, hint)) = project_name(name) {
                            return Some(JType::Class(format!("?{n}{}", hint.map(|h| format!("@{h}")).unwrap_or_default()), Vec::new()));
                        }
                        let inner_name = format!("{name}${n}");
                        if self.exists(&inner_name) {
                            return Some(JType::Class(inner_name, Vec::new()));
                        }
                    }
                    if let Some(simple) = name.strip_prefix('?') {
                        return project(simple, n, call);
                    }
                }
                if call { call_type(&it, n, inner) } else { self.member_type(&it, n, false) }
            }
        }
    }

    /// 型 `t` のメソッド `m` の `k` 番目の引数（関数型のインターフェース）に渡すラムダの `p` 番目の引数の型
    /// 型 `t`（と親）の中で、メンバー `name` を宣言している型の内部名
    pub fn declaring(&self, t: &JType, name: &str) -> Option<String> {
        self.hierarchy(t).into_iter().find(|(c, _)| c.methods.iter().chain(&c.fields).any(|m| m.name == name)).map(|(c, _)| c.name)
    }

    /// static の import のメソッド・フィールド `name` のクラスの内部名
    pub fn static_owner(&self, name: &str) -> Option<String> {
        let (_, class) = self.statics.iter().find(|(m, _)| m == name)?;
        self.dotted(class)
    }

    /// 型 `t` と親の型の名前（`?Name` のプロジェクトの型は空）
    fn supertypes(&self, t: &JType) -> std::collections::HashSet<String> {
        match t {
            JType::Class(n, _) if !n.starts_with('?') => self.hierarchy(t).into_iter().map(|(c, _)| c.name).collect(),
            _ => Default::default(),
        }
    }

    /// 引数の型 `a` が、引数の宣言 `p` にどれだけ合うか（大きいほど合う。合わなければ負）
    fn fit(p: &JType, a: Option<&JType>, sup: Option<&std::collections::HashSet<String>>) -> i32 {
        let Some(a) = a else { return 0 };
        let numeric = |c: u8| b"BCDFIJS".contains(&c);
        match (p, a) {
            (JType::Var(_), _) => 1,
            (JType::Prim(x), JType::Prim(y)) if x == y => 4,
            (JType::Prim(x), JType::Prim(y)) if numeric(*x) && numeric(*y) => 1,
            (JType::Prim(x), JType::Class(n, _)) if boxed(*x) == n => 2,
            (JType::Class(n, _), JType::Prim(y)) if boxed(*y) == n => 3,
            (JType::Class(n, _), JType::Prim(_)) if matches!(n.as_str(), "java/lang/Object" | "java/lang/Number" | "java/lang/Comparable" | "java/io/Serializable") => 1,
            (JType::Class(pn, _), JType::Class(an, _)) if pn == an => 4,
            (JType::Class(pn, _), JType::Class(..)) if sup.is_some_and(|s| s.contains(pn)) => 3,
            (JType::Class(pn, _), _) if pn == "java/lang/Object" => 1,
            // プロジェクトの型がライブラリのインターフェースを実装しているかは分からない
            (JType::Class(..), JType::Class(an, _)) if an.starts_with('?') => -1,
            (JType::Array(pe), JType::Array(ae)) => 2 + i32::from(pe == ae),
            (JType::Other, _) => 0,
            _ => -10,
        }
    }

    /// 引数の宣言 `p` と引数の型 `a` から、メソッドの型変数を決める（`<T> T verify(T)`・`List<? extends E>`）
    fn bind(&self, p: &JType, a: &JType, env: &mut HashMap<String, JType>) {
        match (p, a) {
            (JType::Var(v), JType::Prim(c)) => {
                env.entry(v.clone()).or_insert(JType::Class(boxed(*c).into(), Vec::new()));
            }
            (JType::Var(v), JType::Class(..) | JType::Array(_)) => {
                env.entry(v.clone()).or_insert(a.clone());
            }
            (JType::Array(pe), JType::Array(ae)) => self.bind(pe, ae, env),
            (JType::Class(pn, pargs), JType::Class(an, aargs)) if !pargs.is_empty() => {
                if pn == an {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.bind(p, a, env);
                    }
                    return;
                }
                // 親の型として見たときの型引数（ArrayList<String> を List<E> として）
                if let Some((c, cenv)) = self.hierarchy(a).into_iter().find(|(c, _)| c.name == *pn) {
                    let view: Vec<JType> = class_sig(&c).0.iter().map(|v| cenv.get(v).cloned().unwrap_or(JType::Other)).collect();
                    for (p, a) in pargs.iter().zip(&view) {
                        self.bind(p, a, env);
                    }
                }
            }
            _ => {}
        }
    }

    /// 引数の型（分からなければ None）で同じ名前のメソッドの形を選び、その戻り値の型（メソッドの型変数は引数から決める）
    pub fn call_type(&self, t: &JType, name: &str, args: &[Option<JType>]) -> Option<JType> {
        let sups: Vec<Option<std::collections::HashSet<String>>> = args.iter().map(|a| a.as_ref().map(|a| self.supertypes(a))).collect();
        for (c, env) in self.hierarchy(t) {
            let mut best: Option<(i32, JType)> = None;
            for m in c.methods.iter().filter(|m| m.name == name && m.access & ACC_PUBLIC != 0) {
                let (ret, params) = method_types(&m.desc, m.signature.as_deref());
                if params.len() != args.len() {
                    continue;
                }
                let params: Vec<JType> = params.iter().map(|p| subst(p, &env)).collect();
                let score: i32 = params.iter().zip(args).zip(&sups).map(|((p, a), s)| Self::fit(p, a.as_ref(), s.as_ref())).sum();
                if best.as_ref().is_none_or(|b| score > b.0) {
                    let mut menv = HashMap::new();
                    for (p, a) in params.iter().zip(args) {
                        if let Some(a) = a {
                            self.bind(p, a, &mut menv);
                        }
                    }
                    best = Some((score, subst(&subst(&ret, &env), &menv)));
                }
            }
            if let Some((_, r)) = best {
                return match r {
                    JType::Var(_) | JType::Other => None,
                    t => Some(t),
                };
            }
        }
        // 数の合う形が無い（可変長の引数など）
        self.member_type(t, name, true)
    }

    fn lambda_param(&self, t: &JType, m: &str, k: usize, p: usize) -> Option<JType> {
        for (c, env) in self.hierarchy(t) {
            for method in c.methods.iter().filter(|x| x.name == m && x.access & ACC_PUBLIC != 0) {
                let (_, params) = method_types(&method.desc, method.signature.as_deref());
                let Some(JType::Class(iface, args)) = params.get(k).map(|x| subst(x, &env)) else { continue };
                // 関数型のインターフェースの抽象メソッド（default・static でないもの）
                for (ic, ienv) in self.hierarchy(&JType::Class(iface, args)) {
                    let Some(sam) = ic.methods.iter().find(|x| x.access & ACC_ABSTRACT != 0 && x.access & ACC_STATIC == 0) else { continue };
                    let (_, sp) = method_types(&sam.desc, sam.signature.as_deref());
                    if let Some(t @ JType::Class(..)) = sp.get(p).map(|x| subst(x, &ienv)) {
                        return Some(t);
                    }
                    break;
                }
            }
        }
        None
    }
}

pub use super::rules::source_package;

/// Java の `.` の前の式（行の中の `before`。`line_start` は行の始まり、`at` は `.` の後の語の位置）を、前の行からの連なりと
/// 型を書かない変数（var・for の var・ラムダの引数）を展開し、頭のパッケージの名前を外した形に（`java.awt.Color` → `Color` と
/// (単純名, 完全な名前)）。`known` はパッケージ（内部名）か。パッケージの名前だけなら Err(その内部名 `java/util`)
pub fn receiver_text(src: &str, before: &str, line_start: usize, at: usize, known: &dyn Fn(&str) -> bool) -> Result<(String, Option<(String, String)>), String> {
    let before = text::with_previous_lines(LX, src, line_start, before);
    let mut expanded = expand_head(src, &before, at);
    let Some(chain) = text::dotted_chain(LX, &expanded) else { return Ok((expanded, None)) };
    let off = expanded.len() - chain.len();
    let segs: Vec<String> = chain.split('.').map(String::from).collect();
    if !(segs[0].starts_with(char::is_lowercase) && known(&segs[0]) && crate::project::var_type_with(super::lang::STYLES, src, &segs[0], at).is_none()) {
        return Ok((expanded, None));
    }
    // パッケージとして知っている長さ（大文字で始まるパッケージもある）
    let k = (1..=segs.len()).take_while(|&k| known(&segs[..k].join("/"))).last().unwrap_or(0);
    if k == segs.len() {
        return Err(segs.join("/"));
    }
    let fqn = (segs[k].clone(), format!("{}.{}", segs[..k].join("."), segs[k]));
    let cut: usize = segs[..k].iter().map(|s| s.len() + 1).sum();
    expanded.replace_range(off..off + cut, "");
    Ok((expanded, Some(fqn)))
}

/// Java の受け手の式 `expanded`（receiver_text の結果）の、ライブラリの型（jar・JDK の索引で、型引数・引数の型もたどって）。
/// 途中のプロジェクトの型は `ranker` で引き、結果がプロジェクトの型なら `?Name`。`inside` は今の型
#[allow(clippy::too_many_arguments)]
pub fn lib_receiver<'a>(jars: &'a JarIndex, ranker: &mut Ranker, src: &str, rel: &str, at: usize, inside: Option<&str>, expanded: &str, fqn: Option<&(String, String)>) -> (LibTypes<'a>, Option<JType>) {
    let mut lib = LibTypes::new(jars, src);
    if let Some((simple, full)) = fqn {
        lib.alias(simple, full);
    }
    lib_receiver_in(lib, ranker, src, rel, at, inside, expanded, &|n| var_decl_text(src, n, at), &|a| expand_head(src, a, at))
}

/// `lib_receiver` を、型の名前の表 `lib`・変数の宣言の型の文字 `var_decl`・頭の展開 `expand` を渡して（Kotlin も使う）
#[allow(clippy::too_many_arguments)]
pub fn lib_receiver_in<'a>(
    lib: LibTypes<'a>,
    ranker: &mut Ranker,
    src: &str,
    rel: &str,
    _at: usize,
    inside: Option<&str>,
    expanded: &str,
    var_decl: &dyn Fn(&str) -> Option<String>,
    expand: &dyn Fn(&str) -> String,
) -> (LibTypes<'a>, Option<JType>) {
    let jars = lib.jars;
    let at = _at;
    // 変数は宣言の型（型引数つき）に印 `=` を付けて、クラス名（static の呼び出し）と分ける
    let decl = |n: &str| match n.strip_prefix("new ") {
        Some(t) => Some(format!("={t}")),
        None => var_decl(n).map(|d| format!("={d}")),
    };
    let r = text::receiver_before(LX, expanded, expanded.len(), &decl);
    let base = |t: &str| t.strip_prefix('=').and_then(|d| lib.parse(d));
    // 途中のプロジェクトの型のメンバーの型は、プロジェクトの索引で（型引数は宣言のファイルの import で読む）
    let mut project = |t: &str, n: &str, call: bool| {
        // 型が空なら今の型（型の中から呼ぶメソッド）
        let (here, hint) = match t {
            "" => (inside?, Some(rel)),
            t => t.split_once('@').map_or((t, None), |(a, b)| (a, Some(b))),
        };
        if let Some((text, path, lines)) = ranker.member_decl_text(rel, src, here, hint, n) {
            return LibTypes::from_lines(jars, lines.iter().map(String::as_str)).parse(&text).map(|t| t.written_in(&path));
        }
        // ライブラリの親（型引数つき）のメンバー
        let from_super = ranker.outside_supers(here).into_iter().find_map(|(sup, lines)| {
            let l = LibTypes::from_lines(jars, lines.iter().map(String::as_str));
            let st = l.parse(&sup).filter(|x| matches!(x, JType::Class(n, _) if !n.starts_with('?')))?;
            l.member_type(&st, n, call)
        });
        if from_super.is_some() || t.is_empty() {
            return from_super;
        }
        let inner = Box::new(Receiver::Type(here.to_string()));
        let r = if call { Receiver::Call(inner, n.to_string()) } else { Receiver::Member(inner, n.to_string()) };
        // 型の名前（小文字はプロジェクトに無いメソッドの名前なので除く）
        ranker.receiver_type_name(rel, src, at, &r).filter(|t| t.starts_with(char::is_uppercase)).and_then(|t| lib.parse(&t))
    };
    // 呼び出しの引数の型（形を選ぶ・型変数を決める）
    let mut args = Vec::new();
    for part in text::chain_args(LX, expanded) {
        args.push(part.map(|list| {
            list.iter()
                .map(|a| {
                    literal_type(a).unwrap_or_else(|| {
                        let a = expand(a);
                        let r = text::receiver_before(LX, &a, a.len(), &decl).filter(|_| text::chain_start(LX, &a) == Some(0))?;
                        lib.receiver(&r, &base, &mut project, &[])
                    })
                })
                .collect::<Vec<_>>()
        }));
    }
    let t = r.as_ref().and_then(|r| lib.receiver(r, &base, &mut project, &args));
    (lib, t)
}

/// 字句だけで分かる引数の型（文字列・数・文字・真偽の値）。Some(None) は null などの分からないもの
pub fn literal_type(a: &str) -> Option<Option<JType>> {
    let a = a.trim();
    let c = a.chars().next()?;
    Some(Some(match c {
        '"' => JType::Class("java/lang/String".into(), Vec::new()),
        '\'' => JType::Prim(b'C'),
        '0'..='9' | '-' => JType::Prim(if a.ends_with(['L', 'l']) {
            b'J'
        } else if a.contains('.') || a.ends_with(['d', 'D']) {
            b'D'
        } else {
            b'I'
        }),
        _ if a == "true" || a == "false" => JType::Prim(b'Z'),
        _ if a == "null" => return Some(None),
        _ => return None,
    }))
}

/// 改行と字下げをまたいだ連なり（`foo\n    .bar()`）を 1 行に
fn squash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.split('\n').peekable();
    while let Some(l) = it.next() {
        let l = l.trim_end();
        out.push_str(if out.is_empty() { l } else { l.trim_start() });
        let _ = it.peek();
    }
    out
}

/// `.` の前の式 `before` の頭の変数が型を書かずに宣言されていれば、頭を式に置き換える（`at` は src の中の位置。5 回まで）:
/// `var x = 式;` → 式、`for (var x : 式)` → `式.iterator().next()`、ラムダの引数 `x ->` → `受け手.λメソッド_k_p()`
pub fn expand_head(src: &str, before: &str, at: usize) -> String {
    let mut cur = before.to_string();
    for _ in 0..5 {
        let Some(start) = text::chain_start(LX, &cur) else { break };
        let head: String = cur[start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
        let after = &cur[start + head.len()..];
        if head.is_empty() || head == "new" || after.starts_with('(') || head.starts_with(char::is_uppercase) || var_decl_text(src, &head, at).is_some() {
            break;
        }
        let Some(expr) = untyped_init(src, &head, at) else { break };
        cur = format!("{}{expr}{after}", &cur[..start]);
    }
    cur
}

/// 型を書かない宣言の、頭に置き換える式
/// `c ? a : b` なら null でない方の枝（括弧・型引数の中の ? : は見ない）。三項でなければそのまま
fn ternary_branch(e: &str) -> &str {
    let (mut depth, mut q) = (0i32, None);
    for (i, c) in e.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            '"' => return e,
            '?' if depth == 0 && q.is_none() => q = Some(i),
            ':' if depth == 0 && q.is_some() => {
                let a = e[q.unwrap() + 1..i].trim();
                let b = e[i + 1..].trim();
                return if a == "null" { b } else { a };
            }
            _ => {}
        }
    }
    e
}

fn untyped_init(src: &str, name: &str, at: usize) -> Option<String> {
    // 名前を入れずに一度だけ作り、合ったものの名前を比べる（呼ぶたびに作ると遅い）
    static PATS: std::sync::OnceLock<[regex::Regex; 3]> = std::sync::OnceLock::new();
    let [var_re, for_re, lambda_re] = PATS.get_or_init(|| {
        [
            regex::Regex::new(r"\bvar\s+([\w$]+)\s*=\s*([^;]+);").unwrap(),
            regex::Regex::new(r"\bfor\s*\(\s*(?:final\s+)?var\s+([\w$]+)\s*:").unwrap(),
            regex::Regex::new(r"(?:\(([\w\s,]*)\)|\b([\w$]+))\s*->").unwrap(),
        ]
    });
    let upto = &src[..at.min(src.len())];
    if !upto.contains(name) {
        return None;
    }
    let mut best: Option<(usize, String)> = None;
    let mut consider = |pos: usize, e: String| {
        if best.as_ref().is_none_or(|b| pos > b.0) {
            best = Some((pos, e));
        }
    };
    // var x = 式;
    for c in var_re.captures_iter(upto).filter(|c| &c[1] == name) {
        let e = squash(c[2].trim());
        let e = ternary_branch(&e).to_string();
        if text::chain_start(LX, &e) == Some(0) {
            consider(c.get(0)?.start(), e);
        }
    }
    // for (var x : 式)
    for m in for_re.captures_iter(upto).filter(|c| &c[1] == name).filter_map(|c| c.get(0)) {
        // 式は for の ( に対応する ) まで
        let rest = &upto[m.end()..];
        let mut depth = 1;
        let close = rest.char_indices().find(|&(_, c)| {
            depth += match c {
                '(' => 1,
                ')' => -1,
                _ => 0,
            };
            depth == 0
        })?;
        let e = squash(rest[..close.0].trim());
        if text::chain_start(LX, &e) == Some(0) {
            consider(m.start(), format!("{e}.iterator().next()"));
        }
    }
    // ラムダの引数: x -> / (x, y) ->
    for c in lambda_re.captures_iter(upto) {
        let whole = c.get(0)?;
        let p = match (c.get(1), c.get(2)) {
            (Some(list), _) => match list.as_str().split(',').position(|x| x.trim() == name) {
                Some(p) => p,
                None => continue,
            },
            (None, Some(n)) if n.as_str() == name => 0,
            _ => continue,
        };
        // ラムダの前は ( か , で、その呼び出しの ( までの , の数が何番目の引数か
        let pre = upto[..whole.start()].trim_end();
        if !pre.ends_with(['(', ',']) {
            continue;
        }
        let (mut depth, mut k, mut open) = (0i32, 0usize, None);
        for (i, ch) in pre.char_indices().rev() {
            match ch {
                ')' | ']' | '}' => depth += 1,
                '(' | '[' | '{' if depth > 0 => depth -= 1,
                '(' => {
                    open = Some(i);
                    break;
                }
                '[' | '{' | ';' => break,
                ',' if depth == 0 => k += 1,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        let call = squash(&pre[..open]);
        let Some(cs) = text::chain_start(LX, &call) else { continue };
        let chain = &call[cs..];
        let Some((recv, m)) = chain.rsplit_once('.') else { continue };
        if recv.is_empty() || !m.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        consider(whole.start(), format!("{recv}.λ{m}_{k}_{p}()"));
    }
    best.map(|b| b.1)
}

/// `Type name` の形の宣言（Java・C#）の (位置, 型の文字)。型は名前の前から後ろ向きに読む（`<...>` は括弧を数えて）
pub fn typed_decls(src: &str, name: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    for (at, _) in src.match_indices(name) {
        // 語の切れ目で、後ろが = ; , ) : か行・ファイルの終わり
        if src[..at].chars().next_back().is_some_and(word) {
            continue;
        }
        let after = src[at + name.len()..].trim_start_matches([' ', '\t']);
        if !(after.is_empty() || after.starts_with(['=', ';', ',', ')', ':', '\n', '\r'])) || after.starts_with("==") {
            continue;
        }
        let before = &src[..at];
        let t = before.trim_end();
        // 名前の前に空白が要る（`a.name` などは除く）。型は同じ行に（前の行の終わりの語を型にしない）
        if t.len() == before.len() || before[t.len()..].contains('\n') {
            continue;
        }
        let mut end = t.len();
        while t[..end].ends_with("[]") {
            end -= 2;
        }
        let mut i = end;
        if t[..i].ends_with('>') {
            let mut depth = 0;
            let mut open = None;
            for (k, c) in t[..i].char_indices().rev() {
                match c {
                    '>' => depth += 1,
                    '<' => {
                        depth -= 1;
                        if depth == 0 {
                            open = Some(k);
                            break;
                        }
                    }
                    ';' | '{' | '}' | '(' | ')' | '=' => break,
                    _ => {}
                }
            }
            let Some(o) = open else { continue };
            i = o;
        }
        let head = t[..i].char_indices().rev().take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '.').last().map_or(i, |x| x.0);
        let ty = &t[head..];
        // 行のコメントの中は除く
        let line_start = t[..head].rfind('\n').map_or(0, |k| k + 1);
        if ty.starts_with(|c: char| c.is_ascii_uppercase()) && !matches!(&t[head..i], "return" | "new") && !t[line_start..head].contains("//") && !t[line_start..head].trim_start().starts_with('*') {
            out.push((head, ty.to_string()));
        }
    }
    out
}

/// `src` の中の変数 `name` の宣言の型の文字（型引数つき。`at` より前の最後の宣言、無ければ後ろの最初）
pub fn var_decl_text(src: &str, name: &str, at: usize) -> Option<String> {
    let mut all = typed_decls(src, name);
    // var x = new ArrayList<String>(...)
    static NEW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let new = NEW.get_or_init(|| regex::Regex::new(r"\bvar\s+([\w$]+)\s*=\s*new\s+([A-Z][\w.]*(?:<[^;(){}=]*>)?)\s*\(").unwrap());
    all.extend(new.captures_iter(src).filter(|c| &c[1] == name).map(|c| (c.get(0).map_or(0, |m| m.start()), c[2].to_string())));
    pick(all, at)
}

/// `at` より前の最後、無ければ後ろの最初
pub fn pick(all: Vec<(usize, String)>, at: usize) -> Option<String> {
    let before = all.iter().filter(|x| x.0 < at).max_by_key(|x| x.0);
    let after = all.iter().filter(|x| x.0 >= at).min_by_key(|x| x.0);
    before.or(after).map(|x| x.1.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        // class ArrayList<E> extends AbstractList<E> implements List<E>
        let mut p = Sig { s: b"<E:Ljava/lang/Object;>Ljava/util/AbstractList<TE;>;Ljava/util/List<TE;>;", i: 0 };
        assert_eq!(p.type_params(), ["E"]);
        assert_eq!(p.ty(), JType::Class("java/util/AbstractList".into(), vec![JType::Var("E".into())]));
        // <R:Ljava/lang/Object;>(Ljava/util/function/Function<-TT;+TR;>;)Ljava/util/stream/Stream<TR;>;
        let (ret, n) = method_sig("", Some("<R:Ljava/lang/Object;>(Ljava/util/function/Function<-TT;+TR;>;)Ljava/util/stream/Stream<TR;>;"));
        assert_eq!(n, 1);
        assert_eq!(ret, JType::Class("java/util/stream/Stream".into(), vec![JType::Var("R".into())]));
        assert_eq!(method_sig("(I[JLjava/lang/String;)Ljava/util/List;", None), (JType::Class("java/util/List".into(), vec![]), 3));
        // <T::Ljava/lang/Comparable<-TT;>;>
        let mut p = Sig { s: b"<T::Ljava/lang/Comparable<-TT;>;U:Ljava/lang/Object;>", i: 0 };
        assert_eq!(p.type_params(), ["T", "U"]);
        // 入れ子の型
        let mut p = Sig { s: b"Ljava/util/Map<TK;TV;>.Entry<TK;TV;>;", i: 0 };
        assert_eq!(p.ty(), JType::Class("java/util/Map$Entry".into(), vec![JType::Var("K".into()), JType::Var("V".into())]));
        let env: HashMap<String, JType> = [("E".to_string(), JType::Class("java/lang/String".into(), vec![]))].into();
        assert_eq!(subst(&JType::Var("E".into()), &env), JType::Class("java/lang/String".into(), vec![]));
    }

    #[test]
    fn heads() {
        let src = "class A {\n  void f(List<String> names) {\n    var s = names.stream()\n        .filter(x -> true);\n    for (var n : names) {}\n    names.forEach(v -> v);\n    m.put(k, (a, b) -> a);\n  }\n}\n";
        let end = src.len() - 8;
        assert_eq!(expand_head(src, "s", end), "names.stream().filter(x -> true)");
        assert_eq!(expand_head(src, "n.trim()", end), "names.iterator().next().trim()");
        assert_eq!(expand_head(src, "v", end), "names.λforEach_0_0()");
        assert_eq!(expand_head(src, "b", end), "m.λput_1_1()");
        assert_eq!(expand_head(src, "names", end), "names", "型を書いた宣言はそのまま");
        assert_eq!(expand_head("var xs = r == null ? null\n    : r.items();\n", "xs", 43), "r.items()", "三項は null でない方");
    }

    #[test]
    fn chains() {
        assert_eq!(source_package("app/src/main/java/com/x/Foo.java"), Some("com/x".into()));
        assert_eq!(source_package("java/Foo.java"), Some("".into()));
        assert_eq!(source_package("web/Foo.java"), None);
    }

    #[test]
    fn decl_text() {
        let src = "class A {\n  private Map<String, List<Integer>> byName;\n  void f(List<String> names, int n) {\n    Map.Entry<String, Integer> e = null;\n  }\n}";
        assert_eq!(var_decl_text(src, "byName", 200).as_deref(), Some("Map<String, List<Integer>>"));
        assert_eq!(var_decl_text(src, "names", 200).as_deref(), Some("List<String>"));
        assert_eq!(var_decl_text(src, "e", 200).as_deref(), Some("Map.Entry<String, Integer>"));
        assert_eq!(var_decl_text(src, "n", 200), None);
        let src = "  Result r;\n  // see Foo\n  r = make();\n  r.x();";
        assert_eq!(typed_decls(src, "r"), vec![(2, "Result".to_string())], "代入は宣言にしない");
        assert_eq!(var_decl_text("var xs = new ArrayList<String>();", "xs", 0).as_deref(), Some("ArrayList<String>"));
    }
}
