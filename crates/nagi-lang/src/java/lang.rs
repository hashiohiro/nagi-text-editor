//! Java の差し込み口

use nagi_workers::index::Ranker;

use super::types::{JType, LibTypes};
use crate::eco;
use crate::lang::{Cx, Head, Import, Item, Language, LibMember, ScopeNames, mark_of};
use crate::text::Lexis;

pub struct Java;

pub static JAVA: Java = Java;

const KEYWORDS: &[&str] = &[
    "abstract", "boolean", "break", "case", "catch", "char", "class", "continue", "default", "double", "else", "enum", "extends", "final", "finally", "float", "for", "if",
    "implements", "import", "instanceof", "int", "interface", "long", "new", "null", "package", "private", "protected", "public", "return", "static", "super", "switch",
    "synchronized", "this", "throw", "throws", "try", "var", "void", "while", "true", "false", "record", "yield",
];

impl Language for Java {
    fn name(&self) -> &'static str {
        "java"
    }

    fn lexis(&self) -> &'static Lexis {
        super::types::LX
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    /// 前の行からの連なりと型を書かない変数を広げ、頭のパッケージの名前を外す。パッケージだけなら下のパッケージとクラス
    fn head(&self, cx: &Cx, before: &str) -> Head {
        let jars = eco::jars(cx.libs);
        let known = |pkg: &str| cx.index.is_some_and(|i| i.is_java_package(pkg)) || jars.as_ref().is_some_and(|j| j.is_package(pkg));
        match super::types::receiver_text(cx.src, before, cx.start - cx.ws, cx.start, &known) {
            Ok((e, f)) => Head::Expr(e, f),
            Err(pkg) => Head::Items(package_items(cx, &pkg)),
        }
    }

    fn decl_text(&self, src: &str, name: &str, at: usize) -> Option<String> {
        super::types::var_decl_text(src, name, at)
    }

    /// プロジェクトの型の、ライブラリの親（extends ControllerBase<..> など）のメンバー（protected も）
    fn inherited(&self, cx: &Cx, ranker: &mut Ranker, ty: &str, rank: u8) -> Vec<Item> {
        let Some(jars) = eco::jars(cx.libs) else { return Vec::new() };
        let mut out = Vec::new();
        for (sup, lines) in ranker.outside_supers(ty) {
            let lib = LibTypes::new(&jars, &lines.join("\n"));
            let Some(t @ JType::Class(..)) = lib.parse(&sup) else { continue };
            for m in lib.members(&t, false, true) {
                out.push(Item::member(m.name, m.method, m.params, m.owner, rank));
            }
        }
        out
    }

    /// ライブラリの型（jar・JDK の索引から、型引数もたどって）。プロジェクトの索引で見つかれば出さない
    fn library_members(&self, cx: &Cx, ranker: &mut Ranker, expanded: &str, fqn: Option<&(String, String)>, statics: bool, found: bool) -> Vec<LibMember> {
        let Some(jars) = eco::jars(cx.libs).filter(|_| !found) else { return Vec::new() };
        let (lib, t) = super::types::lib_receiver(&jars, ranker, cx.src, &cx.rel, cx.start, cx.inside.as_deref(), expanded, fqn);
        let mut out = Vec::new();
        match t {
            Some(JType::Class(name, _)) if name.starts_with('?') => {
                out.push(LibMember::Project(super::types::project_name(&name).map_or("", |x| x.0).to_string()));
            }
            Some(t) => {
                for m in lib.members(&t, statics, false) {
                    out.push(LibMember::Item(Item::member(m.name, m.method, m.params, m.owner, 0)));
                }
                // クラスの名前の後は入れ子のクラスも
                if let (true, JType::Class(n, _)) = (statics, &t) {
                    for c in jars.nested(n) {
                        out.push(LibMember::Item(Item::new(c.rsplit('$').next().unwrap_or(c), 'c', "", 0)));
                    }
                }
            }
            None => {}
        }
        out
    }

    fn class_keywords(&self) -> &'static [&'static str] {
        &["class"]
    }

    /// 依存の jar・JDK の型（打った語で始まるもの。決めたら import を足す）
    fn scope_names(&self, cx: &Cx) -> ScopeNames {
        let Some(jars) = eco::jars(cx.libs) else { return ScopeNames::default() };
        let prefix = cx.prefix;
        let imported = |fqn: &str| cx.src.contains(&format!("import {fqn};"));
        let mut up = prefix.to_string();
        if let Some(f) = up.get(0..1) {
            up.replace_range(0..1, &f.to_ascii_uppercase());
        }
        let mut names = jars.by_prefix(prefix, 200);
        if up != prefix {
            names.extend(jars.by_prefix(&up, 200));
        }
        let mut last = Vec::new();
        for internal in names {
            let fqn = super::import::java_name(internal);
            let Some((pkg, simple)) = fqn.rsplit_once('.') else { continue };
            let rank = if pkg == "java.lang" || imported(&fqn) { 2 } else { 3 };
            last.push(Item { name: simple.to_string(), mark: 'c', note: pkg.to_string(), call: None, rank, insert: None, import: Some(fqn.clone()) });
        }
        ScopeNames { first: Vec::new(), last }
    }

    fn import_plan(&self, src: &str, _name: &str, key: &str) -> Import {
        match super::import::plan(src, key) {
            super::import::Plan::None => Import::None,
            super::import::Plan::Insert(at, line) => Import::Insert(at, line),
            super::import::Plan::Conflict => Import::Qualified(key.to_string()),
        }
    }
}

/// パッケージ `pkg`（内部名 `java/util`）の中のクラスと、下のパッケージ
fn package_items(cx: &Cx, pkg: &str) -> Vec<Item> {
    let jars = eco::jars(cx.libs);
    let dotted = pkg.replace('/', ".");
    let under = format!("{pkg}/");
    let mut out = Vec::new();
    let proj: Vec<(String, &str)> = cx.index.map(|i| i.files().filter(|f| f.ends_with(".java")).filter_map(|f| Some((super::types::source_package(f)?, f))).collect()).unwrap_or_default();
    for (p, f) in &proj {
        if p == pkg {
            let name = f.rsplit('/').next().unwrap_or(f).trim_end_matches(".java");
            let mark = cx.index.and_then(|i| i.file_type_kind(f)).map_or('c', mark_of);
            out.push(Item::new(name, mark, dotted.clone(), 0));
        }
    }
    if let Some(j) = &jars {
        for c in j.classes_in(pkg) {
            out.push(Item::new(c.rsplit('/').next().unwrap_or(c), 'c', dotted.clone(), 0));
        }
    }
    let mut subs: std::collections::BTreeSet<&str> = proj.iter().filter_map(|(p, _)| p.strip_prefix(&under)?.split('/').next()).collect();
    if let Some(j) = &jars {
        subs.extend(j.subpackages(pkg));
    }
    for s in subs {
        out.push(Item::new(s, 'p', "", 0));
    }
    out
}
