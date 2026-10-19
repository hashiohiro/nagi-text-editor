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

use crate::editor::{Editor, Effect, State, Side};
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

/// タブの並びの左端（その手前は ▾ のボタン）
pub const BTAB_X: usize = 2;

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
        // 左端の 2 列は ▾ のボタン
        self.bottom_tabs().get(col.checked_sub(BTAB_X)? / crate::tree_panel::TAB_W).map(|t| t.1)
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
                    self.set_state(State::Log);
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
        self.set_state(State::List);
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
            BottomKind::Git => self.state() == State::Log,
            BottomKind::Terminal => self.state() == State::Terminal,
            _ => (self.state() == State::List && self.list_side == Side::Bottom) || self.state() == State::GrepPrompt,
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
                self.set_state(State::Normal);
            }
            return Effect::None;
        }
        let moved = crate::list::step(key, p.selected, last + 1, h);
        match key {
            _ if moved.is_some() => p.selected = moved.unwrap_or(p.selected),
            Key::Char('g') if g => p.selected = 0,
            Key::Char('g') => p.pending_g = true,
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
                    self.set_state(if stay { State::List } else { State::Normal });
                    self.list_side = Side::Bottom;
                }
            }
            Key::Char('q') => self.hide_bottom(),
            Key::Esc => self.set_state(State::Normal),
            Key::Char(':') => self.enter_command(""),
            _ => {}
        }
        let p = &mut self.problems;
        crate::list::fit(p.selected, &mut p.scroll, h);
        Effect::None
    }

    /// 問題の一覧のクリック（中身の行。2 回で開く）
    pub(crate) fn problems_click(&mut self, row: usize, clicks: u32) {
        self.list_side = Side::Bottom;
        self.set_state(State::List);
        let n = self.problem_rows().len();
        let p = &mut self.problems;
        if crate::list::pick_row(&mut p.selected, p.scroll, row, 0, n, clicks) == Some(crate::list::Pick::Accept(p.selected)) {
            self.problems_key(Key::Enter);
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
            BottomKind::Grep | BottomKind::Output => self.panel_click_at(row, col, clicks),
        }
    }
}



/// この部品の入口（component.rs）
pub struct ProblemsComponent;

impl crate::component::Component for ProblemsComponent {
    fn status(&self, ed: &crate::editor::Editor) -> Option<crate::component::Status> {
        let _ = ed;
        crate::component::Status::new("-- 問題 --", "j/k 移動  Enter 開く  o 欄に残って開く  ]e [e 次・前  q 隠す  Ctrl-w k 窓へ ")
    }

    fn key(&self, ed: &mut crate::editor::Editor, key: nagi_vim::Key) -> crate::editor::Effect {
        ed.problems_key(key)
    }

    fn text(&self, ed: &mut crate::editor::Editor, text: &str) {
        ed.bottom_text(text);
    }
}


/// 下の欄（検索・ログ・問題・ターミナル・結果）（layout.rs の View）
pub struct BottomView;

impl crate::layout::View for BottomView {
    fn rect(&self, ed: &crate::editor::Editor, s: crate::layout::Screen) -> Option<crate::panes::Rect> {
        let _ = s;
        ed.panel_rect()
    }

    fn draw(&self, ed: &crate::editor::Editor, p: &mut crate::view::Painter, r: crate::panes::Rect, s: crate::layout::Screen) {
        let _ = (r, s);
        draw::draw_bottom_panel(p, ed, r);
    }

    fn click(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, clicks: u32) -> crate::layout::Click {
        let _ = (r, row, col, clicks);
        // ターミナルの画面: 押して選び始める（ドラッグで範囲）
        if ed.bottom_kind == BottomKind::Terminal && ed.terminals.chooser.is_none() && ed.terminal_rect().is_some_and(|t| t.contains(row, col)) && ed.terminal_press(row, col) {
            return crate::layout::Click::SelectTerminal;
        }
        // 1 行目はタブ、2 行目から中身
        ed.bottom_click(row - r.y, col.saturating_sub(r.x), clicks);
        // 検索・ログの行を押したら、そのままドラッグで範囲を選べる
        if clicks == 1 && row > r.y + 1 && matches!(ed.bottom_kind, BottomKind::Grep | BottomKind::Output) {
            crate::layout::Click::SelectList
        } else {
            crate::layout::Click::Done
        }
    }

    fn wheel(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize, dy: isize, dx: isize) -> bool {
        let _ = (row, col);
        // 1 行目はタブ（ターミナルは main.rs が先に受け取る）
        let h = r.h.saturating_sub(1);
        match ed.bottom_kind {
            BottomKind::Grep | BottomKind::Output => match &mut ed.grep {
                // 出力・診断の読む画面は、中の Editor を送る（カーソルは見える範囲に留める）
                Some(g) if g.view.is_some() && g.title.is_some() => {
                    let v = g.view.as_mut().unwrap();
                    v.ed.scroll(dy);
                    v.ed.hscroll(dx);
                }
                Some(g) => {
                    crate::list::wheel(&mut g.selected, &mut g.scroll, g.hits.len(), h, dy);
                    if dx != 0 {
                        let width = |h: &nagi_workers::grep::Hit| {
                            let head = if h.path.is_empty() { 0 } else { nagi_core::display_col(&h.path, h.path.len(), false) + format!(":{}: ", h.line).len() };
                            head + nagi_core::display_col(&h.text, h.text.len(), false)
                        };
                        let widest = g.hits.iter().skip(g.scroll).take(h).map(width).max().unwrap_or(0);
                        crate::list::hwheel(&mut g.left, widest + 1, r.w, dx);
                    }
                }
                None => {}
            },
            BottomKind::Result => {
                let res = &mut ed.db_results;
                if res.active == 0 {
                    // 出力（下から数える。0 が最新）
                    res.log_scroll = (res.log_scroll as isize - dy).clamp(0, res.log.len().saturating_sub(1) as isize) as usize;
                } else if let Some(t) = res.active_tab_mut() {
                    let head = if t.filterable { 4 } else { 3 };
                    crate::list::wheel(&mut t.row, &mut t.scroll, t.rows.len(), r.h.saturating_sub(head), dy);
                    crate::result_grid::grid_hwheel(&mut t.col, &mut t.hscroll, &t.widths, r.w, dx);
                }
            }
            BottomKind::Git => ed.log_wheel(dy, dx, r.w, col.saturating_sub(r.x)),
            BottomKind::Problems => {
                let n = ed.problem_rows().len();
                let p = &mut ed.problems;
                p.scroll = (p.scroll as isize + dy).clamp(0, n.saturating_sub(h.saturating_sub(1)) as isize) as usize;
            }
            BottomKind::Terminal => {}
        }
        true
    }

    fn context(&self, ed: &mut crate::editor::Editor, r: crate::panes::Rect, row: usize, col: usize) -> bool {
        // Git のタブ（履歴）: 1 行目はタブなので、2 行目から中身
        if ed.bottom_kind != BottomKind::Git || row <= r.y {
            return false;
        }
        let w = ed.bottom_content_rect().map_or(0, |c| c.w);
        ed.log_context(row - r.y - 1, col.saturating_sub(r.x), w, (row, col))
    }
}

/// 描き方（view.rs の Painter で描く）
pub(crate) mod draw {
    #![allow(unused_imports)]
    use super::*;
    use crate::view::*;

    /// 下の欄: 1 行目にタブ（Git・検索・問題・ログ・ターミナル・クエリの結果）、2 行目から選んでいるタブの中身
    pub(crate) fn draw_bottom_panel(p: &mut Painter, ed: &Editor, r: Rect) {
        use crate::result_grid::BottomKind;
        let cell = p.fonts.cell();
        let (x0, y0, wpx) = (r.x as f32 * cell.w, r.y as f32 * cell.h, r.w as f32 * cell.w);
        p.out.push(Instance::fill(x0, y0, wpx, r.h as f32 * cell.h, BG));
        p.out.push(Instance::fill(x0, y0, wpx, 1.0, LINE_NO));
        let focused = ed.bottom_focused();
        // 左右の欄と同じく、タブはアイコンで並べ、出しているタブの名前を横に
        let tabs: Vec<(TabIcon, &'static str, bool)> = ed
            .bottom_tabs()
            .into_iter()
            .map(|(l, k)| {
                let icon = match k {
                    BottomKind::Git => TabIcon::Git,
                    BottomKind::Grep => TabIcon::Search,
                    BottomKind::Problems => TabIcon::Problems,
                    BottomKind::Output => TabIcon::Output,
                    BottomKind::Terminal => TabIcon::Terminal,
                    BottomKind::Result => TabIcon::Result,
                };
                (icon, l, k == ed.bottom_kind)
            })
            .collect();
        // 左端の 2 列は ▾ のボタン
        let bx = crate::bottom_panel::BTAB_X;
        draw_icon_tabs(p, Rect { x: r.x + bx, y: r.y, w: r.w.saturating_sub(bx), h: 1 }, &tabs, focused, "");
        p.out.push(Instance::fill(x0, y0 + cell.h - 1.0, bx as f32 * cell.w, 1.0, LINE_NO));
        let c = Rect { x: r.x, y: r.y + 1, w: r.w, h: r.h.saturating_sub(1) };
        let empty = |p: &mut Painter, s: &str| {
            p.text(s, c.x + 1, (c.y + 1) as f32 * cell.h, STATUS_FG);
        };
        match ed.bottom_kind {
            BottomKind::Result => draw_db_results(p, ed, c),
            BottomKind::Grep | BottomKind::Output => match &ed.grep {
                Some(g) => draw_grep_panel(p, g, c, matches!(ed.state(), State::List | State::GrepPrompt)),
                None if ed.bottom_kind == BottomKind::Grep => empty(p, "検索していません（:Grep 語、Ctrl+Shift+F）"),
                None => empty(p, "コマンドの出力はありません（:Make、Maven のタブのゴール）"),
            },
            BottomKind::Problems => draw_problems(p, ed, c, focused),
            BottomKind::Git => match &ed.log {
                Some(l) if l.docked => draw_log_view(p, l, c, ed.log_cols_for(c.w), &ed.preedit, ed.git.status.as_ref().map(|s| (s.branch.as_str(), s.ahead as usize, s.behind as usize))),
                _ => empty(p, "git のリポジトリではありません"),
            },
            BottomKind::Terminal => draw_terminal(p, ed, c, focused),
        }
    }

    /// 問題（診断）の一覧。ファイルごとにまとめ、エラーは赤、警告は黄
    pub(crate) fn draw_problems(p: &mut Painter, ed: &Editor, r: Rect, focused: bool) {
        use crate::bottom_panel::ProblemRow;
        let cell = p.fonts.cell();
        let rows = ed.problem_rows();
        if rows.is_empty() {
            p.text(" 問題はありません（:Make で集める）", r.x, r.y as f32 * cell.h, STATUS_FG);
            return;
        }
        let root = ed.nav_root();
        let saved = p.cols;
        p.cols = r.x + r.w;
        for (k, row) in rows.iter().enumerate().skip(ed.problems.scroll).take(r.h) {
            let y = (r.y + k - ed.problems.scroll) as f32 * cell.h;
            let sel = k == ed.problems.selected;
            if sel {
                p.out.push(Instance::fill(r.x as f32 * cell.w, y, r.w as f32 * cell.w, cell.h, if focused { FG } else { MATCH_BG }));
            }
            let rev = sel && focused;
            match row {
                ProblemRow::File(path, e, w) => {
                    let rel = path.strip_prefix(&root).map_or(path.display().to_string(), |x| x.display().to_string());
                    let c = p.text(&format!(" ▾ {rel}"), r.x, y, if rev { BG } else { FG });
                    let n = match (e, w) {
                        (0, w) => format!("  警告 {w}"),
                        (e, 0) => format!("  エラー {e}"),
                        (e, w) => format!("  エラー {e}・警告 {w}"),
                    };
                    p.text(&n, c, y, if rev { BG } else { STATUS_FG });
                }
                ProblemRow::Item(i) => {
                    let d = &ed.diags[*i];
                    let c = p.text("     ● ", r.x, y, if rev { BG } else if d.error { DIAG_ERROR } else { DIAG_WARN });
                    let c = p.text(&d.msg, c, y, if rev { BG } else { FG });
                    p.text(&format!("  {}:{}", d.line + 1, d.col + 1), c, y, if rev { BG } else { LINE_NO });
                }
            }
        }
        p.cols = saved;
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
        assert_eq!(e.state(), State::List);
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
        assert_eq!((m.cursor.line, m.state()), (0, State::Normal), "{}", m.notify.text);
        assert_eq!(m.bottom_kind, BottomKind::Output);
        assert_eq!(m.problem_rows().len(), 3, "問題のタブにも出る");
        // タブの位置
        assert_eq!(e.btab_at(0), None, "▾ のボタン");
        assert_eq!(e.btab_at(3), Some(BottomKind::Git));
        assert_eq!(e.btab_at(7), Some(BottomKind::Grep));
    }
}
