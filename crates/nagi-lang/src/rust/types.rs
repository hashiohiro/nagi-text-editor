//! Rust の式の型（プロジェクトの impl・構造体と、ライブラリ（生態系 Cargo の索引）の戻り値の型をたどる）。
//!
//! 型は文字のまま持つ（`Vec<String>`・`Option<&'a T>`）。戻り値の型の中の型変数は、impl の型（`Vec<T, A>`）と受け手の型引数を
//! 並べて置き換え、`Self` は受け手、`Self::Item` は受け手のトレイトの impl の関連型に。
//! トレイトのメソッドは、範囲にあるもの（prelude と `use`）だけ: 型に impl があるか、すべての型への impl があるもの

use std::collections::{HashMap, HashSet};

use nagi_workers::index::Index;

use crate::eco::cargo::{CargoIndex, LibItem, type_name};
use crate::lang::{Cx, Item};

/// std の prelude のトレイト（use しなくても使える）
const PRELUDE: &[&str] = &[
    "Clone", "Copy", "Send", "Sized", "Sync", "Unpin", "Drop", "Fn", "FnMut", "FnOnce", "ToOwned", "ToString", "Iterator", "Extend", "IntoIterator", "DoubleEndedIterator",
    "ExactSizeIterator", "Default", "Eq", "Ord", "PartialEq", "PartialOrd", "AsRef", "AsMut", "Into", "From", "TryFrom", "TryInto", "FromIterator",
];

/// 型の文字の型引数（寿命は除く。`HashMap<K, V>` → [K, V]）
pub fn type_args(t: &str) -> Vec<String> {
    let t = strip_ref(t);
    // [T]・[T; N] の要素
    if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        let e = crate::text::split_top(&crate::rust::LX, inner, b';', true).into_iter().next().unwrap_or(inner);
        return vec![e.trim().to_string()];
    }
    let Some(o) = t.find('<') else { return Vec::new() };
    let Some(c) = t.rfind('>') else { return Vec::new() };
    crate::text::split_top(&crate::rust::LX, &t[o + 1..c], b',', true).into_iter().map(|a| a.trim().to_string()).filter(|a| !a.starts_with('\'') && !a.is_empty()).collect()
}

/// `&'a mut T` → T
fn strip_ref(t: &str) -> &str {
    let mut t = t.trim();
    loop {
        if let Some(r) = t.strip_prefix('&') {
            t = r.trim_start();
            if t.starts_with('\'') {
                t = t.split_once(' ').map_or("", |x| x.1).trim_start();
            }
            t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
            continue;
        }
        if let Some(r) = t.strip_prefix("mut ") {
            t = r.trim_start();
            continue;
        }
        return t;
    }
}

/// 語ごとに置き換える（`T` → `String`。`Tx` は置き換えない）
fn replace_word(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    let w = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < s.len() {
        if s[i..].starts_with(from) && (i == 0 || !w(b[i - 1])) && !s[i + from.len()..].bytes().next().is_some_and(w) && !(i > 0 && b[i - 1] == b':' && from != "Self") {
            out.push_str(to);
            i += from.len();
        } else {
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// 戻り値の型 `ret` の、impl の型 `target` の型変数を受け手 `recv` の型引数に、`Self` を受け手にする
fn subst(ret: &str, target: &str, recv: &str) -> String {
    let params = type_args(target);
    let args = type_args(recv);
    let mut r = ret.to_string();
    for (p, a) in params.iter().zip(&args) {
        if p.chars().all(|c| c.is_alphanumeric() || c == '_') && p.chars().count() <= 2 {
            r = replace_word(&r, p, a);
        }
    }
    replace_word(&r, "Self", strip_ref(recv))
}

/// 型の文字の名前
fn name_of(t: &str) -> String {
    type_name(t)
}

/// 型を見る場所
pub struct Types<'a> {
    pub cx: &'a Cx<'a>,
    pub idx: Option<&'a Index>,
    pub lib: Option<&'a CargoIndex>,
    /// 範囲にあるトレイト
    pub scope: HashMap<String, String>,
}

impl<'a> Types<'a> {
    pub fn new(cx: &'a Cx<'a>, lib: Option<&'a CargoIndex>) -> Self {
        Types { cx, idx: cx.index, lib, scope: scope_traits(cx.src) }
    }

    /// 型の名前 `n` がプロジェクトの型か
    pub fn is_project(&self, n: &str) -> bool {
        self.idx.is_some_and(|i| i.lookup(n).iter().any(|l| l.kind.is_type()))
    }

    /// 型 `t` の名前 `m` の項目の場所（固有・Deref の先・impl したトレイト・すべての型への impl）
    pub fn member_loc(&self, t: &str, m: &str) -> Option<(&'a std::path::Path, u32, String)> {
        let lib = self.lib?;
        for (owner, _) in self.owners(t) {
            let at = lib.items_of(&owner).find(|i| &*i.name == m).map(|i| (i.file, i.line));
            if let Some((f, l)) = at {
                return Some((lib.file(f), l, owner));
            }
            for tr in self.traits_of(&owner) {
                if let Some(i) = self.trait_items(&tr).find(|i| &*i.name == m) {
                    return Some((lib.file(i.file), i.line, tr));
                }
            }
        }
        for tr in lib.blanket_traits() {
            if let Some(i) = self.trait_items(tr).find(|i| &*i.name == m) {
                return Some((lib.file(i.file), i.line, tr.to_string()));
            }
        }
        None
    }

    /// 型 `t` の、ライブラリの項目の持ち主（型と Deref の先。3 段まで）
    fn owners(&self, t: &str) -> Vec<(String, String)> {
        let mut out = vec![(name_of(t), strip_ref(t).to_string())];
        // 整数のメソッドは core のマクロから（持ち主は uint・int）
        match out[0].0.as_str() {
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => out.push(("uint".into(), out[0].1.clone())),
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => out.push(("int".into(), out[0].1.clone())),
            _ => {}
        }
        if let Some(lib) = self.lib {
            for _ in 0..3 {
                let (n, full) = out.last().cloned().unwrap();
                let Some(target) = lib.deref(&n) else { break };
                // Deref の impl の型 (Vec<T, A>) と受け手から、先の型（[T] → [String]）
                let tgt_full = lib.items_of(&n).find(|i| &*i.trait_ == "Deref").map_or(target.to_string(), |i| subst(target, &i.target, &full));
                let nn = name_of(&tgt_full);
                if out.iter().any(|x| x.0 == nn) {
                    break;
                }
                out.push((nn, tgt_full));
            }
        }
        out
    }

    /// 型 `t` に impl したトレイト（プロジェクトとライブラリ）
    fn traits_of(&self, owner: &str) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if let Some(lib) = self.lib {
            v.extend(lib.traits_of(owner).into_iter().map(String::from));
        }
        if let Some(idx) = self.idx {
            v.extend(super::impl_items(idx, self.cx, owner).into_iter().filter(|m| !m.trait_.is_empty()).map(|m| m.trait_));
        }
        v.sort();
        v.dedup();
        v
    }

    /// トレイト `tr` の定義の項目。同じ名前のトレイトが幾つもあれば、use の道のもの（prelude なら std・core・alloc のもの）
    fn trait_items(&self, tr: &str) -> impl Iterator<Item = &'a LibItem> + 'a {
        let lib = self.lib.expect("lib");
        let defs: Vec<&crate::eco::cargo::ModItem> = lib.mods.iter().filter(|m| m.kind == 'I' && &*m.name == tr).collect();
        let path = self.scope.get(tr).cloned().unwrap_or_default();
        let group = |m: &str| -> String {
            let c = m.split("::").next().unwrap_or("");
            if matches!(c, "std" | "core" | "alloc") { "std".into() } else { c.to_string() }
        };
        let want_module = path.rsplit_once("::").map(|x| x.0.to_string());
        let want_group = if path.is_empty() { "std".to_string() } else { group(&path) };
        let exact: Vec<u32> = defs.iter().filter(|m| want_module.as_deref() == Some(&*m.module)).map(|m| m.file).collect();
        let same: Vec<u32> = defs.iter().filter(|m| group(&m.module) == want_group).map(|m| m.file).collect();
        let files: Option<HashSet<u32>> = if !exact.is_empty() {
            Some(exact.into_iter().collect())
        } else if !same.is_empty() {
            Some(same.into_iter().collect())
        } else {
            None
        };
        let owner = format!("trait:{tr}");
        lib.items.iter().filter(move |i| *i.owner == *owner && files.as_ref().is_none_or(|f| f.contains(&i.file)))
    }

    /// 型 `t` のメソッド・関連関数の候補（`statics` なら `Type::` の後: self を取らないものを先に）
    pub fn members(&self, t: &str, statics: bool) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |out: &mut Vec<Item>, i: Item| {
            if seen.insert(i.name.clone()) {
                out.push(i);
            }
        };
        let Some(lib) = self.lib else { return out };
        let item = |i: &LibItem, owner: &str, rank: u8| match i.kind {
            'f' => Item::member(i.name.to_string(), true, i.params > 0, owner.to_string(), rank),
            'c' => Item::new(i.name.to_string(), 'f', owner.to_string(), rank),
            _ => Item::new(i.name.to_string(), 'c', owner.to_string(), rank),
        };
        let fits = |i: &LibItem| if statics { true } else { i.method };
        let rank = |i: &LibItem| u8::from(statics && i.method);
        for (k, (owner, _)) in self.owners(t).iter().enumerate() {
            // 固有のメソッド（Deref の先は . の後だけ）
            if k > 0 && statics {
                break;
            }
            for i in lib.items_of(owner).filter(|i| i.trait_.is_empty() && i.kind != 't' && fits(i)) {
                push(&mut out, item(i, owner, rank(i)));
            }
            // impl したトレイトのメソッド（範囲にあるトレイト）
            for tr in self.traits_of(owner).iter().filter(|tr| self.scope.contains_key(tr.as_str())) {
                for i in lib.items_of(owner).filter(|i| &*i.trait_ == tr.as_str() && i.kind == 'f' && fits(i)) {
                    push(&mut out, item(i, tr, rank(i) + 1));
                }
                for i in self.trait_items(tr).filter(|i| i.kind == 'f' && fits(i)) {
                    push(&mut out, item(i, tr, rank(i) + 1));
                }
            }
        }
        // すべての型への impl があるトレイト（ToString など）
        if !statics {
            for tr in lib.blanket_traits().into_iter().filter(|tr| self.scope.contains_key(*tr)) {
                for i in self.trait_items(tr).filter(|i| i.kind == 'f' && i.method) {
                    push(&mut out, item(i, tr, 2));
                }
            }
        }
        out
    }

    /// 型 `t` のメソッド `m` の戻り値の型（プロジェクトの impl → ライブラリ（Deref の先・トレイトも））
    fn method_ret(&self, t: &str, m: &str) -> Option<String> {
        let name = name_of(t);
        if let Some(idx) = self.idx {
            if let Some(it) = super::impl_items(idx, self.cx, &name).into_iter().find(|i| i.name == m) {
                return (!it.ret.is_empty()).then(|| replace_word(&it.ret, "Self", strip_ref(t)));
            }
        }
        let lib = self.lib?;
        for (owner, full) in self.owners(t) {
            if let Some(i) = lib.items_of(&owner).find(|i| &*i.name == m && i.kind == 'f') {
                return self.resolve_assoc(&subst(&i.ret, &i.target, &full), &owner, &full);
            }
            for tr in self.traits_of(&owner) {
                if let Some(i) = self.trait_items(&tr).find(|i| &*i.name == m) {
                    return self.resolve_assoc(&replace_word(&i.ret, "Self", &full), &owner, &full);
                }
            }
        }
        None
    }

    /// `Self::Item`・`<T as Trait>::Item` を、受け手のトレイトの impl の関連型に
    fn resolve_assoc(&self, ret: &str, owner: &str, full: &str) -> Option<String> {
        if ret.is_empty() {
            return None;
        }
        let lib = self.lib?;
        let mut r = ret.to_string();
        let prefix = format!("{full}::");
        while let Some(k) = r.find(&prefix) {
            let rest = &r[k + prefix.len()..];
            let assoc: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let ty = lib.items_of(owner).find(|i| i.kind == 't' && *i.name == assoc).map(|i| subst(&i.ret, &i.target, full)).unwrap_or_else(|| "_".into());
            r = format!("{}{ty}{}", &r[..k], &rest[assoc.len()..]);
        }
        Some(r)
    }

    /// プロジェクトの関数・メソッド `f` が受け取るクロージャ（`impl FnMut(&mut Editor)`・`F: Fn(A, B)`）の `k` 番目の引数の型
    fn project_closure_param(&self, f: &str, k: usize) -> Option<String> {
        let idx = self.idx?;
        let mut fns: Vec<_> = idx.lookup(f).into_iter().filter(|l| matches!(l.kind, nagi_syntax::defs::DefKind::Function | nagi_syntax::defs::DefKind::Method)).collect();
        fns.sort_by_key(|l| *l.path != *self.cx.rel);
        let l = fns.first()?;
        let text = std::fs::read_to_string(idx.root.join(&*l.path)).ok()?;
        let start = text.split_inclusive('\n').take(l.line as usize).map(str::len).sum::<usize>() + l.col as usize;
        let rest = text.get(start..)?;
        let sig = &rest[..rest.find(['{', ';']).unwrap_or(rest.len())];
        let at = ["FnMut(", "FnOnce(", "Fn("].iter().filter_map(|p| sig.find(p).map(|i| i + p.len())).min()?;
        let close = sig[at..].find(')').map(|c| at + c)?;
        let args = crate::text::split_top(&super::LX, &sig[at..close], b',', true);
        args.get(k).map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
    }

    /// プロジェクトの関数 `f` の戻り値の型（今のファイルのものを先に。ほかのファイルどうしで食い違えば分からない）
    fn project_fn_ret(&self, f: &str) -> Option<String> {
        let idx = self.idx?;
        let mut fns: Vec<_> = idx.lookup(f).into_iter().filter(|l| matches!(l.kind, nagi_syntax::defs::DefKind::Function)).collect();
        fns.sort_by_key(|l| *l.path != *self.cx.rel);
        let ret = |l: &nagi_workers::index::Loc| -> Option<String> {
            let text = std::fs::read_to_string(idx.root.join(&*l.path)).ok()?;
            let start = text.split_inclusive('\n').take(l.line as usize).map(str::len).sum::<usize>() + l.col as usize + f.len();
            let rest = text.get(start..)?;
            let end = rest.find(['{', ';']).unwrap_or(rest.len());
            Some(crate::eco::cargo::fn_sig(&rest[..end]).2).filter(|r| !r.is_empty())
        };
        let first = fns.first()?;
        if *first.path == *self.cx.rel || fns.len() == 1 {
            return ret(first);
        }
        let rets: Vec<Option<String>> = fns.iter().take(4).map(|l| ret(l)).collect();
        rets.iter().all(|r| *r == rets[0]).then(|| rets[0].clone()).flatten()
    }

    /// プロジェクトの構造体 `t` のフィールド `f` の型
    fn field_type(&self, t: &str, f: &str) -> Option<String> {
        let idx = self.idx?;
        let name = name_of(t);
        for l in idx.lookup(&name).iter().filter(|l| l.kind.is_type()) {
            let text = std::fs::read_to_string(idx.root.join(&*l.path)).ok()?;
            for line in text.lines().skip(l.line as usize + 1).take(200) {
                let tl = line.trim_start();
                if tl.starts_with('}') {
                    break;
                }
                let tl = tl.strip_prefix("pub").map_or(tl, |r| if r.starts_with('(') { r.split_once(')').map_or(r, |x| x.1) } else { r }).trim_start();
                if let Some(rest) = tl.strip_prefix(f).and_then(|r| r.trim_start().strip_prefix(':')) {
                    return Some(rest.trim().trim_end_matches(',').trim().to_string());
                }
            }
        }
        None
    }

    /// 変数 `name` の型（`let x: T`・`let x = 式`・引数の `x: T`・`for x in 式`・クロージャの `|x|`）。`at` より前の、近い宣言から
    fn var_type(&self, name: &str, at: usize, depth: usize) -> Option<String> {
        let src = &self.cx.src[..at.min(self.cx.src.len())];
        let (pos, d) = nearest_decl(src, name)?;
        match d {
            Decl::Typed(t) => Some(t.to_string()),
            Decl::Expr(e) => self.expr_type(e, pos, depth + 1),
            // for x in 式: 式の要素（into_iter() の要素）
            Decl::Each(e) => self.expr_type(&format!("{e}.into_iter()"), pos, depth + 1).and_then(|t| self.elem(&t)),
            // if let Some(x) = 式: 式の型の最初の型引数
            Decl::Unwrap(e) => self.expr_type(e, pos, depth + 1).and_then(|t| type_args(&t).into_iter().next()),
            Decl::Closure(recv, m, k) => {
                // プロジェクトの関数・メソッドなら、その引数の Fn(…) の型
                if let Some(t) = self.project_closure_param(m, k) {
                    return Some(t);
                }
                let t = self.expr_type(recv, pos, depth + 1)?;
                // 受け手の要素（fold は 2 番目、sort_by などは両方、ほかは 1 番目）
                let elem_at = match m {
                    "fold" | "try_fold" => k == 1,
                    "sort_by" | "sort_unstable_by" | "max_by" | "min_by" | "dedup_by" | "is_sorted_by" => true,
                    _ => k == 0,
                };
                elem_at.then(|| self.elem(&t)).flatten()
            }
        }
    }

    /// 型 `t` の要素（反復子なら next の Some の中、Option・Result・Vec などなら最初の型引数）
    fn elem(&self, t: &str) -> Option<String> {
        if let Some(n) = self.method_ret(t, "next").filter(|r| name_of(r) == "Option") {
            return type_args(&n).into_iter().next();
        }
        type_args(t).into_iter().next()
    }

    /// 式 `e`（`at` は式の位置）の型
    pub fn expr_type(&self, e: &str, at: usize, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        let e = e.trim().trim_end_matches(';').trim();
        let (head, steps) = split_chain(e)?;
        let mut t = self.head_type(&head, at, depth)?;
        for s in steps {
            t = match s {
                Step::Call(m) => self.method_ret(&t, &m)?,
                Step::Field(f) => self.field_type(&t, &f)?,
                Step::Try => type_args(&t).into_iter().next()?,
                // 範囲（s[a..b]）なら str・スライス、ほかは要素（HashMap は値）
                Step::Index(true) if matches!(name_of(&t).as_str(), "String" | "str") => "str".into(),
                Step::Index(true) => format!("[{}]", self.elem(&t)?),
                Step::Index(false) => {
                    let a = type_args(&t);
                    if name_of(&t) == "HashMap" || name_of(&t) == "BTreeMap" { a.get(1).cloned()? } else { a.into_iter().next()? }
                }
            };
        }
        Some(t)
    }

    /// 式の頭の型
    fn head_type(&self, h: &str, at: usize, depth: usize) -> Option<String> {
        let h = h.trim();
        if h.starts_with('"') {
            return Some("&str".into());
        }
        if h == "self" {
            return self.cx.inside.clone();
        }
        if let Some(m) = h.strip_suffix(')').and_then(|x| x.split_once('(')) {
            let callee = m.0.trim();
            if callee.ends_with('!') {
                return match callee.trim_end_matches('!') {
                    "vec" => Some("Vec<_>".into()),
                    "format" => Some("String".into()),
                    _ => None,
                };
            }
            // Type::f(…)・path::Type::f(…)
            if let Some((ty, f)) = callee.rsplit_once("::") {
                let ty = ty.rsplit("::").next().unwrap_or(ty);
                let ty = if ty == "Self" { self.cx.inside.clone()? } else { ty.to_string() };
                if ty.starts_with(char::is_uppercase) {
                    return self.method_ret(&ty, f).or(Some(ty));
                }
            }
            // 関数: プロジェクト（名前で索引から）→ ライブラリのモジュール（std::env::temp_dir・fs::write）
            let f = callee.rsplit("::").next().unwrap_or(callee);
            if let Some(r) = self.project_fn_ret(f) {
                return Some(r);
            }
            return callee.contains("::").then(|| self.lib?.fn_ret(callee.trim_start_matches("::")).map(String::from)).flatten();
        }
        if let Some(n) = h.strip_suffix('}').and_then(|x| x.split_once('{')).map(|x| x.0.trim()) {
            return Some(n.rsplit("::").next().unwrap_or(n).to_string());
        }
        if h.starts_with(char::is_lowercase) && h.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return self.var_type(h, at, depth);
        }
        // Type::Variant・定数
        h.rsplit_once("::").map(|x| x.0.rsplit("::").next().unwrap_or("").to_string()).filter(|t| t.starts_with(char::is_uppercase))
    }
}

/// 変数の宣言の求め方
pub(crate) enum Decl<'s> {
    /// 型を書いた宣言（let x: T・引数・|x: T|）
    Typed(&'s str),
    /// let x = 式
    Expr(&'s str),
    /// for x in 式
    Each(&'s str),
    /// if let Some(x) = 式・let Ok(x) = 式 else
    Unwrap(&'s str),
    /// クロージャの引数: 受け手の式・メソッド・何番目の引数
    Closure(&'s str, &'s str, usize),
}

/// `src`（今の位置まで）の、変数 `name` のいちばん近い宣言（範囲の外のものは除く。分からなくても、その前の隠れた宣言には戻らない）
pub(crate) fn nearest_decl<'s>(src: &'s str, name: &str) -> Option<(usize, Decl<'s>)> {
    if !src.contains(name) {
        return None;
    }
    static PATS: std::sync::OnceLock<[regex::Regex; 5]> = std::sync::OnceLock::new();
    let [decl, param, each, closure, unwrap] = PATS.get_or_init(|| {
        [
            regex::Regex::new(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::\s*([^=;]+?))?\s*=\s*([^;]+);").unwrap(),
            regex::Regex::new(r"[(,|]\s*(?:mut\s+)?(\w+)\s*:\s*([^,)=|]+)").unwrap(),
            regex::Regex::new(r"\bfor\s+(?:mut\s+)?(\w+)\s+in\s+([^{]+)\{").unwrap(),
            regex::Regex::new(r"(\.?)(\w+)\(\s*(?:[^()|;{}]*(?:\([^()|]*\))?[^()|;{}]*,\s*)?(?:move\s+)?\|([^|]*)\|").unwrap(),
            regex::Regex::new(r"\blet\s+(?:Some|Ok)\(\s*(?:ref\s+)?(?:mut\s+)?(\w+)\s*\)\s*=\s*([^{;]*?)\s*(?:\{|\belse\b)").unwrap(),
        ]
    });
    let mut all: Vec<(usize, Decl)> = Vec::new();
    let text = |c: &regex::Captures<'s>, k: usize| c.get(k).map_or("", |m| m.as_str()).trim();
    for c in decl.captures_iter(src).filter(|c| &c[1] == name) {
        let pos = c.get(0).map_or(0, |m| m.start());
        all.push((pos, if c.get(2).is_some() { Decl::Typed(text(&c, 2)) } else { Decl::Expr(text(&c, 3)) }));
    }
    for c in param.captures_iter(src).filter(|c| &c[1] == name) {
        all.push((c.get(0).map_or(0, |m| m.start()), Decl::Typed(text(&c, 2))));
    }
    for c in each.captures_iter(src).filter(|c| &c[1] == name) {
        all.push((c.get(0).map_or(0, |m| m.start()), Decl::Each(text(&c, 2))));
    }
    for c in unwrap.captures_iter(src).filter(|c| &c[1] == name) {
        all.push((c.get(0).map_or(0, |m| m.start()), Decl::Unwrap(text(&c, 2))));
    }
    for c in closure.captures_iter(src) {
        let ps: Vec<&str> = text(&c, 3).split(',').map(str::trim).collect();
        let Some(k) = ps.iter().position(|p| p.trim_start_matches(['&', ' ']).trim_start_matches("mut ").split(':').next().unwrap_or("").trim() == name) else { continue };
        let m0 = c.get(0).unwrap();
        let d = match ps[k].split_once(':') {
            Some((_, t)) => Decl::Typed(t.trim()),
            // 受け手の式（関数なら空）
            None => Decl::Closure(if text(&c, 1).is_empty() { "" } else { chain_text(&src[..m0.start()]) }, c.get(2).map_or("", |m| m.as_str()), k),
        };
        all.push((m0.start(), d));
    }
    // 範囲の外（閉じたブロック・クロージャ・前の関数の引数）は除く
    all.retain(|(pos, d)| {
        let head = &src[..*pos];
        let scope = match d {
            Decl::Expr(_) => Scope::Block,
            Decl::Typed(_) if src[*pos..].starts_with("let") => Scope::Block,
            Decl::Typed(_) if src[*pos..].starts_with('|') => Scope::Paren,
            Decl::Unwrap(_) if head.trim_end().ends_with("if") || head.trim_end().ends_with("while") => Scope::Body,
            Decl::Unwrap(_) => Scope::Block,
            Decl::Typed(_) | Decl::Each(_) => Scope::Body,
            Decl::Closure(..) => Scope::Paren,
        };
        in_scope(src, *pos, scope)
    });
    all.into_iter().max_by_key(|x| x.0)
}

/// 宣言の効く範囲
#[derive(Clone, Copy)]
enum Scope {
    /// let: 囲むブロックの終わりまで
    Block,
    /// fn の引数・for: 次の { … } の中
    Body,
    /// クロージャの引数: 囲む括弧（とブロック）の終わりまで
    Paren,
}

/// `pos` の宣言が `src` の終わり（今の位置）まで効くか（文字列の中の括弧は数えない）
fn in_scope(src: &str, pos: usize, scope: Scope) -> bool {
    let b = src.as_bytes();
    let (mut brace, mut paren) = (0i32, 0i32);
    let mut opened = false;
    let mut i = pos;
    // クロージャは `.m(` の ( の中から
    if let Scope::Paren = scope {
        if let Some(k) = src[pos..].find('(').filter(|k| !src[pos..pos + k].contains('|')) {
            i = pos + k + 1;
        }
    }
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1 + usize::from(b[i] == b'\\');
                }
            }
            b'{' => {
                brace += 1;
                opened = true;
            }
            b'}' => {
                brace -= 1;
                if brace < 0 || (matches!(scope, Scope::Body) && opened && brace == 0) {
                    return false;
                }
            }
            b'(' => paren += 1,
            b')' => {
                paren -= 1;
                if paren < 0 && matches!(scope, Scope::Paren) {
                    return false;
                }
            }
            b';' if matches!(scope, Scope::Paren) && brace == 0 && paren <= 0 => return false,
            _ => {}
        }
        i += 1;
    }
    true
}

/// 連なりの段
#[derive(Debug, PartialEq)]
enum Step {
    Call(String),
    Field(String),
    Try,
    /// 添字（範囲か）
    Index(bool),
}

/// 式を (頭, 段) に（`a.b(x)?.c[0]` → (a, [Call b, Try, Field c, Index])）。頭は `f(…)`・`T::f(…)`・`T { … }`・`m!(…)` も
fn split_chain(e: &str) -> Option<(String, Vec<Step>)> {
    let b = e.as_bytes();
    let mut i = 0;
    // 頭: 名前・道（::）・文字列
    let skip_group = |i: usize, open: u8, close: u8| -> Option<usize> {
        let mut depth = 0;
        let mut k = i;
        while k < b.len() {
            if b[k] == b'"' {
                k += 1;
                while k < b.len() && b[k] != b'"' {
                    k += 1 + usize::from(b[k] == b'\\');
                }
            } else if b[k] == open {
                depth += 1;
            } else if b[k] == close {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            k += 1;
        }
        None
    };
    if b.first() == Some(&b'"') {
        i = skip_group(0, b'"', b'"').or_else(|| e[1..].find('"').map(|x| x + 2))?;
    } else {
        loop {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || (b[i] == b':' && b.get(i + 1) == Some(&b':')) || (b[i] == b':' && i > 0 && b[i - 1] == b':')) {
                i += 1;
            }
            // 型引数 ::<…>・<…>（その後に ::f と続くことも）
            if b.get(i) != Some(&b'<') {
                break;
            }
            i = skip_group(i, b'<', b'>')?;
            if !e[i..].starts_with("::") {
                break;
            }
        }
        if b.get(i) == Some(&b'!') {
            i += 1;
        }
        match b.get(i) {
            Some(b'(') => i = skip_group(i, b'(', b')')?,
            Some(b'[') if e[..i].ends_with('!') => i = skip_group(i, b'[', b']')?,
            Some(b'{') if e[..i].starts_with(char::is_uppercase) || e[..i].contains("::") => i = skip_group(i, b'{', b'}')?,
            _ => {}
        }
    }
    let head = e[..i].to_string();
    if head.is_empty() {
        return None;
    }
    let mut steps = Vec::new();
    while i < b.len() {
        match b[i] {
            b'?' => {
                steps.push(Step::Try);
                i += 1;
            }
            b'[' => {
                let s = i;
                i = skip_group(i, b'[', b']')?;
                steps.push(Step::Index(e[s..i].contains("..")));
            }
            b'.' => {
                i += 1;
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let name = e[s..i].to_string();
                if name == "await" {
                    continue;
                }
                // ::<…>
                if e[i..].starts_with("::<") {
                    i = skip_group(i + 2, b'<', b'>')?;
                }
                if b.get(i) == Some(&b'(') {
                    i = skip_group(i, b'(', b')')?;
                    steps.push(Step::Call(name));
                } else {
                    steps.push(Step::Field(name));
                }
            }
            b' ' | b'\n' | b'\t' | b'\r' => i += 1,
            _ => return None,
        }
    }
    Some((head, steps))
}

/// 行の中の `.` の前 `before` の、式の始まり（括弧・`::`・`?`・`.` をたどって、それ以外の文字の後）
pub fn chain_text(before: &str) -> &str {
    let b = before.as_bytes();
    let mut i = b.len();
    let mut depth = 0i32;
    while i > 0 {
        let c = b[i - 1];
        match c {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' if depth > 0 => depth -= 1,
            b'(' | b'[' | b'{' => break,
            b'"' if depth == 0 => {
                // 文字列: 前の " まで
                let k = before[..i - 1].rfind('"').unwrap_or(0);
                i = k + 1;
            }
            _ if depth > 0 => {}
            c if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'?' | b'!' | b'<' | b'>' | b'&') => {
                // 比べる < > はここで切る（型引数の < > は ::< か名前の直後）
                if matches!(c, b'<' | b'>') && !(before[..i - 1].ends_with("::") || before[..i - 1].ends_with(|x: char| x.is_alphanumeric())) && c == b'<' {
                    break;
                }
                if c == b'&' {
                    break;
                }
            }
            _ => break,
        }
        i -= 1;
    }
    before[i..].trim_start_matches(['.', '?']).trim()
}

/// 範囲にあるトレイト（prelude と、`use a::B;`・`use a::{B, C as D};` の名前）→ use の道（prelude は空）
pub fn scope_traits(src: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = PRELUDE.iter().map(|s| (s.to_string(), String::new())).collect();
    for l in src.lines() {
        let t = l.trim_start();
        let Some(u) = t.strip_prefix("use ").or_else(|| t.strip_prefix("pub use ")) else { continue };
        let u = u.split(';').next().unwrap_or("");
        // (前に付く道, 名前の並び)
        let (prefix, names): (&str, Vec<&str>) = match (u.find('{'), u.rfind('}')) {
            (Some(o), Some(c)) if o < c => (u[..o].trim().trim_end_matches("::"), u[o + 1..c].split(',').collect()),
            _ => ("", vec![u]),
        };
        for n in names {
            // `X as _` は名前を付けずに範囲に入れる
            let full = n.trim().split(" as ").next().unwrap_or("").trim();
            let name = full.rsplit("::").next().unwrap_or("").trim();
            if name.starts_with(char::is_uppercase) {
                let path = if prefix.is_empty() { full.to_string() } else { format!("{prefix}::{full}") };
                out.insert(name.to_string(), path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_and_types() {
        assert_eq!(split_chain("v.iter().map(f)?.len"), Some(("v".into(), vec![Step::Call("iter".into()), Step::Call("map".into()), Step::Try, Step::Field("len".into())])));
        assert_eq!(split_chain("String::from(\"a.b\").trim()"), Some(("String::from(\"a.b\")".into(), vec![Step::Call("trim".into())])));
        assert_eq!(split_chain("xs[0].name"), Some(("xs".into(), vec![Step::Index(false), Step::Field("name".into())])));
        assert_eq!(split_chain("Vec::<u8>::new().len()").map(|x| x.1.len()), Some(1));
        assert_eq!(chain_text("    let n = v.iter().filter(|x| x > 1)"), "v.iter().filter(|x| x > 1)");
        assert_eq!(chain_text("    if a == b && self.items"), "self.items");
        assert_eq!(chain_text("foo(bar.baz"), "bar.baz");
        assert_eq!(type_args("HashMap<String, Vec<u8>>"), vec!["String", "Vec<u8>"]);
        assert_eq!(type_args("Iter<'a, T>"), vec!["T"]);
        assert_eq!(subst("Option<&T>", "Vec<T, A>", "Vec<String>"), "Option<&String>");
        assert_eq!(subst("Map<Self, F>", "Self", "Iter<'_, u8>"), "Map<Iter<'_, u8>, F>");
        assert!(scope_traits("use std::io::{Read, Write as _};\nuse std::fmt::Display;").get("Write").map(String::as_str) == Some("std::io::Write"));
    }
}
