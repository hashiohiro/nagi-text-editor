//! クイックフィックス（IntelliJ の Alt+Enter）。コンパイラは使わず、手元の索引で直せるものだけを出す。
//!
//! - クラスをインポート: カーソルの語が型の名前（大文字で始まる）で、まだ import していなければ、プロジェクトの型と
//!   依存の jar・JDK の型からその名前のものを並べ、選ぶと import を足す（Java・Kotlin）
//! - 未使用のインポート文を削除: import の行の上で、使っていない import があれば整理する（import の整理と同じ）
//!
//! 直せるものが無ければ、そう知らせる。

use crate::editor::Editor;

/// 直し方 1 つ
pub enum Fix {
    /// import を足す（完全な名前）
    Import(String),
    /// import を整理する
    OptimizeImports,
}

impl Fix {
    pub fn label(&self) -> String {
        match self {
            Fix::Import(fqn) => format!("クラスをインポート: {fqn}"),
            Fix::OptimizeImports => "未使用のインポート文を削除".into(),
        }
    }
}

/// Java の `package x.y;` の行のパッケージ
fn package_of(text: &str) -> Option<String> {
    text.lines().map(str::trim).find_map(|l| l.strip_prefix("package ")).map(|p| p.trim_end_matches(';').trim().to_string())
}

impl Editor {
    /// カーソルの位置で使える直し方
    pub(crate) fn quick_fixes(&self) -> Vec<Fix> {
        let mut out = Vec::new();
        let ext = self.core.buf.path.as_deref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let line = self.core.buf.line(self.core.cursor.line);
        // import の行: 使っていないものがあれば整理
        if line.trim_start().starts_with("import ") {
            if let Some(kind) = nagi_lang::imports::Kind::of_ext(&ext) {
                let src = self.core.buf.text().replace("\r\n", "\n");
                let name = self.core.buf.path.as_deref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if nagi_lang::imports::optimize(kind, &src, &name).ok().flatten().is_some() {
                    out.push(Fix::OptimizeImports);
                }
            }
            return out;
        }
        if !matches!(ext.as_str(), "java" | "kt") {
            return out;
        }
        let Some((word, _)) = self.word_at_cursor() else { return out };
        if !word.starts_with(|c: char| c.is_ascii_uppercase()) {
            return out;
        }
        let src = self.core.buf.text();
        // このファイルで決めている型（class Foo）なら足さない
        if src.contains(&format!("class {word}")) || src.contains(&format!("interface {word}")) || src.contains(&format!("enum {word}")) || src.contains(&format!("record {word}")) {
            return out;
        }
        let mut fqns: Vec<String> = Vec::new();
        // プロジェクトの型（ファイルの package の行から）
        if let Some(ix) = self.nav.index.as_ref() {
            for l in ix.lookup(&word).iter().filter(|l| l.kind.is_type()) {
                let text = std::fs::read_to_string(self.nav_root().join(&*l.path)).unwrap_or_default();
                let fqn = match package_of(&text) {
                    Some(p) if !p.is_empty() => format!("{p}.{word}"),
                    _ => continue,
                };
                if !fqns.contains(&fqn) {
                    fqns.push(fqn);
                }
            }
        }
        // 依存の jar・JDK の型（内側のクラスは除く）
        if let Some(jars) = nagi_lang::eco::jars(&self.nav.libs) {
            for internal in jars.by_simple_name(&word) {
                if internal.contains('$') {
                    continue;
                }
                let fqn = nagi_lang::java::import::java_name(internal);
                if !fqns.contains(&fqn) {
                    fqns.push(fqn);
                }
            }
        }
        // もう import している・同じパッケージ・java.lang のものがあれば、足す必要は無い
        let lang = self.language();
        let plans: Vec<(String, nagi_lang::lang::Import)> = fqns.into_iter().map(|f| (f.clone(), lang.import_plan(&src, &word, &f))).collect();
        if plans.iter().any(|(_, p)| matches!(p, nagi_lang::lang::Import::None)) {
            return out;
        }
        let mut cands: Vec<String> = plans.into_iter().filter(|(_, p)| matches!(p, nagi_lang::lang::Import::Insert(..))).map(|(f, _)| f).collect();
        // java.util などの JDK の型を後ろに（プロジェクト・依存のものを先に）。同じなら短い順
        cands.sort_by_key(|f| (f.starts_with("java.") || f.starts_with("javax."), f.len()));
        out.extend(cands.into_iter().map(Fix::Import));
        out
    }

    /// Alt+Enter: 直し方のメニューをカーソルの所に出す（メニューは渡した行の下に開く）
    pub(crate) fn quick_fix(&mut self) {
        let fixes = self.quick_fixes();
        if fixes.is_empty() {
            self.svc.notify.info("ここで使えるクイックフィックスはありません");
            return;
        }
        let r = self.active_rect();
        let (row, col) = self.screen_pos(self.core.cursor.line, self.core.cursor.byte).map_or((r.y, r.x), |(y, x)| (r.y + y, r.x + self.gutter() + x));
        let mut m = crate::menu::Menu::new(row, col);
        for f in fixes {
            let label = f.label();
            m = m.item(label, move |e| e.apply_fix(&f));
        }
        self.open_menu(m);
    }

    fn apply_fix(&mut self, f: &Fix) {
        match f {
            Fix::OptimizeImports => self.optimize_imports(),
            Fix::Import(fqn) => {
                let src = self.core.buf.text();
                let word = fqn.rsplit('.').next().unwrap_or(fqn);
                if let nagi_lang::lang::Import::Insert(at, text) = self.language().import_plan(&src, word, fqn) {
                    let (line, byte) = (self.core.cursor.line, self.core.cursor.byte);
                    let above = at <= self.core.buf.line_start(line);
                    let added = text.matches('\n').count();
                    self.core.hist.begin();
                    self.edit(at, at, &text);
                    self.core.hist.close();
                    // カーソルは同じ語の上のまま（上に足した行の分だけ下へ）
                    self.core.cursor.line = line + if above { added } else { 0 };
                    self.core.cursor.byte = byte;
                    self.clamp_normal();
                    self.svc.notify.info(format!("import {fqn}; を足しました"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    /// Alt+Enter: 型の名前の上で、プロジェクトの型の import を足す。import の行では、使っていない import を消す。
    /// 直せるものが無ければ知らせる
    #[test]
    fn imports_and_removes() {
        let dir = std::env::temp_dir().join(format!("nagi-qfix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src/main/java/com/shop/model")).unwrap();
        std::fs::create_dir_all(dir.join("src/main/java/com/shop/app")).unwrap();
        std::fs::write(dir.join("src/main/java/com/shop/model/Order.java"), "package com.shop.model;\n\npublic class Order {}\n").unwrap();
        let app = dir.join("src/main/java/com/shop/app/App.java");
        std::fs::write(&app, "package com.shop.app;\n\nimport java.util.List;\n\nclass App {\n  Order o;\n}\n").unwrap();
        let mut e = Editor::new(Buffer::open(&app).unwrap(), None);
        e.core.buf.path = Some(app.clone());
        e.cwd = dir.clone();
        e.set_screen(120, 40);
        e.start_index();
        for _ in 0..500 {
            e.poll_index();
            if e.nav.index.as_ref().is_some_and(|i| i.file_count() == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Order の上
        e.core.cursor.line = 5;
        e.core.cursor.byte = 3;
        let fixes: Vec<String> = e.quick_fixes().iter().map(Fix::label).collect();
        assert_eq!(fixes, ["クラスをインポート: com.shop.model.Order"]);
        e.quick_fix();
        e.menu_run(0);
        assert_eq!(e.core.buf.text(), "package com.shop.app;\n\nimport com.shop.model.Order;\nimport java.util.List;\n\nclass App {\n  Order o;\n}\n");
        assert_eq!(e.core.buf.line(e.core.cursor.line), "  Order o;", "カーソルは同じ語の上のまま");
        // もう import したので、もう出さない
        assert!(e.quick_fixes().is_empty());
        // import の行: 使っていない java.util.List を消す
        e.core.cursor.line = 3;
        let fixes: Vec<String> = e.quick_fixes().iter().map(Fix::label).collect();
        assert_eq!(fixes, ["未使用のインポート文を削除"]);
        e.quick_fix();
        e.menu_run(0);
        assert!(!e.core.buf.text().contains("java.util.List"), "{}", e.core.buf.text());
        // 直せるものが無い所
        e.core.cursor.line = 0;
        e.quick_fix();
        assert!(e.menu.is_none());
        assert!(e.svc.notify.text.contains("クイックフィックスはありません"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
