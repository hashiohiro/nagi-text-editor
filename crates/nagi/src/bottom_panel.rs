//! 下の欄（IntelliJ の下のツールウィンドウと同じ）。「Git」「検索」「問題」「ログ」「ターミナル」のタブ
//! （DB のクエリの結果があれば「クエリの結果」も）。
//!
//! 1 行目にタブと右端の隠すボタン、2 行目から中身。隠しても中身は残す。高さは上の窓との間の線をドラッグして変える。
//! - Git: 履歴（グラフ付きのログ）を下の欄に収めて出す（Alt+9）
//! - 検索: 全文検索の結果（Alt+3）
//! - 問題: :Make などで集めたエラー・警告をファイルごとに（Alt+6）
//! - ログ: 実行したコマンド（:Make・Maven）の出力（Alt+4）
//! - ターミナル: シェル（Alt+F12）
//!
//! 検索とログは同じ入れ物（GrepPanel）の 2 つで、出している方を `grep`、もう一方を `grep_other` に置いて入れ替える。

use std::path::PathBuf;

use nagi_vim::Key;

use crate::editor::{Editor, Effect, Mode, Side};
use crate::panes::Rect;
use crate::result_grid::BottomKind;

/// タブ（名前, 種類）。左右の欄と同じくアイコンで並べ、出しているタブの名前を横に出す。クエリの結果は結果があるときだけ
pub const BTABS: [(&str, BottomKind); 6] = [
    ("Git", BottomKind::Git),
    ("検索", BottomKind::Grep),
    ("問題", BottomKind::Problems),
    ("ログ", BottomKind::Output),
    ("ターミナル", BottomKind::Terminal),
    ("クエリの結果", BottomKind::Result),
];

/// 問題の一覧の 1 行
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemRow {
    /// ファイル（パス, エラーの数, 警告の数）
    File(PathBuf, usize, usize),
    /// 診断（diags の番号）
    Item(usize),
}

#[derive(Default)]
pub struct ProblemsView {
    pub selected: usize,
    pub scroll: usize,
    pending_g: bool,
    pending_w: bool,
}

impl Editor {
    /// 出すタブ（クエリの結果は結果があるときだけ）
    pub fn bottom_tabs(&self) -> Vec<(&'static str, BottomKind)> {
        BTABS.iter().copied().filter(|(_, k)| *k != BottomKind::Result || !self.db_results.is_empty()).collect()
    }

    /// 下の欄の中身の矩形（1 行目のタブを除く）
    pub fn bottom_content_rect(&self) -> Option<Rect> {
        let r = self.panel_rect()?;
        Some(Rect { x: r.x, y: r.y + 1, w: r.w, h: r.h.saturating_sub(1) })
    }

    /// 1 行目の `col` にあるタブ
    pub fn btab_at(&self, col: usize) -> Option<BottomKind> {
        self.bottom_tabs().get(col / crate::tree_panel::TAB_W).map(|t| t.1)
    }

    /// ログ（コマンドの出力）の入れ物（出していなくても）
    pub(crate) fn output_panel_mut(&mut self) -> Option<&mut crate::grep_panel::GrepPanel> {
        if self.grep.as_ref().is_some_and(|g| g.title.is_some()) {
            self.grep.as_mut()
        } else {
            self.grep_other.as_mut().filter(|g| g.title.is_some())
        }
    }

    /// 今の `grep` がログ（コマンドの出力）か
    fn grep_is_output(&self) -> bool {
        self.grep.as_ref().is_some_and(|g| g.title.is_some())
    }

    /// 検索とログの入れ物を、`kind` の方が `grep` に来るように入れ替える
    pub(crate) fn swap_grep_for(&mut self, kind: BottomKind) {
        let want_output = kind == BottomKind::Output;
        if self.grep.is_some() && self.grep_is_output() != want_output {
            std::mem::swap(&mut self.grep, &mut self.grep_other);
        }
        if self.grep.is_none() && self.grep_other.as_ref().is_some_and(|g| g.title.is_some() == want_output) {
            std::mem::swap(&mut self.grep, &mut self.grep_other);
        }
    }

    /// 新しく検索・ログを始める前に、出している方がもう一方なら脇へ置く
    pub(crate) fn stash_grep_unless(&mut self, output: bool) {
        if self.grep.is_some() && self.grep_is_output() != output {
            self.grep_other = self.grep.take();
        }
    }

    /// タブを出して移る
    pub(crate) fn select_bottom_tab(&mut self, kind: BottomKind) {
        self.bottom_hidden = false;
        self.bottom_kind = kind;
        match kind {
            BottomKind::Grep | BottomKind::Output => self.swap_grep_for(kind),
            BottomKind::Git => {
                if self.log.is_none() {
                    self.open_log(false);
                }
                if let Some(l) = &mut self.log {
                    l.docked = true;
                }
                if self.log.is_some() {
                    self.mode = Mode::Log;
                    self.rows = self.text_rows(self.active_rect());
                    return;
                }
            }
            BottomKind::Terminal => {
                self.open_terminal();
                return;
            }
            BottomKind::Problems | BottomKind::Result => {}
        }
        self.list_side = Side::Bottom;
        self.mode = Mode::List;
        self.pending_keys.clear();
        self.rows = self.text_rows(self.active_rect());
    }

    /// Alt+9 などで: 出ていて移っていれば隠す、そうでなければ出して移る
    pub(crate) fn toggle_bottom_tab(&mut self, kind: BottomKind) {
        let here = self.bottom_visible() && self.bottom_kind == kind && self.bottom_focused();
        if here {
            self.hide_bottom();
        } else {
            self.select_bottom_tab(kind);
        }
    }

    /// 下の欄にいるか
    pub fn bottom_focused(&self) -> bool {
        match self.bottom_kind {
            BottomKind::Git => self.mode == Mode::Log,
            BottomKind::Terminal => self.mode == Mode::Terminal,
            _ => (self.mode == Mode::List && self.list_side == Side::Bottom) || self.mode == Mode::GrepPrompt,
        }
    }

    /// 履歴を全画面で出しているか（下の欄に収めていない）
    pub fn log_fullscreen(&self) -> bool {
        self.log.as_ref().is_some_and(|l| !l.docked)
    }

    /// 問題の一覧の行（ファイルごとにまとめ、エラーのあるファイルを先に）
    pub fn problem_rows(&self) -> Vec<ProblemRow> {
        let mut files: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for (i, d) in self.diags.iter().enumerate() {
            match files.iter_mut().find(|(p, _)| *p == d.path) {
                Some(f) => f.1.push(i),
                None => files.push((d.path.clone(), vec![i])),
            }
        }
        files.sort_by_key(|(p, v)| (!v.iter().any(|&i| self.diags[i].error), p.clone()));
        let mut out = Vec::new();
        for (p, mut v) in files {
            v.sort_by_key(|&i| (!self.diags[i].error, self.diags[i].line));
            let e = v.iter().filter(|&&i| self.diags[i].error).count();
            out.push(ProblemRow::File(p, e, v.len() - e));
            out.extend(v.into_iter().map(ProblemRow::Item));
        }
        out
    }

    pub(crate) fn problems_key(&mut self, key: Key) -> Effect {
        let rows = self.problem_rows();
        let h = self.bottom_content_rect().map_or(10, |r| r.h).max(1);
        let last = rows.len().saturating_sub(1);
        let p = &mut self.problems;
        let g = std::mem::take(&mut p.pending_g);
        if std::mem::take(&mut p.pending_w) {
            if matches!(key, Key::Char('k') | Key::Ctrl('k') | Key::Char('w') | Key::Ctrl('w') | Key::Char('p')) {
                self.mode = Mode::Normal;
            }
            return Effect::None;
        }
        match key {
            Key::Char('j') | Key::Down => p.selected = (p.selected + 1).min(last),
            Key::Char('k') | Key::Up => p.selected = p.selected.saturating_sub(1),
            Key::Ctrl('d') => p.selected = (p.selected + h / 2).min(last),
            Key::Ctrl('u') => p.selected = p.selected.saturating_sub(h / 2),
            Key::Char('g') if g => p.selected = 0,
            Key::Char('g') => p.pending_g = true,
            Key::Char('G') => p.selected = last,
            Key::Ctrl('w') => p.pending_w = true,
            Key::Enter | Key::Char('o') => {
                let stay = key == Key::Char('o');
                let target = match rows.get(p.selected) {
                    Some(ProblemRow::Item(i)) => Some(*i),
                    // ファイルの行なら最初の問題へ
                    Some(ProblemRow::File(..)) => match rows.get(p.selected + 1) {
                        Some(ProblemRow::Item(i)) => Some(*i),
                        _ => None,
                    },
                    None => None,
                };
                if let Some(i) = target {
                    let d = self.diags[i].clone();
                    self.jump_to(&d.path, d.line, d.col);
                    self.mode = if stay { Mode::List } else { Mode::Normal };
                    self.list_side = Side::Bottom;
                }
            }
            Key::Char('q') => self.hide_bottom(),
            Key::Esc => self.mode = Mode::Normal,
            Key::Char(':') => self.enter_command(""),
            _ => {}
        }
        let p = &mut self.problems;
        if p.selected < p.scroll {
            p.scroll = p.selected;
        } else if p.selected >= p.scroll + h {
            p.scroll = p.selected + 1 - h;
        }
        Effect::None
    }

    /// 問題の一覧のクリック（中身の行。2 回で開く）
    pub(crate) fn problems_click(&mut self, row: usize, clicks: u32) {
        self.list_side = Side::Bottom;
        self.mode = Mode::List;
        let n = self.problem_rows().len();
        let i = self.problems.scroll + row;
        if i < n {
            self.problems.selected = i;
            if clicks >= 2 {
                self.problems_key(Key::Enter);
            }
        }
    }

    /// 下の欄の押した位置（欄の中の行・列。0 行目はタブ）
    pub(crate) fn bottom_click(&mut self, row: usize, col: usize, clicks: u32) {
        if row == 0 {
            if let Some(k) = self.btab_at(col) {
                self.select_bottom_tab(k);
            }
            return;
        }
        let row = row - 1;
        match self.bottom_kind {
            BottomKind::Result => self.result_click(row, col.saturating_sub(1)),
            BottomKind::Problems => self.problems_click(row, clicks),
            BottomKind::Git => {
                let w = self.bottom_content_rect().map_or(0, |r| r.w);
                self.log_click(row, col, w, clicks);
            }
            BottomKind::Terminal => self.terminal_click(row, col),
            BottomKind::Grep | BottomKind::Output => self.panel_click(row, clicks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Diag;
    use nagi_core::Buffer;

    #[test]
    fn tabs_problems_and_search_vs_output() {
        let mut e = Editor::new(Buffer::from_str(""), None);
        e.set_screen(120, 40);
        assert!(!e.bottom_visible(), "初めは出ていない");
        // 問題: ファイルごと、エラーのあるファイルを先に
        let d = |p: &str, line, error| Diag { path: PathBuf::from(p), line, col: 0, error, msg: "m".into() };
        e.diags = vec![d("b.ts", 3, false), d("a.ts", 9, true), d("b.ts", 1, false), d("a.ts", 2, false)];
        e.toggle_bottom_tab(BottomKind::Problems);
        assert!(e.bottom_visible());
        assert_eq!(e.bottom_kind, BottomKind::Problems);
        assert_eq!(e.mode, Mode::List);
        let rows = e.problem_rows();
        assert_eq!(rows[0], ProblemRow::File(PathBuf::from("a.ts"), 1, 1));
        assert_eq!(rows[1], ProblemRow::Item(1), "エラーを先に");
        assert_eq!(rows[3], ProblemRow::File(PathBuf::from("b.ts"), 0, 2));
        // もう一度で隠す
        e.toggle_bottom_tab(BottomKind::Problems);
        assert!(!e.bottom_visible());
        // 検索とログは別々に持つ
        e.open_grep("x");
        e.stash_grep_unless(true);
        e.open_result_panel("Maven: compile");
        assert!(e.grep.as_ref().unwrap().title.is_some());
        assert!(e.grep_other.as_ref().unwrap().title.is_none(), "検索は脇に残す");
        e.select_bottom_tab(BottomKind::Grep);
        assert!(e.grep.as_ref().unwrap().title.is_none());
        assert_eq!(e.grep.as_ref().unwrap().query.text, "x");
        e.select_bottom_tab(BottomKind::Output);
        assert_eq!(e.grep.as_ref().unwrap().title.as_deref(), Some("Maven: compile"));
        // :Make をキーで流しても、カーソルは動かない（結果はログのタブへ）
        let dir = std::env::temp_dir().join(format!("nagi-bottom-make-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.rs")).unwrap();
        buf.path = Some(dir.join("a.rs"));
        let mut m = Editor::new(buf, None);
        m.set_screen(120, 40);
        for k in nagi_vim::parse_keys(":Make echo a.rs:3:4: error: boom&& echo a.rs:2:1: warning: careful<CR>", " ") {
            m.key(k);
        }
        for _ in 0..500 {
            m.poll_make();
            if m.make.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!((m.cursor.line, m.mode), (0, Mode::Normal), "{}", m.message);
        assert_eq!(m.bottom_kind, BottomKind::Output);
        assert_eq!(m.problem_rows().len(), 3, "問題のタブにも出る");
        // タブの位置
        assert_eq!(e.btab_at(1), Some(BottomKind::Git));
        assert_eq!(e.btab_at(5), Some(BottomKind::Grep));
    }
}
