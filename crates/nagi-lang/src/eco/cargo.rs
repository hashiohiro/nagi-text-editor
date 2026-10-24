//! 生態系 Cargo（Rust の標準ライブラリ（rust-src の core・alloc・std）と、ワークスペースが直接使う依存のクレートのソース）。
//!
//! ソースを 1 行ずつ読む小さな読み取り器で（rustfmt で整形されているので項目は行の頭から）、括弧の深さでどの impl・トレイト・
//! モジュールの中かを追い、次を表にする:
//! - impl・トレイトの中の関数・定数（持ち主の型、トレイト、impl の型の文字（型引数の名前）、self を取るか、引数の数、戻り値の型の文字）
//! - モジュールの公開の項目（fn・struct・enum・trait・type・const・mod と pub use の名前）と、その場所
//! - Deref の先（Vec → [T]、String → str）
//!
//! マクロで作る impl（整数の型のメソッドなど）は読めない。ソースが大きすぎるクレート（windows など）は読まない

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 読まないクレートの大きさ（ソースの合計）
const MAX_CRATE_BYTES: u64 = 8 << 20;

/// impl・トレイトの中の項目
#[derive(Debug, Clone)]
pub struct LibItem {
    /// 持ち主: impl の型の名前（`Vec`・`[T]` は `slice`）か、トレイトの定義なら `trait:名前`
    pub owner: Arc<str>,
    /// impl のトレイト（`impl Iterator for Iter` の Iterator。無ければ空）
    pub trait_: Arc<str>,
    /// impl の型の文字（`Vec<T, A>`。型引数を戻り値の型に当てる）
    pub target: Arc<str>,
    pub name: Box<str>,
    /// 'f' 関数・'c' 定数・'t' 関連型
    pub kind: char,
    /// self を取る（`.` で呼ぶ）か
    pub method: bool,
    /// self を除いた引数の数
    pub params: u8,
    /// 戻り値の型の文字（無ければ空）。関連型は `= 型` の型
    pub ret: Box<str>,
    /// すべての型への impl（`impl<T: Display> ToString for T`）
    pub blanket: bool,
    pub file: u32,
    pub line: u32,
}

/// モジュールの公開の項目
#[derive(Debug, Clone)]
pub struct ModItem {
    /// モジュールの道（`std::collections`）
    pub module: Arc<str>,
    pub name: Box<str>,
    /// 'f' fn・'c' struct・'e' enum・'I' trait・'t' type・'k' const・'m' mod・'u' pub use
    pub kind: char,
    /// fn の戻り値の型・type の別名の先（無ければ空）
    pub ret: Box<str>,
    pub file: u32,
    pub line: u32,
}

/// Cargo の索引
#[derive(Default)]
pub struct CargoIndex {
    pub items: Vec<LibItem>,
    pub mods: Vec<ModItem>,
    /// (型, Deref の先の型の文字)
    pub derefs: Vec<(Box<str>, Box<str>)>,
    pub files: Vec<PathBuf>,
    /// 読んだクレート（コードでの名前）と、読まなかったもの（大きすぎる・ソースが無い）
    pub crates: Vec<String>,
    pub skipped: Vec<String>,
    /// 読むあいだだけ: 繰り返す文字（持ち主の型・トレイト・impl の型・モジュール）を 1 つにする
    pool: std::collections::HashSet<Arc<str>>,
}

impl CargoIndex {
    fn share(&mut self, s: &str) -> Arc<str> {
        if let Some(a) = self.pool.get(s) {
            return a.clone();
        }
        let a: Arc<str> = s.into();
        self.pool.insert(a.clone());
        a
    }

    /// 持ち主 `owner` の項目（impl の型・`trait:名前`）
    pub fn items_of<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = &'a LibItem> + 'a {
        self.items.iter().filter(move |i| &*i.owner == owner)
    }

    /// 型 `ty` に impl したトレイトの名前（ブランケットの impl は含めない）
    pub fn traits_of(&self, ty: &str) -> Vec<&str> {
        let mut v: Vec<&str> = self.items.iter().filter(|i| &*i.owner == ty && !i.trait_.is_empty()).map(|i| &*i.trait_).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// すべての型への impl があるトレイト
    pub fn blanket_traits(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.items.iter().filter(|i| i.blanket).map(|i| &*i.trait_).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn deref(&self, ty: &str) -> Option<&str> {
        self.derefs.iter().find(|d| &*d.0 == ty).map(|d| &*d.1)
    }

    /// モジュール `module`（`std::collections`）の公開の項目
    pub fn module<'a>(&'a self, module: &'a str) -> impl Iterator<Item = &'a ModItem> + 'a {
        self.mods.iter().filter(move |m| &*m.module == module)
    }

    /// モジュールがあるか
    pub fn has_module(&self, module: &str) -> bool {
        self.mods.iter().any(|m| &*m.module == module) || self.mods.iter().any(|m| m.module.starts_with(module) && m.module[module.len()..].starts_with("::"))
    }

    /// 道 `path`（`std::env::temp_dir`・`fs::write`）の関数の戻り値の型。モジュールの道が途中からなら、その道で終わるモジュール（std を先に）
    pub fn fn_ret(&self, path: &str) -> Option<&str> {
        let (module, name) = path.rsplit_once("::")?;
        let tail = format!("::{module}");
        let fits = |m: &ModItem| &*m.name == name && m.kind == 'f' && (&*m.module == module || m.module.ends_with(&tail));
        let mut found: Vec<&ModItem> = self.mods.iter().filter(|m| fits(m)).collect();
        found.sort_by_key(|m| (!m.module.starts_with("std"), m.module.len()));
        found.first().map(|m| &*m.ret).filter(|r| !r.is_empty())
    }

    /// 型の名前の定義の場所（struct・enum・trait・type）
    pub fn type_loc(&self, name: &str) -> Option<(&Path, u32)> {
        self.mods.iter().find(|m| &*m.name == name && matches!(m.kind, 'c' | 'e' | 'I' | 't')).map(|m| (self.files[m.file as usize].as_path(), m.line))
    }

    pub fn file(&self, k: u32) -> &Path {
        &self.files[k as usize]
    }
}

/// 型の文字の名前（`&'a mut Vec<T>` → Vec、`[T]` → slice、`std::string::String` → String）
pub fn type_name(t: &str) -> String {
    let t = t.trim().trim_start_matches('&').trim_start();
    let t = if t.starts_with('\'') { t.split_once(' ').map_or("", |x| x.1) } else { t };
    let t = t.trim_start_matches("mut ").trim_start_matches("dyn ").trim_start_matches("impl ").trim();
    if t.starts_with('[') {
        return "slice".into();
    }
    let head = t.split('<').next().unwrap_or("").trim();
    head.rsplit("::").next().unwrap_or("").trim().to_string()
}

/// 1 行ずつ読む読み取り器の、開いている括弧
enum Frame {
    Module(String),
    Impl { owner: String, trait_: String, target: String, blanket: bool },
    Trait(String),
    /// 型のメソッドを作るマクロ（core の整数のメソッド）。中の深さは問わない
    MacroImpl(&'static str),
    Other,
}

/// 整数の型のメソッドを作るマクロのファイル（core の中の道）と、その持ち主の名前（Rust の言語が u8 … を uint に読み替える）
const MACRO_IMPLS: &[(&str, &str)] = &[("num/uint_macros.rs", "uint"), ("num/int_macros.rs", "int")];

/// 文字列・文字・コメントの中を空白にする（行の数は変えない）
fn clean(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        } else if c == b'r' && matches!(b.get(i + 1), Some(b'#' | b'"')) && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            // r"…"・r#"…"#
            let mut k = i + 1;
            let mut hashes = 0;
            while b.get(k) == Some(&b'#') {
                hashes += 1;
                k += 1;
            }
            if b.get(k) != Some(&b'"') {
                out.push(c);
                i += 1;
                continue;
            }
            let close: Vec<u8> = std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hashes)).collect();
            let end = src[k + 1..].find(std::str::from_utf8(&close).unwrap_or("\"")).map_or(b.len(), |e| k + 1 + e + close.len());
            for &x in &b[i..end] {
                out.push(if x == b'\n' { b'\n' } else { b' ' });
            }
            out.push(b'"');
            out.pop();
            i = end;
        } else if c == b'"' {
            out.push(b'"');
            i += 1;
            while i < b.len() && b[i] != b'"' {
                let skip = if b[i] == b'\\' { 2 } else { 1 };
                for _ in 0..skip.min(b.len() - i) {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
            if i < b.len() {
                out.push(b'"');
                i += 1;
            }
        } else if c == b'\'' {
            // 文字（'x'・'\n'・'\u{..}'）なら空白に、寿命（'a）はそのまま
            let rest = &src[i + 1..];
            let len = if rest.starts_with('\\') {
                rest.find('\'').map(|e| e + 2)
            } else {
                let ch = rest.chars().next();
                ch.filter(|_| rest[ch.map_or(0, char::len_utf8)..].starts_with('\'')).map(|ch| ch.len_utf8() + 2)
            };
            match len {
                Some(n) => {
                    out.push(b'\'');
                    out.extend(std::iter::repeat_n(b' ', n - 2));
                    out.push(b'\'');
                    i += n;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// 項目の頭（`pub(crate) unsafe extern "C" fn`）から、公開か・項目の種類・名前・残り
fn item_head(l: &str) -> Option<(bool, &str, &str, &str)> {
    let mut t = l.trim_start();
    // 行の頭の #[…]
    while t.starts_with("#[") || t.starts_with("#![") {
        let e = t.find(']')?;
        t = t[e + 1..].trim_start();
    }
    let mut public = false;
    if let Some(r) = t.strip_prefix("pub") {
        if r.starts_with('(') {
            t = r[r.find(')')? + 1..].trim_start();
            public = r.starts_with("(crate)") || r.starts_with("(super)") || r.starts_with("(in");
            public = !public;
        } else if r.starts_with(' ') {
            t = r.trim_start();
            public = true;
        }
    }
    loop {
        let w = t.split_whitespace().next()?;
        if matches!(w, "const" | "async" | "unsafe" | "extern" | "default" | "auto") && !t.trim_start_matches(w).trim_start().starts_with(['{', '=']) {
            // const fn・const NAME: …（定数は名前の前に : がある）
            if w == "const" {
                let after = t[5..].trim_start();
                let next = after.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
                if !matches!(next, "fn" | "unsafe" | "async" | "extern" | "impl" | "trait") {
                    return Some((public, "const", next, &after[next.len()..]));
                }
            }
            t = t[w.len()..].trim_start();
            if t.starts_with('"') {
                t = t[1..].split_once('"').map_or("", |x| x.1).trim_start();
            }
            continue;
        }
        break;
    }
    let kw = t.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!')).next()?;
    let rest = t[kw.len()..].trim_start();
    match kw {
        "fn" | "struct" | "enum" | "trait" | "type" | "union" | "static" | "mod" | "macro_rules!" => {
            let name = rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
            (!name.is_empty()).then_some((public, kw, name, &rest[name.len()..]))
        }
        "impl" | "use" => Some((public, kw, "", rest)),
        _ => None,
    }
}

/// `impl<…> Trait<…> for Target<…>` の (持ち主の名前, トレイトの名前, 型の文字, すべての型への impl か)
pub(crate) fn impl_target(head: &str) -> Option<(String, String, String, bool)> {
    let mut h = head.trim();
    let mut generics: Vec<String> = Vec::new();
    if h.starts_with('<') {
        let e = matching(h, '<', '>')?;
        generics = crate::text::split_top(&crate::rust::LX, &h[1..e], b',', true).into_iter().map(|g| g.trim().split([':', ' ', '=']).next().unwrap_or("").to_string()).collect();
        h = h[e + 1..].trim();
    }
    let h = h.split(" where").next().unwrap_or(h).trim().trim_end_matches('{').trim();
    let h = h.trim_start_matches('!');
    let (tr, target) = match split_for(h) {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => (String::new(), h.to_string()),
    };
    let owner = type_name(&target);
    let blanket = !tr.is_empty() && generics.iter().any(|g| *g == owner);
    Some((owner, type_name(&tr), target, blanket))
}

/// `A<B> for C` の外側の " for "
fn split_for(h: &str) -> Option<(&str, &str)> {
    let b = h.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b' ' if depth == 0 && h[i..].starts_with(" for ") => return Some((&h[..i], &h[i + 5..])),
            _ => {}
        }
    }
    None
}

/// `s` の頭の開き括弧に対応する閉じ括弧の位置
fn matching(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// fn の後ろ（`<T>(&self, a: A) -> R where … {`）から (self を取るか, 引数の数, 戻り値の型の文字)
pub(crate) fn fn_sig(rest: &str) -> (bool, u8, String) {
    let mut r = rest.trim_start();
    if r.starts_with('<') {
        r = matching(r, '<', '>').map_or("", |e| &r[e + 1..]).trim_start();
    }
    let Some(close) = r.starts_with('(').then(|| matching(r, '(', ')')).flatten() else { return (false, 0, String::new()) };
    let params: Vec<&str> = crate::text::split_top(&crate::rust::LX, &r[1..close], b',', true).into_iter().map(str::trim).filter(|p| !p.is_empty()).collect();
    let method = params.first().is_some_and(|p| {
        let p = p.trim_start_matches(['&', ' ']).trim_start();
        let p = if p.starts_with('\'') { p.split_once(' ').map_or("", |x| x.1) } else { p };
        p.trim_start_matches("mut ").starts_with("self")
    });
    let after = r[close + 1..].trim_start();
    let ret = after.strip_prefix("->").map_or(String::new(), |x| {
        let x = x.trim_start();
        let end = [x.find(" where"), x.find('{'), x.find(';')].into_iter().flatten().min().unwrap_or(x.len());
        x[..end].trim().to_string()
    });
    (method, (params.len() - usize::from(method)).min(255) as u8, ret)
}

/// クレート（コードでの名前 `crate_name`、ソースのフォルダ `src`）を読んで、索引に足す
pub fn scan_crate(ix: &mut CargoIndex, crate_name: &str, src: &Path) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name();
            // 試験と、std の中の環境ごとの実装（sys の下の File など。外からは使えない）は読まない
            let skip = matches!(name.to_str(), Some("tests" | "benches" | "tests.rs")) || (crate_name == "std" && p.parent() == Some(src) && name == "sys");
            if skip {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let rel = f.strip_prefix(src).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        // ファイルのモジュールの道（lib.rs → クレート、a/b.rs → クレート::a::b、a/mod.rs → クレート::a）
        let stem = rel.trim_end_matches(".rs");
        let module = if matches!(stem, "lib" | "main") {
            crate_name.to_string()
        } else {
            let s = stem.trim_end_matches("/mod").replace('/', "::");
            format!("{crate_name}::{s}")
        };
        let k = ix.files.len() as u32;
        ix.files.push(f.clone());
        let macro_owner = MACRO_IMPLS.iter().find(|m| crate_name == "core" && rel == m.0).map(|m| m.1);
        scan_file(ix, &module, &clean(&text), k, macro_owner);
    }
    ix.crates.push(crate_name.to_string());
}

/// 1 つのファイルを読む
fn scan_file(ix: &mut CargoIndex, module: &str, text: &str, file: u32, macro_owner: Option<&'static str>) {
    // (開いた深さ, 括弧)
    let mut stack: Vec<(usize, Frame)> = vec![(0, Frame::Module(module.to_string()))];
    let mut depth = 0usize;
    // 見出しの続きの行（項目を探さない）と、次の { で開く括弧（その行）
    let mut skip_until = 0usize;
    let mut open_at: Option<(usize, Frame)> = None;
    // 不安定（#[unstable(…)]。nightly でしか使えない）: 次の項目に付いているか・その括弧（stack の位置）の中か
    let mut unstable_attr = false;
    let mut unstable_open = false;
    let mut unstable_from: Option<usize> = None;
    let lines: Vec<&str> = text.lines().collect();
    for (ln, line) in lines.iter().enumerate() {
        let top = stack.last().unwrap();
        let at_item = ln >= skip_until && open_at.is_none() && (depth == top.0 || (matches!(top.1, Frame::MacroImpl(_)) && depth > top.0));
        if at_item && line.trim_start().starts_with("#[unstable(") {
            unstable_attr = true;
        }
        if at_item {
            if let Some((public, kw, name, rest)) = item_head(line) {
                let unstable = std::mem::take(&mut unstable_attr) || unstable_from.is_some();
                let counts = (ix.items.len(), ix.mods.len(), ix.derefs.len());
                // 見出しが { か ; で終わるまで行をつなぐ
                let mut head = rest.to_string();
                let mut k = ln;
                while !head.contains(['{', ';']) && k + 1 < lines.len() && k - ln < 12 {
                    k += 1;
                    head.push(' ');
                    head.push_str(lines[k].trim());
                }
                let brace = head.find('{');
                let semi = head.find(';');
                let body = brace.is_some_and(|b| semi.is_none_or(|s| b < s));
                // { のある行
                let brace_line = (ln..=k).find(|&x| lines[x].contains('{')).unwrap_or(k);
                skip_until = k + 1;
                let frame = &stack.last().unwrap().1;
                let mut opened: Option<Frame> = None;
                match (kw, frame) {
                    ("impl", Frame::Module(_)) => {
                        if let Some((owner, trait_, target, blanket)) = impl_target(head.split('{').next().unwrap_or("")) {
                            if trait_ == "Deref" {
                                let tgt = lines[ln..lines.len().min(ln + 12)].iter().find_map(|l| l.trim().strip_prefix("type Target =").map(|x| x.trim().trim_end_matches(';').trim().to_string()));
                                if let Some(t) = tgt {
                                    ix.derefs.push((owner.as_str().into(), t.into()));
                                }
                            }
                            opened = Some(Frame::Impl { owner, trait_, target, blanket });
                        }
                    }
                    ("fn", Frame::Impl { owner, trait_, target, blanket }) if public || !trait_.is_empty() => {
                        let (method, params, ret) = fn_sig(&head);
                        let it = LibItem { owner: ix.share(owner), trait_: ix.share(trait_), target: ix.share(target), name: name.into(), kind: 'f', method, params, ret: ret.into(), blanket: *blanket, file, line: ln as u32 };
                        ix.items.push(it);
                    }
                    ("const" | "type", Frame::Impl { owner, trait_, target, blanket }) if public || !trait_.is_empty() => {
                        let ret = head.split_once('=').map_or("", |x| x.1).trim().trim_end_matches(';').trim();
                        let it = LibItem { owner: ix.share(owner), trait_: ix.share(trait_), target: ix.share(target), name: name.into(), kind: if kw == "type" { 't' } else { 'c' }, method: false, params: 0, ret: ret.into(), blanket: *blanket, file, line: ln as u32 };
                        ix.items.push(it);
                    }
                    ("fn", Frame::MacroImpl(owner)) if public => {
                        let (method, params, ret) = fn_sig(&head);
                        let it = LibItem { owner: ix.share(owner), trait_: ix.share(""), target: ix.share(owner), name: name.into(), kind: 'f', method, params, ret: ret.into(), blanket: false, file, line: ln as u32 };
                        ix.items.push(it);
                    }
                    ("macro_rules!", Frame::Module(_)) if macro_owner.is_some() => opened = macro_owner.map(Frame::MacroImpl),
                    ("fn", Frame::Trait(t)) => {
                        let (method, params, ret) = fn_sig(&head);
                        let it = LibItem { owner: ix.share(&format!("trait:{t}")), trait_: ix.share(t), target: ix.share("Self"), name: name.into(), kind: 'f', method, params, ret: ret.into(), blanket: false, file, line: ln as u32 };
                        ix.items.push(it);
                    }
                    ("type", Frame::Trait(t)) => {
                        let it = LibItem { owner: ix.share(&format!("trait:{t}")), trait_: ix.share(t), target: ix.share("Self"), name: name.into(), kind: 't', method: false, params: 0, ret: "".into(), blanket: false, file, line: ln as u32 };
                        ix.items.push(it);
                    }
                    (_, Frame::Module(m)) => {
                        let m = m.clone();
                        if kw == "trait" {
                            opened = Some(Frame::Trait(name.to_string()));
                        } else if kw == "mod" && body {
                            opened = Some(Frame::Module(format!("{m}::{name}")));
                        }
                        if public {
                            match kw {
                                "use" => {
                                    // pub use a::B;・pub use a::{B, C as D};（* は読めないので外す）
                                    let u = head.split(';').next().unwrap_or("");
                                    let names: Vec<String> = match (u.find('{'), u.rfind('}')) {
                                        (Some(o), Some(c)) if o < c => u[o + 1..c].split(',').map(|x| x.trim().to_string()).collect(),
                                        _ => vec![u.trim().to_string()],
                                    };
                                    for n in names {
                                        let n = n.rsplit(" as ").next().unwrap_or(&n).rsplit("::").next().unwrap_or("").trim().to_string();
                                        if !n.is_empty() && n != "*" && n != "self" && n.chars().all(|c| c.is_alphanumeric() || c == '_') {
                                            let it = ModItem { module: ix.share(&m), name: n.into(), kind: 'u', ret: "".into(), file, line: ln as u32 };
                                            ix.mods.push(it);
                                        }
                                    }
                                }
                                "macro_rules!" => {}
                                _ => {
                                    let mark = match kw {
                                        "fn" => 'f',
                                        "struct" | "union" => 'c',
                                        "enum" => 'e',
                                        "trait" => 'I',
                                        "type" => 't',
                                        "mod" => 'm',
                                        _ => 'k',
                                    };
                                    let ret = match mark {
                                        'f' => fn_sig(&head).2,
                                        't' => head.split_once('=').map_or("", |x| x.1).trim().trim_end_matches(';').trim().to_string(),
                                        _ => String::new(),
                                    };
                                    let it = ModItem { module: ix.share(&m), name: name.into(), kind: mark, ret: ret.into(), file, line: ln as u32 };
                                    ix.mods.push(it);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                if unstable {
                    ix.items.truncate(counts.0);
                    ix.mods.truncate(counts.1);
                    ix.derefs.truncate(counts.2);
                }
                if body {
                    open_at = Some((brace_line, opened.unwrap_or(Frame::Other)));
                    unstable_open = unstable && unstable_from.is_none();
                }
            }
        }
        // 括弧の深さ。見出しの { で括弧を開く
        let mut first = true;
        for c in line.bytes() {
            match c {
                b'{' => {
                    depth += 1;
                    if first && open_at.as_ref().is_some_and(|o| o.0 == ln) {
                        let (_, f) = open_at.take().unwrap();
                        stack.push((depth, f));
                        if std::mem::take(&mut unstable_open) {
                            unstable_from = Some(stack.len() - 1);
                        }
                    }
                    first = false;
                }
                b'}' => {
                    while stack.len() > 1 && stack.last().is_some_and(|f| f.0 >= depth) {
                        stack.pop();
                    }
                    if unstable_from.is_some_and(|k| stack.len() <= k) {
                        unstable_from = None;
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
        }
    }
}

/// rust-src の標準ライブラリのフォルダ（`<sysroot>/lib/rustlib/src/rust/library`）
fn std_library(root: &Path) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new("rustc");
    cmd.args(["--print", "sysroot"]).current_dir(root);
    nagi_workers::sys::no_window(&mut cmd);
    let sysroot = cmd.output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let lib = PathBuf::from(sysroot).join("lib").join("rustlib").join("src").join("rust").join("library");
    lib.join("std").join("src").is_dir().then_some(lib)
}

/// Cargo の置き場所（CARGO_HOME か ~/.cargo）
fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join(".cargo")))
}

/// ワークスペースの Cargo.toml が直接使う依存の名前（[dependencies]・[dev-dependencies]・[build-dependencies]・[workspace.dependencies]。
/// `package = "…"` で名前を変えたものは元の名前）
fn direct_deps(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut tomls = vec![root.join("Cargo.toml")];
    for e in nagi_workers::files::walker(root).build().flatten() {
        if e.file_name() == "Cargo.toml" && e.path() != root.join("Cargo.toml") {
            tomls.push(e.into_path());
        }
    }
    for t in tomls {
        let Ok(text) = std::fs::read_to_string(&t) else { continue };
        let mut on = false;
        for l in text.lines() {
            let l = l.trim();
            if l.starts_with('[') {
                on = matches!(l, "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]" | "[workspace.dependencies]") || l.ends_with(".dependencies]");
                continue;
            }
            if !on || l.starts_with('#') {
                continue;
            }
            let Some((name, rest)) = l.split_once('=') else { continue };
            let name = rest.split("package").nth(1).and_then(|p| p.split('"').nth(1)).unwrap_or(name.trim());
            if !name.is_empty() && !out.iter().any(|x: &String| x == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Cargo.lock の (名前, 版)（レジストリのものだけ）
fn locked(root: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) else { return Vec::new() };
    let mut out = Vec::new();
    for pkg in text.split("[[package]]") {
        let field = |k: &str| pkg.lines().find_map(|l| l.trim().strip_prefix(k).and_then(|r| r.trim().strip_prefix('=')).map(|v| v.trim().trim_matches('"').to_string()));
        if let (Some(n), Some(v), Some(src)) = (field("name"), field("version"), field("source")) {
            if src.starts_with("registry") {
                out.push((n, v));
            }
        }
    }
    out
}

/// ソースの合計の大きさ
fn src_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                total += e.metadata().map_or(0, |m| m.len());
            }
            if total > MAX_CRATE_BYTES {
                return total;
            }
        }
    }
    total
}

/// ワークスペース `root` の索引を作る
pub fn load(root: &Path) -> CargoIndex {
    let mut ix = CargoIndex::default();
    match std_library(root) {
        Some(lib) => {
            for c in ["core", "alloc", "std"] {
                scan_crate(&mut ix, c, &lib.join(c).join("src"));
            }
        }
        None => ix.skipped.push("std（rust-src が無い。rustup component add rust-src）".into()),
    }
    let lock = locked(root);
    let registry = cargo_home().map(|h| h.join("registry").join("src"));
    let regs: Vec<PathBuf> = registry.into_iter().flat_map(|r| std::fs::read_dir(r).into_iter().flatten().flatten().map(|e| e.path())).collect();
    for dep in direct_deps(root) {
        // いちばん新しい版
        let Some(ver) = lock.iter().filter(|(n, _)| *n == dep).map(|(_, v)| v.clone()).max_by(|a, b| nagi_workers::sys::version_key(a).cmp(&nagi_workers::sys::version_key(b))) else { continue };
        let Some(src) = regs.iter().map(|r| r.join(format!("{dep}-{ver}")).join("src")).find(|p| p.is_dir()) else {
            ix.skipped.push(format!("{dep}（ソースが無い）"));
            continue;
        };
        if src_bytes(&src) > MAX_CRATE_BYTES {
            ix.skipped.push(format!("{dep}（大きすぎる）"));
            continue;
        }
        scan_crate(&mut ix, &dep.replace('-', "_"), &src);
    }
    ix.pool = Default::default();
    ix
}

/// 生態系 Cargo（Rust が使う）
pub struct Cargo;

pub static ECO: Cargo = Cargo;

impl super::Ecosystem for Cargo {
    fn name(&self) -> &'static str {
        super::CARGO
    }

    /// Cargo.toml のあるワークスペース（Cargo.lock のあるところ）
    fn root(&self, root: &Path) -> Option<PathBuf> {
        let mut d = Some(root);
        while let Some(x) = d {
            if x.join("Cargo.lock").is_file() || (x.join("Cargo.toml").is_file() && x == root) {
                return Some(x.to_path_buf());
            }
            d = x.parent();
        }
        None
    }

    fn load(&self, root: &Path) -> Result<super::Loaded, String> {
        let ix = load(root);
        let mut log = format!("Rust のライブラリの索引: {} クレート・{} 項目・{} モジュールの項目", ix.crates.len(), ix.items.len(), ix.mods.len());
        if !ix.skipped.is_empty() {
            log.push_str(&format!("（読まなかったもの: {}）", ix.skipped.join("、")));
        }
        Ok(super::Loaded { index: Arc::new(ix), log })
    }

    fn rebuild_on(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|n| n == "Cargo.lock" || n == "Cargo.toml")
    }
}

/// 試験用: 文字から（ファイルは 1 つ）
#[cfg(test)]
pub fn from_text(crate_name: &str, text: &str) -> CargoIndex {
    let mut ix = CargoIndex::default();
    ix.files.push(PathBuf::from("lib.rs"));
    scan_file(&mut ix, crate_name, &clean(text), 0, None);
    ix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_items() {
        let src = r#"
//! doc
pub mod inner {
    pub fn helper() {}
}
pub use self::inner::{helper, Other as Renamed};

/// a { brace in doc
pub struct Vec<T, A = Global> {
    buf: RawVec<T, A>,
}

impl<T> Vec<T> {
    pub const fn new() -> Vec<T> {
        Vec { buf: RawVec::NEW }
    }

    #[unstable(feature = "x", issue = "1")]
    pub fn push_nightly(&mut self) {}

    pub fn push(&mut self, value: T) {
        let c = '{';
        let s = "}}}";
    }

    fn private(&self) {}

    pub fn iter<'a>(&'a self)
        -> Iter<'a, T>
    {
        todo!()
    }
}

impl<T, A: Allocator> ops::Deref for Vec<T, A> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
    }
}

impl<T: Clone> Clone for Vec<T> {
    fn clone(&self) -> Self {
        todo!()
    }
}

pub trait Iterator {
    type Item;

    fn next(&mut self) -> Option<Self::Item>;

    fn count(self) -> usize
    where
        Self: Sized,
    {
        0
    }
}

impl<T: fmt::Display + ?Sized> ToString for T {
    default fn to_string(&self) -> String {
        String::new()
    }
}

impl<T> [T] {
    pub const fn len(&self) -> usize {
        0
    }
}
"#;
        let ix = from_text("alloc", src);
        let names = |owner: &str| ix.items_of(owner).map(|i| i.name.to_string()).collect::<Vec<_>>();
        assert_eq!(names("Vec"), vec!["new", "push", "iter", "Target", "deref", "clone"], "private は出さない");
        let iter = ix.items_of("Vec").find(|i| &*i.name == "iter").unwrap();
        assert!(iter.method && iter.params == 0 && &*iter.ret == "Iter<'a, T>", "{iter:?}");
        let new = ix.items_of("Vec").find(|i| &*i.name == "new").unwrap();
        assert!(!new.method && &*new.ret == "Vec<T>" && &*new.target == "Vec<T>");
        assert_eq!(names("trait:Iterator"), vec!["Item", "next", "count"]);
        assert_eq!(ix.deref("Vec"), Some("[T]"));
        assert_eq!(ix.traits_of("Vec"), vec!["Clone", "Deref"]);
        assert_eq!(ix.blanket_traits(), vec!["ToString"]);
        assert_eq!(names("slice"), vec!["len"]);
        let m: Vec<String> = ix.module("alloc").map(|m| format!("{}{}", m.kind, m.name)).collect();
        assert_eq!(m, vec!["minner", "uhelper", "uRenamed", "cVec", "IIterator"]);
        assert_eq!(ix.module("alloc::inner").map(|m| m.name.to_string()).collect::<Vec<_>>(), vec!["helper"]);
        assert_eq!(type_name("&'a mut std::vec::Vec<T>"), "Vec");
    }

    /// 1 つのファイルの項目（NAGI_CARGO_FILE=… cargo test -p nagi-lang cargo_file -- --ignored --nocapture）
    #[test]
    #[ignore]
    fn cargo_file() {
        let Some(f) = std::env::var_os("NAGI_CARGO_FILE").map(PathBuf::from) else { return };
        let ix = from_text("x", &std::fs::read_to_string(&f).unwrap());
        for i in &ix.items {
            println!("{} {} {} {}", i.line + 1, i.owner, i.trait_, i.name);
        }
    }

    /// 実際の rust-src と、このワークスペースの依存（NAGI_CARGO_ROOT=… cargo test -p nagi-lang cargo_real -- --ignored --nocapture）
    #[test]
    #[ignore]
    fn cargo_real() {
        let Some(root) = std::env::var_os("NAGI_CARGO_ROOT").map(PathBuf::from) else { return };
        let t = std::time::Instant::now();
        let ix = load(&root);
        println!("{:?}: {} クレート {:?}・{} 項目・{} モジュールの項目・読まなかった {:?}", t.elapsed(), ix.crates.len(), ix.crates, ix.items.len(), ix.mods.len(), ix.skipped);
        for (o, n) in [("Vec", "push"), ("slice", "iter"), ("str", "trim"), ("String", "push_str"), ("HashMap", "insert"), ("Option", "unwrap"), ("trait:Iterator", "map"), ("Regex", "is_match")] {
            println!("{o}::{n} {:?}", ix.items_of(o).find(|i| &*i.name == n).map(|i| (&i.ret, &i.target, ix.file(i.file), i.line)));
        }
        println!("std::collections: {:?}", ix.module("std::collections").map(|m| m.name.to_string()).collect::<Vec<_>>());
        println!("deref String {:?} Vec {:?}", ix.deref("String"), ix.deref("Vec"));
        let strs: usize = ix.items.iter().map(|i| i.owner.len() + i.trait_.len() + i.target.len() + i.name.len() + i.ret.len()).sum::<usize>() + ix.mods.iter().map(|m| m.module.len() + m.name.len()).sum::<usize>();
        let fixed = ix.items.len() * std::mem::size_of::<LibItem>() + ix.mods.len() * std::mem::size_of::<ModItem>();
        println!("大きさ: 文字 {} KB・固定 {} KB", strs / 1024, fixed / 1024);
    }
}
