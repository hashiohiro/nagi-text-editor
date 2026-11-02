//! 行番号の欄の、実行の ▶ と折りたたみの印（IntelliJ と同じ）。
//!
//! - ▶: main（Java・Kotlin・Rust・Python）とテスト（@Test・#[test] の次の行）。押すと 実行・デバッグ のメニュー
//! - 折りたたみ: 構文木の折りたためる範囲の頭の行。閉じていれば ▸ をいつも、開いていれば ▾ をマウスを欄に載せている間だけ出す。
//!   押すと開け閉めする

use crate::editor::{Editor, GUTTER, GUTTER_FOLD, GUTTER_RUN};

/// ▶ の種類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Main,
    Test,
}

/// `line` の行に ▶ を出すか（`prev` は 1 つ上の行、`ext` はファイルの拡張子）
pub fn run_kind(ext: &str, line: &str, prev: &str) -> Option<RunKind> {
    let t = line.trim_start();
    let p = prev.trim();
    let decl = t.contains('(') && !t.starts_with("//") && !t.starts_with('#') && !t.starts_with('@');
    match ext {
        "java" | "kt" | "kts" => {
            if t.contains("static void main(") || t.starts_with("fun main(") {
                Some(RunKind::Main)
            } else if decl && (p.starts_with("@Test") || p.starts_with("@ParameterizedTest") || p.starts_with("@RepeatedTest")) {
                Some(RunKind::Test)
            } else {
                None
            }
        }
        "rs" => {
            if t.starts_with("fn main(") || t.starts_with("pub fn main(") {
                Some(RunKind::Main)
            } else if decl && (p == "#[test]" || p.starts_with("#[tokio::test")) && t.contains("fn ") {
                Some(RunKind::Test)
            } else {
                None
            }
        }
        "py" => (t.starts_with("if __name__") && t.contains("__main__")).then_some(RunKind::Main).or_else(|| (t.starts_with("def test") && t.contains('(')).then_some(RunKind::Test)),
        _ => None,
    }
}

impl Editor {
    /// 作業中のバッファの `line` 行の ▶
    pub fn run_marker(&self, line: usize) -> Option<RunKind> {
        let ext = self.core.buf.path.as_deref().and_then(|p| p.extension()).map(|x| x.to_string_lossy().to_ascii_lowercase())?;
        let prev = if line > 0 { self.core.buf.line(line - 1) } else { String::new() };
        run_kind(&ext, &self.core.buf.line(line), &prev)
    }

    /// `line` 行から始まる折りたためる範囲（閉じていればそれ、無ければ構文木で一番外側のもの）
    pub fn fold_starting_at(&self, line: usize) -> Option<((usize, usize), bool)> {
        if let Some(&f) = self.folds.closed.iter().find(|f| f.0 == line) {
            return Some((f, true));
        }
        let s = self.syntax.as_ref()?;
        s.folds_in(line, line + 1).into_iter().max_by_key(|f| f.1).map(|f| (f, false))
    }

    /// 行番号の欄の ▶・折りたたみの印を押した（`col` は窓の中の列）。受け取ったら true
    pub(crate) fn gutter_icon_click(&mut self, row: usize, col: usize, at: (usize, usize)) -> bool {
        let bw = self.gutter() - GUTTER;
        if col < bw + GUTTER_RUN || col >= bw + GUTTER {
            return false;
        }
        let line = self.pos_at(row, self.gutter()).line;
        if col >= bw + GUTTER_FOLD {
            let Some((f, closed)) = self.fold_starting_at(line) else { return false };
            if closed {
                self.folds.closed.retain(|&x| x != f);
            } else {
                self.folds.closed.push(f);
                // カーソルが畳んだ中にあれば頭の行へ
                if self.core.cursor.line > f.0 && self.core.cursor.line <= f.1 {
                    self.core.cursor.line = f.0;
                    self.clamp_normal();
                }
            }
            return true;
        }
        let Some(kind) = self.run_marker(line) else { return false };
        // テストはその行で（カーソルの位置のテストとして）動かす
        let m = crate::menu::Menu::new(at.0, at.1)
            .item("実行", move |e| {
                e.core.cursor.line = line;
                match kind {
                    RunKind::Main => match e.current_file_config() {
                        Some(c) => e.run_config(c),
                        None => e.svc.notify.info("このファイルの実行の仕方が分かりません"),
                    },
                    RunKind::Test => e.run_test_at_cursor(),
                }
            })
            .item("デバッグ", move |e| {
                e.core.cursor.line = line;
                match kind {
                    RunKind::Main => match e.current_file_config() {
                        Some(c) => e.debug_config(c),
                        None => e.svc.notify.info("このファイルの実行の仕方が分かりません"),
                    },
                    RunKind::Test => e.debug_test(),
                }
            });
        self.open_menu(m);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn run_markers() {
        assert_eq!(run_kind("java", "    public static void main(String[] args) {", ""), Some(RunKind::Main));
        assert_eq!(run_kind("java", "    void adds() {", "    @Test"), Some(RunKind::Test));
        assert_eq!(run_kind("java", "    void adds() {", "    @Override"), None);
        assert_eq!(run_kind("rs", "fn main() {", ""), Some(RunKind::Main));
        assert_eq!(run_kind("rs", "    fn parses() {", "    #[test]"), Some(RunKind::Test));
        assert_eq!(run_kind("py", "if __name__ == \"__main__\":", ""), Some(RunKind::Main));
        assert_eq!(run_kind("txt", "fn main() {", ""), None);
    }

    /// 折りたたみの印を押すと畳み、もう一度で開く。▶ を押すと 実行・デバッグ のメニュー。数字の所はブレークポイント
    #[test]
    fn fold_and_run_icons() {
        let dir = std::env::temp_dir().join(format!("nagi-gutter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {\n    let a = 1;\n    let b = 2;\n}\nfn other() {}\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.rs")).unwrap();
        buf.path = Some(dir.join("a.rs"));
        let mut syn = nagi_syntax::Syntax::new(nagi_syntax::Lang::Rust).unwrap();
        syn.parse(&mut buf);
        let mut e = Editor::new(buf, Some(syn));
        e.set_screen(100, 30);
        let bw = e.gutter() - GUTTER;
        assert_eq!(e.fold_starting_at(0), Some(((0, 3), false)));
        assert!(e.gutter_icon_click(0, bw + GUTTER_FOLD, (1, 1)));
        assert_eq!(e.folds.closed, vec![(0, 3)]);
        assert!(e.gutter_icon_click(0, bw + GUTTER_FOLD, (1, 1)));
        assert!(e.folds.closed.is_empty());
        // 折りたためない行では受け取らない
        assert!(!e.gutter_icon_click(1, bw + GUTTER_FOLD, (1, 1)));
        // ▶
        assert_eq!(e.run_marker(0), Some(RunKind::Main));
        assert!(e.gutter_icon_click(0, bw + GUTTER_RUN, (1, 1)));
        let labels: Vec<String> = e.menu.as_ref().unwrap().items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(labels, ["実行", "デバッグ"]);
        e.menu = None;
        assert!(!e.gutter_icon_click(1, bw + GUTTER_RUN, (1, 1)), "▶ の無い行");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
