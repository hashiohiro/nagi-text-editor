//! Java の import を足す（補完でライブラリの型を決めたとき）。
//!
//! 要らないとき: java.lang の型、同じパッケージの型、もう import している（`pkg.*` も）。
//! 同じ単純名の別の型を import していれば足さず、完全な名前で書く（呼んだ側）。
//! 足す場所: import の並びの中で名前の順に収まる所。import が無ければ package の行の後に空行を挟んで、package も無ければ先頭に。

use regex::Regex;

/// 足し方
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// 足さなくてよい
    None,
    /// (byte の位置, 入れる文字)
    Insert(usize, String),
    /// 同じ単純名の別の型を import している（完全な名前で書く）
    Conflict,
}

/// `src` で `fqn`（`java.util.List`）を使うための import の足し方
pub fn plan(src: &str, fqn: &str) -> Plan {
    let Some((pkg, simple)) = fqn.rsplit_once('.') else { return Plan::None };
    let package = Regex::new(r"(?m)^[ \t]*package[ \t]+([\w.]+)[ \t]*;[^\n]*\n?").unwrap();
    let pk = package.captures(src);
    if pkg == "java.lang" || pk.as_ref().is_some_and(|c| &c[1] == pkg) {
        return Plan::None;
    }
    let import = Regex::new(r"(?m)^[ \t]*import[ \t]+(static[ \t]+)?([\w.]+(?:\.\*)?)[ \t]*;[^\n]*\n?").unwrap();
    // import static は後ろに並べる（IntelliJ の既定）。ふつうの import はその前に
    let mut last_end = None;
    let mut first_static = None;
    let mut before = None;
    let mut wildcard = false;
    for c in import.captures_iter(src) {
        let whole = c.get(0).unwrap();
        let name = &c[2];
        let is_static = c.get(1).is_some();
        if !is_static {
            if name == fqn {
                return Plan::None;
            }
            // 1 つの型の import は `pkg.*` より強いので、同じ単純名の別の型があれば衝突
            if name.rsplit('.').next() == Some(simple) {
                return Plan::Conflict;
            }
            if name == format!("{pkg}.*") {
                wildcard = true;
            }
            if before.is_none() && name > fqn {
                before = Some(whole.start());
            }
            last_end = Some(whole.end());
        } else if first_static.is_none() {
            first_static = Some(whole.start());
        }
    }
    if wildcard {
        return Plan::None;
    }
    let line = format!("import {fqn};\n");
    let before = before.or(last_end.is_none().then_some(first_static).flatten());
    match (before, last_end, pk) {
        (Some(at), _, _) => Plan::Insert(at, line),
        // 最後の import の行が改行で終わっていなければ（ファイルの終わり）、改行から
        (None, Some(end), _) if !src[..end].ends_with('\n') => Plan::Insert(end, format!("\n{}", line.trim_end())),
        (None, Some(end), _) => Plan::Insert(end, line),
        (None, None, Some(p)) => Plan::Insert(p.get(0).unwrap().end(), format!("\n{line}")),
        (None, None, None) => Plan::Insert(0, format!("{line}\n")),
    }
}

/// 内部名（`java/util/Map$Entry`）を Java の書き方（`java.util.Map.Entry`）に
pub fn java_name(internal: &str) -> String {
    internal.replace(['/', '$'], ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(src: &str, fqn: &str) -> String {
        match plan(src, fqn) {
            Plan::Insert(at, text) => format!("{}{text}{}", &src[..at], &src[at..]),
            Plan::None => src.to_string(),
            Plan::Conflict => "CONFLICT".into(),
        }
    }

    #[test]
    fn adds_in_order() {
        let src = "package a.b;\n\nimport java.util.Map;\nimport org.x.Y;\n\nclass C {}\n";
        assert_eq!(apply(src, "java.util.List"), "package a.b;\n\nimport java.util.List;\nimport java.util.Map;\nimport org.x.Y;\n\nclass C {}\n");
        assert_eq!(apply(src, "org.z.Z"), "package a.b;\n\nimport java.util.Map;\nimport org.x.Y;\nimport org.z.Z;\n\nclass C {}\n");
        // import static の並びには混ぜない
        let src = "import a.B;\nimport static z.Y.f;\n";
        assert_eq!(apply(src, "c.D"), "import a.B;\nimport c.D;\nimport static z.Y.f;\n");
    }

    #[test]
    fn not_needed_or_conflict() {
        let src = "package a.b;\n\nimport java.util.*;\nimport java.awt.List;\nimport static org.Assert.assertEquals;\n";
        assert_eq!(plan(src, "java.lang.String"), Plan::None);
        assert_eq!(plan(src, "a.b.Other"), Plan::None, "同じパッケージ");
        assert_eq!(plan(src, "java.util.Map"), Plan::None, "pkg.*");
        assert_eq!(plan(src, "java.util.List"), Plan::Conflict, "java.awt.List の import は java.util.* より強い");
        assert_eq!(plan("import java.awt.List;\n", "java.util.List"), Plan::Conflict);
        assert_eq!(plan("import static org.Assert.List;\n", "java.util.List"), Plan::Insert(0, "import java.util.List;\n".into()), "ふつうの import は import static の前に");
    }

    #[test]
    fn first_import() {
        assert_eq!(apply("package a;\n\nclass C {}\n", "java.util.List"), "package a;\n\nimport java.util.List;\n\nclass C {}\n");
        assert_eq!(apply("class C {}\n", "java.util.List"), "import java.util.List;\n\nclass C {}\n");
        assert_eq!(apply("import a.B;", "c.D"), "import a.B;\nimport c.D;");
        assert_eq!(java_name("java/util/Map$Entry"), "java.util.Map.Entry");
    }
}
