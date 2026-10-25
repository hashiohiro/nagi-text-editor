//! .editorconfig（https://editorconfig.org）を読む。ファイルのフォルダから上へ root = true まで探し、近いものほど強い。
//!
//! 効かせるもの: indent_style・indent_size・tab_width（インデント・タブの表示幅）、trim_trailing_whitespace・
//! insert_final_newline（保存時）、end_of_line・charset（新しいファイル）、max_line_length（右端の線）。
//! 設定の画面の値より強く、:set より弱い（IntelliJ と同じ）。設定の「.editorconfig を使う」で切れる。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// `file` に効く値（キーは小文字。値も小文字）
pub fn props_for(file: &Path) -> HashMap<String, String> {
    let Some(dir) = file.parent() else { return HashMap::new() };
    let mut confs: Vec<(PathBuf, String)> = Vec::new();
    for d in dir.ancestors() {
        let p = d.join(".editorconfig");
        if let Ok(text) = std::fs::read_to_string(&p) {
            let root = text.lines().take_while(|l| !l.trim_start().starts_with('[')).any(|l| {
                let l = l.trim().to_ascii_lowercase().replace(' ', "");
                l == "root=true"
            });
            confs.push((d.to_path_buf(), text));
            if root {
                break;
            }
        }
    }
    let mut out = HashMap::new();
    // 遠いものから当てる（近いものが後から上書き）
    for (d, text) in confs.iter().rev() {
        let Ok(rel) = file.strip_prefix(d) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let mut on = false;
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with(';') {
                continue;
            }
            if let Some(sec) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                on = glob_match(sec, &rel);
                continue;
            }
            if let (true, Some((k, v))) = (on, l.split_once('=')) {
                out.insert(k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
            }
        }
    }
    // indent_size = tab は tab_width と同じ。tab_width が無ければ indent_size
    if out.get("indent_size").is_some_and(|v| v == "tab") {
        if let Some(t) = out.get("tab_width").cloned() {
            out.insert("indent_size".into(), t);
        } else {
            out.remove("indent_size");
        }
    }
    if !out.contains_key("tab_width") {
        if let Some(s) = out.get("indent_size").filter(|s| s.parse::<usize>().is_ok()).cloned() {
            out.insert("tab_width".into(), s);
        }
    }
    out
}

/// 節の名前（*.{js,ts}・**/*.md・Makefile・lib/**.js）が、.editorconfig のフォルダからの相対パスに合うか
pub fn glob_match(pat: &str, rel: &str) -> bool {
    // / を含まなければ、どのフォルダの下のファイル名にも
    let pat = if pat.contains('/') { pat.trim_start_matches('/').to_string() } else { format!("**/{pat}") };
    let mut re = String::from("^");
    let mut chars = pat.chars().peekable();
    let mut depth = 0;
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                // **/ はフォルダが無くてもよい
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                depth += 1;
                re.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                re.push(')');
            }
            ',' if depth > 0 => re.push('|'),
            '[' => {
                let mut class = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    class.push(d);
                }
                let class = class.strip_prefix('!').map_or(class.clone(), |c| format!("^{c}"));
                re.push_str(&format!("[{}]", class.replace('\\', "\\\\")));
            }
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).is_ok_and(|r| r.is_match(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*", "a.js"));
        assert!(glob_match("*", "src/a.js"));
        assert!(glob_match("*.{js,ts}", "src/x/a.ts"));
        assert!(!glob_match("*.{js,ts}", "a.java"));
        assert!(glob_match("Makefile", "sub/Makefile"));
        assert!(glob_match("lib/**.js", "lib/a/b.js"));
        assert!(!glob_match("lib/**.js", "src/lib/b.js"));
        assert!(glob_match("*.[ch]", "a.h"));
    }

    #[test]
    fn nearest_wins() {
        let dir = std::env::temp_dir().join(format!("nagi-ec-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(dir.join(".editorconfig"), "root = true\n\n[*]\nindent_style = space\nindent_size = 4\nend_of_line = lf\n\n[*.md]\ntrim_trailing_whitespace = false\n").unwrap();
        std::fs::write(dir.join("web/.editorconfig"), "[*.{js,ts}]\nindent_size = 2\n[Makefile]\nindent_style = tab\nindent_size = tab\n").unwrap();
        let p = props_for(&dir.join("web/a.ts"));
        assert_eq!((p["indent_style"].as_str(), p["indent_size"].as_str(), p["tab_width"].as_str()), ("space", "2", "2"));
        assert_eq!(props_for(&dir.join("web/Makefile"))["indent_style"], "tab");
        assert!(!props_for(&dir.join("web/Makefile")).contains_key("indent_size"), "tab で tab_width が無ければ消す");
        assert_eq!(props_for(&dir.join("README.md"))["trim_trailing_whitespace"], "false");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
