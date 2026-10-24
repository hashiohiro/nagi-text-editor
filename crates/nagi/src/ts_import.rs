//! TypeScript の import を足す（補完でパッケージ・ほかのファイルの名前を決めたとき）。
//!
//! 要らないとき: もう import している（同じ名前）・このファイルで宣言している。
//! 足し方: 同じモジュールからの `import { … } from 'm'` があれば `{ }` の中の最後に足し、無ければ import の並びの後に
//! `import { X } from 'm';` の行を足す（import が無ければ先頭に、空行を挟んで）。引用符はファイルの import に合わせる。

pub use crate::java_import::Plan;

/// `src` で名前 `name` をモジュール `spec` から使うための import の足し方
pub fn plan(src: &str, name: &str, spec: &str) -> Plan {
    let scan = nagi_workers::dts::scan_source(src);
    if scan.imports.iter().any(|i| i.local == name) || scan.decls.iter().any(|d| d.0 == name) {
        return Plan::None;
    }
    let lines: Vec<(usize, &str)> = src.split_inclusive('\n').scan(0, |at, l| {
        let start = *at;
        *at += l.len();
        Some((start, l))
    }).collect();
    let quote = if src.contains("from \"") && !src.contains("from '") { '"' } else { '\'' };
    // 同じモジュールの import { … } の } の前へ
    let mut last_import_end = None;
    let mut i = 0;
    while i < lines.len() {
        let (start, l) = lines[i];
        let t = l.trim_start();
        if t.starts_with("import ") {
            // 複数行の import は ; か from '…' の行まで
            let mut j = i;
            while j + 1 < lines.len() && !lines[j].1.contains(" from ") && !lines[j].1.trim_end().ends_with(';') {
                j += 1;
            }
            let end = lines[j].0 + lines[j].1.len();
            let text = &src[start..end];
            last_import_end = Some(end);
            let from = text.rsplit_once("from").map(|x| x.1.trim().trim_end_matches(';').trim().trim_matches(['\'', '"']));
            if from == Some(spec) && !text.contains("import type") && !text.contains("* as ") {
                if let (Some(open), Some(close)) = (text.find('{'), text.rfind('}')) {
                    let inner = text[open + 1..close].trim_end();
                    let at = start + open + 1 + inner.len();
                    // 1 行に 1 つずつ並べた import は、最後の行の字下げで行を足す（末尾の , も合わせる）
                    let ins = if text[open + 1..close].contains('\n') && !inner.trim().is_empty() {
                        let last = inner.rsplit('\n').next().unwrap_or("");
                        let indent: String = last.chars().take_while(|c| c.is_whitespace()).collect();
                        if inner.ends_with(',') { format!("\n{indent}{name},") } else { format!(",\n{indent}{name}") }
                    } else if inner.trim().is_empty() {
                        format!(" {name}")
                    } else if inner.ends_with(',') {
                        format!(" {name}")
                    } else {
                        format!(", {name}")
                    };
                    return Plan::Insert(at, ins);
                }
            }
            i = j + 1;
            continue;
        }
        if !(t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')) {
            break;
        }
        i += 1;
    }
    let line = format!("import {{ {name} }} from {quote}{spec}{quote};\n");
    match last_import_end {
        Some(at) => Plan::Insert(at, line),
        None => Plan::Insert(0, format!("{line}\n")),
    }
}

/// 名前 `name` を import できるモジュールの指定（有力な順）: このファイルが import しているモジュール → 依存のパッケージ（その名前を
/// プロジェクトで import している回数、モジュールを import している回数の多い順）→ プロジェクトのほかのファイル（索引の型・関数の
/// 定義のあるファイル。ワークスペースの別のライブラリは tsconfig の別名、ほかは相対の指定）
/// `imported` はファイルの import の指定（先に読んでおく）。
/// `minus` の (名前, モジュール) は回数から 1 つ引く（測るとき、測っている import 自身を数えない）
pub fn candidates_with(ix: &nagi_workers::dts::DtsIndex, idx: Option<&nagi_workers::index::Index>, here: &std::path::Path, imported: &[String], name: &str, minus: Option<(&str, &str)>) -> Vec<String> {
    // 依存のパッケージ: プロジェクトでよく import しているものから（同じ名前のアイコンなどより、使っているパッケージ）
    let mut pkgs: Vec<&str> = ix.modules_exporting(name);
    let sub = |n: &str, m: &str| u32::from(minus.is_some_and(|(a, b)| (a.is_empty() || a == n) && b == m));
    let n_use = |m: &str| ix.name_usage.get(&(name.to_string(), m.to_string())).copied().unwrap_or(0).saturating_sub(sub(name, m));
    let s_use = |m: &str| ix.spec_usage.get(m).copied().unwrap_or(0).saturating_sub(sub("", m));
    pkgs.sort_by_key(|m| (std::cmp::Reverse(n_use(m)), std::cmp::Reverse(s_use(m)), m.len()));
    let mut out: Vec<String> = pkgs.into_iter().map(String::from).collect();
    if let Some(idx) = idx {
        for l in idx.lookup(name).iter().filter(|l| l.kind.is_type() || matches!(l.kind, nagi_syntax::defs::DefKind::Function | nagi_syntax::defs::DefKind::Constant | nagi_syntax::defs::DefKind::Variable)) {
            let p = idx.root.join(&*l.path);
            if !l.path.ends_with(".ts") && !l.path.ends_with(".tsx") || p == here {
                continue;
            }
            // ワークスペースの別のライブラリ（projects/<別名>/ の中）は tsconfig の別名で、同じライブラリの中は相対で
            let alias = ix.paths.iter().map(|(k, _)| k).filter(|k| !k.contains('*')).find(|k| {
                let lib = idx.root.join("projects").join(k);
                p.starts_with(&lib) && !here.starts_with(&lib)
            });
            let spec = match alias {
                Some(k) => Some(k.clone()),
                None => relative_spec(here, &p),
            };
            if let Some(spec) = spec {
                if !out.contains(&spec) {
                    out.push(spec);
                }
            }
        }
    }
    // このファイルが import しているモジュールを先に（相対の指定も）
    out.sort_by_key(|m| !imported.iter().any(|i| i == m));
    out
}

/// ファイル `from_file` から `to_file`（.ts）を import する指定（`./x`・`../a/b`。拡張子・/index は外す）
pub fn relative_spec(from_file: &std::path::Path, to_file: &std::path::Path) -> Option<String> {
    let from_dir = from_file.parent()?;
    let to = to_file.with_extension("");
    let to = if to.file_name().is_some_and(|n| n == "index") { to.parent()?.to_path_buf() } else { to };
    let a: Vec<_> = from_dir.components().collect();
    let b: Vec<_> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..a.len() {
        parts.push("..".into());
    }
    for c in &b[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    let s = parts.join("/");
    Some(if s.starts_with("..") { s } else { format!("./{s}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(src: &str, p: Plan) -> String {
        match p {
            Plan::Insert(at, t) => format!("{}{t}{}", &src[..at], &src[at..]),
            _ => src.to_string(),
        }
    }

    #[test]
    fn plans() {
        let src = "import { Component } from '@angular/core';\nimport { Router } from '@angular/router';\n\nexport class A {}\n";
        assert_eq!(apply(src, plan(src, "OnInit", "@angular/core")), "import { Component, OnInit } from '@angular/core';\nimport { Router } from '@angular/router';\n\nexport class A {}\n");
        assert_eq!(apply(src, plan(src, "HttpClient", "@angular/common/http")), "import { Component } from '@angular/core';\nimport { Router } from '@angular/router';\nimport { HttpClient } from '@angular/common/http';\n\nexport class A {}\n");
        assert_eq!(plan(src, "Router", "@angular/router"), Plan::None);
        assert_eq!(plan(src, "A", "./a"), Plan::None, "このファイルの宣言");
        let multi = "import {\n  A,\n  B,\n} from \"m\";\n\nx();\n";
        assert_eq!(apply(multi, plan(multi, "C", "m")), "import {\n  A,\n  B,\n  C,\n} from \"m\";\n\nx();\n");
        assert_eq!(apply("x();\n", plan("x();\n", "Y", "y")), "import { Y } from 'y';\n\nx();\n");
    }

    /// 実際のプロジェクトの import { X } from 'm' で、その import が無いとしたときの 1 番目の候補が m（相対は同じファイル）になる割合
    /// （NAGI_TS_ROOT=… cargo test -p nagi ts_import_rate -- --ignored --nocapture。読むだけ）
    #[test]
    #[ignore]
    fn ts_import_rate() {
        use std::path::PathBuf;
        let Some(root) = std::env::var_os("NAGI_TS_ROOT").map(PathBuf::from) else { return };
        let rx = nagi_workers::index::build(&root, std::sync::Arc::new(|| {}));
        let idx = loop {
            if let Ok(nagi_workers::index::Progress::Done(i)) = rx.recv() {
                break i;
            }
        };
        let ix = nagi_workers::dts::DtsIndex::load(&root);
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for f in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = f.path();
                let name = f.file_name().to_string_lossy().into_owned();
                if p.is_dir() && !name.starts_with('.') && name != "node_modules" && name != "dist" {
                    stack.push(p);
                } else if name.ends_with(".ts") && !name.ends_with(".d.ts") {
                    files.push(p);
                }
            }
        }
        // [1 番目, 候補にある, 無い]
        let mut n = [0usize; 3];
        let mut miss: std::collections::HashMap<String, usize> = Default::default();
        let mut dump = std::env::var_os("NAGI_RATE_DUMP").map(|f| std::io::BufWriter::new(std::fs::File::create(f).unwrap()));
        files.sort();
        let t0 = std::time::Instant::now();
        for p in &files {
            let Ok(src) = std::fs::read_to_string(p) else { continue };
            let imports = nagi_workers::dts::scan_source(&src).imports;
            let dir = p.parent().unwrap().to_path_buf();
            for (k, imp) in imports.iter().enumerate() {
                if imp.imported == "*" || imp.imported == "default" {
                    continue;
                }
                let others: Vec<String> = imports.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, i)| i.from.clone()).collect();
                let cands = candidates_with(&ix, Some(&idx), p, &others, &imp.local, Some((&imp.imported, &imp.from)));
                if let Some(d) = &mut dump {
                    let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
                    std::io::Write::write_fmt(d, format_args!("{rel}\t{}\t{cands:?}\n", imp.local)).unwrap();
                }
                let same = |c: &String| *c == imp.from || (c.starts_with('.') && imp.from.starts_with('.') && ix.resolve_import(&root, &dir, c) == ix.resolve_import(&root, &dir, &imp.from));
                let k = match cands.iter().position(same) {
                    Some(0) => 0,
                    Some(_) => 1,
                    None => 2,
                };
                n[k] += 1;
                if k > 0 {
                    *miss.entry(format!("{k} {} ← {} (1 番目 {:?})", imp.local, imp.from, cands.first())).or_default() += 1;
                }
            }
        }
        let all: usize = n.iter().sum();
        let pc = |x: usize| x as f64 * 100.0 / all.max(1) as f64;
        println!("{all} 個の import {:?}: 1 番目 {:.1}% / 候補にある {:.1}% / 無い {:.1}%", t0.elapsed(), pc(n[0]), pc(n[1]), pc(n[2]));
        let mut v: Vec<_> = miss.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, c) in v.iter().take(30) {
            println!("{c:5} {k}");
        }
    }

    #[test]
    fn relative() {
        use std::path::Path;
        assert_eq!(relative_spec(Path::new("/p/src/app/a.ts"), Path::new("/p/src/app/b.ts")).as_deref(), Some("./b"));
        assert_eq!(relative_spec(Path::new("/p/src/app/a.ts"), Path::new("/p/src/lib/x/index.ts")).as_deref(), Some("../lib/x"));
    }
}
