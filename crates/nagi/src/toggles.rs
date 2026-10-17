//! 左の欄と下のパネルを、さりげないボタンで出す・隠す。
//!
//! 出ているときは見出しの行の右端に薄い `«`（左の欄）・`▾`（下のパネル）。隠しているときは本文の隅、
//! 行番号の欄の空いたセルに薄い `»`（左上）・`▴`（左下）。マウスを載せると明るくなり、名前を横に出す。
//! 隠しても中身（ツリーの開き具合、書きかけのコミットメッセージ、検索の結果）は消さない。

use crate::editor::{Editor, Mode, Side};
use crate::tree_panel::LeftTab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    HideLeft,
    ShowLeft,
    HideBottom,
    ShowBottom,
    HideRight,
    ShowRight,
}

impl Button {
    pub fn glyph(self) -> &'static str {
        match self {
            Button::HideLeft => "«",
            Button::ShowLeft => "»",
            Button::HideBottom => "▾",
            Button::ShowBottom => "▴",
            Button::HideRight => "»",
            Button::ShowRight => "«",
        }
    }

    /// マウスを載せたときに横に出す名前
    pub fn label(self, ed: &Editor) -> String {
        match self {
            Button::HideLeft => "隠す".into(),
            Button::ShowLeft => match ed.left_last() {
                LeftTab::Project => "プロジェクト（Alt-1）".into(),
                LeftTab::Commit => "コミット（Alt-0）".into(),
            },
            Button::HideBottom => "隠す".into(),
            Button::ShowBottom => if ed.bottom_kind == crate::result_grid::BottomKind::Result { "クエリの結果".into() } else { "検索の結果".into() },
            Button::HideRight => "隠す".into(),
            Button::ShowRight => match (ed.db.is_some() || ed.maven.is_some(), ed.right_last()) {
                (false, _) => "データベース・Maven".into(),
                (true, crate::right_panel::RightTab::Database) => "データベース".into(),
                (true, crate::right_panel::RightTab::Maven) => "Maven".into(),
            },
        }
    }
}

impl Editor {
    /// 左の欄が出ていれば、最後に選んだタブ（隠していても覚えている）
    pub fn left_last(&self) -> LeftTab {
        match (self.tree.is_some(), self.commit.is_some()) {
            (true, true) => self.left_tab,
            (false, true) => LeftTab::Commit,
            _ => LeftTab::Project,
        }
    }

    /// 下のパネル（全文検索の結果かクエリの結果）があるか
    pub fn bottom_exists(&self) -> bool {
        match self.bottom_kind {
            crate::result_grid::BottomKind::Grep => self.grep.is_some(),
            crate::result_grid::BottomKind::Result => !self.db_results.is_empty(),
        }
    }

    /// 下のパネルが出ているか
    pub fn bottom_visible(&self) -> bool {
        self.bottom_exists() && !self.bottom_hidden
    }

    /// 画面にあるボタン（ボタン, 行, 列）。幅はどれも 1 セル
    pub fn toggle_buttons(&self) -> Vec<(Button, usize, usize)> {
        let mut v = Vec::new();
        match self.left_rect() {
            Some(r) => v.push((Button::HideLeft, r.y, r.x + r.w - 2)),
            None if self.diff.is_none() && self.log.is_none() && self.db_table.is_none() => v.push((Button::ShowLeft, 0, 0)),
            None => {}
        }
        match self.panel_rect() {
            Some(r) => v.push((Button::HideBottom, r.y, r.x + r.w - 2)),
            None if self.bottom_exists() && self.diff.is_none() && self.log.is_none() && self.db_table.is_none() => {
                v.push((Button::ShowBottom, self.windows_rows().saturating_sub(1), self.windows_x().0))
            }
            None => {}
        }
        // 右の欄: タブの行の右端に「隠す」、隠している間は右上の隅に「出す」
        match self.right_rect() {
            Some(r) => v.push((Button::HideRight, r.y, r.x + r.w - 2)),
            // 右の欄はまだ開いていなくても出す（押すと最後に使ったタブ。初めてならデータベース）
            None if self.diff.is_none() && self.log.is_none() && self.db_table.is_none() => v.push((Button::ShowRight, 0, self.screen.0.saturating_sub(1))),
            None => {}
        }
        v
    }

    pub fn button_at(&self, row: usize, col: usize) -> Option<Button> {
        self.toggle_buttons().into_iter().find(|&(_, r, c)| r == row && c == col).map(|b| b.0)
    }

    pub(crate) fn press_button(&mut self, b: Button) {
        match b {
            Button::HideLeft => self.hide_left(),
            Button::ShowLeft => self.select_left_tab(self.left_last()),
            Button::HideBottom => self.hide_bottom(),
            Button::ShowBottom => self.show_bottom(),
            Button::HideRight => self.hide_right(),
            Button::ShowRight => self.select_right_tab(self.right_last()),
        }
    }

    /// 左の欄を隠す（中身は残す）
    pub(crate) fn hide_left(&mut self) {
        self.left_hidden = true;
        if self.mode == Mode::CommitMsg || (self.mode == Mode::List && matches!(self.list_side, Side::Left | Side::Commit)) {
            self.mode = Mode::Normal;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    /// 下のパネルを隠す（結果は残す）
    pub(crate) fn hide_bottom(&mut self) {
        self.bottom_hidden = true;
        if self.mode == Mode::GrepPrompt || (self.mode == Mode::List && self.list_side == Side::Bottom) {
            self.mode = Mode::Normal;
        }
        self.rows = self.text_rows(self.active_rect());
    }

    pub(crate) fn show_bottom(&mut self) {
        self.bottom_hidden = false;
        self.focus_panel();
        self.rows = self.text_rows(self.active_rect());
    }

    /// Alt-1 / Alt-0: そのタブを出して移る。もう出ていて移っていれば隠す（IntelliJ と同じ）
    pub(crate) fn toggle_left_tab(&mut self, tab: LeftTab) {
        let focused = match tab {
            LeftTab::Project => self.mode == Mode::List && self.list_side == Side::Left,
            LeftTab::Commit => self.mode == Mode::CommitMsg || (self.mode == Mode::List && self.list_side == Side::Commit),
        };
        if self.left_visible() == Some(tab) && focused {
            self.hide_left();
        } else {
            self.select_left_tab(tab);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagi_core::Buffer;

    #[test]
    fn hide_and_show_left() {
        let dir = std::env::temp_dir().join(format!("nagi-toggles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let mut buf = Buffer::open(&dir.join("a.txt")).unwrap();
        buf.path = Some(dir.join("a.txt"));
        let mut e = Editor::new(buf, None);
        e.set_screen(120, 40);
        // 何も出ていなければ左上に「出す」
        assert_eq!(e.button_at(0, 0), Some(Button::ShowLeft));
        e.press_button(Button::ShowLeft);
        assert_eq!(e.left_visible(), Some(LeftTab::Project));
        assert_eq!(e.windows_x(), (37, 83));
        // タブの行の右端の「隠す」で隠れ、窓が広がる。ツリーは残る
        let (b, r, c) = e.toggle_buttons()[0];
        assert_eq!((b, r, c), (Button::HideLeft, 0, 34));
        e.press_button(b);
        assert_eq!((e.left_visible(), e.windows_x(), e.mode), (None, (0, 120), Mode::Normal));
        assert!(e.tree.is_some());
        // Alt-1 で出して移り、もう一度で隠す
        e.key(nagi_vim::Key::Alt('1'));
        assert_eq!((e.left_visible(), e.mode), (Some(LeftTab::Project), Mode::List));
        e.key(nagi_vim::Key::Alt('1'));
        assert_eq!(e.left_visible(), None);
        // 右上の隅には、右の欄を開いていなくても「出す」ボタン
        assert_eq!(e.button_at(0, 119), Some(Button::ShowRight));
        e.press_button(Button::ShowRight);
        assert_eq!(e.right_visible(), Some(crate::right_panel::RightTab::Database));
        // 右の欄の中で : を押しても、コマンドの候補が出る
        e.key(nagi_vim::Key::Char(':'));
        assert_eq!(e.mode, Mode::Command);
        assert!(e.cmd_complete.as_ref().is_some_and(|c| c.items.len() > 20), "パネルの中の : でも候補を出す");
        let (_, r, c) = e.toggle_buttons().into_iter().find(|b| b.0 == Button::HideRight).unwrap();
        e.key(nagi_vim::Key::Esc);
        e.press_button(Button::HideRight);
        assert_eq!(e.right_visible(), None);
        let _ = (r, c);
    }
}
