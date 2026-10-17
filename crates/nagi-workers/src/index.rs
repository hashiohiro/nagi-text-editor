//! プロジェクトの定義の索引（tags の代わり。architecture.md §10）。
//!
//! `.gitignore` に従って対応する言語のファイルを集め、複数のスレッドで構文木を作って定義を取り出す。
//! 結果はファイルの (更新時刻, 大きさ) と一緒に `%LOCALAPPDATA%\nagi\index\` に保存し、次からは変わったファイルだけを
//! 解析し直す（差分更新）。保存したファイルは 1 つずつ索引し直す。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use nagi_syntax::Lang;
use nagi_syntax::defs::{Arg, DefKind, DefParser, Receiver as Recv};

use crate::Waker;

/// 索引の 1 項目
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: DefKind,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug, Default)]
struct FileDefs {
    stamp: (u64, u64),
    defs: Vec<Entry>,
}

/// 名前で引く定義の場所
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    /// 根からの相対パス（区切りは `/`）
    pub path: Arc<str>,
    pub line: u32,
    pub col: u32,
    pub kind: DefKind,
}

#[derive(Default)]
pub struct Index {
    pub root: PathBuf,
    files: HashMap<Arc<str>, FileDefs>,
    by_name: HashMap<String, Vec<Loc>>,
}

impl Index {
    pub fn lookup(&self, name: &str) -> &[Loc] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn def_count(&self) -> usize {
        self.by_name.values().map(Vec::len).sum()
    }

    /// すべての定義（シンボル検索用）: (名前, 場所)
    pub fn all(&self) -> impl Iterator<Item = (&str, &Loc)> {
        self.by_name.iter().flat_map(|(n, v)| v.iter().map(move |l| (n.as_str(), l)))
    }

    /// 識別子の名前の一覧（補完用）
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    fn rebuild(&mut self) {
        let mut by: HashMap<String, Vec<Loc>> = HashMap::new();
        for (path, f) in &self.files {
            for d in &f.defs {
                by.entry(d.name.clone()).or_default().push(Loc { path: path.clone(), line: d.line, col: d.col, kind: d.kind });
            }
        }
        self.by_name = by;
    }

    /// 1 ファイルの索引を差し替える（保存したとき）
    pub fn replace_file(&mut self, rel: &str, stamp: (u64, u64), defs: Vec<Entry>) {
        let key: Arc<str> = rel.into();
        if let Some(old) = self.files.remove(rel) {
            for d in &old.defs {
                if let Some(v) = self.by_name.get_mut(&d.name) {
                    v.retain(|l| &*l.path != rel);
                    if v.is_empty() {
                        self.by_name.remove(&d.name);
                    }
                }
            }
        }
        for d in &defs {
            self.by_name.entry(d.name.clone()).or_default().push(Loc { path: key.clone(), line: d.line, col: d.col, kind: d.kind });
        }
        self.files.insert(key, FileDefs { stamp, defs });
    }

    /// 保存用の写し（名前の表は持たない）
    pub fn snapshot(&self) -> Index {
        Index { root: self.root.clone(), files: self.files.clone(), by_name: HashMap::new() }
    }

    /// 索引を保存する（裏のスレッドから呼ぶ）
    pub fn save(&self) {
        let Some(p) = cache_path(&self.root) else { return };
        let mut out = String::from("nagi-index 1\n");
        for (path, f) in &self.files {
            out.push_str(&format!("F\t{path}\t{}\t{}\n", f.stamp.0, f.stamp.1));
            for d in &f.defs {
                out.push_str(&format!("D\t{}\t{}\t{}\t{}\n", d.kind.code(), d.line, d.col, d.name));
            }
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// 索引の保存先（根のパスの FNV-1a を名前にする）
fn cache_path(root: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("NAGI_INDEX_DIR").map(PathBuf::from).or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("nagi").join("index")))?;
    let key = root.to_string_lossy().to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    Some(base.join(format!("{h:016x}.idx")))
}

fn load_cache(root: &Path) -> HashMap<Arc<str>, FileDefs> {
    let mut out = HashMap::new();
    let Some(p) = cache_path(root) else { return out };
    let Ok(text) = std::fs::read_to_string(p) else { return out };
    let mut lines = text.lines();
    if lines.next() != Some("nagi-index 1") {
        return out;
    }
    let mut cur: Option<(Arc<str>, FileDefs)> = None;
    for l in lines {
        let mut it = l.split('\t');
        match it.next() {
            Some("F") => {
                if let Some((k, v)) = cur.take() {
                    out.insert(k, v);
                }
                let (Some(path), Some(m), Some(sz)) = (it.next(), it.next(), it.next()) else { continue };
                cur = Some((path.into(), FileDefs { stamp: (m.parse().unwrap_or(0), sz.parse().unwrap_or(0)), defs: Vec::new() }));
            }
            Some("D") => {
                let (Some(k), Some(line), Some(col), Some(name)) = (it.next(), it.next(), it.next(), it.next()) else { continue };
                let Some(kind) = k.parse().ok().and_then(DefKind::from_code) else { continue };
                if let Some((_, f)) = &mut cur {
                    f.defs.push(Entry { name: name.to_string(), kind, line: line.parse().unwrap_or(0), col: col.parse().unwrap_or(0) });
                }
            }
            _ => {}
        }
    }
    if let Some((k, v)) = cur.take() {
        out.insert(k, v);
    }
    out
}

/// ファイルの (更新時刻の ns, 大きさ)
pub fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some((t, m.len()))
}

/// 1 ファイルの定義を取り出す
pub fn index_file(path: &Path, parser: &mut DefParser) -> Option<((u64, u64), Vec<Entry>)> {
    let lang = Lang::detect(path)?;
    let st = stamp(path)?;
    let bytes = std::fs::read(path).ok()?;
    // 大きすぎるもの（生成物など）は入れない
    if bytes.len() > 2 << 20 {
        return Some((st, Vec::new()));
    }
    let src = String::from_utf8_lossy(&bytes);
    let defs = parser.defs(lang, &src).into_iter().map(|d| Entry { name: d.name, kind: d.kind, line: d.line as u32, col: d.col as u32 }).collect();
    Some((st, defs))
}

/// 作っている途中の知らせ
pub enum Progress {
    /// (解析したファイル, 解析が要るファイル)
    Step(usize, usize),
    Done(Box<Index>),
}

/// 索引を作る（保存したものがあれば、変わったファイルだけ解析し直す）
pub fn build(root: &Path, waker: Waker) -> Receiver<Progress> {
    let (tx, rx) = channel();
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new().name("index".into()).spawn(move || {
        let cached = load_cache(&root);
        // 対応する言語のファイルを集める
        let mut paths = Vec::new();
        for e in ignore::WalkBuilder::new(&root).hidden(true).require_git(false).build().flatten() {
            if e.file_type().is_some_and(|t| t.is_file()) && Lang::detect(e.path()).is_some() {
                paths.push(e.into_path());
            }
        }
        let mut files: HashMap<Arc<str>, FileDefs> = HashMap::new();
        let mut todo = Vec::new();
        for p in paths {
            let rel: Arc<str> = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/").into();
            match (cached.get(&rel), stamp(&p)) {
                (Some(c), Some(st)) if c.stamp == st => {
                    files.insert(rel, c.clone());
                }
                _ => todo.push((rel, p)),
            }
        }
        let total = todo.len();
        let changed = total > 0 || files.len() != cached.len();
        // 解析は CPU の数だけスレッドに分ける
        let done = Arc::new(Mutex::new(Vec::with_capacity(total)));
        let queue = Arc::new(Mutex::new(todo));
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
        std::thread::scope(|s| {
            for _ in 0..n {
                let (queue, done, tx, waker) = (queue.clone(), done.clone(), tx.clone(), waker.clone());
                s.spawn(move || {
                    let mut parser = DefParser::default();
                    loop {
                        let Some((rel, p)) = queue.lock().ok().and_then(|mut q| q.pop()) else { break };
                        let r = index_file(&p, &mut parser);
                        let k = {
                            let mut d = done.lock().unwrap();
                            if let Some((st, defs)) = r {
                                d.push((rel, FileDefs { stamp: st, defs }));
                            } else {
                                d.push((rel, FileDefs::default()));
                            }
                            d.len()
                        };
                        if k % 200 == 0 {
                            let _ = tx.send(Progress::Step(k, total));
                            waker();
                        }
                    }
                });
            }
        });
        for (rel, f) in Arc::try_unwrap(done).map(|m| m.into_inner().unwrap_or_default()).unwrap_or_default() {
            files.insert(rel, f);
        }
        let mut idx = Index { root, files, by_name: HashMap::new() };
        idx.rebuild();
        if changed {
            idx.save();
        }
        let _ = tx.send(Progress::Done(Box::new(idx)));
        waker();
    });
    rx
}

/// ファイルの import 文（Java は完全な名前、TS / JS は (取り込む名前, モジュールの指定)）
pub fn imports_of(src: &str) -> Vec<(Vec<String>, String)> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find("import") {
        let at_line_start = rest[..i].rsplit('\n').next().is_none_or(|l| l.trim().is_empty());
        let tail = &rest[i + 6..];
        rest = tail;
        if !at_line_start || !tail.starts_with([' ', '\t', '{']) {
            continue;
        }
        let end = tail.find(';').unwrap_or(tail.len()).min(800);
        let stmt = tail[..end].trim();
        if let Some(fi) = stmt.rfind("from") {
            // TS / JS: import { A, B as C } from './x' / import D from 'y'
            let spec = stmt[fi + 4..].trim().trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string();
            let names: Vec<String> = stmt[..fi]
                .replace(['{', '}', ',', '*'], " ")
                .split_whitespace()
                .filter(|w| !matches!(*w, "type" | "as"))
                .map(String::from)
                .collect();
            out.push((names, spec));
        } else {
            // Java: import [static] a.b.C / a.b.*
            let fq = stmt.trim_start_matches("static").trim().to_string();
            if !fq.is_empty() && fq.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '_' || c == '*' || c == '$') {
                let last = fq.rsplit('.').next().unwrap_or("").to_string();
                out.push((vec![last], fq));
            }
        }
    }
    out
}

/// 定義の候補が複数のとき絞る（いちばん有力なものだけ）。`from_rel` は今のファイルの根からの相対パス（区切りは `/`）
pub fn narrow_candidates(from_rel: &str, src: &str, name: &str, cands: Vec<Loc>) -> Vec<Loc> {
    let (mut v, best) = Ranker::new(None).rank(from_rel, src, name, None, cands);
    v.truncate(best);
    v
}

/// 定義を探した結果
#[derive(Debug)]
pub enum Ranked {
    /// (有力な順の候補, いちばん有力なものの数)
    Locs(Vec<Loc>, usize),
    /// 受け手の型（またはその親）がプロジェクトの外（ライブラリ）にある。その型の名前
    Outside(String),
}

/// 受け手の型をたどった結果
enum Hier {
    /// 候補のあるファイル
    Files(std::collections::HashSet<String>),
    /// Lombok の getter・setter のフィールド
    Lombok(Vec<Loc>),
    Outside(String),
    Unknown,
}

/// 呼び出しの引数の型（索引でたどった後）
enum ArgTy {
    Lit(&'static str),
    Named(String),
    Unknown,
}

/// JDK のよくある型の親（オーバーロードを引数の型で選ぶため）
fn jdk_supers(t: &str) -> Option<&'static [&'static str]> {
    Some(match t {
        "LocalDate" | "LocalDateTime" | "LocalTime" | "ZonedDateTime" | "OffsetDateTime" | "OffsetTime" | "Instant" | "Year" | "YearMonth" => &["Temporal", "TemporalAccessor", "Comparable", "Serializable"],
        "Temporal" => &["TemporalAccessor"],
        "Timestamp" => &["Date"],
        "Date" | "Calendar" => &["Serializable", "Comparable"],
        "Integer" | "Long" | "Short" | "Byte" | "Double" | "Float" | "BigDecimal" | "BigInteger" => &["Number", "Comparable", "Serializable"],
        "String" => &["CharSequence", "Comparable", "Serializable"],
        "StringBuilder" | "StringBuffer" => &["CharSequence"],
        "ArrayList" | "LinkedList" => &["List"],
        "List" | "Set" | "Queue" => &["Collection"],
        "HashSet" | "LinkedHashSet" | "TreeSet" => &["Set"],
        "Collection" => &["Iterable"],
        "HashMap" | "LinkedHashMap" | "TreeMap" | "ConcurrentHashMap" => &["Map"],
        _ => return None,
    })
}

/// 表の親の側に出てくる型（親が分かっているものとして扱う）
fn jdk_known(t: &str) -> bool {
    matches!(t, "Temporal" | "TemporalAccessor" | "Number" | "CharSequence" | "Map" | "Iterable" | "Comparable" | "Serializable" | "int" | "long" | "short" | "byte" | "double" | "float" | "boolean" | "char")
}

/// getFoo / isFoo / setFoo → foo
fn accessor_field(name: &str) -> Option<String> {
    let rest = name.strip_prefix("get").or_else(|| name.strip_prefix("set")).or_else(|| name.strip_prefix("is"))?;
    let mut c = rest.chars();
    let first = c.next().filter(|c| c.is_uppercase())?;
    Some(first.to_lowercase().chain(c).collect())
}

/// 定義の候補を有力な順に並べる。受け手の型（とその親クラス）→ import → 型の名前なら型 → 今のファイルとパスが長く共通するもの
pub struct Ranker<'a> {
    idx: Option<&'a Index>,
    /// 「ファイル#型」→ 直接の親（extends / implements）
    supers: HashMap<String, Vec<String>>,
    /// 読んだファイル（根からの相対パス → 行）
    files: HashMap<String, Arc<Vec<String>>>,
}

/// 受け手を型に直したもの
enum Ty {
    This,
    /// 型の名前と、その型の名前が書かれていたファイル（内部クラスを先に探す）
    Named(String, Option<String>),
    Outside(String),
}

type Files = std::collections::HashSet<String>;

impl<'a> Ranker<'a> {
    pub fn new(idx: Option<&'a Index>) -> Self {
        Ranker { idx, supers: HashMap::new(), files: HashMap::new() }
    }

    /// (有力な順の候補, いちばん有力なものの数)。数が 1 ならすぐ跳んでよい
    pub fn rank(&mut self, from_rel: &str, src: &str, name: &str, recv: Option<&Recv>, cands: Vec<Loc>) -> (Vec<Loc>, usize) {
        match self.resolve(from_rel, src, name, recv, None, cands) {
            Ranked::Locs(v, best) => (v, best),
            Ranked::Outside(_) => (Vec::new(), 0),
        }
    }

    /// 定義を探す。受け手の型がプロジェクトの外（ライブラリ）なら Outside。`args` は呼び出しの引数（オーバーロードを数と型で選ぶ）
    pub fn resolve(&mut self, from_rel: &str, src: &str, name: &str, recv: Option<&Recv>, args: Option<&[Arg]>, cands: Vec<Loc>) -> Ranked {
        let ty = recv.and_then(|r| self.type_of(from_rel, src, r, 0));
        match (self.resolve_ty(from_rel, src, name, ty, cands), args) {
            (Ranked::Locs(v, best), Some(args)) if best > 1 => {
                let (v, best) = self.by_args(from_rel, src, v, best, args);
                Ranked::Locs(v, best)
            }
            (r, _) => r,
        }
    }

    /// 有力な候補のうち、引数の数が合うもの → 引数の型がいちばん合うものを先に
    fn by_args(&mut self, from_rel: &str, src: &str, v: Vec<Loc>, best: usize, args: &[Arg]) -> (Vec<Loc>, usize) {
        let n = args.len();
        let params: Vec<Option<(Vec<Option<String>>, bool)>> = v[..best].to_vec().iter().map(|l| self.params(l)).collect();
        // ぴったり合うもの、無ければ可変長で合うもの
        let exact: Vec<bool> = params.iter().map(|p| p.as_ref().is_some_and(|(p, var)| p.len() == n && !var)).collect();
        let fits: Vec<bool> = if exact.contains(&true) {
            exact
        } else {
            params.iter().map(|p| p.as_ref().is_some_and(|(p, var)| p.len() == n || (*var && n + 1 >= p.len()))).collect()
        };
        // 引数の型（分かるものだけ）
        let arg_tys: Vec<ArgTy> = args
            .iter()
            .map(|a| match a {
                Arg::Lit(l) => ArgTy::Lit(l),
                Arg::Expr(r) => match self.type_of(from_rel, src, r, 0) {
                    Some(Ty::Named(t, _) | Ty::Outside(t)) => ArgTy::Named(t),
                    _ => ArgTy::Unknown,
                },
                Arg::Unknown => ArgTy::Unknown,
            })
            .collect();
        let mut scores: Vec<Option<i32>> = Vec::new();
        for (p, f) in params.iter().zip(&fits) {
            scores.push(if !*f {
                None
            } else {
                let (ps, var) = p.as_ref().unwrap();
                let mut sc = 0;
                for (i, a) in arg_tys.iter().enumerate() {
                    let pt = ps.get(i).or_else(|| if *var { ps.last() } else { None }).cloned().flatten();
                    if let Some(pt) = pt {
                        sc += self.compat(a, &pt);
                    }
                }
                Some(sc)
            });
        }
        let top = scores.iter().flatten().max().copied();
        let Some(top) = top else { return (v, best) };
        let k = scores.iter().filter(|s| **s == Some(top)).count();
        if k == best {
            return (v, best);
        }
        let mut order: Vec<usize> = (0..best).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(scores[i].map_or(i32::MIN, |s| s)));
        let mut out: Vec<Loc> = order.iter().map(|&i| v[i].clone()).collect();
        out.extend(v[best..].iter().cloned());
        (out, k)
    }

    /// 引数の型と引数の宣言の型がどれだけ合うか（2: 同じ、1: 子の型・数の広げ、0: 分からない、負: 合わない）
    fn compat(&mut self, a: &ArgTy, param: &str) -> i32 {
        let p = param;
        let loose = matches!(p, "Object" | "any" | "unknown") || p.chars().count() == 1;
        let numeric = ["int", "long", "short", "byte", "double", "float", "Integer", "Long", "Short", "Byte", "Double", "Float", "Number", "number", "BigDecimal", "BigInteger"];
        match a {
            ArgTy::Unknown => 0,
            _ if loose => 0,
            ArgTy::Lit("String") => if matches!(p, "String" | "CharSequence" | "string") { 2 } else { -3 },
            ArgTy::Lit("int") => match p {
                "int" | "Integer" | "number" => 2,
                _ if numeric.contains(&p) => 1,
                _ => -3,
            },
            ArgTy::Lit("long") => match p {
                "long" | "Long" => 2,
                "float" | "double" | "Float" | "Double" | "Number" => 1,
                _ => -3,
            },
            ArgTy::Lit("double") => match p {
                "double" | "Double" | "number" => 2,
                "float" | "Float" | "Number" | "BigDecimal" => 1,
                _ => -3,
            },
            ArgTy::Lit("number") => if numeric.contains(&p) { 2 } else { -3 },
            ArgTy::Lit("boolean") => if matches!(p, "boolean" | "Boolean") { 2 } else { -3 },
            ArgTy::Lit("char") => match p {
                "char" | "Character" => 2,
                "int" | "long" => 1,
                _ => -3,
            },
            ArgTy::Lit("null") => if ["int", "long", "short", "byte", "double", "float", "boolean", "char"].contains(&p) { -3 } else { 0 },
            ArgTy::Lit(_) => 0,
            ArgTy::Named(t) => {
                if t == p {
                    return 2;
                }
                if self.is_subtype(t, p) {
                    return 1;
                }
                // 両方とも親が分かる型なのに関係が無ければ合わない（LocalDateTime と Date など）
                let known = |r: &mut Self, x: &str| jdk_supers(x).is_some() || jdk_known(x) || r.idx.is_some_and(|i| i.lookup(x).iter().any(|l| l.kind.is_type()));
                if known(self, t) && known(self, p) { -2 } else { 0 }
            }
        }
    }

    /// `t` が `p` の子孫か（JDK のよくある型の表と、プロジェクトの extends / implements）
    fn is_subtype(&mut self, t: &str, p: &str) -> bool {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue = vec![t.to_string()];
        while let Some(x) = queue.pop() {
            if !seen.insert(x.clone()) || seen.len() > 64 {
                continue;
            }
            let mut sup: Vec<String> = jdk_supers(&x).map(|v| v.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
            if let Some(idx) = self.idx {
                let files: Vec<String> = idx.lookup(&x).iter().filter(|l| l.kind.is_type()).take(4).map(|l| l.path.to_string()).collect();
                for f in files {
                    sup.extend(self.supers_in(&f, &x));
                }
            }
            if sup.iter().any(|s| s == p) {
                return true;
            }
            queue.extend(sup);
        }
        false
    }

    /// 定義の引数の宣言の型（分からないものは None）と、可変長引数か
    fn params(&mut self, l: &Loc) -> Option<(Vec<Option<String>>, bool)> {
        let body = self.param_text(l)?;
        let ts = [".ts", ".tsx", ".js", ".jsx", ".mts"].iter().any(|e| l.path.ends_with(e));
        if body.trim().is_empty() {
            return Some((Vec::new(), false));
        }
        // 深さ 0 の , で分ける
        let mut parts = Vec::new();
        let mut depth = 0;
        let mut cur = String::new();
        for c in body.chars() {
            match c {
                '(' | '<' | '[' | '{' => depth += 1,
                ')' | '>' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        parts.push(cur);
        let var = body.contains("...");
        let tys = parts
            .iter()
            .map(|p| {
                if ts {
                    let t = p.split_once(':')?.1;
                    let t = t.split('=').next().unwrap_or(t);
                    let w: String = t.trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
                    (!w.is_empty()).then_some(w)
                } else {
                    // 注釈・final を除き、名前の前の型（総称型は外側、可変長の ... と配列は外す）
                    let mut flat = String::new();
                    let mut d = 0;
                    for c in p.chars() {
                        match c {
                            '<' | '(' => d += 1,
                            '>' | ')' => d -= 1,
                            _ if d == 0 => flat.push(c),
                            _ => {}
                        }
                    }
                    let words: Vec<&str> = flat.split_whitespace().filter(|w| !w.starts_with('@') && *w != "final").collect();
                    let ty = words.len().checked_sub(2).map(|i| words[i])?;
                    let ty = ty.trim_end_matches("...").trim_end_matches("[]");
                    Some(ty.rsplit('.').next().unwrap_or(ty).to_string())
                }
            })
            .collect();
        Some((tys, var))
    }

    /// 定義の行の ( … ) の中身
    fn param_text(&mut self, l: &Loc) -> Option<String> {
        let lines = self.lines(&l.path)?;
        let first = lines.get(l.line as usize)?;
        let mut text = first.get(l.col as usize..)?.to_string();
        for extra in lines.iter().skip(l.line as usize + 1).take(12) {
            if text.contains(')') {
                break;
            }
            text.push(' ');
            text.push_str(extra);
        }
        let open = text.find('(')?;
        let mut depth = 0;
        let mut inner = String::new();
        for c in text[open..].chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if depth >= 1 {
                inner.push(c);
            }
        }
        Some(inner.trim_start_matches('(').to_string())
    }

    /// 定義の行の引数の数（可変長引数なら true）
    #[allow(dead_code)]
    fn param_count(&mut self, l: &Loc) -> Option<(usize, bool)> {
        let lines = self.lines(&l.path)?;
        let first = lines.get(l.line as usize)?;
        let mut text = first.get(l.col as usize..)?.to_string();
        for extra in lines.iter().skip(l.line as usize + 1).take(12) {
            if text.contains(')') {
                break;
            }
            text.push(' ');
            text.push_str(extra);
        }
        let open = text.find('(')?;
        let mut depth = 0;
        let mut commas = 0;
        let mut inner = String::new();
        for c in text[open..].chars() {
            match c {
                '(' | '<' | '[' | '{' => depth += 1,
                ')' | '>' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                ',' if depth == 1 => commas += 1,
                _ => {}
            }
            if depth >= 1 {
                inner.push(c);
            }
        }
        let body = inner.trim_start_matches('(').trim();
        if body.is_empty() {
            return Some((0, false));
        }
        Some((commas + 1, body.contains("...")))
    }

    fn resolve_ty(&mut self, from_rel: &str, src: &str, name: &str, ty: Option<Ty>, cands: Vec<Loc>) -> Ranked {
        let hier = match ty {
            Some(Ty::Outside(t)) => return Ranked::Outside(t),
            Some(t) => self.receiver_files(from_rel, src, name, t, &cands),
            None => Hier::Unknown,
        };
        let recv_files = match hier {
            Hier::Files(f) => Some(f),
            Hier::Outside(t) => return Ranked::Outside(t),
            Hier::Lombok(v) => {
                let n = v.len();
                return Ranked::Locs(v, n);
            }
            Hier::Unknown => None,
        };
        let (v, best) = self.rank_in(from_rel, src, name, recv_files, cands);
        Ranked::Locs(v, best)
    }

    /// 受け手の型。x.f は f の宣言の型、x.m() は m の戻り値の型を、定義の行から読む
    fn type_of(&mut self, from_rel: &str, src: &str, r: &Recv, depth: usize) -> Option<Ty> {
        match r {
            Recv::This => Some(Ty::This),
            Recv::Type(t) => Some(Ty::Named(t.clone(), None)),
            Recv::Member(inner, m) | Recv::Call(inner, m) => {
                if depth > 6 {
                    return None;
                }
                let it = self.type_of(from_rel, src, inner, depth + 1)?;
                if let Ty::Outside(t) = it {
                    return Some(Ty::Outside(t));
                }
                let idx = self.idx?;
                let cands = idx.lookup(m).to_vec();
                let none = cands.is_empty();
                match self.resolve_ty(from_rel, src, m, Some(it), cands) {
                    Ranked::Outside(t) => Some(Ty::Outside(t)),
                    // プロジェクトのどこにも無い（Lombok のフィールドでもない）メソッド・フィールド: ライブラリのもの（Mockito の mock() など）
                    Ranked::Locs(v, _) if v.is_empty() && none => Some(Ty::Outside(m.clone())),
                    Ranked::Locs(v, 1) => {
                        let l = &v[0];
                        let t = self.decl_type(l)?;
                        Some(Ty::Named(t, Some(l.path.to_string())))
                    }
                    Ranked::Locs(..) => None,
                }
            }
        }
    }

    fn lines(&mut self, path: &str) -> Option<Arc<Vec<String>>> {
        if let Some(v) = self.files.get(path) {
            return Some(v.clone());
        }
        let idx = self.idx?;
        let text = std::fs::read_to_string(idx.root.join(path)).ok()?;
        let v = Arc::new(text.lines().map(String::from).collect::<Vec<_>>());
        self.files.insert(path.to_string(), v.clone());
        Some(v)
    }

    /// 定義の行から、フィールドの型・メソッドの戻り値の型を読む
    fn decl_type(&mut self, l: &Loc) -> Option<String> {
        let lines = self.lines(&l.path)?;
        let line = lines.get(l.line as usize)?;
        let col = (l.col as usize).min(line.len());
        let ts = [".ts", ".tsx", ".js", ".jsx", ".mts"].iter().any(|e| l.path.ends_with(e));
        if ts { ts_decl_type(&line[col..]) } else { java_decl_type(&line[..col]) }
    }

    /// 受け手の型を定義しているファイル。そこに候補が無ければ親クラス・インタフェースへたどる
    fn receiver_files(&mut self, from_rel: &str, src: &str, name: &str, ty: Ty, cands: &[Loc]) -> Hier {
        let Some(idx) = self.idx else { return Hier::Unknown };
        let has = |files: &Files| cands.iter().any(|c| files.contains(&*c.path));
        let this = matches!(ty, Ty::This);
        // (型, その名前が書かれていたファイル)
        let mut types: Vec<(String, Option<String>)> = match ty {
            Ty::This => {
                let here: Files = [from_rel.to_string()].into();
                if has(&here) {
                    return Hier::Files(here);
                }
                supertypes(src, None).into_iter().map(|t| (t, Some(from_rel.to_string()))).collect()
            }
            Ty::Named(t, hint) => vec![(t, hint)],
            Ty::Outside(t) => return Hier::Outside(t),
        };
        let mut seen: Files = Files::new();
        // たどった型のファイル（Lombok の getter のフィールドを探す）と、プロジェクトに無かった型
        let mut visited: Files = Files::new();
        let mut outside: Option<String> = None;
        if this {
            visited.insert(from_rel.to_string());
        }
        for _ in 0..8 {
            types.retain(|(t, h)| seen.insert(format!("{}#{t}", h.as_deref().unwrap_or(""))));
            if types.is_empty() {
                break;
            }
            let mut files = Files::new();
            let mut level: Vec<(String, String)> = Vec::new();
            for (t, hint) in &types {
                let defs: Vec<Loc> = idx.lookup(t).iter().filter(|l| l.kind.is_type()).cloned().collect();
                // 1 文字の名前は型引数（T・E など）なので分からないものとする
                if defs.is_empty() && t.chars().count() > 1 && outside.is_none() {
                    outside = Some(t.clone());
                }
                // 名前が書かれていたファイルの中の型（内部クラス）→ 同じ名前が複数なら、そのファイルの import と同じモジュール
                let defs = match hint.as_deref().map(|h| defs.iter().filter(|l| &*l.path == h).cloned().collect::<Vec<_>>()) {
                    Some(inner) if !inner.is_empty() => inner,
                    _ if defs.len() > 1 => match hint.as_deref().filter(|h| *h != from_rel).and_then(|h| self.lines(h).map(|l| (h.to_string(), l.join("\n")))) {
                        Some((h, hsrc)) => narrow_candidates(&h, &hsrc, t, defs),
                        None => narrow_candidates(from_rel, src, t, defs),
                    },
                    _ => defs,
                };
                for d in &defs {
                    files.insert(d.path.to_string());
                    level.push((t.clone(), d.path.to_string()));
                }
            }
            if has(&files) {
                return Hier::Files(files);
            }
            visited.extend(files.iter().cloned());
            let mut next = Vec::new();
            for (t, f) in &level {
                next.extend(self.supers_in(f, t).into_iter().map(|s| (s, Some(f.clone()))));
            }
            types = next;
        }
        if visited.is_empty() && outside.is_none() {
            return Hier::Unknown;
        }
        // Lombok（@Getter / @Data）: getFoo / isFoo / setFoo はフィールド foo へ
        if let Some(field) = accessor_field(name) {
            let locs: Vec<Loc> = idx.lookup(&field).iter().filter(|l| l.kind == DefKind::Field && visited.contains(&*l.path)).cloned().collect();
            if !locs.is_empty() {
                return Hier::Lombok(locs);
            }
        }
        // フィールドそのもの（this.f が親クラスのフィールド）: 候補は f の定義なので、たどったファイルにあるもの
        match outside {
            Some(t) => Hier::Outside(t),
            None => Hier::Unknown,
        }
    }

    /// 受け手の型の名前（this なら今のファイルで `at` を囲む型）
    pub fn receiver_type_name(&mut self, from_rel: &str, src: &str, at: usize, recv: &Recv) -> Option<String> {
        match self.type_of(from_rel, src, recv, 0)? {
            Ty::Named(t, _) | Ty::Outside(t) => Some(t),
            Ty::This => enclosing_type(src, at),
        }
    }

    /// 型 `ty` を継承・実装している型（孫も）。`member` があれば、それらの型の中のその名前の定義。（名前, 場所）
    pub fn implementations(&mut self, ty: &str, member: Option<&str>) -> Vec<(String, Loc)> {
        let Some(idx) = self.idx else { return Vec::new() };
        // 子 → 親の表を、型の定義のあるファイルを読んで作る（呼ばれたときだけ）
        let types: Vec<(String, Loc)> = idx.all().filter(|(_, l)| l.kind.is_type()).map(|(n, l)| (n.to_string(), l.clone())).collect();
        let mut children: HashMap<String, Vec<(String, Loc)>> = HashMap::new();
        for (n, l) in &types {
            for sup in self.supers_in(&l.path, n) {
                children.entry(sup).or_default().push((n.clone(), l.clone()));
            }
        }
        let mut subs: Vec<(String, Loc)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = [ty.to_string()].into();
        let mut queue = vec![ty.to_string()];
        while let Some(t) = queue.pop() {
            for (n, l) in children.get(&t).into_iter().flatten() {
                if seen.insert(n.clone()) {
                    queue.push(n.clone());
                }
                if !subs.iter().any(|(_, x)| x == l) {
                    subs.push((n.clone(), l.clone()));
                }
            }
        }
        let mut out: Vec<(String, Loc)> = match member {
            None => subs,
            Some(m) => {
                let files: std::collections::HashSet<&str> = subs.iter().map(|(_, l)| &*l.path).collect();
                idx.lookup(m).iter().filter(|l| files.contains(&*l.path) && !l.kind.is_type()).map(|l| (m.to_string(), l.clone())).collect()
            }
        };
        out.sort_by(|a, b| a.1.path.cmp(&b.1.path).then(a.1.line.cmp(&b.1.line)));
        out
    }

    /// ファイル `path` の中の型 `t` の直接の親
    fn supers_in(&mut self, path: &str, t: &str) -> Vec<String> {
        let key = format!("{path}#{t}");
        if let Some(v) = self.supers.get(&key) {
            return v.clone();
        }
        let out = match self.lines(path) {
            Some(lines) => supertypes(&lines.join("\n"), Some(t)),
            None => Vec::new(),
        };
        self.supers.insert(key, out.clone());
        out
    }

    fn rank_in(&mut self, from_rel: &str, src: &str, name: &str, recv_files: Option<Files>, cands: Vec<Loc>) -> (Vec<Loc>, usize) {
        if cands.len() < 2 {
            let n = cands.len();
            return (cands, n);
        }
        // 全部が同じファイルの中（Java のクラスとそのコンストラクタなど）なら、型の定義を先に
        // 型が無ければ同じファイルのオーバーロードなので、引数で選ぶ（Java・Kotlin）
        if cands.iter().all(|c| c.path == cands[0].path) {
            let mut v = cands;
            v.sort_by_key(|c| (!c.kind.is_type(), c.line));
            // オーバーロードは Java・Kotlin だけ（TS の同じ名前は get / set の対やシグネチャの宣言）
            let jvm = [".java", ".kt", ".kts"].iter().any(|e| v[0].path.ends_with(e));
            let best = if v[0].kind.is_type() || !jvm { 1 } else { v.len() };
            return (v, best);
        }
        // 言語のまとまり（Java と Kotlin、TS と JS は同じ）
        let family = |p: &str| match p.rsplit('.').next().unwrap_or("") {
            "java" | "kt" | "kts" => 1,
            "ts" | "tsx" | "js" | "jsx" | "mts" | "mjs" | "cts" | "cjs" => 2,
            _ => 0,
        };
        let hit = import_hits(from_rel, src, name, &cands);
        // TS / JS: 別のファイルの export していない関数・定数は使えない
        let hidden: Files = cands
            .iter()
            .filter(|c| &*c.path != from_rel && family(&c.path) == 2 && !c.kind.is_type() && !matches!(c.kind, DefKind::Method | DefKind::Field))
            .filter(|c| self.lines(&c.path).and_then(|l| l.get(c.line as usize).cloned()).is_some_and(|l| !l.trim_start().starts_with("export")))
            .map(|c| c.path.to_string())
            .collect();
        let upper = name.chars().next().is_some_and(char::is_uppercase);
        let common = |p: &str| p.split('/').zip(from_rel.split('/')).take_while(|(a, b)| a == b).count();
        let mine = family(from_rel);
        let key = |c: &Loc| {
            (
                recv_files.as_ref().is_some_and(|f| !f.contains(&*c.path)),
                mine != 0 && family(&c.path) != mine,
                hidden.contains(&*c.path),
                !hit.is_empty() && !hit.contains(&*c.path),
                upper && !c.kind.is_type(),
                std::cmp::Reverse(common(&c.path)),
            )
        };
        let mut v = cands;
        v.sort_by(|a, b| key(a).cmp(&key(b)).then_with(|| a.path.cmp(&b.path)).then(a.line.cmp(&b.line)));
        let top = key(&v[0]);
        let best = v.iter().take_while(|c| key(c) == top).count();
        (v, best)
    }
}

/// Java: 名前の前の型（`public Criteria createCriteria(` / `private Foo bar;` → Criteria / Foo）。総称型は外側
fn java_decl_type(before: &str) -> Option<String> {
    let mut b = before.trim_end();
    // 配列は分からないものとする
    if b.ends_with(']') {
        return None;
    }
    if b.ends_with('>') {
        let mut depth = 0;
        let mut cut = None;
        for (i, c) in b.char_indices().rev() {
            match c {
                '>' => depth += 1,
                '<' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        b = b[..cut?].trim_end();
    }
    let word: String = b.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    let last = word.rsplit('.').next().unwrap_or("").to_string();
    let not_type = ["void", "public", "private", "protected", "static", "final", "abstract", "synchronized", "default", "native", "transient", "volatile", "int", "long", "short", "byte", "char", "boolean", "float", "double", "var", "return", "new"];
    (!last.is_empty() && last.chars().next().is_some_and(|c| c.is_alphabetic()) && !not_type.contains(&last.as_str())).then_some(last)
}

/// TS: 名前の後ろの型（`f: Foo` / `m(a, b): Foo {` / `x = inject(Foo)` / `x = new Foo(`）
fn ts_decl_type(after: &str) -> Option<String> {
    let mut a = after.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '$').trim_start().trim_start_matches(['?', '!']).trim_start();
    if a.starts_with('(') || a.starts_with('<') {
        // 引数の ( … ) を飛ばす
        let mut depth = 0;
        let mut end = None;
        for (i, c) in a.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        a = a[end? + 1..].trim_start();
    }
    let ident = |s: &str| -> Option<String> {
        let w: String = s.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == '.').collect();
        let last = w.rsplit('.').next().unwrap_or("").to_string();
        let prim = ["string", "number", "boolean", "any", "unknown", "void", "never", "object", "null", "undefined"];
        (!last.is_empty() && !prim.contains(&last.as_str())).then_some(last)
    };
    if let Some(t) = a.strip_prefix(':') {
        return ident(t);
    }
    let v = a.strip_prefix('=')?.trim_start();
    if let Some(x) = v.strip_prefix("inject(") {
        return ident(x);
    }
    if let Some(x) = v.strip_prefix("new ") {
        return ident(x);
    }
    None
}

/// `at` より前で最後に宣言されたクラス・インタフェースの名前（`at` を囲む型の近似）
pub fn enclosing_type(src: &str, at: usize) -> Option<String> {
    let head = &src[..at.min(src.len())];
    let mut best: Option<(usize, String)> = None;
    for kw in ["class ", "interface ", "enum ", "record "] {
        let mut from = 0;
        while let Some(i) = head[from..].find(kw).map(|i| i + from) {
            from = i + kw.len();
            if i > 0 && head.as_bytes()[i - 1].is_ascii_alphanumeric() {
                continue;
            }
            let name: String = head[from..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            if !name.is_empty() && best.as_ref().is_none_or(|(b, _)| i > *b) {
                best = Some((i, name));
            }
        }
    }
    best.map(|(_, n)| n)
}

/// クラス・インタフェースの宣言の extends / implements にある型の名前（`of` なら、その名前の宣言だけ）
pub fn supertypes(src: &str, of: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    for kw in ["class ", "interface "] {
        let mut from = 0;
        while let Some(i) = src[from..].find(kw).map(|i| i + from) {
            from = i + kw.len();
            if i > 0 && ident(b[i - 1]) {
                continue;
            }
            let rest = &src[from..];
            let name: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            if name.is_empty() || of.is_some_and(|o| o != name) {
                continue;
            }
            let head = &rest[..rest.find(['{', ';']).unwrap_or(rest.len()).min(600)];
            // 総称型の <…> を消す
            let mut flat = String::new();
            let mut depth = 0;
            for c in head.chars() {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ if depth == 0 => flat.push(c),
                    _ => {}
                }
            }
            let mut on = false;
            for w in flat.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '.')).filter(|w| !w.is_empty()) {
                match w {
                    "extends" | "implements" => on = true,
                    "permits" | "with" => on = false,
                    w if on => out.push(w.rsplit('.').next().unwrap_or(w).to_string()),
                    _ => {}
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// import している完全な名前・相対パスに当たる候補のファイル
fn import_hits(from_rel: &str, src: &str, name: &str, cands: &[Loc]) -> std::collections::HashSet<String> {
    let from_dir: Vec<&str> = from_rel.split('/').collect::<Vec<_>>().split_last().map_or(Vec::new(), |x| x.1.to_vec());
    let stem = |p: &str| p.rsplit_once('.').map_or(p.to_string(), |(a, _)| a.to_string());
    let mut hit = std::collections::HashSet::new();
    for (names, spec) in imports_of(src) {
        // TS / JS（相対パス・パッケージ名）と Java（a.b.C）を分ける
        if spec.contains('/') || spec.starts_with('.') || !spec.contains('.') {
            // TS / JS: その名前を取り込んでいるモジュールのファイル
            if !names.iter().any(|n| n == name) {
                continue;
            }
            if !spec.starts_with('.') {
                // パッケージ（tsconfig の paths・ワークスペース）: その名前のフォルダの中のもの
                let pkg = spec.trim_start_matches('@').split('/').next_back().unwrap_or("");
                if !pkg.is_empty() {
                    hit.extend(cands.iter().filter(|c| c.path.split('/').any(|seg| seg == pkg)).map(|c| c.path.to_string()));
                }
                continue;
            }
            let mut parts = from_dir.clone();
            for seg in spec.split('/') {
                match seg {
                    "." | "" => {}
                    ".." => {
                        parts.pop();
                    }
                    s => parts.push(s),
                }
            }
            let target = parts.join("/");
            hit.extend(cands.iter().filter(|c| stem(&c.path) == target || stem(&c.path) == format!("{target}/index")).map(|c| c.path.to_string()));
        } else if spec.ends_with(".*") || names.iter().any(|n| n == name) {
            // Java: a.b.C → a/b/C.java、a.b.* → a/b/
            let path = spec.trim_end_matches(".*").replace('.', "/");
            let want = if spec.ends_with(".*") { format!("{path}/{name}") } else { path };
            hit.extend(cands.iter().filter(|c| stem(&c.path).ends_with(&want)).map(|c| c.path.to_string()));
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc_at(p: &str) -> Loc {
        Loc { path: p.into(), line: 0, col: 0, kind: DefKind::Function }
    }

    /// 受け手の型のファイル → 無ければ親クラスへたどる
    #[test]
    fn rank_by_receiver() {
        let dir = std::env::temp_dir().join(format!("nagi-rank-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/Base.java"), "abstract class Base<T> implements Saver {\n  void save() {}\n}\n").unwrap();
        std::fs::write(dir.join("src/Repo.java"), "class Repo extends Base<Item> {\n  void find() {}\n}\n").unwrap();
        std::fs::write(dir.join("src/Other.java"), "class Other {\n  void save() {}\n  void find() {}\n}\n").unwrap();
        std::fs::write(dir.join("src/Saver.java"), "interface Saver {\n  void flush();\n}\n").unwrap();
        std::fs::write(dir.join("src/Item.java"), "class Item {\n  void flush() {}\n}\n").unwrap();
        std::fs::write(dir.join("src/Dto.java"), "@Data\nclass Dto extends BaseDto {\n  private String oksmNo;\n}\n").unwrap();
        std::fs::write(dir.join("src/BaseDto.java"), "@Getter\nclass BaseDto {\n  private boolean active;\n}\n").unwrap();
        std::fs::write(dir.join("src/UserRepo.java"), "interface UserRepo extends JpaRepository<User, Long> {\n}\n").unwrap();
        std::fs::write(dir.join("src/Holder.java"), "class Holder {\n  void findById() {}\n  void get() {}\n}\n").unwrap();
        // MyBatis の Example（内部クラスの Criteria がファイルごとにある）
        for ex in ["FooExample", "BarExample"] {
            let body = format!("class {ex} {{\n  public Criteria createCriteria() {{}}\n  public static class Criteria extends GeneratedCriteria {{\n  }}\n  protected abstract static class GeneratedCriteria {{\n    public Criteria andNoEqualTo(String v) {{}}\n  }}\n}}\n");
            std::fs::write(dir.join(format!("src/{ex}.java")), body).unwrap();
        }
        // Lombok の getter の連鎖
        std::fs::write(dir.join("src/Req.java"), "@Data\nclass Req {\n  private List<Jokn> list;\n  private Jokn jokn;\n}\n").unwrap();
        std::fs::write(dir.join("src/Jokn.java"), "@Data\nclass Jokn {\n  private String cd;\n}\n").unwrap();
        std::fs::write(dir.join("src/Z.java"), "class Z {\n  private String cd;\n}\n").unwrap();
        // TS: 基底クラスのフィールド
        std::fs::write(dir.join("src/base.ts"), "export class Base {\n  protected store = inject(SessionStore);\n  form(): FormGroup { return null; }\n}\n").unwrap();
        std::fs::write(dir.join("src/store.ts"), "export class SessionStore {\n  set(v: number) {}\n}\n").unwrap();
        std::fs::write(dir.join("src/m.ts"), "export class M {\n  set(v) {}\n  get(k) {}\n}\n").unwrap();
        let rx = build(&dir, Arc::new(|| {}));
        let idx = loop {
            if let Ok(Progress::Done(i)) = rx.recv() {
                break i;
            }
        };
        let mut r = Ranker::new(Some(&idx));
        let src = "class Use { Repo repo; }\n";
        let rank = |r: &mut Ranker, name: &str, recv: Option<&Recv>| {
            let (v, best) = r.rank("src/Use.java", src, name, recv, idx.lookup(name).to_vec());
            (v.iter().map(|l| l.path.to_string()).collect::<Vec<_>>(), best)
        };
        // repo.find(): Repo のファイル
        assert_eq!(rank(&mut r, "find", Some(&Recv::Type("Repo".into()))), (vec!["src/Repo.java".into(), "src/Other.java".into()], 1));
        // repo.save(): Repo に無いので親の Base
        assert_eq!(rank(&mut r, "save", Some(&Recv::Type("Repo".into()))).0[0], "src/Base.java");
        assert_eq!(rank(&mut r, "save", Some(&Recv::Type("Repo".into()))).1, 1);
        // repo.flush(): Base の implements Saver
        assert_eq!(rank(&mut r, "flush", Some(&Recv::Type("Repo".into()))), (vec!["src/Saver.java".into(), "src/Item.java".into()], 1));
        // 受け手が分からなければ選ばせる
        assert_eq!(rank(&mut r, "save", None).1, 2);
        // this.save()（Repo の中）: 今のファイルに無いので extends をたどる
        let repo_src = std::fs::read_to_string(dir.join("src/Repo.java")).unwrap();
        let (v, best) = r.rank("src/Repo.java", &repo_src, "save", Some(&Recv::This), idx.lookup("save").to_vec());
        assert_eq!((&*v[0].path, best), ("src/Base.java", 1));
        assert_eq!(supertypes("export class A extends B<C> implements D, E.F {", None), vec!["B", "D", "F"]);
        let res = |r: &mut Ranker, name: &str, t: &str| match r.resolve("src/Use.java", src, name, Some(&Recv::Type(t.into())), None, idx.lookup(name).to_vec()) {
            Ranked::Locs(v, b) => format!("{} {b}", v.iter().map(|l| format!("{}:{}", l.path, l.line)).collect::<Vec<_>>().join(",")),
            Ranked::Outside(t) => format!("外 {t}"),
        };
        // Lombok: dto.getOksmNo() → フィールド oksmNo、親の isActive() → 親のフィールド
        assert_eq!(res(&mut r, "getOksmNo", "Dto"), "src/Dto.java:2 1");
        assert_eq!(res(&mut r, "isActive", "Dto"), "src/BaseDto.java:2 1");
        // ライブラリの型・ライブラリの親: プロジェクトの同じ名前へは跳ばない
        assert_eq!(res(&mut r, "get", "FormGroup"), "外 FormGroup");
        assert_eq!(res(&mut r, "findById", "UserRepo"), "外 JpaRepository");
        let call = |r: Recv, m: &str| Recv::Call(Box::new(r), m.into());
        let chain = |r: &mut Ranker, from: &str, src: &str, name: &str, recv: Recv| match r.resolve(from, src, name, Some(&recv), None, idx.lookup(name).to_vec()) {
            Ranked::Locs(v, b) => format!("{} {b}", v.iter().take(b).map(|l| format!("{}:{}", l.path, l.line)).collect::<Vec<_>>().join(",")),
            Ranked::Outside(t) => format!("外 {t}"),
        };
        // example.createCriteria().andNoEqualTo(): FooExample の中の Criteria → その親の GeneratedCriteria
        assert_eq!(chain(&mut r, "src/Use.java", src, "andNoEqualTo", call(Recv::Type("FooExample".into()), "createCriteria")), "src/FooExample.java:5 1");
        // req.getJokn().getCd(): Lombok のフィールド jokn の型 Jokn → フィールド cd
        assert_eq!(chain(&mut r, "src/Use.java", src, "getCd", call(Recv::Type("Req".into()), "getJokn")), "src/Jokn.java:2 1");
        // req.getList().get(0): List はライブラリ
        assert_eq!(chain(&mut r, "src/Use.java", src, "get", call(Recv::Type("Req".into()), "getList")), "外 List");
        // TS: this.store.set()（store は基底クラスのフィールド）、this.form().get()（戻り値はライブラリの型）
        let page = "export class Page extends Base {\n  f() { this.store.set(1); }\n}\n";
        assert_eq!(chain(&mut r, "src/page.ts", page, "set", Recv::Member(Box::new(Recv::This), "store".into())), "src/store.ts:1 1");
        assert_eq!(chain(&mut r, "src/page.ts", page, "get", call(Recv::This, "form")), "外 FormGroup");
        // 実装: Saver を実装しているのは Base（implements）とその子の Repo。flush の実装は Saver を実装した型の中
        let impls = |r: &mut Ranker, t: &str, m: Option<&str>| r.implementations(t, m).iter().map(|(_, l)| format!("{}:{}", l.path, l.line)).collect::<Vec<_>>();
        assert_eq!(impls(&mut r, "Saver", None), vec!["src/Base.java:0", "src/Repo.java:0"]);
        assert_eq!(impls(&mut r, "Base", Some("find")), vec!["src/Repo.java:1"]);
        assert_eq!(impls(&mut r, "GeneratedCriteria", None).len(), 2, "内部クラスの Criteria（2 つのファイル）");
        assert_eq!(enclosing_type("interface A {\n  void f();\n}\nclass B implements A {\n  void g() {}\n", 60).as_deref(), Some("B"));
        // mock(Repo.class).save(): mock はプロジェクトに無いのでライブラリ
        assert_eq!(chain(&mut r, "src/Use.java", src, "save", call(Recv::This, "mock")), "外 mock");
        assert_eq!(java_decl_type("    public Map<String, List<Foo>> "), Some("Map".into()));
        assert_eq!(ts_decl_type("load(a: string, b = 1): Observable<Foo> {"), Some("Observable".into()));
        // this.getOksmNo()（Lombok、親クラスなし）: 今のファイルのフィールド
        assert_eq!(chain(&mut r, "src/Dto.java", "@Data\nclass Dto extends BaseDto {\n  private String oksmNo;\n}\n", "getOksmNo", Recv::This), "src/Dto.java:2 1");
        let jokn = std::fs::read_to_string(dir.join("src/Jokn.java")).unwrap();
        assert_eq!(chain(&mut r, "src/Jokn.java", &jokn, "getCd", Recv::This), "src/Jokn.java:2 1");
        // オーバーロード: 引数の数で選ぶ
        std::fs::write(dir.join("src/DateUtil.java"), "class DateUtil {\n  static String format(Date d) {}\n  static String format(Date d, String p) {}\n  static String format(String... xs) {}\n}\n").unwrap();
        std::fs::write(dir.join("src/Fmt.java"), "class Fmt {\n  void format() {}\n}\n").unwrap();
        let idx2 = {
            let rx = build(&dir, Arc::new(|| {}));
            loop {
                if let Ok(Progress::Done(i)) = rx.recv() {
                    break i;
                }
            }
        };
        let mut r2 = Ranker::new(Some(&idx2));
        let over = |r: &mut Ranker, n: usize| {
            let args = vec![Arg::Unknown; n];
            match r.resolve("src/Use.java", src, "format", Some(&Recv::Type("DateUtil".into())), Some(&args), idx2.lookup("format").to_vec()) {
                Ranked::Locs(v, b) => (v[0].line, b),
                Ranked::Outside(_) => (99, 0),
            }
        };
        assert_eq!(over(&mut r2, 2), (2, 1));
        assert_eq!(over(&mut r2, 3), (3, 1), "可変長");
        assert_eq!(over(&mut r2, 1), (1, 1), "1 引数は可変長よりぴったりの format(Date)");
        // 引数の型: 同じ数のオーバーロード
        std::fs::write(dir.join("src/Fmt2.java"), "class Fmt2 {\n  static String fmt(TemporalAccessor t, String f) {}\n  static String fmt(Date d, String f) {}\n  static String fmt(@NonNull final Integer n, String f) {}\n  static String fmt(Base b, String f) {}\n}\n").unwrap();
        let idx4 = {
            let rx = build(&dir, Arc::new(|| {}));
            loop {
                if let Ok(Progress::Done(i)) = rx.recv() {
                    break i;
                }
            }
        };
        let mut r4 = Ranker::new(Some(&idx4));
        let mut by_ty = |a: Arg| match r4.resolve("src/Use.java", src, "fmt", Some(&Recv::Type("Fmt2".into())), Some(&[a, Arg::Lit("String")]), idx4.lookup("fmt").to_vec()) {
            Ranked::Locs(v, b) => (v[0].line, b),
            Ranked::Outside(_) => (99, 0),
        };
        let ty = |t: &str| Arg::Expr(Recv::Type(t.into()));
        assert_eq!(by_ty(ty("LocalDateTime")), (1, 1), "LocalDateTime は TemporalAccessor");
        assert_eq!(by_ty(ty("Date")), (2, 1));
        assert_eq!(by_ty(ty("Timestamp")), (2, 1), "Timestamp は Date の子");
        assert_eq!(by_ty(Arg::Lit("int")), (3, 1));
        assert_eq!(by_ty(ty("Repo")), (4, 1), "プロジェクトの型: Repo extends Base");
        assert_eq!(by_ty(Arg::Unknown).1, 4, "分からなければ選ばせる");
        // TS: export していない同じ名前の関数は後ろへ
        std::fs::write(dir.join("src/va.ts"), "function notBlank(m) {}\nexport const use = 1;\n").unwrap();
        std::fs::write(dir.join("src/vb.ts"), "export const notBlank = (m) => m;\n").unwrap();
        let idx3 = {
            let rx = build(&dir, Arc::new(|| {}));
            loop {
                if let Ok(Progress::Done(i)) = rx.recv() {
                    break i;
                }
            }
        };
        let (v, b) = Ranker::new(Some(&idx3)).rank("src/page.ts", "notBlank('x');\n", "notBlank", None, idx3.lookup("notBlank").to_vec());
        assert_eq!((&*v[0].path, b), ("src/vb.ts", 1));
        // TS のパッケージ名の import（tsconfig の paths）
        let cands = vec![loc_at("projects/my-shared-lib/src/v.ts"), loc_at("projects/other/src/v.ts")];
        assert_eq!(narrow_candidates("projects/app/src/p.ts", "import {\n  notBlank,\n} from 'my-shared-lib';\n", "notBlank", cands)[0].path.as_ref(), "projects/my-shared-lib/src/v.ts");
        // 型引数（1 文字）は分からないものとして選ばせる
        assert_eq!(res(&mut r, "get", "T"), "src/Holder.java:2,src/m.ts:2 1");
    }

    #[test]
    fn narrow() {
        let loc = |p: &str| Loc { path: p.into(), line: 0, col: 0, kind: nagi_syntax::defs::DefKind::Class };
        // Java: 同じパッケージがモジュールごとに複製されている → 同じモジュールのもの
        let cands = vec![loc("app.core/src/main/java/com/x/error/BusinessException.java"), loc("rest.a/src/main/java/com/x/error/BusinessException.java")];
        let src = "package com.x.svc;\nimport com.x.error.BusinessException;\n";
        let r = narrow_candidates("rest.a/src/main/java/com/x/svc/Foo.java", src, "BusinessException", cands.clone());
        assert_eq!(r.len(), 1);
        assert!(r[0].path.starts_with("rest.a/"));
        // import で決まる
        let cands = vec![loc("m/src/com/a/Item.java"), loc("m/src/com/b/Item.java")];
        let r = narrow_candidates("m/src/com/c/Use.java", "import com.b.Item;\n", "Item", cands);
        assert_eq!(&*r[0].path, "m/src/com/b/Item.java");
        assert_eq!(r.len(), 1);
        // TS: 相対の import を解く
        let cands = vec![loc("src/app/a/user.service.ts"), loc("src/app/b/user.service.ts")];
        let src = "import { Injectable } from '@angular/core';\nimport { UserService } from '../b/user.service';\n";
        let r = narrow_candidates("src/app/pages/page.component.ts", src, "UserService", cands);
        assert_eq!(r.len(), 1);
        assert_eq!(&*r[0].path, "src/app/b/user.service.ts");
        assert_eq!(imports_of("import {\n  A,\n  B as C,\n} from './x';\n")[0], (vec!["A".to_string(), "B".into(), "C".into()], "./x".to_string()));
        // クラスとそのコンストラクタ（同じファイル）→ クラス
        let mut k = loc("m/E.java");
        k.kind = nagi_syntax::defs::DefKind::Method;
        k.line = 9;
        let r = narrow_candidates("m/Use.java", "", "E", vec![k.clone(), loc("m/E.java"), k]);
        assert_eq!((r.len(), r[0].line), (1, 0));
    }

    #[test]
    fn build_save_and_incremental() {
        let dir = std::env::temp_dir().join(format!("nagi-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        // 索引の保存先も一時フォルダへ
        unsafe { std::env::set_var("NAGI_INDEX_DIR", dir.join("cache")) };
        std::fs::write(dir.join("src/A.java"), "class A { void run() {} }\n").unwrap();
        std::fs::write(dir.join("src/b.ts"), "export function helper() {}\nexport class B {}\n").unwrap();
        std::fs::write(dir.join("readme.md"), "# x\n").unwrap();
        let wait = |rx: Receiver<Progress>| loop {
            if let Ok(Progress::Done(i)) = rx.recv() {
                return i;
            }
        };
        let idx = wait(build(&dir, Arc::new(|| {})));
        assert_eq!(idx.file_count(), 2);
        assert_eq!(idx.lookup("run").len(), 1);
        assert_eq!(&*idx.lookup("helper")[0].path, "src/b.ts");
        assert_eq!(idx.lookup("B")[0].kind, DefKind::Class);
        // 2 回目は保存したものを使う。変えたファイルだけ解析し直す
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("src/b.ts"), "export function renamed() {}\n").unwrap();
        let idx = wait(build(&dir, Arc::new(|| {})));
        assert!(idx.lookup("helper").is_empty());
        assert_eq!(idx.lookup("renamed").len(), 1);
        assert_eq!(idx.lookup("run").len(), 1, "変えていないファイルは保存したものから");
        // 1 ファイルの差し替え
        let mut idx = idx;
        idx.replace_file("src/A.java", (1, 1), vec![Entry { name: "stop".into(), kind: DefKind::Method, line: 0, col: 0 }]);
        assert!(idx.lookup("run").is_empty());
        assert_eq!(idx.lookup("stop").len(), 1);
    }
}
