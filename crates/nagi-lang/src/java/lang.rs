//! Java の差し込み口

use crate::project::Ranker;

use super::types::{JType, LibTypes};
use crate::eco;
use crate::lang::{Cx, Head, Import, Item, Language, LibMember, ScopeNames, Target, mark_of};
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
        let known = |pkg: &str| cx.index.is_some_and(|i| super::rules::is_project_package(i, pkg)) || jars.as_ref().is_some_and(|j| j.is_package(pkg));
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

    /// 依存の jar・JDK の型・メンバーの定義（ソースか、無ければ class ファイルから作った骨組みを書いて開く）
    fn library_definition(&self, cx: &Cx, name: &str) -> Option<Target> {
        let jars = eco::jars(cx.libs)?;
        let (owner, member) = library_target(cx, name)?;
        let dir = cx.out_dir.map(std::path::Path::to_path_buf).or_else(super::source::sources_dir)?;
        let o = super::source::open(&jars, &owner, member.as_deref(), &dir)?;
        let fqn = owner.replace(['/', '$'], ".");
        let note = if o.source { format!("ライブラリ: {fqn}（ソース。読むだけ）") } else { format!("ライブラリ: {fqn}（ソースが無いので class ファイルの宣言から作った骨組み）") };
        Some(Target { path: o.path, line: o.line, col: o.col, note: Some(note) })
    }

    fn library_site(&self, cx: &Cx, name: &str) -> bool {
        library_target(cx, name).is_some()
    }

    fn ecosystem(&self) -> Option<&'static str> {
        Some(eco::JVM)
    }

    fn overloads(&self) -> bool {
        true
    }

    fn implicit_members(&self, lines: &[String], line: usize, ty: &str, defs: &[nagi_workers::index::Entry]) -> Vec<(String, usize)> {
        super::rules::implicit_members(lines, line, ty, defs)
    }

    fn accessor_field(&self, name: &str) -> Option<String> {
        super::rules::accessor_field(name)
    }

    fn builtin_supers(&self, t: &str) -> Option<&'static [&'static str]> {
        super::rules::jdk_supers(t)
    }

    fn builtin_known(&self, t: &str) -> bool {
        super::rules::jdk_known(t)
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
    let mut proj: Vec<(String, &str)> = cx.index.map(|i| i.files().filter(|f| f.ends_with(".java")).filter_map(|f| Some((super::types::source_package(f)?, f))).collect()).unwrap_or_default();
    // ファイルの表は HashMap なので、パスの順に（候補の順を毎回同じに）
    proj.sort_by(|a, b| a.1.cmp(b.1));
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

/// `cx.start` の名前 `name` のライブラリの (型の内部名, メンバー)
pub fn library_target(cx: &Cx, name: &str) -> Option<(String, Option<String>)> {
    let jars = eco::jars(cx.libs)?;
    let (src, line, ws, word_at) = (cx.src, cx.line, cx.ws, cx.start);
    let ls = word_at - ws;
    let idx = cx.index;
    let mut ranker = Ranker::new(idx);
    let inside = cx.inside.as_deref();
    if ws > 0 && line[..ws].ends_with('.') {
        let known = |pkg: &str| idx.is_some_and(|i| super::rules::is_project_package(i, pkg)) || jars.is_package(pkg);
        match super::types::receiver_text(src, &line[..ws - 1], ls, word_at, &known) {
            // パッケージの中のクラス
            Err(pkg) => Some(format!("{pkg}/{name}")).filter(|c| jars.class(c).is_some()).map(|c| (c, None)),
            Ok((expanded, fqn)) => {
                let (lib, t) = super::types::lib_receiver(&jars, &mut ranker, src, &cx.rel, word_at, inside, &expanded, fqn.as_ref());
                match t {
                    Some(t @ JType::Class(..)) if !matches!(&t, JType::Class(n, _) if n.starts_with('?')) => {
                        let JType::Class(n, _) = &t else { unreachable!() };
                        let nested = format!("{n}${name}");
                        if jars.class(&nested).is_some() {
                            Some((nested, None))
                        } else {
                            lib.declaring(&t, name).map(|c| (c, Some(name.to_string())))
                        }
                    }
                    // プロジェクトの型のライブラリの親のメンバー
                    Some(JType::Class(n, _)) => {
                        let simple = super::types::project_name(&n).map_or("", |x| x.0).to_string();
                        lib_super_member(&jars, &mut ranker, &simple, name)
                    }
                    // this.・super. は今の型のライブラリの親
                    None if crate::text::chain_start(super::types::LX, &expanded).is_some_and(|i| matches!(&expanded[i..], "this" | "super")) => inside.and_then(|t| lib_super_member(&jars, &mut ranker, t, name)),
                    _ => None,
                }
            }
        }
    } else {
        let lib = LibTypes::new(&jars, src);
        if name.starts_with(char::is_uppercase) && let Some(c) = lib.resolve(name) {
            Some((c, None))
        } else if let Some(c) = lib.static_owner(name) {
            Some((c, Some(name.to_string())))
        } else {
            // 今の型のライブラリの親のメソッド・フィールド
            inside.and_then(|t| lib_super_member(&jars, &mut ranker, t, name))
        }
    }
}

/// プロジェクトの型 `ty` のライブラリの親（型引数つき）で、メンバー `name` を宣言している型
fn lib_super_member(jars: &crate::eco::jvm::JarIndex, ranker: &mut Ranker, ty: &str, name: &str) -> Option<(String, Option<String>)> {
    ranker.outside_supers(ty).into_iter().find_map(|(sup, lines)| {
        let l = LibTypes::from_lines(jars, lines.iter().map(String::as_str));
        let t = l.parse(&sup).filter(|x| matches!(x, JType::Class(n, _) if !n.starts_with('?')))?;
        l.declaring(&t, name).map(|c| (c, Some(name.to_string())))
    })
}
