//! Kotlin の差し込み口。ライブラリは JVM（Java と同じ jar・JDK の索引）。
//!
//! 型の求め方は Java のもの（java::types の LibTypes・lib_receiver_in）を借りる。Kotlin の書き方は先に Java の形に読み替える:
//! `?.`・`!!` を外し、コンストラクタの呼び出し `ArrayList<Int>()` は `new ArrayList<Int>()` に、`List`・`String`・`Any` などは
//! JVM の型（`java.util.List`・`java.lang.String`・`java.lang.Object`）に。変数の型は `val x: T`・`x: T`（引数）・`val x = T()` から

use crate::project::Ranker;

use crate::eco;
use crate::java::types::{JType, LibTypes};
use crate::lang::{Cx, Head, Import, Item, Language, LibMember, ScopeNames, Target};
use crate::project::{ImportStyle, VarStyles};
use crate::text::{self, C_LIKE, Lexis};

/// Kotlin の字句（`new` が無い）
pub const LX: Lexis = Lexis { new_keyword: None, ..C_LIKE };

pub struct Kotlin;

pub static KOTLIN: Kotlin = Kotlin;

const KEYWORDS: &[&str] = &[
        "as", "break", "class", "companion", "const", "continue", "data", "do", "else", "enum", "false", "for", "fun", "if", "import", "in", "interface", "is", "lateinit",
        "null", "object", "open", "override", "package", "private", "protected", "public", "return", "sealed", "super", "this", "throw", "true", "try", "val", "var", "when",
        "while",
    ];

/// Kotlin の型の名前 → JVM の型（import が無くても使える）
const MAPPED: &[(&str, &str)] = &[
    ("Any", "java.lang.Object"),
    ("String", "java.lang.String"),
    ("CharSequence", "java.lang.CharSequence"),
    ("Number", "java.lang.Number"),
    ("Comparable", "java.lang.Comparable"),
    ("Throwable", "java.lang.Throwable"),
    ("Exception", "java.lang.Exception"),
    ("RuntimeException", "java.lang.RuntimeException"),
    ("Iterable", "java.lang.Iterable"),
    ("Collection", "java.util.Collection"),
    ("MutableCollection", "java.util.Collection"),
    ("List", "java.util.List"),
    ("MutableList", "java.util.List"),
    ("ArrayList", "java.util.ArrayList"),
    ("Set", "java.util.Set"),
    ("MutableSet", "java.util.Set"),
    ("HashSet", "java.util.HashSet"),
    ("LinkedHashSet", "java.util.LinkedHashSet"),
    ("Map", "java.util.Map"),
    ("MutableMap", "java.util.Map"),
    ("HashMap", "java.util.HashMap"),
    ("LinkedHashMap", "java.util.LinkedHashMap"),
    ("Iterator", "java.util.Iterator"),
];

/// ファイル `src` の型の名前の表（import・package と、Kotlin の型の名前・`import a.B as C` の別名）
pub fn lib_types<'a>(jars: &'a eco::jvm::JarIndex, src: &str) -> LibTypes<'a> {
    let mut lib = LibTypes::new(jars, src);
    let imported = |simple: &str| src.lines().any(|l| l.trim_start().starts_with("import ") && (l.trim_end().ends_with(&format!(".{simple}")) || l.trim_end().ends_with(&format!(" as {simple}"))));
    for (k, j) in MAPPED {
        if !imported(k) {
            lib.alias(k, j);
        }
    }
    for l in src.lines() {
        if let Some((full, alias)) = l.trim().strip_prefix("import ").and_then(|i| i.split_once(" as ")) {
            lib.alias(alias.trim(), full.trim());
        }
    }
    lib
}

/// 式を Java の形に（`?.`・`!!` を外し、コンストラクタの呼び出しに `new` を付ける）
pub fn java_form(e: &str) -> String {
    let e = e.replace("?.", ".").replace("!!", "");
    let mut out = String::with_capacity(e.len() + 8);
    let b = e.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let word_before = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.');
        if c.is_ascii_uppercase() && !word_before {
            // 名前 → 型引数 → ( ならコンストラクタの呼び出し
            let mut k = i;
            while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                k += 1;
            }
            let mut m = k;
            if b.get(m) == Some(&b'<') {
                let mut depth = 0;
                while m < b.len() {
                    match b[m] {
                        b'<' => depth += 1,
                        b'>' => {
                            depth -= 1;
                            if depth == 0 {
                                m += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    m += 1;
                }
            }
            if b.get(m) == Some(&b'(') && !e[..i].trim_end().ends_with("new") {
                out.push_str("new ");
            }
        }
        out.push(c as char);
        i += 1;
    }
    // ASCII 以外の文字（文字列の中など）はそのまま
    if e.is_ascii() { out } else { e.to_string() }
}

/// 変数・引数・プロパティ `name` の宣言の型の文字（型引数つき。`?` は外す。`at` に近い宣言から）
pub fn decl_text(src: &str, name: &str, at: usize) -> Option<String> {
    static PATS: std::sync::OnceLock<[regex::Regex; 3]> = std::sync::OnceLock::new();
    let [typed, ctor, string] = PATS.get_or_init(|| {
        [
            // val x: T・var x: T・(x: T・, x: T
            regex::Regex::new(r"(?:\b(?:val|var)\s+|[(,]\s*(?:(?:private|protected|public|internal|override)\s+)*(?:val\s+|var\s+)?)(\w+)\s*:\s*([A-Z][\w.]*)").unwrap(),
            // val x = T<..>(
            regex::Regex::new(r"\b(?:val|var)\s+(\w+)\s*=\s*([A-Z]\w*(?:\.[A-Z]\w*)*)").unwrap(),
            // val x = "…"（文字列）
            regex::Regex::new(r#"\b(?:val|var)\s+(\w+)\s*=\s*""#).unwrap(),
        ]
    });
    if !src.contains(name) {
        return None;
    }
    let mut all: Vec<(usize, String)> = Vec::new();
    for (re, need_call) in [(typed, false), (ctor, true)] {
        for c in re.captures_iter(src).filter(|c| &c[1] == name) {
            let m = c.get(2).unwrap();
            let rest = &src[m.end()..];
            let targs = if rest.starts_with('<') { rest.find('>').map_or("", |k| &rest[..=balanced(rest).unwrap_or(k)]) } else { "" };
            if need_call && !rest[targs.len()..].starts_with('(') {
                continue;
            }
            all.push((c.get(0).map_or(0, |x| x.start()), format!("{}{targs}", m.as_str())));
        }
    }
    for c in string.captures_iter(src).filter(|c| &c[1] == name) {
        all.push((c.get(0).map_or(0, |x| x.start()), "String".to_string()));
    }
    crate::java::types::pick(all, at)
}

/// `<…>` の終わり（`s` は `<` で始まる）
fn balanced(s: &str) -> Option<usize> {
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

/// `.` の前の式の頭の変数が型を書かずに宣言されていれば（`val x = 式`・`for (x in 式)`）、頭を式に置き換える（5 回まで）
pub fn expand(src: &str, before: &str, at: usize) -> String {
    let mut cur = java_form(before);
    for _ in 0..5 {
        let Some(start) = text::chain_start(&C_LIKE, &cur) else { break };
        let head: String = cur[start..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let after = cur[start + head.len()..].to_string();
        if head.is_empty() || head == "new" || after.starts_with('(') || head.starts_with(char::is_uppercase) || decl_text(src, &head, at).is_some() {
            break;
        }
        let upto = &src[..at.min(src.len())];
        let init = regex::Regex::new(&format!(r"\b(?:val|var)\s+{}\s*=\s*([^\n;]+)", regex::escape(&head))).ok().and_then(|re| re.captures_iter(upto).last().map(|c| c[1].trim().to_string()));
        let each = regex::Regex::new(&format!(r"\bfor\s*\(\s*{}\s+in\s+([^)\n]+)\)", regex::escape(&head))).ok().and_then(|re| re.captures_iter(upto).last().map(|c| format!("{}.iterator().next()", c[1].trim())));
        let Some(expr) = init.or(each) else { break };
        cur = format!("{}{}{after}", &cur[..start], java_form(&expr));
    }
    cur
}

impl Language for Kotlin {
    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn lexis(&self) -> &'static Lexis {
        &LX
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    fn ecosystem(&self) -> Option<&'static str> {
        Some(eco::JVM)
    }

    fn overloads(&self) -> bool {
        true
    }

    fn supertypes(&self, src: &str, of: Option<&str>) -> Vec<String> {
        kotlin_supertypes(src, of)
    }

    fn supertype_texts(&self, src: &str, of: &str) -> Vec<String> {
        kotlin_supertype_texts(src, of)
    }

    /// `name: Type` と `val name = Type(`
    fn var_styles(&self) -> VarStyles {
        VarStyles { type_first: false, colon: true, init: true }
    }

    fn import_style(&self) -> ImportStyle {
        ImportStyle::Dotted
    }

    fn head(&self, cx: &Cx, before: &str) -> Head {
        // `x?.`・`x!!.` の ? と !! は外す
        let b = text::with_previous_lines(&LX, cx.src, cx.start - cx.ws, before.trim_end_matches(['?', '!']));
        Head::Expr(expand(cx.src, &b, cx.start), None)
    }

    fn decl_text(&self, src: &str, name: &str, at: usize) -> Option<String> {
        decl_text(src, name, at)
    }

    /// プロジェクトの型の、ライブラリの親のメンバー（protected も）
    fn inherited(&self, cx: &Cx, ranker: &mut Ranker, ty: &str, rank: u8) -> Vec<Item> {
        let Some(jars) = eco::jars(cx.libs) else { return Vec::new() };
        let mut out = Vec::new();
        for (sup, lines) in ranker.outside_supers(ty) {
            let lib = lib_types(&jars, &lines.join("\n"));
            let Some(t @ JType::Class(..)) = lib.parse(&sup) else { continue };
            for m in lib.members(&t, false, true) {
                out.push(Item::member(m.name, m.method, m.params, m.owner, rank));
            }
        }
        out
    }

    /// ライブラリの型のメンバー（Java の求め方で。プロジェクトの索引で見つかれば出さない）
    fn library_members(&self, cx: &Cx, ranker: &mut Ranker, expanded: &str, _fqn: Option<&(String, String)>, statics: bool, found: bool) -> Vec<LibMember> {
        let Some(jars) = eco::jars(cx.libs).filter(|_| !found) else { return Vec::new() };
        let (lib, t) = receiver(&jars, ranker, cx, expanded);
        let mut out = Vec::new();
        match t {
            Some(JType::Class(name, _)) if name.starts_with('?') => {
                out.push(LibMember::Project(crate::java::types::project_name(&name).map_or("", |x| x.0).to_string()));
            }
            Some(t) => {
                for m in lib.members(&t, statics, false) {
                    out.push(LibMember::Item(Item::member(m.name, m.method, m.params, m.owner, 0)));
                }
            }
            None => {}
        }
        out
    }

    /// 依存の jar・JDK の型（打った語で始まるもの。決めたら import を足す）
    fn names_from_tree(&self) -> bool {
        true
    }

    fn scope_names(&self, cx: &Cx) -> ScopeNames {
        crate::java::lang::JAVA.scope_names(cx)
    }

    fn import_plan(&self, src: &str, _name: &str, key: &str) -> Import {
        import_plan(src, key)
    }

    fn library_definition(&self, cx: &Cx, name: &str) -> Option<Target> {
        let jars = eco::jars(cx.libs)?;
        let (owner, member) = library_target(cx, name)?;
        let dir = cx.out_dir.map(std::path::Path::to_path_buf).or_else(crate::java::source::sources_dir)?;
        let o = crate::java::source::open(&jars, &owner, member.as_deref(), &dir)?;
        let fqn = owner.replace(['/', '$'], ".");
        let note = if o.source { format!("ライブラリ: {fqn}（ソース。読むだけ）") } else { format!("ライブラリ: {fqn}（ソースが無いので class ファイルの宣言から作った骨組み）") };
        Some(Target { path: o.path, line: o.line, col: o.col, note: Some(note) })
    }

    fn library_site(&self, cx: &Cx, name: &str) -> bool {
        library_target(cx, name).is_some()
    }
}

/// 受け手の式 `expanded`（Java の形に読み替え済み）のライブラリの型
fn receiver<'a>(jars: &'a eco::jvm::JarIndex, ranker: &mut Ranker, cx: &Cx, expanded: &str) -> (LibTypes<'a>, Option<JType>) {
    let lib = lib_types(jars, cx.src);
    let (src, at) = (cx.src, cx.start);
    crate::java::types::lib_receiver_in(lib, ranker, src, &cx.rel, at, cx.inside.as_deref(), expanded, &|n| decl_text(src, n, at), &|a| expand(src, a, at))
}

/// `cx.start` の名前 `name` のライブラリの (型の内部名, メンバー)
pub fn library_target(cx: &Cx, name: &str) -> Option<(String, Option<String>)> {
    let jars = eco::jars(cx.libs)?;
    let mut ranker = Ranker::new(cx.index);
    if cx.after_dot() {
        let before = text::with_previous_lines(&LX, cx.src, cx.start - cx.ws, cx.line[..cx.ws - 1].trim_end_matches(['?', '!']));
        let expanded = expand(cx.src, &before, cx.start);
        let (lib, t) = receiver(&jars, &mut ranker, cx, &expanded);
        match t {
            Some(t @ JType::Class(..)) if !matches!(&t, JType::Class(n, _) if n.starts_with('?')) => {
                let JType::Class(n, _) = &t else { unreachable!() };
                let nested = format!("{n}${name}");
                if jars.class(&nested).is_some() { Some((nested, None)) } else { lib.declaring(&t, name).map(|c| (c, Some(name.to_string()))) }
            }
            Some(JType::Class(n, _)) => {
                let simple = crate::java::types::project_name(&n).map_or("", |x| x.0).to_string();
                crate::java::lang::lib_super_member(&jars, &mut ranker, &simple, name)
            }
            _ => None,
        }
    } else {
        let lib = lib_types(&jars, cx.src);
        if name.starts_with(char::is_uppercase) && let Some(c) = lib.resolve(name) {
            Some((c, None))
        } else {
            cx.inside.as_deref().and_then(|t| crate::java::lang::lib_super_member(&jars, &mut ranker, t, name))
        }
    }
}

/// `src` で `fqn` を使うための import の足し方（`;` なし。package の後か import の並びの後）
pub fn import_plan(src: &str, fqn: &str) -> Import {
    let Some((pkg, simple)) = fqn.rsplit_once('.') else { return Import::None };
    let mut last_import_end = None;
    let mut package_end = None;
    let mut pos = 0;
    for l in src.split_inclusive('\n') {
        let t = l.trim();
        if let Some(i) = t.strip_prefix("import ") {
            let i = i.trim_end_matches(';').trim();
            if i == fqn || i == format!("{pkg}.*") {
                return Import::None;
            }
            if i.rsplit('.').next() == Some(simple) || i.ends_with(&format!(" as {simple}")) {
                return Import::Qualified(fqn.to_string());
            }
            last_import_end = Some(pos + l.len());
        } else if t.starts_with("package ") {
            if t.trim_start_matches("package ").trim().trim_end_matches(';') == pkg {
                return Import::None;
            }
            package_end = Some(pos + l.len());
        } else if !(t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.starts_with('@')) {
            break;
        }
        pos += l.len();
    }
    if MAPPED.iter().any(|(k, j)| *k == simple && *j == fqn) || pkg == "java.lang" || pkg == "kotlin" {
        return Import::None;
    }
    let line = format!("import {fqn}\n");
    match (last_import_end, package_end) {
        (Some(at), _) => Import::Insert(at, line),
        (None, Some(at)) => Import::Insert(at, format!("\n{line}")),
        (None, None) => Import::Insert(0, format!("{line}\n")),
    }
}

/// Kotlin: `class Name<T>(引数) : Parent(..), Iface<X>, Other by x {` の親の型の名前（`of` なら、その名前の宣言だけ）
pub fn kotlin_supertypes(src: &str, of: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = kotlin_parents(src, of).into_iter().map(|t| t.split('<').next().unwrap_or("").rsplit('.').next().unwrap_or("").to_string()).filter(|t| !t.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// Kotlin: 型 `of` の親の型を、型引数ごとの文字で（`Iface<X>`。呼び出しの括弧と by の後は外す）
pub fn kotlin_supertype_texts(src: &str, of: &str) -> Vec<String> {
    kotlin_parents(src, Some(of))
}

/// Kotlin の親の型の文字（書いた順）
fn kotlin_parents(src: &str, of: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for kw in ["class ", "interface ", "object "] {
        let mut from = 0;
        while let Some(i) = src[from..].find(kw).map(|i| i + from) {
            from = i + kw.len();
            if i > 0 && src[..i].ends_with(word) {
                continue;
            }
            let rest = src[from..].trim_start();
            let name: String = rest.chars().take_while(|c| word(*c)).collect();
            if name.is_empty() || of.is_some_and(|o| o != name) {
                continue;
            }
            // 型引数・主コンストラクタを飛ばして : を探す（{ か行の終わりまで）
            let mut k = name.len();
            let b = rest.as_bytes();
            let mut depth = 0i32;
            while k < b.len() {
                match b[k] {
                    b'<' | b'(' => depth += 1,
                    b'>' | b')' => depth -= 1,
                    b':' if depth == 0 => break,
                    b'{' | b'\n' if depth == 0 => {
                        k = b.len();
                        break;
                    }
                    _ => {}
                }
                k += 1;
            }
            if k >= b.len() {
                continue;
            }
            let head = &rest[k + 1..];
            let head = &head[..head.find(['{', '\n']).unwrap_or(head.len())];
            for part in crate::text::split_top(&crate::text::C_LIKE, head, b',', true) {
                let part = part.split(" by ").next().unwrap_or(part).trim();
                // 呼び出しの括弧（Parent(x)・Repo<A>()）を外す。型引数（<..>）は残す
                let gen_end = part.rfind('>').map_or(0, |g| g + 1);
                let t = match part[gen_end..].find('(') {
                    Some(p) => &part[..gen_end + p],
                    None => part,
                };
                let t = t.trim();
                if t.starts_with(|c: char| c.is_alphabetic()) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_parents() {
        let src = "open class Base\nclass Repo<T>(val x: Int) : Base(), Saver<T>, java.io.Serializable {\n}\nobject Single : Other by impl\ninterface I : J\n";
        assert_eq!(kotlin_supertypes(src, Some("Repo")), vec!["Base", "Saver", "Serializable"]);
        assert_eq!(kotlin_supertypes(src, Some("Single")), vec!["Other"]);
        assert_eq!(kotlin_supertypes(src, None), vec!["Base", "J", "Other", "Saver", "Serializable"]);
        assert!(kotlin_supertypes(src, Some("Base")).is_empty());
        assert_eq!(kotlin_supertype_texts("class C : Repo<User, Long>(), java.io.Closeable {\n", "C"), vec!["Repo<User, Long>", "java.io.Closeable"]);
    }

    #[test]
    fn java_forms_and_decls() {
        assert_eq!(java_form("ArrayList<Int>().stream()"), "new ArrayList<Int>().stream()");
        assert_eq!(java_form("user?.name!!.trim()"), "user.name.trim()");
        assert_eq!(java_form("x.Foo(1)"), "x.Foo(1)", "メソッドの呼び出しはそのまま");
        let src = "class A(private val repo: Repo<User>) {\n    fun f(names: List<String>?) {\n        val xs = ArrayList<Map<String, Int>>()\n        val n: Int = 1\n        val y = g()\n    }\n}\n";
        let at = src.len();
        assert_eq!(decl_text(src, "repo", at).as_deref(), Some("Repo<User>"));
        assert_eq!(decl_text(src, "names", at).as_deref(), Some("List<String>"));
        assert_eq!(decl_text(src, "xs", at).as_deref(), Some("ArrayList<Map<String, Int>>"));
        assert_eq!(decl_text(src, "y", at), None);
        assert_eq!(decl_text("val s = \"a\"", "s", 20).as_deref(), Some("String"));
        assert_eq!(decl_text("val id = UUID.randomUUID()", "id", 30), None, "static メソッドの呼び出しは型でない");
        assert_eq!(expand(src, "y.size", at), "g().size");
    }

    #[test]
    fn imports() {
        let apply = |src: &str, fqn: &str| match import_plan(src, fqn) {
            Import::Insert(at, t) => format!("{}{t}{}", &src[..at], &src[at..]),
            Import::Qualified(q) => format!("Q {q}"),
            Import::None => src.to_string(),
        };
        assert_eq!(apply("package app\n\nimport java.util.List\n\nclass A\n", "java.util.UUID"), "package app\n\nimport java.util.List\nimport java.util.UUID\n\nclass A\n");
        assert_eq!(apply("package app\n\nclass A\n", "java.util.UUID"), "package app\n\nimport java.util.UUID\n\nclass A\n");
        assert_eq!(apply("import a.UUID\nclass A\n", "java.util.UUID"), "Q java.util.UUID");
        assert_eq!(apply("class A\n", "java.util.List"), "class A\n", "Kotlin の List は import が要らない");
    }
}
