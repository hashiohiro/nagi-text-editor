//! Rust の差し込み口。
//!
//! 定義の索引は `impl` の中の関数を型に結びつけないので、ファイルの `impl X { … }`・`impl T for X { … }` の範囲を読んで結びつける。
//! - `x.` の後: 構造体のフィールド（索引）に加えて、`impl` の中の `self` を取るメソッド
//! - `Type::` の後: enum の値・関連関数・関連定数（`self` を取るメソッドは後に）。`Self::` は今いる `impl` の型
//! - `module::` の後（`crate::`・`self::`・`super::`・クレート名・`mod` の名前）: そのモジュールの上の段の項目と下のモジュール

use std::collections::HashSet;

use nagi_syntax::defs::DefKind;
use nagi_workers::index::{Entry, Index};

use crate::lang::{Cx, Item, Language, mark_of};
use crate::project::Ranker;
use crate::text::{C_LIKE, Lexis};

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

    /// `Type::`・`module::` の後
    fn complete_all(&self, cx: &Cx) -> Option<Vec<Item>> {
        let before = &cx.line[..cx.ws];
        if !before.ends_with("::") {
            return None;
        }
        let idx = cx.index?;
        let path: Vec<&str> = path_before(&before[..before.len() - 2]);
        let last = *path.last()?;
        let last = if last == "Self" { cx.inside.clone()? } else { last.to_string() };
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

/// `impl` の範囲（型の名前、`{` と対応する `}` の位置）
struct ImplBlock {
    ty: String,
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
        let Some((ty, open)) = impl_heads(&src[ls..=brace]).into_iter().next().map(|(t, o)| (t, ls + o)) else { continue };
        if open < at && matching_brace(src, open).is_none_or(|c| at <= c) {
            return Some(ty);
        }
    }
    None
}

/// ファイルの中の `impl X { … }`・`impl<T> Trait for X<T> { … }`
fn impl_blocks(src: &str) -> Vec<ImplBlock> {
    impl_heads(src).into_iter().filter_map(|(ty, open)| Some(ImplBlock { ty, open, close: matching_brace(src, open)? })).collect()
}

/// `impl` の見出し（型の名前, `{` の位置）
fn impl_heads(src: &str) -> Vec<(String, usize)> {
    static IMPL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = IMPL.get_or_init(|| regex::Regex::new(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?impl\b").unwrap());
    let mut out = Vec::new();
    for m in re.find_iter(src) {
        let rest = &src[m.end()..];
        let Some(brace) = rest.find('{') else { continue };
        let mut head = rest[..brace].trim();
        if rest[..brace].contains(';') {
            continue;
        }
        // impl の型変数
        if head.starts_with('<') {
            let Some(e) = matching_angle(head) else { continue };
            head = head[e + 1..].trim();
        }
        let head = head.split(" where").next().unwrap_or(head);
        let target = head.rsplit_once(" for ").map_or(head, |x| x.1).trim().trim_start_matches(['&', ' ']).trim_start_matches("dyn ");
        let target = target.split('<').next().unwrap_or("").rsplit("::").next().unwrap_or("").trim();
        if target.is_empty() || !target.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            continue;
        }
        out.push((target.to_string(), m.end() + brace));
    }
    out
}

/// `<…>` の終わりの位置（`s` は `<` で始まる）
fn matching_angle(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
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
struct ImplItem {
    name: String,
    kind: DefKind,
    /// `self` を取るメソッドか
    method: bool,
    /// `self` を除いた引数の数
    params: usize,
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
            let line = starts.get(d.line as usize).map_or("", |&s| text[s..].lines().next().unwrap_or(""));
            let after = line.get(d.col as usize..).unwrap_or("");
            let params = after.find('(').map(|o| &after[o + 1..after[o..].find(')').map_or(after.len(), |c| o + c)]).unwrap_or("");
            let parts: Vec<&str> = params.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
            let method = parts.first().is_some_and(|p| p.trim_start_matches(['&', ' ']).trim_start_matches("'_ ").trim_start_matches("mut ").starts_with("self"));
            let n = parts.len() - usize::from(method);
            out.push((b.ty.clone(), ImplItem { name: d.name.clone(), kind: d.kind, method, params: n }));
        }
    }
    out
}

/// 型 `ty` の `impl` の中の項目（クレートの中のどのファイルの impl も。同じ名前は先のもの）
fn impl_items(idx: &Index, cx: &Cx, ty: &str) -> Vec<ImplItem> {
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
        let at = src.len();
        assert_eq!(RUST.var_type(src, "x", at).as_deref(), Some("Foo"));
        assert_eq!(RUST.var_type(src, "a", at).as_deref(), Some("Baz"));
        assert_eq!(RUST.var_type(src, "b", at).as_deref(), Some("Qux"));
        assert_eq!(RUST.var_type(src, "c", at).as_deref(), Some("Point"));
        assert_eq!(RUST.var_type(src, "d", at), None);
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
}
