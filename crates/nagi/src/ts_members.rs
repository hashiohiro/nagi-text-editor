//! TypeScript の `.` の後の補完の型（LSP も tsc も使わない）。
//!
//! 型の宣言は、`.d.ts` の索引（nagi_workers::dts。組み込みの型と node_modules）と、プロジェクトの `.ts`（その場で読む）から。
//! 受け手の式の頭（変数・引数・フィールド・import・大域）の型を、書いた型（`x: T`）か初期値（`const x = 式`）から求め、
//! メソッドの戻り値・プロパティの型をたどる。型引数は宣言の型変数に置き換え、extends・implements・同じ名前の宣言の合併もたどる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nagi_syntax::defs::Receiver;
use nagi_workers::dts::{self, DeclKind, DtsIndex, Member};

/// 型の文字を読んだもの
#[derive(Clone, Debug, PartialEq)]
enum Ast {
    Name(String, Vec<Ast>),
    Array(Box<Ast>),
    Union(Vec<Ast>),
    Inter(Vec<Ast>),
    Func(Vec<Ast>, Box<Ast>),
    Obj(String),
    Str,
    Num,
    Bool,
    Any,
}

/// 外側の括弧の外で `sep` の位置ごとに分ける（`=>` の > は括弧に数えない）
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0i32, 0, 0);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1 + usize::from(b[i] == b'\\');
                }
            }
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i > 0 && b[i - 1] != b'=' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// 外側の `=>` の位置
fn top_arrow(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i > 0 && b[i - 1] == b'=' => {
                if depth == 0 {
                    return Some(i - 1);
                }
            }
            b'>' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// 型の文字を読む
fn parse_type(text: &str) -> Ast {
    let t = text.trim().trim_start_matches('|').trim_start_matches('&').trim();
    if t.is_empty() {
        return Ast::Any;
    }
    // 関数の型 (a: A) => R / <T>(a: T) => R
    if (t.starts_with('(') || t.starts_with('<')) && let Some(k) = top_arrow(t) {
        let params = t[..k].trim();
        let params = params.find('(').map_or("", |o| params[o + 1..].trim_end().trim_end_matches(')'));
        let ps = split_top(params, b',').into_iter().filter(|p| !p.trim().is_empty()).map(|p| parse_type(p.split_once(':').map_or("any", |x| x.1))).collect();
        return Ast::Func(ps, Box::new(parse_type(&t[k + 2..])));
    }
    let parts = split_top(t, b'|');
    if parts.len() > 1 {
        return Ast::Union(parts.into_iter().filter(|p| !p.trim().is_empty()).map(parse_type).collect());
    }
    let parts = split_top(t, b'&');
    if parts.len() > 1 {
        return Ast::Inter(parts.into_iter().map(parse_type).collect());
    }
    // 条件の型 A extends B ? C : D は C（よくある形の近似）
    if let Some(q) = t.find(" ? ").filter(|_| t.contains(" extends ") && split_top(t, b'?').len() > 1) {
        let rest = &t[q + 3..];
        let c = split_top(rest, b':');
        return parse_type(c[0]);
    }
    let t = t.strip_prefix("readonly ").unwrap_or(t).trim();
    if let Some(inner) = t.strip_suffix("[]") {
        return Ast::Array(Box::new(parse_type(inner)));
    }
    if t.starts_with('(') && t.ends_with(')') {
        return parse_type(&t[1..t.len() - 1]);
    }
    if t.starts_with('{') {
        return Ast::Obj(t.trim_start_matches('{').trim_end_matches('}').to_string());
    }
    if t.starts_with('[') {
        // 組（タプル）は要素の共用体の配列として
        let inner = t.trim_start_matches('[').trim_end_matches(']');
        let first = split_top(inner, b',').into_iter().next().unwrap_or("");
        let first = first.split_once(':').map_or(first, |x| x.1).trim_start_matches("...");
        return Ast::Array(Box::new(parse_type(first)));
    }
    if t.starts_with(['\'', '"', '`']) {
        return Ast::Str;
    }
    if t.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return Ast::Num;
    }
    match t {
        "string" => return Ast::Str,
        "number" | "bigint" => return Ast::Num,
        "boolean" | "true" | "false" => return Ast::Bool,
        "any" | "unknown" | "never" | "void" | "undefined" | "null" | "object" | "this" => return Ast::Any,
        _ => {}
    }
    if t.starts_with("keyof ") || t.starts_with("typeof ") || t.starts_with("infer ") || t.starts_with("unique ") || t.starts_with("asserts ") || t.contains(" is ") {
        return Ast::Any;
    }
    // 名前<型引数>（T[K] などは Any）
    let (head, args) = match t.find('<') {
        Some(k) if t.ends_with('>') => (&t[..k], split_top(&t[k + 1..t.len() - 1], b',').into_iter().map(parse_type).collect()),
        Some(_) => return Ast::Any,
        None => (t, Vec::new()),
    };
    if !head.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.') || head.is_empty() {
        return Ast::Any;
    }
    Ast::Name(head.to_string(), args)
}

/// プロジェクトのソース
pub struct SrcFile {
    pub path: PathBuf,
    pub text: String,
    scan: dts::SourceScan,
}

/// 型の名前を引く場所
#[derive(Clone)]
pub enum Ctx {
    Dts(u32),
    Src(Rc<SrcFile>),
    /// namespace の中（`NS.` を付けた名前を先に引く）
    Ns(Box<Ctx>, Rc<str>),
}

/// 宣言 `d` の中の名前を引く場所（namespace の中の宣言なら、その namespace を先に）
fn inner_ctx(d: &DRef) -> Ctx {
    match d.name.rfind('.') {
        Some(k) if !d.name.starts_with('\'') => Ctx::Ns(Box::new(d.ctx.clone()), d.name[..=k].into()),
        _ => d.ctx.clone(),
    }
}

/// 宣言（範囲）
#[derive(Clone)]
pub struct DRef {
    pub ctx: Ctx,
    pub kind: DeclKind,
    pub name: String,
    start: u32,
    end: u32,
}

/// 型
#[derive(Clone)]
pub enum TT {
    /// 宣言（同じ名前の合併は全部）と型引数
    Decl(Vec<DRef>, Vec<TT>),
    Var(String),
    Array(Box<TT>),
    Union(Vec<TT>),
    Func(Vec<TT>, Box<TT>),
    /// オブジェクトの型 { … }
    Obj(Rc<str>, Ctx, Rc<HashMap<String, TT>>),
    /// string・number・boolean（大域の String などのメンバー）
    Prim(&'static str),
    /// 交差（A & B。メンバーは全部の合わせ）
    Inter(Vec<TT>),
    /// クラス・enum・namespace そのもの（static の側）
    Static(Vec<DRef>),
    Any,
}

impl std::fmt::Debug for TT {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TT::Decl(d, a) => write!(f, "{}<{a:?}>", d.first().map_or("?", |d| d.name.as_str())),
            TT::Var(v) => write!(f, "'{v}"),
            TT::Array(e) => write!(f, "{e:?}[]"),
            TT::Union(v) => write!(f, "{v:?}"),
            TT::Func(_, r) => write!(f, "() => {r:?}"),
            TT::Obj(..) => write!(f, "{{…}}"),
            TT::Prim(p) => write!(f, "{p}"),
            TT::Inter(v) => write!(f, "{v:?}&"),
            TT::Static(d) => write!(f, "typeof {}", d.first().map_or("?", |d| d.name.as_str())),
            TT::Any => write!(f, "any"),
        }
    }
}

/// 補完の候補のメンバー
pub struct TsMember {
    pub name: String,
    pub method: bool,
    pub params: bool,
    pub owner: String,
}

/// 型を求める（1 回の補完の間だけ、読んだソースを覚える）
pub struct TsTypes<'a> {
    ix: &'a DtsIndex,
    root: PathBuf,
    files: RefCell<HashMap<PathBuf, Rc<SrcFile>>>,
    /// 呼び出しに書いた型引数（`get<User>(` の User。名前の `$G番号` で指す）と、それを書いたファイル
    targs: RefCell<Vec<(String, Rc<SrcFile>)>>,
}

type Env = Rc<HashMap<String, TT>>;

impl<'a> TsTypes<'a> {
    pub fn new(ix: &'a DtsIndex, root: &Path) -> Self {
        TsTypes { ix, root: root.to_path_buf(), files: RefCell::new(HashMap::new()), targs: RefCell::new(Vec::new()) }
    }

    /// ソースを読む（今のバッファは `text` を渡す）
    pub fn src(&self, path: &Path, text: Option<&str>) -> Option<Rc<SrcFile>> {
        let p = std::path::absolute(path).ok()?;
        if let Some(f) = self.files.borrow().get(&p) {
            return Some(f.clone());
        }
        let text = match text {
            Some(t) => t.to_string(),
            None => std::fs::read_to_string(&p).ok()?,
        };
        let scan = dts::scan_source(&text);
        let f = Rc::new(SrcFile { path: p.clone(), text, scan });
        self.files.borrow_mut().insert(p, f.clone());
        Some(f)
    }

    fn dref_dts(&self, d: &dts::Decl) -> DRef {
        DRef { ctx: Ctx::Dts(d.file), kind: d.kind, name: d.name.clone(), start: d.start, end: d.end }
    }

    /// 宣言の文字
    pub fn decl_text(&self, d: &DRef) -> String {
        let mut c = &d.ctx;
        while let Ctx::Ns(inner, _) = c {
            c = inner;
        }
        match c {
            Ctx::Dts(f) => self.ix.files[*f as usize].text().get(d.start as usize..d.end as usize).unwrap_or("").to_string(),
            Ctx::Src(s) => s.text.get(d.start as usize..d.end as usize).unwrap_or("").to_string(),
            Ctx::Ns(..) => String::new(),
        }
    }

    /// モジュールのファイル（`.d.ts` は索引の番号、`.ts` はソース）が export している `name`
    fn module_export(&self, target: &Path, name: &str) -> Vec<DRef> {
        let s = target.to_string_lossy();
        if s.ends_with(".d.ts") || s.ends_with(".d.mts") {
            return match self.ix.file_of(target) {
                Some(f) => self.ix.exported(f, name).into_iter().map(|d| self.dref_dts(d)).collect(),
                None => Vec::new(),
            };
        }
        let Some(sf) = self.src(target, None) else { return Vec::new() };
        sf.scan.decls.iter().filter(|d| d.0 == name && d.4).map(|d| DRef { ctx: Ctx::Src(sf.clone()), kind: d.1, name: d.0.clone(), start: d.2, end: d.3 }).collect()
    }

    /// 名前 `name`（`NS.X` も）が `ctx` で指す宣言（そのファイル → import → 大域）
    fn resolve_name(&self, ctx: &Ctx, name: &str) -> Vec<DRef> {
        match ctx {
            Ctx::Ns(inner, prefix) => {
                // NS.A.B. → NS.A.B.name, NS.A.name, NS.name, name
                let mut p: &str = prefix;
                while !p.is_empty() {
                    let found = self.resolve_name(inner, &format!("{p}{name}"));
                    if !found.is_empty() {
                        return found;
                    }
                    p = p[..p.len() - 1].rfind('.').map_or("", |k| &p[..=k]);
                }
                self.resolve_name(inner, name)
            }
            Ctx::Dts(f) => self.ix.resolve_in(*f, name).into_iter().map(|d| self.dref_dts(d)).collect(),
            Ctx::Src(sf) => {
                let local: Vec<DRef> = sf.scan.decls.iter().filter(|d| d.0 == name).map(|d| DRef { ctx: ctx.clone(), kind: d.1, name: d.0.clone(), start: d.2, end: d.3 }).collect();
                if !local.is_empty() {
                    return local;
                }
                let (head, rest) = name.split_once('.').map_or((name, None), |(a, b)| (a, Some(b)));
                let dir = sf.path.parent().map(Path::to_path_buf).unwrap_or_default();
                for imp in sf.scan.imports.iter().filter(|i| i.local == head) {
                    let Some(target) = dts::resolve_spec(&self.root, &dir, &imp.from) else { continue };
                    let want = match (imp.imported.as_str(), rest) {
                        ("*", Some(r)) => r.to_string(),
                        ("*", None) => continue,
                        (n, Some(r)) => format!("{n}.{r}"),
                        (n, None) => n.to_string(),
                    };
                    let found = self.module_export(&target, &want);
                    if !found.is_empty() {
                        return found;
                    }
                }
                self.ix.globals(name).into_iter().map(|d| self.dref_dts(d)).collect()
            }
        }
    }

    /// 型の文字を `ctx`・型変数 `env` で型に
    fn eval(&self, ctx: &Ctx, ast: &Ast, env: &Env, depth: usize) -> TT {
        if depth > 12 {
            return TT::Any;
        }
        match ast {
            Ast::Str => TT::Prim("String"),
            Ast::Num => TT::Prim("Number"),
            Ast::Bool => TT::Prim("Boolean"),
            Ast::Any => TT::Any,
            Ast::Obj(body) => TT::Obj(body.as_str().into(), ctx.clone(), env.clone()),
            Ast::Array(e) => TT::Array(Box::new(self.eval(ctx, e, env, depth + 1))),
            Ast::Func(ps, r) => TT::Func(ps.iter().map(|p| self.eval(ctx, p, env, depth + 1)).collect(), Box::new(self.eval(ctx, r, env, depth + 1))),
            Ast::Inter(v) => {
                let ts: Vec<TT> = v.iter().map(|a| self.eval(ctx, a, env, depth + 1)).filter(|t| !matches!(t, TT::Any)).collect();
                match ts.len() {
                    0 => TT::Any,
                    1 => ts.into_iter().next().unwrap(),
                    _ => TT::Inter(ts),
                }
            }
            Ast::Union(v) => {
                // null・undefined は外す。1 つなら それ
                let ts: Vec<TT> = v.iter().map(|a| self.eval(ctx, a, env, depth + 1)).filter(|t| !matches!(t, TT::Any)).collect();
                match ts.len() {
                    0 => TT::Any,
                    1 => ts.into_iter().next().unwrap(),
                    _ => TT::Union(ts),
                }
            }
            Ast::Name(n, args) => {
                if let Some(t) = env.get(n) {
                    return t.clone();
                }
                let args: Vec<TT> = args.iter().map(|a| self.eval(ctx, a, env, depth + 1)).collect();
                match n.as_str() {
                    "Array" | "ReadonlyArray" if args.len() == 1 => return TT::Array(Box::new(args[0].clone())),
                    "String" => return TT::Prim("String"),
                    "Number" => return TT::Prim("Number"),
                    "Boolean" => return TT::Prim("Boolean"),
                    _ => {}
                }
                let decls: Vec<DRef> = self.resolve_name(ctx, n).into_iter().filter(|d| !matches!(d.kind, DeclKind::Var | DeclKind::Function)).collect();
                if decls.is_empty() {
                    // 型変数（宣言の外から来たもの）
                    return if n.chars().count() <= 2 && n.chars().next().is_some_and(char::is_uppercase) { TT::Var(n.clone()) } else { TT::Any };
                }
                // 型の別名は右辺に
                if let Some(alias) = decls.iter().find(|d| d.kind == DeclKind::Type) {
                    let text = self.decl_text(alias);
                    let (head, rhs) = text.split_once('=').unwrap_or((&text, "any"));
                    let tparams = head.find('<').map(|k| head[k + 1..].trim_end().trim_end_matches('>').to_string()).unwrap_or_default();
                    let env2 = bind(&tparams, &args);
                    return self.eval(&inner_ctx(alias), &parse_type(rhs.trim().trim_end_matches(';')), &env2, depth + 1);
                }
                TT::Decl(decls, args)
            }
        }
    }

    /// 型の文字（`ctx` で）を型に
    pub fn type_of_text(&self, ctx: &Ctx, text: &str) -> TT {
        self.eval(ctx, &parse_type(text), &Rc::new(HashMap::new()), 0)
    }

    /// メンバーを集める: (メンバー, その宣言の ctx, 型変数, 持っている型の名前)
    fn collect(&self, t: &TT, statics: bool, out: &mut Vec<(Member, Ctx, Env, String)>, depth: usize) {
        if depth > 10 {
            return;
        }
        match t {
            TT::Prim(p) => {
                let d: Vec<DRef> = self.ix.globals(p).into_iter().filter(|d| d.kind == DeclKind::Interface).map(|d| self.dref_dts(d)).collect();
                self.collect(&TT::Decl(d, Vec::new()), false, out, depth + 1);
            }
            TT::Array(e) => {
                let d: Vec<DRef> = self.ix.globals("Array").into_iter().filter(|d| d.kind == DeclKind::Interface).map(|d| self.dref_dts(d)).collect();
                self.collect(&TT::Decl(d, vec![(**e).clone()]), false, out, depth + 1);
            }
            TT::Func(..) => {
                let d: Vec<DRef> = self.ix.globals("Function").into_iter().map(|d| self.dref_dts(d)).collect();
                self.collect(&TT::Decl(d, Vec::new()), false, out, depth + 1);
            }
            TT::Union(v) => {
                if let Some(f) = v.first() {
                    self.collect(f, statics, out, depth + 1);
                }
            }
            TT::Inter(v) => {
                for t in v {
                    self.collect(t, statics, out, depth + 1);
                }
            }
            TT::Obj(body, ctx, env) => {
                for m in dts::members(body) {
                    out.push((m, ctx.clone(), env.clone(), String::new()));
                }
            }
            TT::Static(decls) => {
                for d in decls {
                    let text = self.decl_text(d);
                    match d.kind {
                        DeclKind::Class => {
                            let (_, supers, body) = dts::split_decl(&text);
                            let env: Env = Rc::new(HashMap::new());
                            for m in dts::members(body).into_iter().filter(|m| m.is_static) {
                                out.push((m, inner_ctx(d), env.clone(), d.name.clone()));
                            }
                            // 親クラスの static
                            if let Some(s) = supers.first() {
                                if let TT::Decl(sd, _) = self.eval(&inner_ctx(d), &parse_type(s), &env, depth + 1) {
                                    self.collect(&TT::Static(sd), true, out, depth + 1);
                                }
                            }
                        }
                        DeclKind::Enum => {
                            let (_, _, body) = dts::split_decl(&text);
                            for part in split_top(&strip_comments(body), b',') {
                                let n = part.split('=').next().unwrap_or("").trim().trim_matches(['\'', '"']);
                                if !n.is_empty() {
                                    out.push((Member { name: n.to_string(), ty: String::new(), ..Default::default() }, d.ctx.clone(), Rc::new(HashMap::new()), d.name.clone()));
                                }
                            }
                        }
                        DeclKind::Namespace => {
                            // namespace の中の宣言
                            if let Ctx::Dts(_) = d.ctx {
                                let prefix = format!("{}.", d.name);
                                for x in self.ix.with_prefix(&prefix) {
                                    let short = &x.name[prefix.len()..];
                                    if !short.contains('.') {
                                        out.push((Member { name: short.to_string(), method: x.kind == DeclKind::Function, ..Default::default() }, Ctx::Dts(x.file), Rc::new(HashMap::new()), d.name.clone()));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            TT::Decl(decls, args) => {
                for d in decls {
                    let text = self.decl_text(d);
                    match d.kind {
                        DeclKind::Interface | DeclKind::Class => {
                            let (tparams, supers, body) = dts::split_decl(&text);
                            let env = bind(&tparams, args);
                            let dctx = inner_ctx(d);
                            for m in dts::members(body).into_iter().filter(|m| !m.is_static || statics) {
                                // コンストラクタの引数のフィールド（private x: T）
                                if m.name == "constructor" {
                                    for p in split_top(&m.params, b',') {
                                        let p = strip_decorators(p.trim());
                                        let words: Vec<&str> = p.split_whitespace().collect();
                                        if words.first().is_some_and(|w| ["private", "public", "protected", "readonly"].contains(w)) {
                                            let rest = p.splitn(2, ' ').nth(1).unwrap_or("").trim_start_matches("readonly ").trim();
                                            if let Some((n, ty)) = rest.split_once(':') {
                                                let n = n.trim().trim_end_matches('?').to_string();
                                                let ty = ty.split('=').next().unwrap_or("").trim().to_string();
                                                out.push((Member { name: n, ty, ..Default::default() }, dctx.clone(), env.clone(), d.name.clone()));
                                            }
                                        }
                                    }
                                }
                                out.push((m, dctx.clone(), env.clone(), d.name.clone()));
                            }
                            for s in supers {
                                let st = self.eval(&dctx, &parse_type(&s), &env, depth + 1);
                                self.collect(&st, false, out, depth + 1);
                            }
                        }
                        DeclKind::Enum | DeclKind::Namespace => self.collect(&TT::Static(vec![d.clone()]), true, out, depth + 1),
                        _ => {}
                    }
                }
            }
            TT::Var(_) | TT::Any => {}
        }
    }

    /// 型 `t` のメンバー（補完の候補。同じ名前は先のもの）
    pub fn members(&self, t: &TT) -> Vec<TsMember> {
        let mut all = Vec::new();
        self.collect(t, matches!(t, TT::Static(_)), &mut all, 0);
        let mut out: Vec<TsMember> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (m, _, _, owner) in all {
            if m.name == "[]" || m.name == "()" || m.name == "new" || m.name == "constructor" || m.name.starts_with('#') {
                continue;
            }
            if seen.insert(m.name.clone()) {
                out.push(TsMember { name: m.name, method: m.method, params: !m.params.trim().is_empty(), owner });
            }
        }
        out
    }

    /// メンバー `name`（同じ名前は先のもの）と、その宣言の ctx・型変数（メソッドの型変数は分からないものに）
    fn find_member(&self, t: &TT, name: &str) -> Option<(Member, Ctx, Env)> {
        // `get$G0` は書いた型引数つきの呼び出し
        let (name, targs) = match name.split_once("$G") {
            Some((n, k)) => (n, k.parse::<usize>().ok().and_then(|k| self.targs.borrow().get(k).cloned())),
            None => (name, None),
        };
        let mut all = Vec::new();
        self.collect(t, matches!(t, TT::Static(_)), &mut all, 0);
        let (m, ctx, env, _) = all.into_iter().find(|(m, ..)| m.name == name)?;
        let mut env2 = (*env).clone();
        let given: Vec<TT> = match &targs {
            Some((text, sf)) => split_top(text, b',').into_iter().map(|a| self.type_of_text(&Ctx::Src(sf.clone()), a)).collect(),
            None => Vec::new(),
        };
        for (k, tp) in split_top(&m.type_params, b',').into_iter().enumerate() {
            let n = tp.split(['=', ' ']).next().unwrap_or("").trim();
            if !n.is_empty() {
                env2.insert(n.to_string(), given.get(k).cloned().unwrap_or(TT::Var(n.to_string())));
            }
        }
        Some((m, ctx, Rc::new(env2)))
    }

    /// 型 `t` のメソッド `m` の `k` 番目の引数（関数の型）の `p` 番目の引数の型（ラムダの引数）
    fn lambda_param(&self, t: &TT, m: &str, k: usize, p: usize) -> Option<TT> {
        let (mem, ctx, env) = self.find_member(t, m)?;
        let param = split_top(&mem.params, b',').into_iter().nth(k)?;
        let ty = param.split_once(':')?.1;
        match self.eval(&ctx, &parse_type(ty), &env, 0) {
            TT::Func(ps, _) => ps.into_iter().nth(p).filter(|t| !matches!(t, TT::Var(_) | TT::Any)),
            _ => None,
        }
    }

    /// メンバー `name` の型（`call` なら戻り値）
    pub fn member_type(&self, t: &TT, name: &str, call: bool) -> Option<TT> {
        let (m, ctx, env) = self.find_member(t, name)?;
        if m.ty.starts_with('=') {
            // 型の無い初期値のフィールド（プロジェクトのクラス）
            return match &ctx {
                Ctx::Src(sf) => self.expr_type(sf, &m.ty[1..], 0, 0),
                _ => None,
            };
        }
        let ty = self.eval(&ctx, &parse_type(&m.ty), &env, 0);
        if call && !m.method {
            // 関数の型のプロパティを呼ぶ
            return match ty {
                TT::Func(_, r) => Some(*r),
                _ => None,
            };
        }
        Some(ty)
    }

    // ---- 式 ----

    /// ソース `sf` の位置 `at` での名前 `name` の型（変数・引数・フィールド・import・大域）
    pub fn name_type(&self, sf: &Rc<SrcFile>, name: &str, at: usize, depth: usize) -> Option<TT> {
        if depth > 6 {
            return None;
        }
        let ctx = Ctx::Src(sf.clone());
        if name == "this" {
            let d = sf.scan.decls.iter().filter(|d| d.1 == DeclKind::Class && (d.2 as usize) < at && at < d.3 as usize).max_by_key(|d| d.2)?;
            return Some(TT::Decl(vec![DRef { ctx, kind: d.1, name: d.0.clone(), start: d.2, end: d.3 }], Vec::new()));
        }
        // 書いた型・初期値・ラムダの引数・for of（at より前の、いちばん近いもの）
        let found = [decl_of(&sf.text, name, at), lambda_of(&sf.text, name, at), for_of(&sf.text, name, at)].into_iter().flatten().max_by_key(|x| x.0);
        if let Some((_, t)) = found {
            return match t {
                Decl::Typed(ty) => Some(self.type_of_text(&ctx, &ty)),
                Decl::Init(e, pos) => self.expr_type(sf, &e, pos, depth + 1),
                Decl::Lambda(recv, m, k, p, pos) => {
                    let rt = self.expr_type(sf, &recv, pos, depth + 1)?;
                    self.lambda_param(&rt, &m, k, p)
                }
                Decl::Elem(e, pos) => match self.expr_type(sf, &e, pos, depth + 1)? {
                    TT::Array(el) => Some(*el),
                    _ => None,
                },
            };
        }
        // import・ファイルの宣言・大域: 変数はその型（同じ名前が複数なら型の分かるもの）、クラス・enum・namespace は static の側
        let decls = self.resolve_name(&ctx, name);
        for v in decls.iter().filter(|d| d.kind == DeclKind::Var) {
            let text = self.decl_text(v);
            // const X: T = … / const X = 式（外側の : と = の先のほう）
            let rest = text.trim_start_matches(|c: char| c.is_alphabetic()).trim_start();
            let rest = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '$');
            let colon = rest.trim_start().starts_with(':');
            let t = if colon {
                let ty = &rest.trim_start()[1..];
                let e = type_end(ty);
                self.type_of_text(&inner_ctx(v), ty[..e].trim().trim_end_matches(';'))
            } else if let (Some(init), Ctx::Src(vsf)) = (rest.trim_start().strip_prefix('='), &v.ctx) {
                self.expr_type(vsf, init, v.start as usize, depth + 1).unwrap_or(TT::Any)
            } else {
                TT::Any
            };
            if !matches!(t, TT::Any) {
                return Some(t);
            }
        }
        if decls.iter().any(|d| matches!(d.kind, DeclKind::Class | DeclKind::Enum | DeclKind::Namespace)) {
            return Some(TT::Static(decls.into_iter().filter(|d| matches!(d.kind, DeclKind::Class | DeclKind::Enum | DeclKind::Namespace)).collect()));
        }
        None
    }

    /// 式 `e`（ソース `sf` の位置 `at` の）の型
    pub fn expr_type(&self, sf: &Rc<SrcFile>, e: &str, at: usize, depth: usize) -> Option<TT> {
        if depth > 6 {
            return None;
        }
        let e = e.trim().trim_end_matches(';').trim();
        // x as T（as const は外す）
        let e = e.strip_suffix("as const").map_or(e, str::trim_end);
        if let Some(k) = e.rfind(" as ") {
            let ty = &e[k + 4..];
            if !ty.contains(['(', ')']) {
                return Some(self.type_of_text(&Ctx::Src(sf.clone()), ty));
            }
        }
        if e.starts_with(['\'', '"', '`']) {
            return Some(TT::Prim("String"));
        }
        if e.starts_with(|c: char| c.is_ascii_digit()) {
            return Some(TT::Prim("Number"));
        }
        if e == "true" || e == "false" || e.starts_with('!') {
            return Some(TT::Prim("Boolean"));
        }
        if e.starts_with('[') {
            return Some(TT::Array(Box::new(TT::Any)));
        }
        // オブジェクトのリテラル { a: 1, b: 'x' }（as const も）
        if e.starts_with('{') {
            let body = e.trim_end_matches("as const").trim().trim_start_matches('{').trim_end_matches('}');
            return Some(TT::Obj(body.into(), Ctx::Src(sf.clone()), Rc::new(HashMap::new())));
        }
        let clean = self.encode_targs(sf, &strip_optional(e));
        let r = crate::smart_complete::receiver_before(&clean, clean.len(), &|n: &str| n.strip_prefix("new ").map(|t| format!("new:{}", t.split('<').next().unwrap_or(t))))?;
        self.receiver(sf, &r, at, depth + 1)
    }

    /// 呼び出しに書いた型引数 `name<A, B>(` を `name$G番号(` に（番号の先に文字を覚える）
    fn encode_targs(&self, sf: &Rc<SrcFile>, e: &str) -> String {
        let b = e.as_bytes();
        let mut out = String::with_capacity(e.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'<' && i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$') {
                // 対応する > の次が ( なら型引数
                let mut depth = 0;
                let mut close = None;
                for (k, &c) in b.iter().enumerate().skip(i) {
                    match c {
                        b'<' => depth += 1,
                        b'>' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(k);
                                break;
                            }
                        }
                        b'(' | b')' | b';' | b'=' | b'&' | b'|' if depth > 0 && c != b'|' && c != b'&' => break,
                        _ => {}
                    }
                }
                if let Some(c) = close.filter(|&c| b.get(c + 1) == Some(&b'(')) {
                    let mut t = self.targs.borrow_mut();
                    out.push_str(&format!("$G{}", t.len()));
                    t.push((e[i + 1..c].to_string(), sf.clone()));
                    i = c + 1;
                    continue;
                }
            }
            let ch = e[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// 受け手の連なりの型
    pub fn receiver(&self, sf: &Rc<SrcFile>, r: &Receiver, at: usize, depth: usize) -> Option<TT> {
        match r {
            Receiver::This => self.name_type(sf, "this", at, depth),
            Receiver::Type(t) => {
                if let Some(c) = t.strip_prefix("new:") {
                    let decls = self.resolve_name(&Ctx::Src(sf.clone()), c);
                    return (!decls.is_empty()).then(|| TT::Decl(decls, Vec::new()));
                }
                self.name_type(sf, t, at, depth)
            }
            Receiver::Member(inner, n) if matches!(**inner, Receiver::This) && n != "this" => {
                // this のない頭の名前（変数）か、this.x
                self.name_type(sf, n, at, depth).or_else(|| {
                    let this = self.name_type(sf, "this", at, depth)?;
                    self.member_type(&this, n, false)
                })
            }
            Receiver::Call(inner, n) if matches!(**inner, Receiver::This) => {
                // 関数の呼び出し: import・大域の関数、inject(T)
                let n = n.split("$G").next().unwrap_or(n);
                if n == "inject" {
                    return None;
                }
                let decls = self.resolve_name(&Ctx::Src(sf.clone()), n);
                let f = decls.iter().find(|d| d.kind == DeclKind::Function)?;
                let text = self.decl_text(f);
                let ret = fn_ret(&text)?;
                Some(self.type_of_text(&f.ctx, &ret))
            }
            Receiver::Member(inner, n) | Receiver::Call(inner, n) => {
                let call = matches!(r, Receiver::Call(..));
                let it = self.receiver(sf, inner, at, depth)?;
                self.member_type(&it, n, call)
            }
        }
    }
}

/// `.` の前の式 `before`（ファイル `path` の中身 `src`、位置 `at`）の型のメンバー
pub fn complete(ix: &DtsIndex, root: &Path, path: &Path, src: &str, before: &str, at: usize) -> Vec<TsMember> {
    let tt = TsTypes::new(ix, root);
    let Some(sf) = tt.src(path, Some(src)) else { return Vec::new() };
    let clean = strip_optional(before.trim_end_matches(['?', '!']));
    let start = crate::java_members::chain_start(&clean).unwrap_or(0);
    let Some(t) = tt.expr_type(&sf, &clean[start..], at, 0) else { return Vec::new() };
    tt.members(&t)
}

/// 型変数の並び（`T, U extends X = Y`）を型引数に結ぶ（足りないものは分からないもの）
fn bind(tparams: &str, args: &[TT]) -> Env {
    let mut env = HashMap::new();
    for (k, tp) in split_top(tparams, b',').into_iter().enumerate() {
        let n = tp.trim().split([' ', '=']).next().unwrap_or("").trim();
        if n.is_empty() {
            continue;
        }
        env.insert(n.to_string(), args.get(k).cloned().unwrap_or(TT::Var(n.to_string())));
    }
    Rc::new(env)
}

/// コメントを外す
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// 頭のデコレーター（`@Optional() @Inject(X) x: T`）を外す
fn strip_decorators(mut p: &str) -> &str {
    while let Some(r) = p.strip_prefix('@') {
        let k = r.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).unwrap_or(r.len());
        let mut rest = &r[k..];
        if rest.starts_with('(') {
            let mut depth = 0;
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = &rest[end..];
        }
        p = rest.trim_start();
    }
    p
}

/// `?.`・`!.` を `.` に
fn strip_optional(e: &str) -> String {
    e.replace("?.", ".").replace("!.", ".")
}

/// 関数の宣言の戻り値の型の文字（`function f<T>(a: A): R;`）
fn fn_ret(text: &str) -> Option<String> {
    let open = text.find('(')?;
    let mut depth = 0;
    let b = text.as_bytes();
    let mut close = None;
    for i in open..b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let rest = text[close? + 1..].trim_start().strip_prefix(':')?;
    Some(rest.split('{').next().unwrap_or(rest).trim().trim_end_matches(';').to_string())
}

/// 変数の宣言
enum Decl {
    /// 書いた型
    Typed(String),
    /// 初期値（と、その位置）
    Init(String, usize),
    /// ラムダの引数: (受け手の式, メソッド, 何番目の引数か, ラムダの何番目の引数か, 位置)
    Lambda(String, String, usize, usize, usize),
    /// for (const x of 式) の要素
    Elem(String, usize),
}

/// `name` がラムダの引数なら（`recv.m(a, (x, name) => …)`）、at より前の最後のもの
fn lambda_of(src: &str, name: &str, at: usize) -> Option<(usize, Decl)> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:\(([^()]*)\)|\b([\w$]+))\s*(?::\s*[\w<>\[\]|. ]+)?\s*=>").unwrap());
    let upto = &src[..at.min(src.len())];
    if !upto.contains(name) {
        return None;
    }
    let mut best = None;
    for c in re.captures_iter(upto) {
        let whole = c.get(0)?;
        let p = match (c.get(1), c.get(2)) {
            (Some(list), _) => match split_top(list.as_str(), b',').iter().position(|x| x.split(':').next().unwrap_or("").trim().trim_end_matches('?') == name) {
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
        let b = pre.as_bytes();
        let (mut depth, mut k, mut open) = (0i32, 0usize, None);
        for i in (0..b.len()).rev() {
            match b[i] {
                b')' | b']' | b'}' => depth += 1,
                b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
                b'(' => {
                    open = Some(i);
                    break;
                }
                b'[' | b'{' | b';' => break,
                b',' if depth == 0 => k += 1,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        let call = strip_optional(&pre[..open]);
        let Some(cs) = crate::java_members::chain_start(&call) else { continue };
        let chain = &call[cs..];
        let Some((recv, m)) = chain.rsplit_once('.') else { continue };
        if recv.is_empty() {
            continue;
        }
        best = Some((whole.start(), Decl::Lambda(recv.to_string(), m.to_string(), k, p, whole.start())));
    }
    best
}

/// `for (const name of 式)`
fn for_of(src: &str, name: &str, at: usize) -> Option<(usize, Decl)> {
    let upto = &src[..at.min(src.len())];
    let mut best = None;
    for (i, _) in upto.match_indices(" of ") {
        let before = upto[..i].trim_end();
        if !before.ends_with(name) || !before[..before.len() - name.len()].trim_end().ends_with(|c: char| c == 't' || c == 'r') {
            continue;
        }
        let head = before[..before.len() - name.len()].trim_end();
        if !(head.ends_with("const") || head.ends_with("let") || head.ends_with("var")) || !head.trim_end_matches(|c: char| c.is_alphabetic()).trim_end().ends_with('(') {
            continue;
        }
        let rest = &upto[i + 4..];
        let e = expr_end(rest);
        best = Some((i, Decl::Elem(rest[..e].trim().to_string(), i)));
    }
    best
}

/// `name` の宣言（`at` より前の最後）: `name: T`（引数・変数・フィールド）か `const name = 式`・フィールドの `name = 式`
fn decl_of(src: &str, name: &str, at: usize) -> Option<(usize, Decl)> {
    let upto = &src[..at.min(src.len())];
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut best: Option<(usize, Decl)> = None;
    for (i, _) in upto.match_indices(name) {
        if upto[..i].ends_with(word) || upto[..i].ends_with('.') || upto[i + name.len()..].starts_with(word) {
            continue;
        }
        let after = upto[i + name.len()..].trim_start();
        let before = upto[..i].trim_end();
        let line_start = upto[..i].rfind('\n').map_or(0, |k| k + 1);
        let head = upto[line_start..i].trim();
        // 文字列の中（"window:beforeunload" など）
        if ['"', '\'', '`'].iter().any(|q| upto[line_start..i].matches(*q).count() % 2 == 1) {
            continue;
        }
        // name: T（オブジェクトのリテラルの key: value は除く。引数・フィールド・変数）。前の修飾子・デコレーターは外して見る
        const MODS: &[&str] = &["const", "let", "var", "private", "public", "protected", "readonly", "static", "declare", "override"];
        let mut pre = before;
        loop {
            let w = pre.rsplit(|c: char| c.is_whitespace() || c == '(' || c == ',').next().unwrap_or("");
            if !w.is_empty() && (MODS.contains(&w) || w.starts_with('@')) {
                pre = pre[..pre.len() - w.len()].trim_end();
            } else if pre.ends_with(')') && pre.rfind('@').is_some_and(|a| !pre[a..].contains(['\n', ';', '{'])) {
                // @Inject(X) のようなデコレーター
                pre = pre[..pre.rfind('@').unwrap()].trim_end();
            } else {
                break;
            }
        }
        let typed = after.strip_prefix('?').unwrap_or(after).trim_start().strip_prefix(':').filter(|_| {
            pre.ends_with(['(', ',']) || pre.len() < line_start || pre.ends_with(['{', ';', '}']) || head.split_whitespace().all(|w| MODS.contains(&w) || w.starts_with('@'))
        });
        if let Some(rest) = typed {
            let e = type_end(rest);
            let ty = rest[..e].trim();
            if !ty.is_empty() && !ty.starts_with('/') {
                best = Some((i, Decl::Typed(ty.to_string())));
                continue;
            }
        }
        // const name = 式 / フィールド name = 式
        if let Some(rest) = after.strip_prefix('=').filter(|r| !r.starts_with(['=', '>'])) {
            let kw = before.ends_with("const") || before.ends_with("let") || before.ends_with("var");
            let field = head.split_whitespace().all(|w| ["private", "public", "protected", "readonly", "static", "override"].contains(&w)) && is_class_level(upto, i);
            if kw || field {
                let e = expr_end(rest);
                best = Some((i, Decl::Init(rest[..e].trim().to_string(), i)));
            }
        }
    }
    best
}

/// 位置 `i` がクラスの本体の直下（フィールドの宣言の場所）か（{ の深さで近似: 直前の開いている { の前が class … ）
fn is_class_level(src: &str, i: usize) -> bool {
    let b = src.as_bytes();
    let mut depth = 0i32;
    for k in (0..i).rev() {
        match b[k] {
            b'}' => depth += 1,
            b'{' => {
                if depth == 0 {
                    let head = src[..k].rsplit(['\n', ';', '}']).next().unwrap_or("");
                    return head.contains("class ");
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

/// 型の文字の終わり（外側の , ) ; = { か改行）
fn type_end(s: &str) -> usize {
    let b = s.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'<' => depth += 1,
            b'{' if depth == 0 && i > 0 && !s[..i].trim().is_empty() => return i,
            b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i > 0 && b[i - 1] != b'=' => depth -= 1,
            b',' | b';' | b'=' | b'\n' if depth == 0 && !(b[i] == b'=' && b.get(i + 1) == Some(&b'>')) => return i,
            _ => {}
        }
    }
    b.len()
}

/// 式の終わり（外側の ; , ) か改行で次が続かないところ）
fn expr_end(s: &str) -> usize {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1 + usize::from(b[i] == b'\\');
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' | b'}' => depth -= 1,
            b';' | b',' if depth == 0 => return i,
            b'\n' if depth == 0 => {
                let next = s[i..].trim_start();
                if !next.starts_with(['.', '?', ':', '+', '-', '*', '|', '&']) {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types() {
        assert_eq!(parse_type("string | undefined"), Ast::Union(vec![Ast::Str, Ast::Any]));
        assert_eq!(parse_type("Array<Foo<T>>"), Ast::Name("Array".into(), vec![Ast::Name("Foo".into(), vec![Ast::Name("T".into(), vec![])])]));
        assert_eq!(parse_type("readonly T[]"), Ast::Array(Box::new(Ast::Name("T".into(), vec![]))));
        assert_eq!(parse_type("(value: T, i: number) => U"), Ast::Func(vec![Ast::Name("T".into(), vec![]), Ast::Num], Box::new(Ast::Name("U".into(), vec![]))));
        assert_eq!(parse_type("{ a: number }"), Ast::Obj(" a: number ".into()));
        assert!(matches!(parse_type("T extends string ? A : B"), Ast::Name(n, _) if n == "A"));
    }

    /// 実際のプロジェクトで式の型とメンバーの数（NAGI_TS_ROOT・NAGI_TS_FILE（根からの相対）・NAGI_TS_EXPRS（; 区切り））
    #[test]
    #[ignore]
    fn ts_real() {
        let (Some(root), Ok(file), Ok(exprs)) = (std::env::var_os("NAGI_TS_ROOT").map(PathBuf::from), std::env::var("NAGI_TS_FILE"), std::env::var("NAGI_TS_EXPRS")) else { return };
        let ix = DtsIndex::load(&root);
        let path = root.join(file);
        let src = std::fs::read_to_string(&path).unwrap();
        let tt = TsTypes::new(&ix, &root);
        let sf = tt.src(&path, Some(&src)).unwrap();
        for e in exprs.split(';') {
            let at = src.len();
            let t = tt.expr_type(&sf, e, at, 0);
            let ms = t.as_ref().map(|t| tt.members(t)).unwrap_or_default();
            println!("{e}: {t:?} → {} {:?}", ms.len(), ms.iter().take(6).map(|m| m.name.as_str()).collect::<Vec<_>>());
            for g in ix.globals(e).iter().take(4) {
                println!("   global {:?} {} {:?}", g.kind, ix.files[g.file as usize].path.file_name().unwrap().to_string_lossy(), ix.text(g).chars().take(60).collect::<String>());
            }
        }
    }

    #[test]
    fn decls() {
        let src = "class A {\n  private http: HttpClient;\n  items = [];\n  constructor(private route: ActivatedRoute) {}\n  run(x: string, y?: Foo<Bar>) {\n    const z = this.http.get(1);\n    let w: number = 1;\n    z.\n  }\n}\n";
        let at = src.find("z.\n").unwrap();
        let t = |n: &str| match decl_of(src, n, at).map(|x| x.1) {
            Some(Decl::Typed(t)) => format!("T {t}"),
            Some(Decl::Init(e, _)) => format!("= {e}"),
            _ => "-".into(),
        };
        assert_eq!(t("http"), "T HttpClient");
        assert_eq!(t("route"), "T ActivatedRoute");
        assert_eq!(t("x"), "T string");
        assert_eq!(t("y"), "T Foo<Bar>");
        assert_eq!(t("z"), "= this.http.get(1)");
        assert_eq!(t("w"), "T number");
        assert_eq!(t("items"), "= []");
    }
}
