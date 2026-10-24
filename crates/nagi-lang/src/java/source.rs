//! ライブラリ（依存の jar・JDK）の型の定義を開く（LSP なし）。
//!
//! ソースの jar（`foo-1.0.jar` の隣の `foo-1.0-sources.jar`）か、JDK の `lib/src.zip` に `.java` があればそれを、
//! 無ければ class ファイルの宣言（索引のかけら）から骨組みを作って、`%LOCALAPPDATA%\nagi\index\sources` に書いて開く。
//! 書いたファイルは読むだけ（読み取り専用にする）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::eco::jvm::{ACC_PROTECTED, ACC_PUBLIC, ACC_STATIC, Class, JarIndex};

use crate::java::types::JType;

const ACC_FINAL: u16 = 0x0010;
const ACC_VARARGS: u16 = 0x0080;
const ACC_INTERFACE: u16 = 0x0200;
const ACC_ABSTRACT: u16 = 0x0400;
const ACC_ANNOTATION: u16 = 0x2000;
const ACC_ENUM: u16 = 0x4000;

/// 開いたもの
pub struct Opened {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    /// ソースがあったか（無ければ骨組み）
    pub source: bool,
}

/// 書く場所（`%LOCALAPPDATA%\nagi\index\sources`）
pub fn sources_dir() -> Option<PathBuf> {
    Some(crate::eco::jvm::jars_dir()?.parent()?.join("sources"))
}

/// 内部名 `owner`（`java/util/Map$Entry`）の定義（`member` があればそのメンバー）を `dir` の下に書いて、その位置
pub fn open(jars: &JarIndex, owner: &str, member: Option<&str>, dir: &Path) -> Option<Opened> {
    let top = owner.split('$').next().unwrap_or(owner);
    let jar = jars.jar_of(top).or_else(|| jars.jar_of(owner))?;
    let stamp = |p: &Path| {
        let m = std::fs::metadata(p).ok()?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(nagi_core::hash::sha256_hex(format!("{}|{}|{t}", p.display(), m.len()).as_bytes())?[..10].to_string())
    };
    // ソース: JDK（ct.sym）は src.zip のモジュールの下、jar は隣の -sources.jar
    let src_zip = if jar.file_name().is_some_and(|n| n == "ct.sym") {
        jar.parent().map(|l| l.join("src.zip"))
    } else {
        let name = jar.file_name()?.to_string_lossy();
        name.strip_suffix(".jar").map(|stem| jar.with_file_name(format!("{stem}-sources.jar")))
    }
    .filter(|p| p.is_file());
    let want = format!("{top}.java");
    let from_source = src_zip.as_deref().and_then(|z| {
        let file = dir.join(format!("src-{}", stamp(z)?)).join(&want);
        if file.is_file() {
            return std::fs::read_to_string(&file).ok().map(|t| (file, t));
        }
        let (_, bytes) = crate::eco::jvm::zip_find(z, |n| n == want || n.ends_with(&format!("/{want}")))?;
        let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        write_read_only(&file, &text)?;
        Some((file, text))
    });
    let source = from_source.is_some();
    let (path, text) = match from_source {
        Some(x) => x,
        None => {
            let file = dir.join(format!("stub-{}", stamp(jar)?)).join(&want);
            let text = match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(_) => {
                    let t = skeleton(jars, top)?;
                    write_read_only(&file, &t)?;
                    t
                }
            };
            (file, text)
        }
    };
    let (line, col) = find_decl(&text, owner, member);
    Some(Opened { path, line, col, source })
}

/// 書いて読み取り専用にする（書きかけを読まないよう、別の名前で書いてから置き換える）
fn write_read_only(file: &Path, text: &str) -> Option<()> {
    std::fs::create_dir_all(file.parent()?).ok()?;
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text).ok()?;
    let mut perm = std::fs::metadata(&tmp).ok()?.permissions();
    perm.set_readonly(true);
    let _ = std::fs::set_permissions(&tmp, perm);
    if std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return file.is_file().then_some(());
    }
    Some(())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `text` の中で `name` が語として出てくる位置（前後が語の文字でない）
fn word_at(line: &str, name: &str) -> Option<usize> {
    line.match_indices(name).map(|(i, _)| i).find(|&i| !line[..i].ends_with(is_word) && !line[i + name.len()..].starts_with(is_word))
}

/// 型の宣言の行（`class Name`・`interface Name` …）。`from` の行から
fn type_line(lines: &[&str], name: &str, from: usize) -> Option<usize> {
    (from..lines.len()).find(|&k| {
        let l = lines[k];
        let t = l.trim_start();
        !t.starts_with(['*', '/']) && ["class ", "interface ", "enum ", "record ", "@interface "].iter().any(|kw| l.match_indices(kw).any(|(i, _)| (i == 0 || !l[..i].ends_with(is_word)) && word_at(&l[i + kw.len()..], name) == Some(0)))
    })
}

/// 型 `owner`（入れ子は `$`）とそのメンバー `member` の宣言の (行, 列)。見つからなければ型の行
pub fn find_decl(text: &str, owner: &str, member: Option<&str>) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let simple = owner.rsplit('/').next().unwrap_or(owner);
    let mut at = 0;
    let mut name = simple;
    for part in simple.split('$') {
        if let Some(k) = type_line(&lines, part, at) {
            at = k;
            name = part;
        }
    }
    let col = |k: usize, n: &str| word_at(lines[k], n).unwrap_or(0);
    let Some(m) = member else { return (at, col(at, name)) };
    if let Some(found) = member_line(&lines, at, m) {
        return found;
    }
    // Lombok の getter・setter はソースに無いので、そのフィールドへ
    let field = ["get", "set", "is"].iter().find_map(|p| m.strip_prefix(p)).filter(|f| f.starts_with(char::is_uppercase)).map(|f| f[..1].to_lowercase() + &f[1..]);
    field.and_then(|f| member_line(&lines, at, &f)).unwrap_or((at, col(at, name)))
}

/// 型の行 `at` より後の、メンバー `m` の宣言の (行, 列)
fn member_line(lines: &[&str], at: usize, m: &str) -> Option<(usize, usize)> {
    for k in at + 1..lines.len() {
        let l = lines[k];
        let t = l.trim_start();
        if t.starts_with(['*', '/', '@']) || t.starts_with("return ") {
            continue;
        }
        let Some(i) = word_at(l, m) else { continue };
        let before = l[..i].trim_end();
        let after = l[i + m.len()..].trim_start();
        // メソッド・フィールドの宣言（名前の前が型）か、列挙の定数（行の頭）
        let typed = before.ends_with(|c: char| is_word(c) || c == '>' || c == ']') && !before.ends_with("new") && !before.contains('=') && !before.contains('.');
        // 列挙の定数は行の頭か、定数の並び（`A(1, "a"), B(2, "b")`）の , の後
        let constants = before.trim().is_empty() || before.ends_with(',');
        let decl = (typed && (after.starts_with('(') || after.starts_with([';', '=', ',']))) || (constants && (after.is_empty() || after.starts_with(['(', ',', ';', '{'])));
        if decl {
            return Some((k, i));
        }
    }
    None
}

/// 型の文字（単純名。パッケージが要るものは `imports` に足す）
fn render(t: &JType, pkg: &str, imports: &mut BTreeSet<String>) -> String {
    match t {
        JType::Class(n, args) => {
            let (p, simple) = n.rsplit_once('/').unwrap_or(("", n));
            if !p.is_empty() && p != "java/lang" && p != pkg {
                imports.insert(format!("{}.{}", p.replace('/', "."), simple.split('$').next().unwrap_or(simple)));
            }
            let mut s = simple.replace('$', ".");
            if !args.is_empty() {
                let a: Vec<String> = args.iter().map(|a| render(a, pkg, imports)).collect();
                s.push_str(&format!("<{}>", a.join(", ")));
            }
            s
        }
        JType::Var(v) => v.clone(),
        JType::Prim(c) => match c {
            b'B' => "byte",
            b'C' => "char",
            b'D' => "double",
            b'F' => "float",
            b'I' => "int",
            b'J' => "long",
            b'S' => "short",
            b'Z' => "boolean",
            _ => "void",
        }
        .to_string(),
        JType::Array(e) => format!("{}[]", render(e, pkg, imports)),
        JType::Other => "?".to_string(),
    }
}

/// 型変数の並び（`<K, V extends Comparable<V>>`）
fn render_params(params: &[(String, Option<JType>)], pkg: &str, imports: &mut BTreeSet<String>) -> String {
    if params.is_empty() {
        return String::new();
    }
    let p: Vec<String> = params
        .iter()
        .map(|(n, b)| match b {
            Some(b @ JType::Class(c, _)) if c != "java/lang/Object" => format!("{n} extends {}", render(b, pkg, imports)),
            _ => n.clone(),
        })
        .collect();
    format!("<{}> ", p.join(", "))
}

fn modifiers(access: u16, interface: bool) -> String {
    let mut m = String::new();
    if access & ACC_PUBLIC != 0 {
        m.push_str("public ");
    } else if access & ACC_PROTECTED != 0 {
        m.push_str("protected ");
    }
    if access & ACC_STATIC != 0 {
        m.push_str("static ");
    }
    if access & ACC_FINAL != 0 {
        m.push_str("final ");
    }
    if access & ACC_ABSTRACT != 0 && !interface {
        m.push_str("abstract ");
    }
    m
}

/// class ファイルの宣言から作る骨組み（入れ子のクラスも）
pub fn skeleton(jars: &JarIndex, top: &str) -> Option<String> {
    let c = jars.class(top)?;
    let pkg = top.rsplit_once('/').map_or("", |x| x.0).to_string();
    let mut imports = BTreeSet::new();
    let mut body = String::new();
    class_text(jars, &c, &pkg, &mut imports, &mut body, 0);
    let mut out = String::new();
    out.push_str("// class ファイルの宣言から作った骨組み（ソースがありません。引数の名前と本体はありません）\n");
    if !pkg.is_empty() {
        out.push_str(&format!("package {};\n\n", pkg.replace('/', ".")));
    }
    for i in &imports {
        out.push_str(&format!("import {i};\n"));
    }
    if !imports.is_empty() {
        out.push('\n');
    }
    out.push_str(&body);
    Some(out)
}

fn class_text(jars: &JarIndex, c: &Class, pkg: &str, imports: &mut BTreeSet<String>, out: &mut String, depth: usize) {
    let ind = "    ".repeat(depth);
    let simple = c.name.rsplit(['/', '$']).next().unwrap_or(&c.name).to_string();
    let interface = c.access & ACC_INTERFACE != 0;
    let kind = if c.access & ACC_ANNOTATION != 0 {
        "@interface"
    } else if interface {
        "interface"
    } else if c.access & ACC_ENUM != 0 {
        "enum"
    } else if c.super_name.as_deref() == Some("java/lang/Record") {
        "record"
    } else {
        "class"
    };
    let mut sig = crate::java::types::SigReader::new(c.signature.as_deref().unwrap_or(""));
    let params = sig.type_params_bounds();
    let mut supers: Vec<JType> = Vec::new();
    if c.signature.is_some() {
        while let Some(t) = sig.next_type() {
            supers.push(t);
        }
    } else {
        supers = c.super_name.iter().chain(&c.interfaces).map(|n| JType::Class(n.clone(), Vec::new())).collect();
    }
    let mods = modifiers(c.access & !(if interface { ACC_ABSTRACT } else { 0 }), interface).replace("final ", if kind == "enum" || kind == "record" { "" } else { "final " });
    out.push_str(&format!("{ind}{mods}{kind} {simple}{}", render_params(&params, pkg, imports).trim_end()));
    // 親（クラスの最初は extends。インターフェースは全部 extends。Object・Enum・Record は書かない）
    let (ext, imp): (Vec<&JType>, Vec<&JType>) = if interface {
        (supers.iter().skip(1).collect(), Vec::new())
    } else {
        (supers.iter().take(1).collect(), supers.iter().skip(1).collect())
    };
    let ext: Vec<String> = ext.into_iter().filter(|t| !matches!(t, JType::Class(n, _) if ["java/lang/Object", "java/lang/Enum", "java/lang/Record"].contains(&n.as_str()))).map(|t| render(t, pkg, imports)).collect();
    let imp: Vec<String> = imp.into_iter().map(|t| render(t, pkg, imports)).collect();
    if !ext.is_empty() {
        out.push_str(&format!(" extends {}", ext.join(", ")));
    }
    if !imp.is_empty() {
        out.push_str(&format!(" implements {}", imp.join(", ")));
    }
    out.push_str(" {\n");
    let inner = "    ".repeat(depth + 1);
    // 列挙の定数
    let constants: Vec<&str> = c.fields.iter().filter(|f| f.access & ACC_ENUM != 0).map(|f| f.name.as_str()).collect();
    if !constants.is_empty() {
        out.push_str(&format!("{inner}{};\n\n", constants.join(", ")));
    }
    for f in c.fields.iter().filter(|f| f.access & ACC_ENUM == 0) {
        let t = crate::java::types::SigReader::new(f.signature.as_deref().unwrap_or(&f.desc)).next_type().unwrap_or(JType::Other);
        out.push_str(&format!("{inner}{}{} {};\n", modifiers(f.access, false), render(&t, pkg, imports), f.name));
    }
    if !c.fields.is_empty() {
        out.push('\n');
    }
    for m in c.methods.iter().filter(|m| m.name != "<clinit>") {
        let mut s = crate::java::types::SigReader::new(m.signature.as_deref().unwrap_or(&m.desc));
        let tparams = s.type_params_bounds();
        let (ret, args) = s.method();
        let n = args.len();
        let args: Vec<String> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let t = render(a, pkg, imports);
                let t = if i + 1 == n && m.access & ACC_VARARGS != 0 { t.strip_suffix("[]").map(|x| format!("{x}...")).unwrap_or(t) } else { t };
                format!("{t} arg{i}")
            })
            .collect();
        let mods = modifiers(m.access, interface);
        let mods = if interface && m.access & (ACC_ABSTRACT | ACC_STATIC) == 0 { format!("{mods}default ") } else { mods };
        let head = if m.name == "<init>" { simple.clone() } else { format!("{}{} {}", render_params(&tparams, pkg, imports), render(&ret, pkg, imports), m.name) };
        out.push_str(&format!("{inner}{mods}{head}({});\n", args.join(", ")));
    }
    for n in jars.nested(&c.name) {
        if let Some(nc) = jars.class(n) {
            out.push('\n');
            class_text(jars, &nc, pkg, imports, out, depth + 1);
        }
    }
    out.push_str(&format!("{ind}}}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JDK の ct.sym と src.zip から（JAVA_HOME が無ければ何もしない）
    #[test]
    fn jdk_source_and_skeleton() {
        use crate::eco::jvm::JarSource;
        let Some(home) = std::env::var_os("JAVA_HOME").map(PathBuf::from).filter(|h| h.join("lib").join("ct.sym").is_file()) else { return };
        let ct = home.join("lib").join("ct.sym");
        let dir = std::env::temp_dir().join(format!("nagi-jsrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rel = crate::eco::jvm::ct_sym_latest(&ct).unwrap();
        let keep = |n: &str| n.split('/').next().is_some_and(|d| d.contains(rel)) && (n.contains("/java/util/") || n.contains("/java/lang/"));
        let ix = JarIndex::load(&[JarSource { path: &ct, keep: &keep, tag: "s" }], &dir.join("jars"));
        if home.join("lib").join("src.zip").is_file() {
            let o = open(&ix, "java/util/List", Some("size"), &dir.join("sources")).unwrap();
            assert!(o.source && o.path.ends_with("java/util/List.java"), "{}", o.path.display());
            let text = std::fs::read_to_string(&o.path).unwrap();
            assert!(text.lines().nth(o.line).unwrap().contains("int size()"), "{:?}", text.lines().nth(o.line));
            assert!(std::fs::metadata(&o.path).unwrap().permissions().readonly(), "読むだけ");
            let o = open(&ix, "java/util/Map$Entry", Some("getKey"), &dir.join("sources")).unwrap();
            assert!(std::fs::read_to_string(&o.path).unwrap().lines().nth(o.line).unwrap().contains("K getKey()"));
        }
        let s = skeleton(&ix, "java/util/Map").unwrap();
        assert!(s.contains("public interface Map<K, V> {"), "{s}");
        assert!(s.contains("V get(Object arg0);"), "{s}");
        assert!(s.contains("interface Entry<K, V>"), "{s}");
        assert!(s.contains("static <K, V> Map<K, V> of("), "{s}");
        let s = skeleton(&ix, "java/util/ArrayList").unwrap();
        assert!(s.contains("public class ArrayList<E> extends AbstractList<E> implements List<E>"), "{s}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decl_lines() {
        let text = "package a;\n\n/** class Map doc */\npublic interface Map<K, V> {\n    int size();\n    V get(Object key);\n    interface Entry<K, V> {\n        K getKey();\n    }\n    static <K, V> Map<K, V> of() { return null; }\n}\n";
        assert_eq!(find_decl(text, "a/Map", None), (3, 17));
        assert_eq!(find_decl(text, "a/Map", Some("get")), (5, 6));
        assert_eq!(find_decl(text, "a/Map$Entry", None), (6, 14));
        assert_eq!(find_decl(text, "a/Map$Entry", Some("getKey")), (7, 10));
        assert_eq!(find_decl(text, "a/Map", Some("of")), (9, 28));
        let e = "enum Color {\n    RED, GREEN;\n    int x = RED.ordinal();\n}\n";
        assert_eq!(find_decl(e, "Color", Some("GREEN")), (1, 9));
        let e = "enum Level {\n    ERROR(1, \"E\"), WARN(2, \"W\");\n}\n";
        assert_eq!(find_decl(e, "Level", Some("WARN")), (1, 19));
        let d = "@Data\npublic class Req {\n    private String validationMode;\n}\n";
        assert_eq!(find_decl(d, "a/Req", Some("getValidationMode")), (2, 19), "Lombok の getter はフィールドへ");
    }
}
