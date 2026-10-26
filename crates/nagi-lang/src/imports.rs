//! import の整理（IntelliJ の Optimize Imports、Ctrl+Alt+O）。コンパイラは使わず、名前が本文に出てくるかで決める。
//!
//! - Java: 使っていない import・java.lang と同じパッケージの import・重なりを消し、IntelliJ の既定の並び
//!   （ほかのもの → 空行 → javax.* → java.* → 空行 → import static）に名前の順で並べる。`.*` は残す（中身が分からないので）。
//! - Kotlin: 使っていない import を消し、IntelliJ の既定の並び（ほか → java → javax → kotlin → as の付いたもの）に。
//!   演算子の関数（`by` の getValue など、名前が本文に出ないもの）は残す。
//! - TypeScript・JavaScript: 使っていない名前を import から外し、全部使っていなければ文ごと消す。並べ替えない（読み込みの順に意味があることがある）。
//! - Python: 使っていない名前を外す（行頭の import・from だけ。`__future__`・`*`・注釈の付いた行・`__init__.py` は触らない）。並べ替えない。
//!
//! 名前は、コメントと文字列を除いた本文の識別子と比べる（Javadoc・テンプレート文字列・f 文字列・`# type:` の中は数える）。

use std::collections::HashSet;

use regex::Regex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Java,
    Kotlin,
    Ts,
    Python,
}

impl Kind {
    /// 拡張子から（整理できない言語なら None）
    pub fn of_ext(ext: &str) -> Option<Kind> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "java" => Kind::Java,
            "kt" | "kts" => Kind::Kotlin,
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Kind::Ts,
            "py" | "pyi" => Kind::Python,
            _ => return None,
        })
    }
}

/// 整理した結果（中身, 外した import の数）。変わらなければ None。整理できない形なら Err（理由）。
/// `src` の改行は \n。`file_name` は `__init__.py` を見分けるため
pub fn optimize(kind: Kind, src: &str, file_name: &str) -> Result<Option<(String, usize)>, String> {
    let r = match kind {
        Kind::Java => jvm(src, false)?,
        Kind::Kotlin => jvm(src, true)?,
        Kind::Ts => ts(src),
        Kind::Python if file_name == "__init__.py" => return Err("__init__.py の import は外へ見せるためのことがあるので整理しません".into()),
        Kind::Python => python(src),
    };
    Ok(r.filter(|(s, _)| s != src))
}

fn words(s: &str, out: &mut HashSet<String>) {
    let mut cur = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' || c == '$' {
            cur.push(c);
        } else if !cur.is_empty() {
            if !cur.starts_with(|c: char| c.is_ascii_digit()) {
                out.insert(std::mem::take(&mut cur));
            }
            cur.clear();
        }
    }
}

/// 本文の識別子（コメント・文字列を除く）
fn idents(src: &str, kind: Kind) -> HashSet<String> {
    let mut out = HashSet::new();
    let b = src.as_bytes();
    let c_like = kind != Kind::Python;
    let (mut i, mut start) = (0usize, 0usize);
    // 文字列・コメントの前までを語として読む
    let flush = |from: usize, to: usize, out: &mut HashSet<String>| words(&src[from..to], out);
    let find = |from: usize, pat: &str| src[from..].find(pat).map_or(src.len(), |k| from + k + pat.len());
    // 引用符で閉じる所（\ の後は飛ばす。改行で終わるものも）
    let close = |from: usize, q: u8, multi: bool| {
        let mut j = from;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                c if c == q => return j + 1,
                b'\n' if !multi => return j,
                _ => j += 1,
            }
        }
        b.len()
    };
    while i < b.len() {
        let rest = &src[i..];
        let c = b[i];
        if c_like && rest.starts_with("//") {
            flush(start, i, &mut out);
            i = find(i, "\n");
            start = i;
        } else if c_like && rest.starts_with("/*") {
            flush(start, i, &mut out);
            let end = find(i + 2, "*/");
            // Javadoc・KDoc の {@link Foo}・[Foo] は使っていることにする（IntelliJ と同じ）
            if rest.starts_with("/**") && matches!(kind, Kind::Java | Kind::Kotlin) {
                words(&src[i..end], &mut out);
            }
            i = end;
            start = i;
        } else if !c_like && c == b'#' {
            flush(start, i, &mut out);
            let end = find(i, "\n");
            if rest.starts_with("# type:") {
                words(&src[i..end], &mut out);
            }
            i = end;
            start = i;
        } else if matches!(c, b'"' | b'\'' | b'`') {
            // Python の接頭辞（f"…" r'…'）
            let prefix: String = src[start..i].chars().rev().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase();
            let prefix = if kind == Kind::Python && prefix.len() <= 2 && prefix.chars().all(|c| "rbfu".contains(c)) { prefix } else { String::new() };
            flush(start, i - prefix.len(), &mut out);
            let triple = rest.starts_with("\"\"\"") || (kind == Kind::Python && rest.starts_with("'''"));
            let mut end = if triple {
                let q = &rest[..3];
                find(i + 3, q)
            } else {
                close(i + 1, c, c == b'`')
            };
            // \ の後の文字が 2 バイト以上のとき
            end = end.min(src.len());
            while !src.is_char_boundary(end) {
                end += 1;
            }
            // 中に式を書けるもの: TS のテンプレート、Kotlin の文字列（$x）、Python の f 文字列。名前は数える
            let code = (kind == Kind::Ts && c == b'`') || (kind == Kind::Kotlin && c == b'"') || prefix.contains('f');
            // Java・Kotlin の 'a' は 1 文字
            if code {
                words(&src[i..end.min(src.len())], &mut out);
            }
            i = end.min(src.len()).max(i + 1);
            start = i;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    flush(start.min(src.len()), src.len(), &mut out);
    out
}

/// 行（終わりの \n を含む）
fn lines_of(src: &str) -> Vec<&str> {
    src.split_inclusive('\n').collect()
}

/// Kotlin の演算子の関数（名前が本文に出ないで使われる）
const KOTLIN_OPERATORS: &[&str] = &[
    "getValue", "setValue", "provideDelegate", "plus", "minus", "times", "div", "rem", "mod", "rangeTo", "rangeUntil", "unaryPlus", "unaryMinus", "not", "inc", "dec", "invoke", "get", "set", "contains", "iterator", "next", "hasNext", "compareTo", "equals", "plusAssign", "minusAssign", "timesAssign", "divAssign", "remAssign", "component1", "component2", "component3", "component4", "component5",
];

fn jvm(src: &str, kotlin: bool) -> Result<Option<(String, usize)>, String> {
    let java_re = Regex::new(r"^\s*import\s+(static\s+)?([\w.]+(?:\.\*)?)\s*;\s*$").unwrap();
    let kt_re = Regex::new(r"^\s*import\s+([\w.`]+(?:\.\*)?)(?:\s+as\s+(\w+))?\s*;?\s*$").unwrap();
    let lines = lines_of(src);
    let is_import = |l: &str| l.trim_start().starts_with("import ") || l.trim_start().starts_with("import\t");
    let Some(first) = lines.iter().position(|l| is_import(l)) else { return Ok(None) };
    let last = lines.iter().rposition(|l| is_import(l)).unwrap();
    // (static, 名前, 別名)
    let mut imports: Vec<(bool, String, Option<String>)> = Vec::new();
    for l in &lines[first..=last] {
        let t = l.trim_end_matches(['\n', '\r']);
        if t.trim().is_empty() {
            continue;
        }
        if !is_import(t) {
            return Err("import の間にコメントなどがあるので整理しません".into());
        }
        if kotlin {
            let c = kt_re.captures(t).ok_or_else(|| format!("読めない import: {}", t.trim()))?;
            imports.push((false, c[1].to_string(), c.get(2).map(|m| m.as_str().to_string())));
        } else {
            let c = java_re.captures(t).ok_or_else(|| format!("読めない import: {}", t.trim()))?;
            imports.push((c.get(1).is_some(), c[2].to_string(), None));
        }
    }
    let body: String = lines[..first].iter().chain(&lines[last + 1..]).copied().collect();
    let used = idents(&body, if kotlin { Kind::Kotlin } else { Kind::Java });
    let package = Regex::new(r"(?m)^\s*package\s+([\w.]+)").unwrap().captures(&body).map(|c| c[1].to_string());
    let mut seen = HashSet::new();
    let total = imports.len();
    let kept: Vec<(bool, String, Option<String>)> = imports
        .into_iter()
        .filter(|(st, name, alias)| {
            if !seen.insert((*st, name.clone(), alias.clone())) {
                return false;
            }
            if name.ends_with(".*") {
                return true;
            }
            let (pkg, simple) = name.rsplit_once('.').unwrap_or(("", name));
            let simple = simple.trim_matches('`');
            if kotlin {
                return match alias {
                    Some(a) => used.contains(a),
                    None => used.contains(simple) || KOTLIN_OPERATORS.contains(&simple),
                };
            }
            if !*st && (pkg == "java.lang" || package.as_deref() == Some(pkg)) {
                return false;
            }
            used.contains(simple)
        })
        .collect();
    let removed = total - kept.len();
    // 並び（IntelliJ の既定）
    let mut groups: Vec<Vec<String>> = Vec::new();
    if kotlin {
        let rank = |(name, alias): (&String, &Option<String>)| -> u8 {
            if alias.is_some() {
                4
            } else if name.starts_with("java.") {
                1
            } else if name.starts_with("javax.") {
                2
            } else if name.starts_with("kotlin.") {
                3
            } else {
                0
            }
        };
        let mut v: Vec<&(bool, String, Option<String>)> = kept.iter().collect();
        v.sort_by(|a, b| rank((&a.1, &a.2)).cmp(&rank((&b.1, &b.2))).then_with(|| a.1.cmp(&b.1)));
        groups.push(v.iter().map(|(_, n, a)| match a {
            Some(a) => format!("import {n} as {a}\n"),
            None => format!("import {n}\n"),
        }).collect());
    } else {
        let mut other: Vec<&String> = kept.iter().filter(|i| !i.0 && !i.1.starts_with("java.") && !i.1.starts_with("javax.")).map(|i| &i.1).collect();
        let mut javax: Vec<&String> = kept.iter().filter(|i| !i.0 && i.1.starts_with("javax.")).map(|i| &i.1).collect();
        let mut java: Vec<&String> = kept.iter().filter(|i| !i.0 && i.1.starts_with("java.")).map(|i| &i.1).collect();
        let mut stat: Vec<&String> = kept.iter().filter(|i| i.0).map(|i| &i.1).collect();
        for v in [&mut other, &mut javax, &mut java, &mut stat] {
            v.sort();
        }
        groups.push(other.iter().map(|n| format!("import {n};\n")).collect());
        groups.push(javax.iter().chain(&java).map(|n| format!("import {n};\n")).collect());
        groups.push(stat.iter().map(|n| format!("import static {n};\n")).collect());
    }
    let block = groups.into_iter().filter(|g| !g.is_empty()).map(|g| g.concat()).collect::<Vec<_>>().join("\n");
    let mut out: String = lines[..first].concat();
    out.push_str(&block);
    let mut after = &lines[last + 1..];
    // 全部消えたら、前後の空行が 2 つ続かないように
    if block.is_empty() && out.ends_with("\n\n") {
        while after.first().is_some_and(|l| l.trim().is_empty()) {
            after = &after[1..];
        }
    }
    // 最後の import の後の改行が無かったとき
    if !block.is_empty() && after.is_empty() && !src.ends_with('\n') {
        out.pop();
    }
    out.push_str(&after.concat());
    Ok(Some((out, removed)))
}

/// import の名前の 1 つ（書いたまま, その名前でファイルの中に入る名前）
fn binding(item: &str) -> (String, String) {
    let item = item.trim();
    let local = match item.split_once(" as ") {
        Some((_, l)) => l.trim(),
        None => item.trim_start_matches("type ").trim(),
    };
    (item.to_string(), local.to_string())
}

fn ts(src: &str) -> Option<(String, usize)> {
    let re = Regex::new(r#"(?m)^import[ \t]+(type[ \t]+)?([^'";]*?)[ \t\n]*from[ \t]*(['"])([^'"\n]+)['"][ \t]*(;?)[ \t]*(\n?)"#).unwrap();
    let found: Vec<regex::Captures> = re.captures_iter(src).collect();
    if found.is_empty() {
        return None;
    }
    // 本文（import の文を除いたもの）
    let mut body = String::new();
    let mut at = 0;
    for c in &found {
        let m = c.get(0).unwrap();
        body.push_str(&src[at..m.start()]);
        body.push('\n');
        at = m.end();
    }
    body.push_str(&src[at..]);
    let used = idents(&body, Kind::Ts);
    let (mut out, mut at, mut removed) = (String::new(), 0, 0);
    for c in &found {
        let m = c.get(0).unwrap();
        out.push_str(&src[at..m.start()]);
        at = m.end();
        let clause = c[2].trim();
        let ty = c.get(1).map_or("", |_| "type ");
        // 既定の名前・* as ns・{ … }
        let (mut default, mut ns, mut named): (Option<String>, Option<String>, Option<Vec<(String, String)>>) = (None, None, None);
        let mut rest = clause;
        if !rest.starts_with('{') && !rest.starts_with('*') {
            let (d, r) = rest.split_once(',').unwrap_or((rest, ""));
            default = Some(d.trim().to_string());
            rest = r.trim();
        }
        if let Some(n) = rest.strip_prefix('*') {
            ns = n.trim().strip_prefix("as").map(|x| x.trim().to_string());
        } else if let Some(inner) = rest.strip_prefix('{').and_then(|r| r.trim_end().strip_suffix('}')) {
            named = Some(inner.split(',').filter(|x| !x.trim().is_empty()).map(binding).collect());
        } else if !rest.is_empty() {
            // 読めない形はそのまま
            out.push_str(m.as_str());
            continue;
        }
        // React は JSX が見えなくても使う（古い JSX の変換）
        let keep = |name: &str| used.contains(name) || name == "React";
        let before = default.iter().count() + ns.iter().count() + named.as_ref().map_or(0, |v| v.len());
        default = default.filter(|d| keep(d));
        ns = ns.filter(|n| keep(n));
        let named_kept: Option<Vec<(String, String)>> = named.as_ref().map(|v| v.iter().filter(|(_, l)| keep(l)).cloned().collect());
        let after = default.iter().count() + ns.iter().count() + named_kept.as_ref().map_or(0, |v| v.len());
        if after == before {
            out.push_str(m.as_str());
            continue;
        }
        removed += before - after;
        if after == 0 {
            continue;
        }
        let mut parts: Vec<String> = default.into_iter().collect();
        if let Some(n) = ns {
            parts.push(format!("* as {n}"));
        }
        if let Some(v) = named_kept.filter(|v| !v.is_empty()) {
            parts.push(format!("{{ {} }}", v.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(", ")));
        }
        let q = &c[3];
        out.push_str(&format!("import {ty}{} from {q}{}{q}{}{}", parts.join(", "), &c[4], &c[5], &c[6]));
    }
    out.push_str(&src[at..]);
    Some((out, removed))
}

fn python(src: &str) -> Option<(String, usize)> {
    let lines = lines_of(src);
    // (始めの行, 終わりの行（含まない）, from の module か None, 名前の書いたもの, 括弧で書いていたか)
    let mut stmts: Vec<(usize, usize, Option<String>, Vec<String>, bool)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_end_matches(['\n', '\r']);
        let simple = l.strip_prefix("import ").map(|r| (None, r));
        let from = l.strip_prefix("from ").and_then(|r| r.split_once(" import ")).map(|(m, r)| (Some(m.trim().to_string()), r));
        let Some((module, rest)) = simple.or(from) else {
            i += 1;
            continue;
        };
        // 注釈（# noqa など）・行の継続（\）は触らない
        if l.contains('#') || l.ends_with('\\') {
            i += 1;
            continue;
        }
        let rest = rest.trim();
        if let Some(open) = rest.strip_prefix('(') {
            // 括弧の中は複数行のことがある
            let mut text = open.to_string();
            let mut j = i;
            while !text.contains(')') {
                j += 1;
                let Some(n) = lines.get(j) else { break };
                if n.contains('#') {
                    break;
                }
                text.push(' ');
                text.push_str(n.trim());
            }
            let Some(inner) = text.split_once(')').map(|x| x.0) else {
                i = j + 1;
                continue;
            };
            if lines.get(j).is_some_and(|n| n.contains('#')) {
                i = j + 1;
                continue;
            }
            stmts.push((i, j + 1, module, inner.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(), true));
            i = j + 1;
        } else {
            stmts.push((i, i + 1, module, rest.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(), false));
            i += 1;
        }
    }
    if stmts.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut k = 0;
    for (n, l) in lines.iter().enumerate() {
        while k < stmts.len() && stmts[k].1 <= n {
            k += 1;
        }
        if !stmts.get(k).is_some_and(|s| s.0 <= n && n < s.1) {
            body.push_str(l);
        }
    }
    let mut used = idents(&body, Kind::Python);
    // __all__ に文字列で書いた名前も
    if body.contains("__all__") {
        words(&body, &mut used);
    }
    let (mut out, mut at, mut removed) = (String::new(), 0usize, 0usize);
    for (s, e, module, names, paren) in &stmts {
        out.push_str(&lines[at..*s].concat());
        at = *e;
        let original = lines[*s..*e].concat();
        if module.as_deref() == Some("__future__") || names.iter().any(|n| n == "*") {
            out.push_str(&original);
            continue;
        }
        let local = |n: &str| -> String {
            match n.split_once(" as ") {
                Some((_, l)) => l.trim().to_string(),
                // import a.b は a が入る
                None if module.is_none() => n.split('.').next().unwrap_or(n).trim().to_string(),
                None => n.trim().to_string(),
            }
        };
        let kept: Vec<&String> = names.iter().filter(|n| used.contains(&local(n))).collect();
        if kept.len() == names.len() {
            out.push_str(&original);
            continue;
        }
        removed += names.len() - kept.len();
        if kept.is_empty() {
            continue;
        }
        let list = kept.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        let nl = if original.ends_with('\n') { "\n" } else { "" };
        match module {
            None => out.push_str(&format!("import {list}{nl}")),
            Some(m) => {
                let one = format!("from {m} import {list}");
                if *paren && one.len() > 79 {
                    out.push_str(&format!("from {m} import (\n{})\n", kept.iter().map(|n| format!("    {n},\n")).collect::<String>()));
                } else {
                    out.push_str(&format!("{one}{nl}"));
                }
            }
        }
    }
    out.push_str(&lines[at..].concat());
    Some((out, removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(kind: Kind, src: &str) -> String {
        optimize(kind, src, "a").unwrap().map(|x| x.0).unwrap_or_else(|| src.to_string())
    }

    #[test]
    fn java_removes_and_sorts_like_intellij() {
        let src = "package com.example;\n\nimport java.util.List;\nimport static org.junit.Assert.assertEquals;\nimport com.example.Same;\nimport java.lang.String;\nimport org.foo.Unused;\nimport javax.inject.Inject;\nimport java.util.Map;\nimport org.bar.*;\nimport java.util.List;\n\n/** {@link Map} */\nclass A {\n  @Inject List<String> xs; // Unused\n  String s = \"Unused\";\n  void t() { assertEquals(1, 1); }\n}\n";
        let out = optimize(Kind::Java, src, "A.java").unwrap().unwrap();
        assert_eq!(out.0, "package com.example;\n\nimport org.bar.*;\n\nimport javax.inject.Inject;\nimport java.util.List;\nimport java.util.Map;\n\nimport static org.junit.Assert.assertEquals;\n\n/** {@link Map} */\nclass A {\n  @Inject List<String> xs; // Unused\n  String s = \"Unused\";\n  void t() { assertEquals(1, 1); }\n}\n");
        assert_eq!(out.1, 4);
        // 全部消えたら空行は 1 つ
        assert_eq!(run(Kind::Java, "package p;\n\nimport a.B;\n\nclass C {}\n"), "package p;\n\nclass C {}\n");
        // 整理済みなら変えない
        assert!(optimize(Kind::Java, "import a.B;\n\nclass C { B b; }\n", "C.java").unwrap().is_none());
        // 間にコメントがあれば整理しない
        assert!(optimize(Kind::Java, "import a.B;\n// x\nimport a.C;\nclass D {}\n", "D.java").is_err());
    }

    #[test]
    fn kotlin_keeps_operators_and_aliases() {
        let src = "package p\n\nimport kotlin.math.max\nimport androidx.compose.runtime.getValue\nimport java.io.File\nimport a.b.Unused\nimport a.b.Thing as T\nimport a.c.Other\n\nval x by lazy { max(1, 2) }\nval t: T? = null\nval s = \"${Other.v}\"\n";
        assert_eq!(run(Kind::Kotlin, src), "package p\n\nimport a.c.Other\nimport androidx.compose.runtime.getValue\nimport kotlin.math.max\nimport a.b.Thing as T\n\nval x by lazy { max(1, 2) }\nval t: T? = null\nval s = \"${Other.v}\"\n");
    }

    #[test]
    fn multibyte_text() {
        // 日本語の文字列・コメント・\ の後の全角
        let src = "import { a, b } from 'x';\nconst s = '日本語\\あ' + a; // コメント b\nconst t = `テンプレート`;\n";
        assert_eq!(run(Kind::Ts, src), "import { a } from 'x';\nconst s = '日本語\\あ' + a; // コメント b\nconst t = `テンプレート`;\n");
        assert_eq!(run(Kind::Java, "import a.B;\nclass C { String s = \"全角\"; /* 説明 B */ }\n"), "class C { String s = \"全角\"; /* 説明 B */ }\n");
    }

    #[test]
    fn ts_drops_unused_names() {
        let src = "import React, { useState, useEffect } from 'react';\nimport type { A, B } from \"./types\";\nimport * as fs from 'fs'\nimport './side.css';\nimport {\n  fromEvent,\n  map,\n} from 'rxjs';\nimport Gone from './gone';\n\nconst x: A = useState(1);\nconst s = `${map}`;\n// fs\n";
        let out = optimize(Kind::Ts, src, "a.tsx").unwrap().unwrap();
        assert_eq!(out.0, "import React, { useState } from 'react';\nimport type { A } from \"./types\";\nimport './side.css';\nimport { map } from 'rxjs';\n\nconst x: A = useState(1);\nconst s = `${map}`;\n// fs\n");
        assert_eq!(out.1, 5);
    }

    #[test]
    fn python_drops_unused_names() {
        let src = "from __future__ import annotations\nimport os, sys\nimport os.path as osp\nfrom typing import (\n    List,\n    Dict,\n)\nfrom x import y  # noqa\nfrom z import *\nimport json\n\nx = 1  # type: Dict[str, int]\nprint(sys.argv, f\"{osp}\")\ns = 'json'\n";
        assert_eq!(run(Kind::Python, src), "from __future__ import annotations\nimport sys\nimport os.path as osp\nfrom typing import Dict\nfrom x import y  # noqa\nfrom z import *\n\nx = 1  # type: Dict[str, int]\nprint(sys.argv, f\"{osp}\")\ns = 'json'\n");
        assert!(optimize(Kind::Python, src, "__init__.py").is_err());
    }

    /// 本物のプロジェクトの写しを整理する（NAGI_IMPORTS_DIR のフォルダの中を書き換える。写しで流し、その後でコンパイルを確かめる）
    #[test]
    #[ignore]
    fn rewrite_copy_of_project() {
        let Some(dir) = std::env::var_os("NAGI_IMPORTS_DIR") else { return };
        let (mut files, mut changed, mut removed, mut skipped) = (0, 0, 0, 0);
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    if !matches!(name.as_str(), "target" | "node_modules" | ".git" | "build" | "dist" | ".angular") {
                        stack.push(p);
                    }
                    continue;
                }
                let Some(kind) = p.extension().and_then(|x| Kind::of_ext(&x.to_string_lossy())) else { continue };
                let Ok(src) = std::fs::read_to_string(&p) else { continue };
                files += 1;
                let crlf = src.contains("\r\n");
                match optimize(kind, &src.replace("\r\n", "\n"), &name) {
                    Ok(Some((out, n))) => {
                        changed += 1;
                        removed += n;
                        std::fs::write(&p, if crlf { out.replace('\n', "\r\n") } else { out }).unwrap();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        skipped += 1;
                        eprintln!("{}: {e}", p.display());
                    }
                }
            }
        }
        eprintln!("files {files} changed {changed} removed {removed} skipped {skipped}");
    }
}
