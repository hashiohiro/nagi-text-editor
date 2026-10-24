//! Rust の差し込み口。
//!
//! 定義の索引は `impl` の中の関数を型に結びつけないので、ファイルの `impl X { … }`・`impl T for X { … }` の範囲を読んで結びつける。
//! - `x.` の後: 構造体のフィールド（索引）に加えて、`impl` の中の `self` を取るメソッド
//! - `Type::` の後: enum の値・関連関数・関連定数（`self` を取るメソッドは後に）。`Self::` は今いる `impl` の型
//! - `module::` の後（`crate::`・`self::`・`super::`・クレート名・`mod` の名前）: そのモジュールの上の段の項目と下のモジュール
//!
//! 標準ライブラリと依存のクレートは生態系 Cargo の索引から（`types`）: 式の型を戻り値の型でたどり、その型の固有のメソッド・
//! Deref の先のメソッド・範囲にある（prelude と use した）トレイトのメソッド。`std::collections::`・`Vec::` の後も

use std::collections::HashSet;

use nagi_syntax::defs::DefKind;
use nagi_workers::index::{Entry, Index};

use crate::eco;
use crate::lang::{Cx, Found, Item, LibMember, Language, Target, mark_of};
use crate::project::Ranker;
use crate::text::{C_LIKE, Lexis};

pub mod types;

/// Rust の字句（`'a` は引用符でない、`new` は無い）
pub const LX: Lexis = Lexis { quotes: &['"'], new_keyword: None, ..C_LIKE };

pub struct Rust;

pub static RUST: Rust = Rust;

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "Self", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok", "Err",
];

impl Language for Rust {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn lexis(&self) -> &'static Lexis {
        &LX
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    fn type_ends_at_brace(&self) -> bool {
        true
    }

    fn trigger(&self, before: &str) -> bool {
        before.ends_with('.') || before.ends_with("::")
    }

    /// 今いる `impl X { … }` の X（無ければ既定の求め方）
    fn enclosing_type(&self, src: &str, at: usize) -> Option<String> {
        enclosing_impl(src, at).or_else(|| crate::project::enclosing_type(&LX, src, at))
    }

    /// `let mut x: &Foo = …`・`let x = Foo::new(…)`・`let x = Foo { … }`・引数の `x: &mut Foo`
    fn var_type(&self, src: &str, name: &str, at: usize) -> Option<String> {
        if !src.contains(name) {
            return None;
        }
        // いちばん近い宣言がクロージャの引数・for・if let なら、ここでは決めない（ライブラリの型からたどる `types` に任せる）
        let near = types::nearest_decl(&src[..at.min(src.len())], name);
        if near.as_ref().is_none_or(|(_, d)| !matches!(d, types::Decl::Typed(_) | types::Decl::Expr(_))) {
            return None;
        }
        // 名前を入れずに一度だけ作り、合ったものの名前を比べる（呼ぶたびに作ると遅い）
        static PATS: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
        let [decl, param] = PATS.get_or_init(|| {
            [
                regex::Regex::new(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::\s*&?(?:'\w+\s+)?(?:mut\s+)?([A-Z]\w*))?\s*=\s*&?(?:mut\s+)?(?:([A-Z]\w*)\s*(?:::|\{|\())?").unwrap(),
                regex::Regex::new(r"[(,]\s*(?:mut\s+)?(\w+)\s*:\s*&?(?:'\w+\s+)?(?:mut\s+)?([A-Z]\w*)").unwrap(),
            ]
        });
        let mut all: Vec<(usize, String)> = Vec::new();
        for c in decl.captures_iter(src).filter(|c| &c[1] == name) {
            let t = c.get(2).or(c.get(3)).map_or(String::new(), |m| m.as_str().to_string());
            all.push((c.get(0).map_or(0, |m| m.start()), t));
        }
        for c in param.captures_iter(src).filter(|c| &c[1] == name) {
            all.push((c.get(0).map_or(0, |m| m.start()), c[2].to_string()));
        }
        crate::java::types::pick(all, at).filter(|t| !t.is_empty())
    }

    /// `impl` の中の、`self` を取るメソッド（`.` の後）
    fn inherited(&self, cx: &Cx, _ranker: &mut Ranker, ty: &str, rank: u8) -> Vec<Item> {
        let Some(idx) = cx.index else { return Vec::new() };
        impl_items(idx, cx, ty).into_iter().filter(|m| m.method).map(|m| Item::member(m.name, true, m.params > 0, ty, rank)).collect()
    }

    fn ecosystem(&self) -> Option<&'static str> {
        Some(eco::CARGO)
    }

    /// `.` の前の式の型をライブラリの戻り値の型でたどって、その型のメソッド（プロジェクトの型ならそのメンバーも）
    fn library_members(&self, cx: &Cx, _ranker: &mut Ranker, expanded: &str, _fqn: Option<&(String, String)>, _statics: bool, found: bool) -> Vec<LibMember> {
        let lib = eco::cargo(cx.libs);
        let tys = types::Types::new(cx, lib.as_deref());
        let Some(t) = tys.expr_type(types::chain_text(expanded), cx.start, 0) else { return Vec::new() };
        let name = eco::cargo::type_name(&t);
        let mut out = Vec::new();
        if !found && tys.is_project(&name) {
            out.push(LibMember::Project(name));
        }
        out.extend(tys.members(&t, false).into_iter().map(LibMember::Item));
        out
    }

    /// ライブラリの型のメソッド・ライブラリのモジュールの項目へ（受け手の型がライブラリと分かるときだけ。分からなければいつもの手順）
    fn definition(&self, cx: &Cx, name: &str) -> Option<Found> {
        let lib = eco::cargo(cx.libs)?;
        let tys = types::Types::new(cx, Some(&lib));
        let before = &cx.line[..cx.ws];
        let t = lib_site(cx, &tys, before)?;
        let (path, line, owner) = match t {
            Site::Member(t) => tys.member_loc(&t, name)?,
            Site::Module(m) => lib.module(&m).find(|i| &*i.name == name).map(|i| (lib.file(i.file), i.line, m.clone()))?,
        };
        Some(Found::Jump(Target { path: path.to_path_buf(), line: line as usize, col: 0, note: Some(format!("ライブラリ: {owner}::{name}（ソース。読むだけ）")) }))
    }

    /// 型の名前（`String`・`HashMap`）など、プロジェクトに無い名前
    fn library_definition(&self, cx: &Cx, name: &str) -> Option<Target> {
        if let Some(Found::Jump(t)) = self.definition(cx, name) {
            return Some(t);
        }
        let lib = eco::cargo(cx.libs)?;
        let (path, line) = lib.type_loc(name)?;
        Some(Target { path: path.to_path_buf(), line: line as usize, col: 0, note: Some(format!("ライブラリ: {name}（ソース。読むだけ）")) })
    }

    fn library_site(&self, cx: &Cx, _name: &str) -> bool {
        let Some(lib) = eco::cargo(cx.libs) else { return false };
        lib_site(cx, &types::Types::new(cx, Some(&lib)), &cx.line[..cx.ws]).is_some()
    }

    /// `Type::`・`module::` の後
    fn complete_all(&self, cx: &Cx) -> Option<Vec<Item>> {
        let before = &cx.line[..cx.ws];
        if !before.ends_with("::") {
            return None;
        }
        let path: Vec<&str> = path_before(&before[..before.len() - 2]);
        let last = *path.last()?;
        let last = if last == "Self" { cx.inside.clone()? } else { last.to_string() };
        let project_type = cx.index.is_some_and(|i| i.lookup(&last).iter().any(|l| l.kind.is_type()));
        // ライブラリのモジュール（std::collections::）・型（Vec::・HashMap::）
        if let Some(lib) = eco::cargo(cx.libs).filter(|_| !project_type) {
            let module = path.join("::");
            if lib.has_module(&module) {
                return Some(lib.module(&module).map(|m| Item { name: m.name.to_string(), mark: lib_mark(m.kind), note: module.clone(), call: (m.kind == 'f').then_some(true), rank: 0, insert: None, import: None }).collect());
            }
            if lib.items_of(&last).next().is_some() {
                return Some(types::Types::new(cx, Some(&lib)).members(&last, true));
            }
        }
        let idx = cx.index?;
        // 型: enum の値・関連関数・定数（self を取るメソッドは後に）
        if idx.lookup(&last).iter().any(|l| l.kind.is_type()) {
            let mut out: Vec<Item> = Vec::new();
            for (name, kind, owner, _, _) in Ranker::new(Some(idx)).members(&last) {
                if kind == DefKind::Constant {
                    out.push(Item::new(name, 'e', owner, 0));
                }
            }
            for m in impl_items(idx, cx, &last) {
                let item = match m.kind {
                    DefKind::Function | DefKind::Method => Item::member(m.name, true, m.params > 0, last.as_str(), u8::from(m.method)),
                    k => Item::new(m.name, mark_of(k), last.as_str(), 0),
                };
                if !out.iter().any(|x| x.name == item.name) {
                    out.push(item);
                }
            }
            return Some(out);
        }
        // モジュール: その上の段の項目と下のモジュール
        let files: HashSet<&str> = idx.files().collect();
        let file = module_file(&files, &cx.rel, &path)?;
        let mut out = Vec::new();
        let text = std::fs::read_to_string(idx.root.join(&file)).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        for d in idx.defs_in(&file) {
            let top = lines.get(d.line as usize).is_some_and(|l| !l.starts_with([' ', '\t']));
            if top && !out.iter().any(|x: &Item| x.name == d.name) {
                let call = matches!(d.kind, DefKind::Function | DefKind::Method).then_some(true);
                out.push(Item { name: d.name.clone(), mark: mark_of(d.kind), note: file.clone(), call, rank: 0, insert: None, import: None });
            }
        }
        Some(out)
    }
}

/// `impl` の範囲（型の名前、トレイトの名前（無ければ空）、`{` と対応する `}` の位置）
struct ImplBlock {
    ty: String,
    trait_: String,
    open: usize,
    close: usize,
}

/// `at` を囲む `impl` の型（`at` より前の impl を近い順に見る）
fn enclosing_impl(src: &str, at: usize) -> Option<String> {
    let at = at.min(src.len());
    let mut end = at;
    while let Some(i) = src[..end].rfind("impl") {
        end = i;
        // 行の頭（pub・unsafe の後）の impl だけ
        let ls = src[..i].rfind('\n').map_or(0, |k| k + 1);
        let pre = src[ls..i].trim();
        if !(pre.is_empty() || pre == "unsafe" || pre.starts_with("pub")) || !src[i + 4..].starts_with([' ', '<']) {
            continue;
        }
        let Some(brace) = src[i..].find('{').map(|b| i + b) else { continue };
        let Some((ty, open)) = impl_heads(&src[ls..=brace]).into_iter().next().map(|(t, _, o)| (t, ls + o)) else { continue };
        if open < at && matching_brace(src, open).is_none_or(|c| at <= c) {
            return Some(ty);
        }
    }
    None
}

/// ファイルの中の `impl X { … }`・`impl<T> Trait for X<T> { … }`
fn impl_blocks(src: &str) -> Vec<ImplBlock> {
    impl_heads(src).into_iter().filter_map(|(ty, trait_, open)| Some(ImplBlock { ty, trait_, open, close: matching_brace(src, open)? })).collect()
}

/// `impl` の見出し（型の名前, トレイトの名前, `{` の位置）
fn impl_heads(src: &str) -> Vec<(String, String, usize)> {
    static IMPL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = IMPL.get_or_init(|| regex::Regex::new(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?impl\b").unwrap());
    let mut out = Vec::new();
    for m in re.find_iter(src) {
        let rest = &src[m.end()..];
        let Some(brace) = rest.find('{') else { continue };
        if rest[..brace].contains(';') {
            continue;
        }
        // ライブラリの索引と同じ読み方で（impl の型変数・トレイト・for の後の型）
        let Some((owner, trait_, _, _)) = crate::eco::cargo::impl_target(&rest[..brace]) else { continue };
        if owner.is_empty() || !owner.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            continue;
        }
        out.push((owner, trait_, m.end() + brace));
    }
    out
}

/// `src[open]` の `{` に対応する `}` の位置（文字列・行のコメントの中は数えない）
fn matching_brace(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    let (mut depth, mut i) = (0i32, open);
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1 + usize::from(b[i] == b'\\');
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `impl` の中の項目
#[derive(Clone)]
pub(crate) struct ImplItem {
    pub name: String,
    pub kind: DefKind,
    /// `self` を取るメソッドか
    pub method: bool,
    /// `self` を除いた引数の数
    pub params: usize,
    /// 戻り値の型の文字（無ければ空）
    pub ret: String,
    /// impl のトレイト（無ければ空）
    pub trait_: String,
}

/// クレートの中の `impl` の型 → そのファイル（索引から作って覚える。ファイルが変われば作り直す）
fn impl_files(idx: &Index, ty: &str) -> Vec<String> {
    let table = idx.derived("rust.impls", |i| {
        let mut v: Vec<String> = Vec::new();
        for f in i.files().filter(|f| f.ends_with(".rs")) {
            let Ok(text) = std::fs::read_to_string(i.root.join(f)) else { continue };
            if !text.contains("impl") {
                continue;
            }
            for b in impl_blocks(&text) {
                v.push(format!("{}\t{f}", b.ty));
            }
        }
        v.sort();
        v.dedup();
        v
    });
    let key = format!("{ty}\t");
    let i = table.partition_point(|x| x.as_str() < key.as_str());
    table[i..].iter().take_while(|x| x.starts_with(&key)).map(|x| x[key.len()..].to_string()).collect()
}

/// ファイルの `impl` の中の項目（型ごと。(更新時刻, 大きさ) で確かめて覚える）
type FileImpls = Vec<(String, ImplItem)>;
type ImplCache = std::collections::HashMap<std::path::PathBuf, ((u64, u64), std::time::Instant, std::sync::Arc<FileImpls>)>;
static IMPL_CACHE: std::sync::Mutex<Option<ImplCache>> = std::sync::Mutex::new(None);

fn file_impls(idx: &Index, rel: &str) -> std::sync::Arc<FileImpls> {
    let path = idx.root.join(rel);
    // 2 秒以内に確かめたものは、そのまま
    if let Some(v) = IMPL_CACHE.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&path).filter(|x| x.1.elapsed() < std::time::Duration::from_secs(2)).map(|x| x.2.clone()))) {
        return v;
    }
    let st = nagi_workers::index::stamp(&path).unwrap_or((0, 0));
    if let Some(v) = IMPL_CACHE.lock().ok().and_then(|mut g| {
        let e = g.as_mut()?.get_mut(&path).filter(|x| x.0 == st)?;
        e.1 = std::time::Instant::now();
        Some(e.2.clone())
    }) {
        return v;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let v = std::sync::Arc::new(impls_in(&text, idx.defs_in(rel)));
    if let Ok(mut g) = IMPL_CACHE.lock() {
        let m = g.get_or_insert_with(Default::default);
        if m.len() > 2000 {
            m.clear();
        }
        m.insert(path, (st, std::time::Instant::now(), v.clone()));
    }
    v
}

/// 中身 `text`・定義 `defs` のファイルの、`impl` の中の項目（入れ子の関数は除く）
fn impls_in(text: &str, defs: &[Entry]) -> FileImpls {
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let line_of = |pos: usize| starts.partition_point(|&s| s <= pos) - 1;
    let indent = |d: &Entry| starts.get(d.line as usize).map_or(0, |&s| text[s..].len() - text[s..].trim_start().len());
    let mut out = Vec::new();
    for b in impl_blocks(text) {
        let (l0, l1) = (line_of(b.open), line_of(b.close));
        let inner: Vec<&Entry> = defs.iter().filter(|d| (d.line as usize) > l0 && (d.line as usize) < l1 && matches!(d.kind, DefKind::Function | DefKind::Method | DefKind::Constant | DefKind::Type)).collect();
        let Some(first) = inner.iter().map(|d| indent(d)).min() else { continue };
        for d in inner.into_iter().filter(|d| indent(d) == first) {
            // 名前の後ろから { か ; まで（数行にまたがる見出しも）
            let from = starts.get(d.line as usize).map_or(0, |&s| s + d.col as usize + d.name.len());
            let end = text[from..].find(['{', ';']).map_or(text.len(), |e| from + e);
            let (method, n, ret) = crate::eco::cargo::fn_sig(&text[from.min(end)..end]);
            out.push((b.ty.clone(), ImplItem { name: d.name.clone(), kind: d.kind, method, params: n as usize, ret, trait_: b.trait_.clone() }));
        }
    }
    out
}

/// ライブラリの場所の受け手
enum Site {
    /// `.` の前の式・`Type::` の型
    Member(String),
    /// `std::collections::` のモジュール
    Module(String),
}

/// 行の `before`（名前の前まで）の受け手が、ライブラリの型・モジュールか
fn lib_site(cx: &Cx, tys: &types::Types, before: &str) -> Option<Site> {
    let lib = tys.lib?;
    if let Some(b) = before.strip_suffix('.') {
        let e = crate::text::with_previous_lines(&LX, cx.src, cx.start - cx.ws, b);
        let t = tys.expr_type(types::chain_text(&e), cx.start, 0)?;
        return (!tys.is_project(&eco::cargo::type_name(&t))).then_some(Site::Member(t));
    }
    let b = before.strip_suffix("::")?;
    let path = path_before(b);
    let last = *path.last()?;
    if tys.is_project(last) {
        return None;
    }
    let module = path.join("::");
    if lib.has_module(&module) {
        return Some(Site::Module(module));
    }
    lib.items_of(last).next().is_some().then(|| Site::Member(last.to_string()))
}

/// ライブラリのモジュールの項目の印
fn lib_mark(k: char) -> char {
    match k {
        'f' => 'm',
        'm' => 'p',
        'k' => 'f',
        'u' => 'c',
        x => x,
    }
}

/// 型 `ty` の `impl` の中の項目（クレートの中のどのファイルの impl も。同じ名前は先のもの）
pub(crate) fn impl_items(idx: &Index, cx: &Cx, ty: &str) -> Vec<ImplItem> {
    let mut files = impl_files(idx, ty);
    if !cx.rel.is_empty() && !files.contains(&cx.rel) && cx.src.contains("impl") {
        files.push(cx.rel.clone());
    }
    let mut out: Vec<ImplItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in files {
        for (t, m) in file_impls(idx, &f).iter() {
            if t == ty && seen.insert(m.name.clone()) {
                out.push(m.clone());
            }
        }
    }
    out
}

/// `::` の前の道（`crate::a::b` → [crate, a, b]）
fn path_before(s: &str) -> Vec<&str> {
    let start = s.char_indices().rev().take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == ':').last().map_or(s.len(), |x| x.0);
    s[start..].split("::").filter(|x| !x.is_empty()).collect()
}

/// 道 `path` のモジュールのファイル（今のファイル `rel` から。無ければ None）
fn module_file(files: &HashSet<&str>, rel: &str, path: &[&str]) -> Option<String> {
    // モジュールのファイルの、下のモジュールを置くフォルダ
    let child_dir = |f: &str| -> String {
        let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
        let stem = name.trim_end_matches(".rs");
        if matches!(stem, "mod" | "lib" | "main") { dir.to_string() } else if dir.is_empty() { stem.to_string() } else { format!("{dir}/{stem}") }
    };
    let join = |d: &str, n: &str| if d.is_empty() { n.to_string() } else { format!("{d}/{n}") };
    let child = |f: &str, n: &str| -> Option<String> {
        let d = child_dir(f);
        [join(&d, &format!("{n}.rs")), join(&d, &format!("{n}/mod.rs"))].into_iter().find(|c| files.contains(c.as_str()))
    };
    // クレートの根（src/lib.rs・src/main.rs）
    let crate_root = |f: &str| -> Option<String> {
        let mut dir = f.rsplit_once('/').map_or("", |x| x.0);
        loop {
            for r in ["lib.rs", "main.rs"] {
                let c = join(dir, r);
                if files.contains(c.as_str()) && (dir.ends_with("src") || dir.is_empty()) {
                    return Some(c);
                }
            }
            dir = dir.rsplit_once('/').map_or("", |x| x.0);
            if dir.is_empty() {
                return None;
            }
        }
    };
    // 親のモジュールのファイル: 下のモジュールを置くフォルダが、f のあるフォルダ（mod.rs ならその上）のもの
    let parent = |f: &str| -> Option<String> {
        let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
        let d = if matches!(name, "mod.rs" | "lib.rs" | "main.rs") { dir.rsplit_once('/').map_or("", |x| x.0) } else { dir };
        let up = d.rsplit_once('/').map_or("", |x| x.0);
        let last = d.rsplit('/').next().unwrap_or("");
        [join(d, "mod.rs"), join(d, "lib.rs"), join(d, "main.rs"), join(up, &format!("{last}.rs"))].into_iter().find(|c| files.contains(c.as_str()) && c != f)
    };
    let mut cur: Option<String> = None;
    for (k, seg) in path.iter().enumerate() {
        let here = cur.clone().unwrap_or_else(|| rel.to_string());
        cur = Some(match *seg {
            "crate" if k == 0 => crate_root(rel)?,
            "self" if k == 0 => rel.to_string(),
            "super" => parent(&here)?,
            n => match child(&here, n) {
                Some(f) => f,
                // クレートの名前（nagi_lang → nagi-lang/src/lib.rs）
                None if k == 0 => {
                    let dash = n.replace('_', "-");
                    files.iter().find(|f| f.ends_with(&format!("{dash}/src/lib.rs")) || f.ends_with(&format!("{n}/src/lib.rs"))).map(|s| s.to_string())?
                }
                None => return None,
            },
        });
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impls() {
        let src = "struct A;\nimpl A {\n    fn new() -> A { A }\n    pub fn go(&self, n: u8) {}\n}\nimpl<'a, T> Show for B<'a, T> where T: X {\n    fn show(&mut self) {}\n}\n";
        let b = impl_blocks(src);
        assert_eq!(b.iter().map(|x| x.ty.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(RUST.enclosing_type(src, src.find("pub fn").unwrap()).as_deref(), Some("A"));
        assert_eq!(RUST.enclosing_type(src, src.find("fn show").unwrap()).as_deref(), Some("B"));
    }

    #[test]
    fn var_types() {
        let src = "fn f(x: &mut Foo, y: Bar) {\n    let mut a: &Baz = g();\n    let b = Qux::new(1);\n    let c = Point { x: 1 };\n    let d = h();\n}\n";
        // 関数の中（閉じる } の前）
        let at = src.len() - 2;
        assert_eq!(RUST.var_type(src, "x", at).as_deref(), Some("Foo"));
        assert_eq!(RUST.var_type(src, "a", at).as_deref(), Some("Baz"));
        assert_eq!(RUST.var_type(src, "b", at).as_deref(), Some("Qux"));
        assert_eq!(RUST.var_type(src, "c", at).as_deref(), Some("Point"));
        assert_eq!(RUST.var_type(src, "d", at), None);
        let src = "pub mod shapes;\n\npub struct Counter {\n    pub n: u32,\n}\n\nimpl Counter {\n    pub fn new() -> Counter {\n        Counter { n: 0 }\n    }\n}\n\nfn use_it() {\n    let mut c = Counter::new();\n    c.\n}\n";
        assert_eq!(RUST.var_type(src, "c", src.find("c.\n").unwrap()).as_deref(), Some("Counter"));
    }

    #[test]
    fn paths() {
        let files: HashSet<&str> = ["crates/a-b/src/lib.rs", "crates/a-b/src/text.rs", "crates/a-b/src/eco/mod.rs", "crates/a-b/src/eco/jvm.rs", "crates/c/src/main.rs"].into();
        let here = "crates/a-b/src/eco/jvm.rs";
        assert_eq!(module_file(&files, here, &["crate"]).as_deref(), Some("crates/a-b/src/lib.rs"));
        assert_eq!(module_file(&files, here, &["crate", "text"]).as_deref(), Some("crates/a-b/src/text.rs"));
        assert_eq!(module_file(&files, here, &["super"]).as_deref(), Some("crates/a-b/src/eco/mod.rs"));
        assert_eq!(module_file(&files, "crates/a-b/src/lib.rs", &["eco", "jvm"]).as_deref(), Some("crates/a-b/src/eco/jvm.rs"));
        assert_eq!(module_file(&files, "crates/c/src/main.rs", &["a_b", "text"]).as_deref(), Some("crates/a-b/src/text.rs"), "クレートの名前");
        assert_eq!(module_file(&files, here, &["std", "fs"]), None);
        assert_eq!(path_before("    let x = crate::eco"), vec!["crate", "eco"]);
    }

    /// 小さな std（生態系 Cargo の索引）で、ライブラリの型のメソッド・トレイトのメソッド・`::` の後・定義へ
    #[test]
    fn library() {
        let std_src = r#"
pub mod collections {
    pub struct HashMap<K, V> {}
}
pub mod fs {
    pub struct File {}
}
pub struct Vec<T> {}
impl<T> IntoIterator for Vec<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {}
}
pub struct IntoIter<T> {}
impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {}
}
impl<T> Vec<T> {
    pub fn new() -> Vec<T> {}
    pub fn push(&mut self, value: T) {}
}
impl<T> Deref for Vec<T> {
    type Target = [T];
}
impl<T> [T] {
    pub fn iter(&self) -> Iter<'_, T> {}
    pub fn first(&self) -> Option<&T> {}
}
pub struct Iter<'a, T> {}
impl<'a, T> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<&'a T> {}
}
pub trait Iterator {
    type Item;
    fn next(&mut self) -> Option<Self::Item>;
    fn count(self) -> usize {}
}
pub struct String {}
impl String {
    pub fn push_str(&mut self, s: &str) {}
}
impl Deref for String {
    type Target = str;
}
impl str {
    pub fn trim(&self) -> &str {}
}
pub trait Read {
    fn read_to_string(&mut self, buf: &mut String) -> usize;
}
impl File {
    pub fn open(p: &str) -> Result<File, Error> {}
}
pub enum Result<T, E> {}
impl<T, E> Result<T, E> {
    pub fn unwrap(self) -> T {}
}
impl Read for File {
    fn read_to_string(&mut self, buf: &mut String) -> usize {}
}
pub enum Option<T> {}
impl<T> Option<T> {
    pub fn unwrap(self) -> T {}
    pub fn map_or<U, F: FnOnce(T) -> U>(self, default: U, f: F) -> U {}
}
"#;
        let mut libs = crate::libs::Libraries::default();
        libs.set(eco::CARGO, std::sync::Arc::new(eco::cargo::from_text("std", std_src)));
        let names = |src: &str| -> Vec<String> {
            let at = src.find('@').unwrap();
            let src = src.replace('@', "");
            let ls = src[..at].rfind('\n').map_or(0, |i| i + 1);
            let line = src[ls..].lines().next().unwrap_or("");
            let cx = Cx::new(&RUST, &src, None, line, at - ls, at, "", None, &libs);
            crate::template::complete::complete(&cx).into_iter().map(|i| i.name).collect()
        };
        let v = names("fn f() {\n    let mut v: Vec<String> = Vec::new();\n    v.@\n}");
        assert!(v.starts_with(&["push".to_string()]) && v.contains(&"iter".to_string()), "Vec と Deref の先の [T]: {v:?}");
        let v = names("fn f(v: Vec<String>) {\n    v.first().unwrap().@\n}");
        assert!(v.contains(&"push_str".to_string()) && v.contains(&"trim".to_string()), "戻り値の型を型引数ごと: {v:?}");
        let v = names("fn f(v: Vec<u8>) {\n    v.iter().@\n}");
        assert!(v.contains(&"next".to_string()) && v.contains(&"count".to_string()), "prelude のトレイト Iterator: {v:?}");
        let v = names("fn f(v: Vec<String>) {\n    v.iter().map(|s| s.@)\n}");
        assert!(v.contains(&"push_str".to_string()), "クロージャの引数は反復子の要素: {v:?}");
        let v = names("fn f(v: Vec<String>) {\n    for s in v {\n        s.@\n    }\n}");
        assert!(v.contains(&"push_str".to_string()), "for の要素: {v:?}");
        let v = names("fn f(v: Vec<String>) {\n    v.first().map_or(0, |s| s.@)\n}");
        assert!(v.contains(&"push_str".to_string()), "2 番目の引数のクロージャ: {v:?}");
        let v = names("fn f(o: Option<String>) {\n    if let Some(s) = o {\n        s.@\n    }\n}");
        assert!(v.contains(&"push_str".to_string()), "if let Some: {v:?}");
        let v = names("fn f(x: File) {\n    x.@\n}");
        assert!(!v.contains(&"read_to_string".to_string()), "use していないトレイトのメソッドは出さない: {v:?}");
        let v = names("use std::io::Read;\nfn f(x: File) {\n    x.@\n}");
        assert!(v.contains(&"read_to_string".to_string()), "use したトレイト: {v:?}");
        let v = names("use std::io::Read;\n\nfn main() {\n    let mut f = std::fs::File::open(\"x\").unwrap();\n    f.@\n}\n");
        assert!(v.contains(&"read_to_string".to_string()), "関連関数の戻り値の型から: {v:?}");
        assert_eq!(names("fn f() {\n    std::collections::@\n}"), vec!["HashMap"]);
        assert_eq!(names("fn f() {\n    Vec::@\n}").first().map(String::as_str), Some("new"), "関連関数を先に");
        // 定義へ
        let src = "fn f(s: String) {\n    s.trim();\n}";
        let at = src.find("trim").unwrap();
        let cx = Cx::at(&RUST, src, None, at, None, &libs);
        match RUST.definition(&cx, "trim") {
            Some(Found::Jump(t)) => assert_eq!(std_src.lines().nth(t.line).map(str::trim), Some("pub fn trim(&self) -> &str {}")),
            x => panic!("{x:?}"),
        }
    }
}
