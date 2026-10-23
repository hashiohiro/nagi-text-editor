//! TypeScript の型の宣言（`.d.ts`）の索引（LSP も tsc も使わない）。
//!
//! TypeScript の組み込みの型（`node_modules/typescript/lib` の `lib.*.d.ts`。tsconfig の target・lib から `/// <reference lib>` をたどる）と、
//! `node_modules` のパッケージの `.d.ts`（package.json の types・exports。import・re-export をたどる）を、メモリに写して（mmap）持ち、
//! 上の段の宣言（interface・class・type・enum・namespace・function・var）の名前と範囲だけを表にする。中身（メンバー）は使うときに
//! その範囲を読む（members）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 宣言の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclKind {
    Interface,
    Class,
    Type,
    Enum,
    Namespace,
    Function,
    Var,
}

/// 上の段の宣言（namespace の中は `NS.Name`）
#[derive(Clone, Debug)]
pub struct Decl {
    pub name: String,
    pub kind: DeclKind,
    pub file: u32,
    /// 宣言の範囲（byte。修飾子の後の、interface などの語から）
    pub start: u32,
    pub end: u32,
    /// export しているか（スクリプトのファイル・declare global の中は全部が大域）
    pub exported: bool,
    /// 大域の宣言（lib の型、declare global の中、import・export の無いファイル）
    pub global: bool,
}

/// import（`import { A as B } from 'm'` は local B・imported A。`* as X` は imported `*`、default は `default`）
#[derive(Clone, Debug)]
pub struct Import {
    pub local: String,
    pub imported: String,
    pub from: String,
    /// 解決したファイル（パッケージは入口の .d.ts）
    pub file: Option<u32>,
}

/// re-export（`export * from 'm'` は names が None、`export { A as B } from 'm'` は (B, A)）
#[derive(Clone, Debug)]
pub struct ReExport {
    pub names: Option<Vec<(String, String)>>,
    pub from: String,
    pub file: Option<u32>,
}

pub struct DtsFile {
    pub path: PathBuf,
    map: Arc<nagi_core::mmap::Mapped>,
    pub imports: Vec<Import>,
    pub reexports: Vec<ReExport>,
    /// `export { A, B as C }`（from の無いもの）: (外の名前, 中の名前)
    pub exports: Vec<(String, String)>,
}

impl DtsFile {
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.map).unwrap_or("")
    }
}

/// 索引
#[derive(Default)]
pub struct DtsIndex {
    pub files: Vec<DtsFile>,
    /// 名前で並べた宣言
    decls: Vec<Decl>,
    /// パッケージの名前（`rxjs`・`@angular/common/http`）→ 入口のファイル
    modules: HashMap<String, u32>,
    /// tsconfig の baseUrl と paths（`my-lib` → [`dist/my-lib`]、`@app/*` → [`src/app/*`]）
    pub base_url: PathBuf,
    pub paths: Vec<(String, Vec<String>)>,
    /// パス → ファイルの番号
    by_path: HashMap<PathBuf, u32>,
    /// import の解決の結果（(フォルダ, 指定) → パス）
    resolved: std::sync::Mutex<HashMap<(PathBuf, String), Option<PathBuf>>>,
}

impl DtsIndex {
    /// 名前 `name` の宣言（全部）
    pub fn named(&self, name: &str) -> &[Decl] {
        let lo = self.decls.partition_point(|d| d.name.as_str() < name);
        let hi = lo + self.decls[lo..].partition_point(|d| d.name == name);
        &self.decls[lo..hi]
    }

    /// 名前が `prefix` で始まる宣言（namespace の中）
    pub fn with_prefix(&self, prefix: &str) -> Vec<&Decl> {
        let lo = self.decls.partition_point(|d| d.name.as_str() < prefix);
        self.decls[lo..].iter().take_while(|d| d.name.starts_with(prefix)).collect()
    }

    /// 大域の `name`（lib の型・declare global。同じ名前のものは合わせて使う）
    pub fn globals(&self, name: &str) -> Vec<&Decl> {
        self.named(name).iter().filter(|d| d.global).collect()
    }

    /// ファイル `file` の中の宣言 `name`（export していないものも）
    pub fn in_file(&self, file: u32, name: &str) -> Vec<&Decl> {
        self.named(name).iter().filter(|d| d.file == file).collect()
    }

    /// パッケージの入口
    pub fn module(&self, spec: &str) -> Option<u32> {
        self.modules.get(spec).copied()
    }

    /// モジュール（ファイル）`file` が export している `name` の宣言（re-export・import してから export もたどる）
    pub fn exported(&self, file: u32, name: &str) -> Vec<&Decl> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        self.exported_into(file, name, &mut out, &mut seen, 0);
        out
    }

    fn exported_into<'a>(&'a self, file: u32, name: &str, out: &mut Vec<&'a Decl>, seen: &mut std::collections::HashSet<(u32, String)>, depth: usize) {
        if depth > 12 || !seen.insert((file, name.to_string())) {
            return;
        }
        let f = &self.files[file as usize];
        out.extend(self.named(name).iter().filter(|d| d.file == file && d.exported));
        // export { A as B }（中の名前を、宣言か import から）
        for (outer, inner) in f.exports.iter().filter(|(o, _)| o == name) {
            let _ = outer;
            out.extend(self.named(inner).iter().filter(|d| d.file == file));
            for imp in f.imports.iter().filter(|i| i.local == *inner) {
                if let Some(t) = imp.file {
                    self.exported_into(t, &imp.imported, out, seen, depth + 1);
                }
            }
        }
        for r in &f.reexports {
            let Some(t) = r.file else { continue };
            match &r.names {
                None => self.exported_into(t, name, out, seen, depth + 1),
                Some(list) => {
                    for (outer, inner) in list.iter().filter(|(o, _)| o == name) {
                        let _ = outer;
                        self.exported_into(t, inner, out, seen, depth + 1);
                    }
                }
            }
        }
    }

    /// ファイル `file` の中で名前 `name` が指す宣言（そのファイルの宣言 → import → 大域）
    pub fn resolve_in(&self, file: u32, name: &str) -> Vec<&Decl> {
        let (head, rest) = name.split_once('.').map_or((name, None), |(a, b)| (a, Some(b)));
        let local = self.in_file(file, name);
        if !local.is_empty() {
            // 大域の宣言は、ほかのファイルの同じ名前と合わせる（lib.es5 と lib.es2015.core の ArrayConstructor など）
            if local.iter().all(|d| d.global) {
                return self.globals(name);
            }
            return local;
        }
        for imp in self.files[file as usize].imports.iter().filter(|i| i.local == head) {
            let Some(t) = imp.file else { continue };
            let found = match (imp.imported.as_str(), rest) {
                ("*", Some(r)) => self.exported(t, r),
                ("*", None) => Vec::new(),
                (n, Some(r)) => self.exported(t, &format!("{n}.{r}")),
                (n, None) => self.exported(t, n),
            };
            if !found.is_empty() {
                return found;
            }
        }
        self.globals(name)
    }

    /// 宣言の文字
    pub fn text(&self, d: &Decl) -> &str {
        self.files[d.file as usize].text().get(d.start as usize..d.end as usize).unwrap_or("")
    }

    pub fn decl_count(&self) -> usize {
        self.decls.len()
    }
}

// ---- 字句 ----

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// 空白とコメントを飛ばした位置
fn skip_ws(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < end && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < end && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(end);
            }
            _ => break,
        }
    }
    i
}

/// skip_ws と同じだが、`///`（/// <reference … />）の前で止まる
fn skip_ws_to_directive(b: &[u8], mut i: usize, end: usize) -> usize {
    loop {
        i = skip_plain_ws(b, i, end);
        if i >= end || b[i..].starts_with(b"///") {
            return i;
        }
        if b[i..].starts_with(b"//") {
            while i < end && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < end && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(end);
        } else {
            return i;
        }
    }
}

/// 空白だけを飛ばす
fn skip_plain_ws(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// 文字列の終わりの次（`i` は引用符）
fn skip_string(b: &[u8], mut i: usize, end: usize) -> usize {
    let q = b[i];
    i += 1;
    while i < end && b[i] != q {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    (i + 1).min(end)
}

/// 識別子を読む（無ければ None）
fn ident(t: &str, i: usize, end: usize) -> Option<(&str, usize)> {
    let b = t.as_bytes();
    let mut j = i;
    while j < end && is_ident(b[j]) {
        j += 1;
    }
    (j > i).then(|| (&t[i..j], j))
}

/// 括弧の対応を見て、外側の `stop` のどれかの位置（無ければ end。外の } に出たらその位置）
fn find_top(t: &str, mut i: usize, end: usize, stop: &[u8], angle: bool) -> usize {
    let b = t.as_bytes();
    let mut depth = 0i32;
    while i < end {
        let c = b[i];
        match c {
            b'"' | b'\'' | b'`' => {
                i = skip_string(b, i, end);
                continue;
            }
            b'/' if matches!(b.get(i + 1), Some(b'/') | Some(b'*')) => {
                i = skip_ws(b, i, end);
                continue;
            }
            _ => {}
        }
        if depth == 0 && stop.contains(&c) {
            return i;
        }
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b'<' if angle => depth += 1,
            b'>' if angle && i > 0 && b[i - 1] != b'=' => depth -= 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    end
}

/// `{` の位置から、対応する `}` の次
fn match_brace(t: &str, open: usize, end: usize) -> usize {
    let close = find_top(t, open + 1, end, &[], false);
    (close + 1).min(end)
}

/// 文の終わり（外側の ; の次。; の無い文は、次の行が新しい文で始まるところ）
fn stmt_end(t: &str, mut i: usize, end: usize) -> usize {
    let b = t.as_bytes();
    loop {
        let j = find_top(t, i, end, b";\n", false);
        if j >= end || b[j] == b'}' {
            return j;
        }
        if b[j] == b';' {
            return j + 1;
        }
        // 改行: 前が続きの記号でなく、次が文の頭の語なら終わり
        let prev = t[i..j].trim_end();
        let k = skip_ws(b, j, end);
        let next = ident(t, k, end).map_or("", |x| x.0);
        let cont = prev.ends_with(['|', '&', ':', '=', ',', '(', '<']) || prev.ends_with("=>") || prev.is_empty();
        if !cont && (STMT.contains(&next) || k >= end || b[k] == b'}') {
            return j;
        }
        i = j + 1;
    }
}

const STMT: &[&str] = &["export", "declare", "interface", "class", "type", "enum", "namespace", "module", "function", "var", "let", "const", "import", "abstract", "default"];

/// 引用符の中身（`'x'` → x）
fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '\'' || c == '"' || c == '`')
}

/// 1 ファイルを読んだもの
#[derive(Default)]
struct Scan {
    decls: Vec<(String, DeclKind, u32, u32, bool, bool)>,
    imports: Vec<Import>,
    reexports: Vec<ReExport>,
    exports: Vec<(String, String)>,
    ref_libs: Vec<String>,
    ref_paths: Vec<String>,
    module: bool,
}

/// `{ A, B as C, type D }` を (外, 中) に（export）・(中, 外) の並びで
fn name_list(s: &str) -> Vec<(String, String)> {
    s.trim().trim_start_matches('{').trim_end_matches('}').split(',').filter_map(|p| {
        let p = p.trim().trim_start_matches("type ").trim();
        if p.is_empty() {
            return None;
        }
        Some(match p.split_once(" as ") {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (p.to_string(), p.to_string()),
        })
    }).collect()
}

fn scan_block(t: &str, mut i: usize, end: usize, prefix: &str, global: bool, out: &mut Scan) {
    let b = t.as_bytes();
    loop {
        i = skip_ws_to_directive(b, i, end);
        if i >= end {
            return;
        }
        // /// <reference lib="x" /> などはコメントとして飛ばす前に読む
        if t[i..].starts_with("///") {
            let line_end = t[i..end].find('\n').map_or(end, |k| i + k);
            let line = &t[i..line_end];
            for (key, list) in [("lib=", &mut out.ref_libs), ("path=", &mut out.ref_paths)] {
                if let Some(k) = line.find(key) {
                    let v = &line[k + key.len()..];
                    let v = v.trim_start().trim_start_matches(['"', '\'']);
                    list.push(v.split(['"', '\'']).next().unwrap_or("").to_string());
                }
            }
            i = line_end;
            continue;
        }
        if b[i] == b';' {
            i += 1;
            continue;
        }
        let start = i;
        // 修飾子
        let mut exported = false;
        let mut word = None;
        let mut j = i;
        while let Some((w, k)) = ident(t, j, end) {
            match w {
                "export" => exported = true,
                "declare" | "default" | "abstract" | "async" => {}
                "const" if ident(t, skip_ws(b, k, end), end).is_some_and(|x| x.0 == "enum") => {}
                _ => {
                    word = Some((w, j, k));
                    break;
                }
            }
            j = skip_ws(b, k, end);
        }
        let Some((w, wstart, wend)) = word else {
            // export { … } from / export * from / export = / 識別子で始まらない文
            let rest_end = stmt_end(t, j, end);
            let stmt = &t[j..rest_end];
            if exported {
                out.module = true;
                let stmt = stmt.trim_end_matches(';');
                if let Some(r) = stmt.strip_prefix('*') {
                    let from = r.split("from").nth(1).map(unquote).unwrap_or("").to_string();
                    let r = r.trim_start();
                    if let Some(ns) = r.strip_prefix("as ") {
                        let ns = ns.split_whitespace().next().unwrap_or("").to_string();
                        out.imports.push(Import { local: ns.clone(), imported: "*".into(), from: from.clone(), file: None });
                        out.exports.push((ns.clone(), ns));
                    } else {
                        out.reexports.push(ReExport { names: None, from, file: None });
                    }
                } else if stmt.starts_with('{') {
                    let close = stmt.find('}').unwrap_or(stmt.len());
                    let names = name_list(&stmt[..close.min(stmt.len())]);
                    match stmt[close..].split_once("from") {
                        Some((_, from)) => out.reexports.push(ReExport { names: Some(names.into_iter().map(|(a, b)| (b, a)).collect()), from: unquote(from).to_string(), file: None }),
                        None => out.exports.extend(names.into_iter().map(|(a, b)| (b, a))),
                    }
                }
            }
            i = rest_end.max(start + 1);
            continue;
        };
        let after = skip_ws(b, wend, end);
        match w {
            "import" => {
                out.module = true;
                let e = stmt_end(t, after, end);
                parse_import(t[after..e].trim_end_matches(';').trim(), &mut out.imports);
                i = e.max(start + 1);
            }
            "interface" | "class" | "enum" => {
                let Some((name, k)) = ident(t, after, end) else {
                    i = stmt_end(t, after, end).max(start + 1);
                    continue;
                };
                let open = find_top(t, k, end, b"{;", true);
                let e = if open < end && b[open] == b'{' { match_brace(t, open, end) } else { (open + 1).min(end) };
                let kind = match w {
                    "interface" => DeclKind::Interface,
                    "class" => DeclKind::Class,
                    _ => DeclKind::Enum,
                };
                out.decls.push((format!("{prefix}{name}"), kind, wstart as u32, e as u32, exported || !prefix.is_empty(), global));
                if exported {
                    out.module = true;
                }
                i = e.max(start + 1);
            }
            "namespace" | "module" | "global" => {
                // declare global { … } / declare module 'm' { … } / namespace A.B { … }
                let (name, k) = if w == "global" {
                    ("".to_string(), after)
                } else if matches!(b.get(after), Some(b'\'') | Some(b'"')) {
                    let k = skip_string(b, after, end);
                    (format!("'{}'", unquote(&t[after..k])), k)
                } else {
                    let mut k = after;
                    while k < end && (is_ident(b[k]) || b[k] == b'.') {
                        k += 1;
                    }
                    (t[after..k].to_string(), k)
                };
                let open = find_top(t, k, end, b"{;", false);
                if open >= end || b[open] != b'{' {
                    i = (open + 1).min(end).max(start + 1);
                    continue;
                }
                let e = match_brace(t, open, end);
                if w == "global" {
                    scan_block(t, open + 1, e - 1, "", true, out);
                } else if name.starts_with('\'') {
                    // 名前のあるモジュールの宣言（declare module 'fs' { … }）: 中は `'fs'.名前` として、大域にしない
                    let mut inner = Scan::default();
                    scan_block(t, open + 1, e - 1, &format!("{name}."), false, &mut inner);
                    out.decls.extend(inner.decls.into_iter().map(|mut d| {
                        d.5 = false;
                        d.4 = true;
                        d
                    }));
                } else {
                    out.decls.push((format!("{prefix}{name}"), DeclKind::Namespace, wstart as u32, e as u32, exported || !prefix.is_empty(), global));
                    scan_block(t, open + 1, e - 1, &format!("{prefix}{name}."), global, out);
                }
                if exported {
                    out.module = true;
                }
                i = e.max(start + 1);
            }
            "type" | "function" | "var" | "let" | "const" => {
                let Some((name, _)) = ident(t, after, end) else {
                    i = stmt_end(t, after, end).max(start + 1);
                    continue;
                };
                // function の本体（.ts）は { } まで
                let e = stmt_end(t, after, end);
                let kind = match w {
                    "type" => DeclKind::Type,
                    "function" => DeclKind::Function,
                    _ => DeclKind::Var,
                };
                out.decls.push((format!("{prefix}{name}"), kind, wstart as u32, e as u32, exported || !prefix.is_empty(), global));
                if exported {
                    out.module = true;
                }
                i = e.max(start + 1);
            }
            _ => {
                i = stmt_end(t, wstart, end).max(start + 1);
            }
        }
    }
}

/// import の文（`import` の後）
fn parse_import(s: &str, out: &mut Vec<Import>) {
    let s = s.strip_prefix("type ").unwrap_or(s);
    // import X = require('m')
    if let Some((local, r)) = s.split_once('=') {
        if let Some(m) = r.trim().strip_prefix("require(") {
            out.push(Import { local: local.trim().to_string(), imported: "*".into(), from: unquote(m.trim_end_matches(')')).to_string(), file: None });
        }
        return;
    }
    let Some((what, from)) = s.rsplit_once("from") else { return };
    let from = unquote(from).to_string();
    let what = what.trim();
    // default, { … } / * as X
    let (default, rest) = match what.find(['{', '*']) {
        Some(0) => ("", what),
        Some(k) => (what[..k].trim().trim_end_matches(','), &what[k..]),
        None => (what, ""),
    };
    if !default.is_empty() {
        out.push(Import { local: default.trim().to_string(), imported: "default".into(), from: from.clone(), file: None });
    }
    if let Some(ns) = rest.trim().strip_prefix('*') {
        let local = ns.trim().trim_start_matches("as").trim().to_string();
        out.push(Import { local, imported: "*".into(), from: from.clone(), file: None });
    } else if rest.trim().starts_with('{') {
        for (imported, local) in name_list(rest) {
            out.push(Import { local, imported, from: from.clone(), file: None });
        }
    }
}

// ---- 読み込み ----

/// JSON（コメント・末尾の , を許す tsconfig も）
fn read_json(p: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(p).ok()?;
    if let Ok(v) = serde_json::from_str(&text) {
        return Some(v);
    }
    // コメントを外す（文字列の中は残す）
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let j = skip_string(b, i, b.len());
                out.push_str(&text[i..j]);
                i = j;
            }
            b'/' if matches!(b.get(i + 1), Some(b'/') | Some(b'*')) => i = skip_ws(b, i, b.len()),
            _ => {
                let c = text[i..].chars().next()?;
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    let out = regex::Regex::new(r",(\s*[}\]])").ok()?.replace_all(&out, "$1").into_owned();
    serde_json::from_str(&out).ok()
}

/// package.json の exports の、型の入口（条件の入れ子の中の types / typings）
fn export_types(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => (s.ends_with(".d.ts") || s.ends_with(".d.mts") || s.ends_with(".d.cts")).then(|| s.clone()),
        serde_json::Value::Object(m) => {
            for k in ["types", "typings"] {
                if let Some(serde_json::Value::String(s)) = m.get(k) {
                    return Some(s.clone());
                }
            }
            m.values().find_map(export_types)
        }
        serde_json::Value::Array(a) => a.iter().find_map(export_types),
        _ => None,
    }
}

/// パッケージの名前とその中のパス（`@angular/common/http` → (`@angular/common`, `http`)）
fn split_spec(spec: &str) -> (&str, &str) {
    let n = if spec.starts_with('@') { 2 } else { 1 };
    let mut it = spec.match_indices('/').map(|(i, _)| i);
    match it.nth(n - 1) {
        Some(k) => (&spec[..k], &spec[k + 1..]),
        None => (spec, ""),
    }
}

/// `.d.ts` の候補（`x` → x.d.ts・x/index.d.ts）
fn dts_file(base: &Path) -> Option<PathBuf> {
    let s = base.to_string_lossy();
    if s.ends_with(".d.ts") && base.is_file() {
        return Some(base.to_path_buf());
    }
    let s = s.trim_end_matches(".js").trim_end_matches(".mjs");
    [format!("{s}.d.ts"), format!("{s}/index.d.ts"), format!("{s}.d.mts")].into_iter().map(PathBuf::from).find(|p| p.is_file())
}

/// パッケージのモジュール名を `.d.ts` に
fn resolve_package(node_modules: &Path, spec: &str) -> Option<PathBuf> {
    let (pkg, sub) = split_spec(spec);
    for dir in [node_modules.join(pkg), node_modules.join("@types").join(pkg.trim_start_matches('@').replace('/', "__"))] {
        if !dir.is_dir() {
            continue;
        }
        let json = read_json(&dir.join("package.json"));
        let key = if sub.is_empty() { ".".to_string() } else { format!("./{sub}") };
        if let Some(t) = json.as_ref().and_then(|j| j.get("exports")).and_then(|e| e.get(&key)).and_then(export_types) {
            if let Some(p) = dts_file(&dir.join(t.trim_start_matches("./"))) {
                return Some(p);
            }
        }
        if sub.is_empty() {
            for k in ["types", "typings"] {
                if let Some(t) = json.as_ref().and_then(|j| j.get(k)).and_then(|v| v.as_str()) {
                    if let Some(p) = dts_file(&dir.join(t.trim_start_matches("./"))) {
                        return Some(p);
                    }
                }
            }
            if let Some(p) = dts_file(&dir.join("index")) {
                return Some(p);
            }
        } else {
            if let Some(p) = dts_file(&dir.join(sub)) {
                return Some(p);
            }
            // 中のフォルダの package.json（types）
            if let Some(t) = read_json(&dir.join(sub).join("package.json")).and_then(|j| j.get("types").or_else(|| j.get("typings")).and_then(|v| v.as_str().map(String::from))) {
                if let Some(p) = dts_file(&dir.join(sub).join(t.trim_start_matches("./"))) {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// tsconfig の target・lib から、入口の lib のファイル名（`lib.es2022.full.d.ts` など）
fn lib_entries(root: &Path) -> Vec<String> {
    let cfg = read_json(&root.join("tsconfig.json"));
    let opts = cfg.as_ref().and_then(|c| c.get("compilerOptions"));
    if let Some(libs) = opts.and_then(|o| o.get("lib")).and_then(|l| l.as_array()) {
        return libs.iter().filter_map(|v| v.as_str()).map(|l| format!("lib.{}.d.ts", l.to_lowercase())).collect();
    }
    let target = opts.and_then(|o| o.get("target")).and_then(|v| v.as_str()).unwrap_or("es5").to_lowercase();
    vec![match target.as_str() {
        "es3" | "es5" => "lib.d.ts".to_string(),
        "es6" | "es2015" => "lib.es6.d.ts".to_string(),
        t => format!("lib.{t}.full.d.ts"),
    }]
}

/// 読む量の上限（node_modules の .d.ts が大きすぎるとき）
const MAX_BYTES: u64 = 96 << 20;

impl DtsIndex {
    /// プロジェクト `root` の組み込みの型と、package.json の依存（とそこから import するもの）の `.d.ts` を読む
    pub fn load(root: &Path) -> DtsIndex {
        let nm = root.join("node_modules");
        let mut ix = DtsIndex::default();
        let mut by_path: HashMap<PathBuf, u32> = HashMap::new();
        let mut scans: Vec<Scan> = Vec::new();
        let mut bytes = 0u64;
        // (ファイル, 大域か)
        let mut todo: Vec<PathBuf> = Vec::new();
        let lib_dir = nm.join("typescript").join("lib");
        for e in lib_entries(root) {
            todo.push(lib_dir.join(e));
        }
        // package.json の依存の入口
        if let Some(pj) = read_json(&root.join("package.json")) {
            for key in ["dependencies", "devDependencies"] {
                for name in pj.get(key).and_then(|d| d.as_object()).into_iter().flat_map(|m| m.keys()) {
                    if let Some(p) = resolve_package(&nm, name) {
                        ix.modules.entry(name.clone()).or_insert(u32::MAX);
                        todo.push(p.clone());
                        by_path.entry(p).or_insert(u32::MAX);
                    }
                }
            }
        }
        by_path.retain(|_, v| *v != u32::MAX);
        let mut pending_specs: Vec<(String, PathBuf)> = Vec::new();
        while let Some(p) = todo.pop() {
            let p = std::path::absolute(&p).unwrap_or(p);
            if by_path.contains_key(&p) || bytes > MAX_BYTES {
                continue;
            }
            let Some(map) = nagi_core::mmap::Mapped::open(&p) else { continue };
            bytes += map.len() as u64;
            let id = ix.files.len() as u32;
            by_path.insert(p.clone(), id);
            let text = std::str::from_utf8(&map).unwrap_or("");
            let mut sc = Scan::default();
            scan_block(text, 0, text.len(), "", false, &mut sc);
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            for l in &sc.ref_libs {
                todo.push(lib_dir.join(format!("lib.{}.d.ts", l.to_lowercase())));
            }
            for r in &sc.ref_paths {
                todo.push(dir.join(r));
            }
            // import・re-export の先も読む（相対はそのファイル、パッケージは node_modules から）
            for spec in sc.imports.iter().map(|i| &i.from).chain(sc.reexports.iter().map(|r| &r.from)) {
                let target = if spec.starts_with('.') { dts_file(&dir.join(spec)) } else { resolve_package(&nm, spec) };
                if let Some(t) = target {
                    pending_specs.push((spec.clone(), t.clone()));
                    todo.push(t);
                }
            }
            ix.files.push(DtsFile { path: p, map, imports: Vec::new(), reexports: Vec::new(), exports: Vec::new() });
            scans.push(sc);
        }
        // import の先をファイルの番号に
        let lookup = |dir: &Path, spec: &str| -> Option<u32> {
            let t = if spec.starts_with('.') { dts_file(&dir.join(spec)) } else { resolve_package(&nm, spec) }?;
            by_path.get(&std::path::absolute(&t).unwrap_or(t)).copied()
        };
        let mut resolved: HashMap<(PathBuf, String), Option<u32>> = HashMap::new();
        for (id, sc) in scans.into_iter().enumerate() {
            let dir = ix.files[id].path.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut res = |spec: &str| -> Option<u32> {
                let key = (if spec.starts_with('.') { dir.clone() } else { PathBuf::new() }, spec.to_string());
                *resolved.entry(key).or_insert_with(|| lookup(&dir, spec))
            };
            let global = !sc.module;
            let f = &mut ix.files[id];
            f.imports = sc.imports.into_iter().map(|mut i| {
                i.file = res(&i.from);
                i
            }).collect();
            f.reexports = sc.reexports.into_iter().map(|mut r| {
                r.file = res(&r.from);
                r
            }).collect();
            f.exports = sc.exports;
            for (name, kind, start, end, exported, g) in sc.decls {
                // declare module 'm' の中（'m'.名前）は大域でない
                let global = (g || global) && !name.starts_with('\'');
                ix.decls.push(Decl { name, kind, file: id as u32, start, end, exported, global });
            }
        }
        // パッケージの入口
        for (spec, _) in &pending_specs {
            if !spec.starts_with('.') {
                if let Some(id) = lookup(Path::new(""), spec) {
                    ix.modules.insert(spec.clone(), id);
                }
            }
        }
        let names: Vec<String> = ix.modules.keys().cloned().collect();
        for n in names {
            if ix.modules[&n] == u32::MAX {
                match lookup(Path::new(""), &n) {
                    Some(id) => {
                        ix.modules.insert(n, id);
                    }
                    None => {
                        ix.modules.remove(&n);
                    }
                }
            }
        }
        ix.decls.sort_by(|a, b| a.name.cmp(&b.name));
        ix.decls.shrink_to_fit();
        (ix.base_url, ix.paths) = tsconfig_paths(root);
        ix.by_path = ix.files.iter().enumerate().map(|(i, f)| (f.path.clone(), i as u32)).collect();
        ix
    }

    /// パッケージ `spec` を（まだなら）解決した入口。load で読んでいないパッケージは None
    pub fn package(&self, spec: &str) -> Option<u32> {
        self.module(spec)
    }
}

/// ソース（プロジェクトの .ts）を読んだもの: 上の段の宣言と import（解決はしない）
pub struct SourceScan {
    /// (名前, 種類, 範囲の始まり, 終わり, export しているか)
    pub decls: Vec<(String, DeclKind, u32, u32, bool)>,
    pub imports: Vec<Import>,
    pub reexports: Vec<ReExport>,
    /// export { A as B }: (外, 中)
    pub exports: Vec<(String, String)>,
}

/// ソースの上の段の宣言と import
pub fn scan_source(text: &str) -> SourceScan {
    let mut sc = Scan::default();
    scan_block(text, 0, text.len(), "", false, &mut sc);
    SourceScan { decls: sc.decls.into_iter().map(|(n, k, a, b, e, _)| (n, k, a, b, e)).collect(), imports: sc.imports, reexports: sc.reexports, exports: sc.exports }
}

/// ソースの候補（`x` → x.ts・x.tsx・x.d.ts・x/index.ts・x/index.d.ts。フォルダはその package.json の types・Angular の public-api.ts も）
fn source_file(base: &Path) -> Option<PathBuf> {
    let s = base.to_string_lossy().to_string();
    let found = [format!("{s}.ts"), format!("{s}.tsx"), format!("{s}.d.ts"), format!("{s}/index.ts"), format!("{s}/index.d.ts"), format!("{s}/src/public-api.ts"), format!("{s}/public-api.ts")].into_iter().map(PathBuf::from).find(|p| p.is_file());
    if found.is_some() || !base.is_dir() {
        return found.or_else(|| base.is_file().then(|| base.to_path_buf()));
    }
    let t = read_json(&base.join("package.json"))?;
    let types = t.get("types").or_else(|| t.get("typings")).and_then(|v| v.as_str())?;
    dts_file(&base.join(types.trim_start_matches("./")))
}

/// tsconfig の baseUrl と paths
fn tsconfig_paths(root: &Path) -> (PathBuf, Vec<(String, Vec<String>)>) {
    let cfg = read_json(&root.join("tsconfig.json"));
    let opts = cfg.as_ref().and_then(|c| c.get("compilerOptions"));
    let base = root.join(opts.and_then(|o| o.get("baseUrl")).and_then(|v| v.as_str()).unwrap_or("."));
    let paths = opts
        .and_then(|o| o.get("paths"))
        .and_then(|p| p.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default())).collect())
        .unwrap_or_default();
    (base, paths)
}

impl DtsIndex {
    /// import の指定（ファイル `dir` から）を `.ts`・`.d.ts` のパスに: 相対・tsconfig の paths・node_modules
    pub fn resolve_import(&self, root: &Path, dir: &Path, spec: &str) -> Option<PathBuf> {
        let key = (if spec.starts_with('.') { dir.to_path_buf() } else { PathBuf::new() }, spec.to_string());
        if let Some(r) = self.resolved.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return r;
        }
        let r = self.resolve_import_uncached(root, dir, spec);
        if let Ok(mut m) = self.resolved.lock() {
            m.insert(key, r.clone());
        }
        r
    }

    fn resolve_import_uncached(&self, root: &Path, dir: &Path, spec: &str) -> Option<PathBuf> {
        if spec.starts_with('.') {
            return source_file(&dir.join(spec));
        }
        for (pat, targets) in &self.paths {
            let captured = match pat.strip_suffix('*') {
                Some(prefix) => match spec.strip_prefix(prefix) {
                    Some(rest) => rest,
                    None => continue,
                },
                None if pat == spec => "",
                None => continue,
            };
            for t in targets {
                if let Some(p) = source_file(&self.base_url.join(t.replace('*', captured))) {
                    return Some(p);
                }
            }
        }
        resolve_package(&root.join("node_modules"), spec)
    }
}

/// 読み込んだファイルの番号（パスから）
impl DtsIndex {
    pub fn file_of(&self, path: &Path) -> Option<u32> {
        let p = std::path::absolute(path).ok()?;
        self.by_path.get(&p).copied()
    }
}

/// 裏で読む
pub fn load_task(root: PathBuf, waker: crate::Waker) -> nagi_core::task::Task<DtsIndex> {
    nagi_core::task::spawn("dts-index", waker, move |ctx| {
        ctx.send(DtsIndex::load(&root));
    })
}

/// interface・class・型の本体のメンバー
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub method: bool,
    pub optional: bool,
    pub is_static: bool,
    /// メソッドの型変数（`<T, U>` の中）
    pub type_params: String,
    /// メソッドの引数（`(` `)` の中）
    pub params: String,
    /// プロパティの型・メソッドの戻り値の型の文字
    pub ty: String,
}

/// 宣言の文字（`interface X<T> extends A<T>, B { … }`）の (型変数, 親の型の並び, 本体)
pub fn split_decl(text: &str) -> (String, Vec<String>, &str) {
    let open = find_top(text, 0, text.len(), b"{", true);
    let head = &text[..open.min(text.len())];
    let body = if open < text.len() { &text[open + 1..text.len().saturating_sub(1).max(open + 1)] } else { "" };
    // 型変数
    let name_end = head.find('<').filter(|&k| head[..k].split_whitespace().count() <= 3);
    let mut tparams = String::new();
    let mut rest = head;
    if let Some(k) = name_end {
        let close = find_top(head, k + 1, head.len(), b">", true);
        tparams = head[k + 1..close.min(head.len())].to_string();
        rest = &head[(close + 1).min(head.len())..];
    }
    let mut supers = Vec::new();
    for kw in ["extends", "implements"] {
        let Some(k) = find_word(rest, kw) else { continue };
        let after = &rest[k + kw.len()..];
        let stop = ["implements", "extends"].iter().filter_map(|w| find_word(after, w)).min().unwrap_or(after.len());
        let list = &after[..stop];
        let mut start = 0;
        let lb = list.as_bytes();
        while start < list.len() {
            let e = find_top(list, start, list.len(), b",", true);
            let s = list[start..e].trim();
            if !s.is_empty() {
                supers.push(s.to_string());
            }
            start = e + 1;
            let _ = lb;
        }
    }
    (tparams, supers, body)
}

/// 語として出てくる位置（外側の括弧の中は見ない）
fn find_word(s: &str, w: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' | b'(' | b'{' | b'[' => depth += 1,
            b'>' | b')' | b'}' | b']' => depth -= 1,
            _ if depth == 0 && s[i..].starts_with(w) && (i == 0 || !is_ident(b[i - 1])) && !b.get(i + w.len()).is_some_and(|&c| is_ident(c)) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// 本体のメンバー（interface・class・オブジェクトの型・enum）
pub fn members(body: &str) -> Vec<Member> {
    let b = body.as_bytes();
    let end = body.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < end {
        i = skip_ws(b, i, end);
        if i >= end {
            break;
        }
        // メンバーの終わり: 外側の ; , か、改行で次がメンバーの頭
        let mut e = i;
        loop {
            let j = find_top(body, e, end, b";,\n{", false);
            if j >= end {
                e = end;
                break;
            }
            if b[j] == b'{' {
                // メソッドの本体（.ts）・オブジェクトの型: 戻り値の型のあとの { は本体として飛ばし、メンバーを終える
                let before = body[i..j].trim_end();
                let close = match_brace(body, j, end);
                if before.ends_with(')') || (before.contains('(') && !before.ends_with(':') && !before.ends_with("=>") && !before.ends_with('|')) {
                    e = close;
                    break;
                }
                e = close;
                continue;
            }
            if b[j] == b'\n' {
                let prev = body[i..j].trim_end();
                let cont = prev.ends_with(['|', '&', ':', '=', ',', '(', '<', '?']) || prev.ends_with("=>") || prev.is_empty() || !balanced(prev);
                if cont {
                    e = j + 1;
                    continue;
                }
            }
            e = j;
            break;
        }
        let text = body[i..e].trim().trim_end_matches([';', ',']).trim();
        if let Some(m) = parse_member(text) {
            out.push(m);
        }
        i = (e + 1).max(i + 1);
    }
    out
}

/// 括弧が閉じているか
fn balanced(s: &str) -> bool {
    let mut d = 0i32;
    for c in s.bytes() {
        match c {
            b'(' | b'[' | b'{' => d += 1,
            b')' | b']' | b'}' => d -= 1,
            _ => {}
        }
    }
    d <= 0
}

fn parse_member(text: &str) -> Option<Member> {
    let mut s = text.trim();
    // デコレーター（@Input() など）
    while s.starts_with('@') {
        let b = s.as_bytes();
        let mut k = 1;
        while k < b.len() && (is_ident(b[k]) || b[k] == b'.') {
            k += 1;
        }
        if s[k..].starts_with('(') {
            k = match_paren(s, k);
        }
        s = s[k..].trim_start();
    }
    let mut m = Member::default();
    let mut getter = false;
    loop {
        let Some((w, k)) = ident(s, 0, s.len()) else { break };
        let rest = s[k..].trim_start();
        let modifier = matches!(w, "public" | "private" | "protected" | "static" | "readonly" | "abstract" | "declare" | "override" | "async" | "get" | "set" | "accessor");
        // 修飾子の語の後に名前が続くときだけ修飾子（get(): T のような名前もある）
        if modifier && rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '[' || c == '#' || c == '\'' || c == '"') {
            if w == "static" {
                m.is_static = true;
            }
            if w == "get" || w == "set" {
                getter = true;
            }
            s = rest;
            continue;
        }
        break;
    }
    if s.is_empty() || s.starts_with('[') {
        // 添字の型 [key: string]: T
        if let Some(close) = s.find(']') {
            let inner = &s[1..close];
            if inner.contains(':') {
                m.name = "[]".into();
                m.ty = s[close + 1..].trim_start().trim_start_matches(':').trim().to_string();
                return Some(m);
            }
        }
        return None;
    }
    let (name, k) = if s.starts_with(['\'', '"']) {
        let k = skip_string(s.as_bytes(), 0, s.len());
        (unquote(&s[..k]).to_string(), k)
    } else if s.starts_with(['(', '<']) {
        ("()".to_string(), 0)
    } else {
        let (n, k) = ident(s, s.starts_with('#') as usize, s.len())?;
        (n.to_string(), k)
    };
    m.name = if name == "new" && s[k..].trim_start().starts_with(['(', '<']) { "new".into() } else { name };
    let mut r = s[k..].trim_start();
    if let Some(x) = r.strip_prefix('?') {
        m.optional = true;
        r = x.trim_start();
    }
    if let Some(x) = r.strip_prefix('!') {
        r = x.trim_start();
    }
    if r.starts_with('<') {
        let close = find_top(r, 1, r.len(), b">", true);
        m.type_params = r[1..close.min(r.len())].to_string();
        r = r[(close + 1).min(r.len())..].trim_start();
    }
    if r.starts_with('(') {
        m.method = true;
        let close = match_paren(r, 0);
        m.params = r[1..close.saturating_sub(1).max(1)].to_string();
        r = r[close..].trim_start();
    }
    if let Some(x) = r.strip_prefix(':') {
        // 型（初期値 = … の前まで。メソッドの本体 { … } は除く）
        let e = find_top(x, 0, x.len(), b"=", true);
        let e = if x[..e].ends_with('=') || x[e..].starts_with("=>") { x.len() } else { e };
        let mut ty = x[..e].trim();
        if m.method {
            let mut k = 0;
            while let Some(open) = Some(find_top(ty, k, ty.len(), b"{", true)).filter(|&o| o < ty.len()) {
                let before = ty[..open].trim_end();
                if !before.is_empty() && !before.ends_with([':', '|', '&', '<', ',', '(']) && !before.ends_with("=>") {
                    ty = before;
                    break;
                }
                k = match_brace(ty, open, ty.len());
            }
        }
        m.ty = ty.to_string();
    } else if let Some(x) = r.strip_prefix('=') {
        // 型の無い初期値: 型を求めるのは使う側（初期値の文字を = 付きで持つ）
        m.ty = format!("={}", x.trim());
    }
    if getter {
        m.method = false;
    }
    Some(m)
}

/// `(` の位置から対応する `)` の次
fn match_paren(s: &str, open: usize) -> usize {
    let close = find_top(s, open + 1, s.len(), b")", false);
    (close + 1).min(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(t: &str) -> Scan {
        let mut sc = Scan::default();
        scan_block(t, 0, t.len(), "", false, &mut sc);
        sc
    }

    #[test]
    fn scans_declarations() {
        let t = "/// <reference lib=\"es2015\" />\nimport { A, B as C } from './a';\nimport * as ns from 'pkg';\nexport * from './b';\nexport { D as E } from './d';\n\
                 interface String {\n  trim(): string;\n  readonly length: number;\n}\ndeclare var console: Console;\nexport declare class Foo<T> extends Bar<T> implements Baz {\n  x: { a: number };\n  get(k: string): T | undefined;\n}\n\
                 export type U = 'a'\n  | 'b';\ndeclare namespace NS {\n  interface In {}\n}\ndeclare global {\n  interface Window { foo: string }\n}\nexport declare function f(a: number): void\nexport { Foo as Default };\n";
        let sc = scan(t);
        let names: Vec<(&str, DeclKind, bool)> = sc.decls.iter().map(|d| (d.0.as_str(), d.1, d.5)).collect();
        assert_eq!(
            names,
            vec![
                ("String", DeclKind::Interface, false),
                ("console", DeclKind::Var, false),
                ("Foo", DeclKind::Class, false),
                ("U", DeclKind::Type, false),
                ("NS", DeclKind::Namespace, false),
                ("NS.In", DeclKind::Interface, false),
                ("Window", DeclKind::Interface, true),
                ("f", DeclKind::Function, false),
            ]
        );
        assert_eq!(sc.ref_libs, vec!["es2015"]);
        assert_eq!(sc.imports.iter().map(|i| (i.local.as_str(), i.imported.as_str(), i.from.as_str())).collect::<Vec<_>>(), vec![("A", "A", "./a"), ("C", "B", "./a"), ("ns", "*", "pkg")]);
        assert_eq!(sc.reexports.len(), 2);
        assert_eq!(sc.exports, vec![("Default".to_string(), "Foo".to_string())]);
        assert!(sc.module);
        let foo = &sc.decls[2];
        let (tp, supers, body) = split_decl(&t[foo.2 as usize..foo.3 as usize]);
        assert_eq!(tp, "T");
        assert_eq!(supers, vec!["Bar<T>", "Baz"]);
        let ms = members(body);
        assert_eq!(ms.iter().map(|m| (m.name.as_str(), m.method, m.ty.as_str())).collect::<Vec<_>>(), vec![("x", false, "{ a: number }"), ("get", true, "T | undefined")]);
    }

    #[test]
    fn member_forms() {
        let ms = members("\n  readonly length: number;\n  [n: number]: T;\n  map<U>(callbackfn: (value: T, index: number) => U, thisArg?: any): U[];\n  get size(): number;\n  static of<T>(...items: T[]): T[];\n  (x: string): void;\n  new (x: string): Foo;\n  opt?: string;\n  private http: HttpClient = inject(HttpClient);\n  form = new FormGroup({ a: new FormControl('') });\n  run(a: string): void {\n    if (a) { return; }\n  }\n  @Input() label!: string;\n");
        let got: Vec<(&str, bool, &str)> = ms.iter().map(|m| (m.name.as_str(), m.method, m.ty.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("length", false, "number"),
                ("[]", false, "T"),
                ("map", true, "U[]"),
                ("size", false, "number"),
                ("of", true, "T[]"),
                ("()", true, "void"),
                ("new", true, "Foo"),
                ("opt", false, "string"),
                ("http", false, "HttpClient"),
                ("form", false, "=new FormGroup({ a: new FormControl('') })"),
                ("run", true, "void"),
                ("label", false, "string"),
            ]
        );
        assert_eq!(ms[2].type_params, "U");
        assert!(ms[4].is_static && ms[7].optional);
    }

    #[test]
    fn specs() {
        assert_eq!(split_spec("@angular/common/http"), ("@angular/common", "http"));
        assert_eq!(split_spec("rxjs"), ("rxjs", ""));
        assert_eq!(split_spec("rxjs/operators"), ("rxjs", "operators"));
    }

    /// 実際のプロジェクトの node_modules（NAGI_TS_ROOT。読むだけ）
    #[test]
    #[ignore]
    fn dts_real() {
        let Some(root) = std::env::var_os("NAGI_TS_ROOT").map(PathBuf::from) else { return };
        let t0 = std::time::Instant::now();
        let ix = DtsIndex::load(&root);
        let bytes: usize = ix.files.iter().map(|f| f.text().len()).sum();
        println!("{} ファイル {} MB・宣言 {}・{:?}", ix.files.len(), bytes >> 20, ix.decl_count(), t0.elapsed());
        if let Some((i, f)) = ix.files.iter().enumerate().find(|(_, f)| f.path.ends_with("lib.es5.d.ts")) {
            let ds: Vec<&Decl> = ix.decls.iter().filter(|d| d.file == i as u32).collect();
            println!("es5: {} 宣言、最後 {:?}", ds.len(), ds.iter().max_by_key(|d| d.start).map(|d| (&d.name, d.start, f.text().len())));
        }
        if let Ok(names) = std::env::var("NAGI_DTS_NAMES") {
            for n in names.split(',') {
                let ds = ix.named(n);
                println!("{n}: {} {:?}", ds.len(), ds.iter().map(|d| (d.kind, d.global, ix.files[d.file as usize].path.file_name().unwrap().to_string_lossy().to_string())).collect::<Vec<_>>());
            }
        }
        for n in ["String", "Array", "Console", "console", "HTMLElement", "Promise"] {
            println!("{n}: {}", ix.globals(n).len());
        }
        for spec in ["@angular/core", "@angular/common/http", "@angular/forms", "rxjs", "@angular/router"] {
            let m = ix.module(spec);
            println!("{spec}: {:?}", m.map(|f| ix.files[f as usize].path.display().to_string()));
            if let Some(f) = m {
                for n in ["HttpClient", "FormGroup", "Observable", "Router", "Component"] {
                    let d = ix.exported(f, n);
                    if !d.is_empty() {
                        println!("  {n}: {} {:?}", d.len(), d[0].kind);
                    }
                }
            }
        }
    }
}
